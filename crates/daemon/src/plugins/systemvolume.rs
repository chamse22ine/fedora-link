//! Volume du PC piloté depuis le téléphone (sorties PipeWire, via `pactl`).
//! Protocole : `kdeconnect.systemvolume` (PC → téléphone : liste et changements),
//! `kdeconnect.systemvolume.request` (téléphone → PC : volume, muet, sortie par défaut).

use super::{Plugin, PluginCtx};
use crate::state::State;
use anyhow::{bail, Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde::Serialize;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, warn};

const TYPE_VOLUME: &str = "kdeconnect.systemvolume";
const TYPE_REQUEST: &str = "kdeconnect.systemvolume.request";
/// Échelle annoncée au téléphone : le volume va de 0 à 100.
const MAX_VOLUME: i32 = 100;
/// Les événements arrivent en rafale pendant un glissement : on attend le calme avant de relire.
const DEBOUNCE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub muted: bool,
    pub volume: i32,
    pub enabled: bool,
}

/// Lit la sortie de `pactl --format=json list sinks`.
pub(crate) fn parse_sinks(json: &str, default_sink: &str) -> Result<Vec<Sink>> {
    let list: Vec<Value> = serde_json::from_str(json).context("sortie de pactl illisible")?;
    Ok(list
        .iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_owned();
            // « 65% » du premier canal ; au-delà de 100 % on plafonne à l'échelle annoncée.
            let volume = s
                .get("volume")?
                .as_object()?
                .values()
                .next()
                .and_then(|c| c.get("value_percent")?.as_str())
                .and_then(|p| p.trim_end_matches('%').trim().parse::<i32>().ok())
                .unwrap_or(0)
                .clamp(0, MAX_VOLUME);
            Some(Sink {
                description: s
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_owned(),
                muted: s.get("mute").and_then(Value::as_bool).unwrap_or(false),
                enabled: name == default_sink,
                volume,
                name,
            })
        })
        .collect())
}

pub(crate) fn sink_list_body(sinks: &[Sink]) -> Value {
    let list: Vec<Value> = sinks
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "muted": s.muted,
                "volume": s.volume,
                "maxVolume": MAX_VOLUME,
                "enabled": s.enabled,
            })
        })
        .collect();
    json!({ "sinkList": list })
}

/// Paquets à envoyer pour passer de `old` à `new` : un petit paquet par changement,
/// ou la liste complète si l'ensemble des sorties a changé.
pub(crate) fn diff(old: &[Sink], new: &[Sink]) -> Vec<Value> {
    let same_set = old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| a.name == b.name && a.description == b.description);
    if !same_set {
        return vec![sink_list_body(new)];
    }
    let mut out = Vec::new();
    for (a, b) in old.iter().zip(new) {
        if a.volume != b.volume {
            out.push(json!({ "name": b.name, "volume": b.volume }));
        }
        if a.muted != b.muted {
            out.push(json!({ "name": b.name, "muted": b.muted }));
        }
        if a.enabled != b.enabled {
            out.push(json!({ "name": b.name, "enabled": b.enabled }));
        }
    }
    out
}

async fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .await
        .context("pactl introuvable (pipewire-pulseaudio)")?;
    if !out.status.success() {
        bail!("pactl {} a échoué", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(crate) async fn list_sinks() -> Result<Vec<Sink>> {
    let json = pactl(&["--format=json", "list", "sinks"]).await?;
    let default = pactl(&["get-default-sink"]).await.unwrap_or_default();
    parse_sinks(&json, default.trim())
}

pub struct SystemVolumePlugin {
    watcher: Once,
}

impl Default for SystemVolumePlugin {
    fn default() -> Self {
        Self {
            watcher: Once::new(),
        }
    }
}

impl Plugin for SystemVolumePlugin {
    fn name(&self) -> &'static str {
        "systemvolume"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_VOLUME]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        self.watcher.call_once(|| {
            tokio::spawn(watch(ctx.state.clone()));
        });
        send_list(ctx);
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let body = packet.body.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if body.get("requestSinks").is_some() {
                send_list(&ctx);
            } else if let Err(e) = apply(&body).await {
                warn!("volume du PC : {e:#}");
            }
        });
        Ok(())
    }
}

fn send_list(ctx: &PluginCtx) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        match list_sinks().await {
            Ok(sinks) => {
                let _ = ctx.send(NetworkPacket::new(TYPE_VOLUME, sink_list_body(&sinks)));
            }
            Err(e) => warn!("sorties audio : {e:#}"),
        }
    });
}

/// Applique une demande du téléphone. Le nom doit être celui d'une sortie existante.
async fn apply(body: &Value) -> Result<()> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .context("nom de sortie manquant")?;
    if !list_sinks().await?.iter().any(|s| s.name == name) {
        bail!("sortie audio inconnue : {name}");
    }
    if let Some(v) = body.get("volume").and_then(Value::as_i64) {
        let v = v.clamp(0, i64::from(MAX_VOLUME));
        pactl(&["set-sink-volume", name, &format!("{v}%")]).await?;
    }
    if let Some(m) = body.get("muted").and_then(Value::as_bool) {
        pactl(&["set-sink-mute", name, if m { "1" } else { "0" }]).await?;
    }
    if body.get("enabled").and_then(Value::as_bool) == Some(true) {
        pactl(&["set-default-sink", name]).await?;
    }
    Ok(())
}

/// Surveille PipeWire et prévient les téléphones des changements faits sur le PC.
async fn watch(state: Arc<State>) {
    let mut last: Vec<Sink> = list_sinks().await.unwrap_or_default();
    loop {
        let child = Command::new("pactl")
            .arg("subscribe")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let Ok(mut child) = child else {
            debug!("pactl subscribe indisponible : volume non surveillé");
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !(line.contains(" on sink #") || line.contains(" on server")) {
                continue;
            }
            // Laisse passer la rafale d'événements avant de relire l'état.
            let quiet = tokio::time::timeout(DEBOUNCE, async {
                while let Ok(Some(_)) = lines.next_line().await {}
            });
            let _ = quiet.await;
            if let Ok(now) = list_sinks().await {
                for body in diff(&last, &now) {
                    state.broadcast("systemvolume", || NetworkPacket::new(TYPE_VOLUME, body.clone()));
                }
                last = now;
            }
        }
        // pactl s'est arrêté (serveur audio redémarré) : on réessaie.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Extrait réel de `pactl --format=json list sinks` sous PipeWire.
    const PACTL: &str = r#"[
      {"index":47,"state":"SUSPENDED","name":"alsa_output.hdmi","description":"HDMI / DisplayPort 3 Output","mute":false,
       "volume":{"front-left":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"front-right":{"value":65536,"value_percent":"100%","db":"0.00 dB"}}},
      {"index":52,"state":"RUNNING","name":"alsa_output.speaker","description":"Haut-parleurs","mute":true,
       "volume":{"front-left":{"value":42598,"value_percent":"65%","db":"-11.2 dB"},"front-right":{"value":42598,"value_percent":"65%","db":"-11.2 dB"}}},
      {"index":53,"name":"alsa_output.boost","description":"Sortie amplifiée","mute":false,
       "volume":{"mono":{"value":98304,"value_percent":"150%","db":"10.0 dB"}}}
    ]"#;

    /// Lit les vraies sorties audio : `cargo test -- --ignored sorties_reelles --nocapture`.
    #[tokio::test]
    #[ignore = "nécessite PipeWire et pactl"]
    async fn sorties_reelles() {
        let sinks = list_sinks().await.expect("sorties audio");
        assert!(!sinks.is_empty());
        assert_eq!(sinks.iter().filter(|s| s.enabled).count(), 1, "une seule sortie par défaut");
        println!("{}", sink_list_body(&sinks));
    }

    #[test]
    fn lit_les_sorties_pactl() {
        let sinks = parse_sinks(PACTL, "alsa_output.speaker").unwrap();
        assert_eq!(sinks.len(), 3);
        assert_eq!((sinks[0].volume, sinks[0].enabled, sinks[0].muted), (100, false, false));
        assert_eq!((sinks[1].volume, sinks[1].enabled, sinks[1].muted), (65, true, true));
        assert_eq!(sinks[1].description, "Haut-parleurs");
        assert_eq!(sinks[2].volume, 100, "plafonné à l'échelle annoncée");
        assert!(parse_sinks("pas du json", "").is_err());
    }

    #[test]
    fn paquet_de_liste_au_format_kde_connect() {
        let sinks = parse_sinks(PACTL, "alsa_output.speaker").unwrap();
        let body = sink_list_body(&sinks[1..2]);
        assert_eq!(
            body,
            json!({"sinkList": [{"name": "alsa_output.speaker", "description": "Haut-parleurs",
                "muted": true, "volume": 65, "maxVolume": 100, "enabled": true}]})
        );
    }

    #[test]
    fn changements_faits_sur_le_pc() {
        let old = parse_sinks(PACTL, "alsa_output.speaker").unwrap();
        let mut new = old.clone();
        new[1].volume = 40;
        new[1].muted = false;
        assert_eq!(
            diff(&old, &new),
            vec![
                json!({"name": "alsa_output.speaker", "volume": 40}),
                json!({"name": "alsa_output.speaker", "muted": false})
            ]
        );
        new[0].enabled = true;
        new[1].enabled = false;
        assert_eq!(diff(&old, &new).len(), 4);
        // Une sortie branchée : liste complète.
        let mut plus = new.clone();
        plus.push(new[0].clone());
        assert_eq!(diff(&new, &plus).len(), 1);
        assert!(diff(&new, &plus)[0].get("sinkList").is_some());
        assert!(diff(&old, &old).is_empty());
    }
}
