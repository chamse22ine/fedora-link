//! Appels : notification GNOME à la sonnerie (avec « Couper la sonnerie »), et option
//! pour mettre en pause ou baisser le son du PC pendant l'appel.
//! Protocole : `kdeconnect.telephony` (téléphone → PC), `kdeconnect.telephony.request_mute`.

use super::{desktop_notify, Plugin, PluginCtx};
use crate::desktop::{escape_markup, ActionNotification};
use crate::pcaudio;
use crate::settings::CallMedia;
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

const TYPE_TELEPHONY: &str = "kdeconnect.telephony";
const TYPE_MUTE: &str = "kdeconnect.telephony.request_mute";
const MUTE_ACTION: &str = "mute";
/// Une sonnerie ne dure pas plus longtemps : au-delà, on cesse d'attendre un clic.
const MAX_RING: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Caller {
    pub number: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallEvent {
    Ringing(Caller),
    /// Décroché (côté téléphone).
    Talking,
    Missed(Caller),
    /// Fin de l'appel ou sonnerie interrompue.
    Cancel,
}

/// Interprète le corps d'un paquet `kdeconnect.telephony`.
pub(crate) fn parse(body: &Value) -> Option<CallEvent> {
    if body.get("isCancel").and_then(Value::as_bool) == Some(true) {
        return Some(CallEvent::Cancel);
    }
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let number = text("phoneNumber").unwrap_or("numéro inconnu");
    let caller = || Caller {
        number: number.to_owned(),
        name: text("contactName").unwrap_or(number).to_owned(),
    };
    match body.get("event").and_then(Value::as_str)? {
        "ringing" => Some(CallEvent::Ringing(caller())),
        "talking" => Some(CallEvent::Talking),
        "missedCall" => Some(CallEvent::Missed(caller())),
        // « sms » : ancien format, remplacé par le plugin SMS.
        _ => None,
    }
}

#[derive(Default)]
pub struct TelephonyPlugin {
    worker: OnceLock<mpsc::UnboundedSender<(PluginCtx, CallEvent)>>,
}

impl Plugin for TelephonyPlugin {
    fn name(&self) -> &'static str {
        "telephony"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_TELEPHONY]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_MUTE]
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let Some(event) = parse(&packet.body) else {
            return Ok(());
        };
        // Un seul travailleur, pour que « sonne » puis « décroché » soient traités dans l'ordre.
        let tx = self.worker.get_or_init(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(run_worker(rx));
            tx
        });
        if tx.send((ctx.clone(), event)).is_err() {
            warn!("traitement des appels arrêté");
        }
        Ok(())
    }
}

/// Ce qu'il faut défaire à la fin de l'appel, par téléphone.
#[derive(Default)]
struct CallState {
    ring: Option<(u32, JoinHandle<()>)>,
    paused: Vec<String>,
    volume: Option<f32>,
}

async fn run_worker(mut rx: mpsc::UnboundedReceiver<(PluginCtx, CallEvent)>) {
    let mut calls: HashMap<String, CallState> = HashMap::new();
    while let Some((ctx, event)) = rx.recv().await {
        let call = calls.entry(ctx.device_id.clone()).or_default();
        match event {
            CallEvent::Ringing(caller) => {
                close_ring(call).await;
                info!(device = %ctx.device_name, "appel entrant");
                duck_media(&ctx, call).await;
                ring(&ctx, call, &caller).await;
            }
            CallEvent::Talking => close_ring(call).await,
            CallEvent::Cancel => {
                close_ring(call).await;
                restore_media(call).await;
            }
            CallEvent::Missed(caller) => {
                close_ring(call).await;
                restore_media(call).await;
                desktop_notify(
                    "Fedora Link".into(),
                    format!("Appel manqué — {}", ctx.device_name),
                    caller.name,
                );
            }
        }
    }
}

async fn close_ring(call: &mut CallState) {
    if let Some((id, task)) = call.ring.take() {
        task.abort();
        crate::desktop::close_notification(id).await;
    }
}

async fn ring(ctx: &PluginCtx, call: &mut CallState, caller: &Caller) {
    let body = if caller.name == caller.number {
        escape_markup(&caller.number)
    } else {
        format!("{}\n{}", escape_markup(&caller.name), escape_markup(&caller.number))
    };
    let summary = format!("Appel entrant — {}", ctx.device_name);
    let mut notif = match ActionNotification::show(
        &summary,
        &body,
        &[(MUTE_ACTION, "Couper la sonnerie")],
        "call-start-symbolic",
        true,
    )
    .await
    {
        Ok(n) => n,
        Err(e) => return warn!("notification d'appel : {e:#}"),
    };
    let id = notif.id;
    let ctx = ctx.clone();
    let task = tokio::spawn(async move {
        let clicked = tokio::time::timeout(MAX_RING, notif.wait_action()).await;
        if let Ok(Some(action)) = clicked {
            if action == MUTE_ACTION {
                if let Err(e) = ctx.send(NetworkPacket::new(TYPE_MUTE, json!({ "action": "mute" }))) {
                    warn!("sonnerie non coupée : {e:#}");
                }
                notif.close().await;
            }
        }
    });
    call.ring = Some((id, task));
}

async fn duck_media(ctx: &PluginCtx, call: &mut CallState) {
    // Déjà fait pour cet appel (sonnerie répétée) : on ne ré-enregistre rien.
    if !call.paused.is_empty() || call.volume.is_some() {
        return;
    }
    match ctx.state.call_media() {
        CallMedia::None => {}
        CallMedia::Pause => match pcaudio::pause_playing_players().await {
            Ok(names) => call.paused = names,
            Err(e) => warn!("pause des lecteurs : {e:#}"),
        },
        CallMedia::Lower => match pcaudio::lower_volume().await {
            Ok(v) => call.volume = Some(v),
            Err(e) => warn!("baisse du volume : {e:#}"),
        },
    }
}

async fn restore_media(call: &mut CallState) {
    if !call.paused.is_empty() {
        pcaudio::resume_players(&std::mem::take(&mut call.paused)).await;
    }
    if let Some(v) = call.volume.take() {
        pcaudio::restore_volume(v).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_link_core::packet::NetworkPacket;

    fn body(line: &str) -> Value {
        NetworkPacket::from_line(line).unwrap().body
    }

    // Paquets tels qu'émis par KDE Connect Android.
    #[test]
    fn sonnerie() {
        let b = body(
            r#"{"id":1729504200123,"type":"kdeconnect.telephony","body":{"event":"ringing","phoneNumber":"+33612345678","contactName":"Alice Martin","phoneThumbnail":"/9j/4AAQSkZJRg=="}}"#,
        );
        assert_eq!(
            parse(&b),
            Some(CallEvent::Ringing(Caller {
                number: "+33612345678".into(),
                name: "Alice Martin".into()
            }))
        );
    }

    #[test]
    fn numero_inconnu_sert_de_nom() {
        let b = body(
            r#"{"id":1,"type":"kdeconnect.telephony","body":{"event":"ringing","phoneNumber":"0123456789","contactName":""}}"#,
        );
        assert_eq!(
            parse(&b),
            Some(CallEvent::Ringing(Caller {
                number: "0123456789".into(),
                name: "0123456789".into()
            }))
        );
    }

    #[test]
    fn decroche_manque_et_fin() {
        let talking = body(r#"{"id":2,"type":"kdeconnect.telephony","body":{"event":"talking","phoneNumber":"+33612345678","contactName":"Alice"}}"#);
        assert_eq!(parse(&talking), Some(CallEvent::Talking));
        let missed = body(r#"{"id":3,"type":"kdeconnect.telephony","body":{"event":"missedCall","phoneNumber":"+33612345678","contactName":"Alice"}}"#);
        assert!(matches!(parse(&missed), Some(CallEvent::Missed(c)) if c.name == "Alice"));
        let cancel = body(r#"{"id":4,"type":"kdeconnect.telephony","body":{"event":"talking","phoneNumber":"+33612345678","isCancel":true}}"#);
        assert_eq!(parse(&cancel), Some(CallEvent::Cancel));
    }

    #[test]
    fn ancien_format_sms_ignore() {
        let b = body(r#"{"id":5,"type":"kdeconnect.telephony","body":{"event":"sms","phoneNumber":"+33612345678","messageBody":"salut"}}"#);
        assert_eq!(parse(&b), None);
    }
}
