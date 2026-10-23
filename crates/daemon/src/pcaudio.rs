//! Son du PC : pause/reprise des lecteurs MPRIS et baisse du volume (PipeWire via `wpctl`).

use crate::desktop::session;
use anyhow::{bail, Context, Result};
use tracing::{debug, warn};
use zbus::fdo::DBusProxy;
use zbus::Proxy;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// Préfixe de nos propres lecteurs (téléphone), à ne pas mettre en pause à la place du PC.
pub(crate) const OWN_PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.fedoralink_";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";
/// Part du volume d'origine conservée pendant un appel.
const LOWERED_FACTOR: f32 = 0.2;

async fn player_proxy(name: &str) -> Result<Proxy<'static>> {
    let conn = session().await?;
    Ok(Proxy::new(&conn, name.to_owned(), MPRIS_PATH, PLAYER_IFACE).await?)
}

/// Met en pause les lecteurs du PC qui jouent ; rend uniquement ceux-là.
pub(crate) async fn pause_playing_players() -> Result<Vec<String>> {
    let conn = session().await?;
    let names = DBusProxy::new(&conn).await?.list_names().await?;
    let mut paused = Vec::new();
    for name in names.iter().map(|n| n.as_str()).filter(|n| n.starts_with(MPRIS_PREFIX) && !n.starts_with(OWN_PLAYER_PREFIX)) {
        let result: Result<bool> = async {
            let player = player_proxy(name).await?;
            let status: String = player.get_property("PlaybackStatus").await?;
            if status != "Playing" {
                return Ok(false);
            }
            player.call_method("Pause", &()).await?;
            Ok(true)
        }
        .await;
        match result {
            Ok(true) => paused.push(name.to_owned()),
            Ok(false) => {}
            Err(e) => debug!(player = name, "lecteur ignoré : {e:#}"),
        }
    }
    Ok(paused)
}

/// Relance les lecteurs mis en pause par nous, s'ils sont toujours en pause.
pub(crate) async fn resume_players(names: &[String]) {
    for name in names {
        let result: Result<()> = async {
            let player = player_proxy(name).await?;
            let status: String = player.get_property("PlaybackStatus").await?;
            if status == "Paused" {
                player.call_method("Play", &()).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            debug!(player = %name, "reprise impossible : {e:#}");
        }
    }
}

/// `Volume: 0.65` ou `Volume: 0.65 [MUTED]` → 0.65.
pub(crate) fn parse_wpctl_volume(output: &str) -> Option<f32> {
    output
        .trim()
        .strip_prefix("Volume:")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

async fn wpctl(args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("wpctl")
        .args(args)
        .output()
        .await
        .context("wpctl introuvable (PipeWire)")?;
    if !out.status.success() {
        bail!("wpctl {} a échoué", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Baisse le volume de la sortie par défaut ; rend le volume d'origine à rétablir.
pub(crate) async fn lower_volume() -> Result<f32> {
    let original = parse_wpctl_volume(&wpctl(&["get-volume", DEFAULT_SINK]).await?)
        .context("volume illisible")?;
    wpctl(&["set-volume", DEFAULT_SINK, &format!("{:.2}", original * LOWERED_FACTOR)]).await?;
    Ok(original)
}

pub(crate) async fn restore_volume(volume: f32) {
    if let Err(e) = wpctl(&["set-volume", DEFAULT_SINK, &format!("{volume:.2}")]).await {
        warn!("volume non rétabli : {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_le_volume_wpctl() {
        assert_eq!(parse_wpctl_volume("Volume: 0.65\n"), Some(0.65));
        assert_eq!(parse_wpctl_volume("Volume: 0.30 [MUTED]\n"), Some(0.30));
        assert_eq!(parse_wpctl_volume("erreur"), None);
    }
}
