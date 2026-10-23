//! SMS/MMS : conversations, envoi, pièces jointes, notification d'un nouveau message.
//! Protocole : `kdeconnect.sms.request_conversations` / `.request_conversation` → `.messages`,
//! `.request` (envoi), `.request_attachment` → `.attachment_file`.

use super::{share, Plugin, PluginCtx};
use crate::desktop::{self, escape_markup, ActionNotification};
use crate::sms::{self, Message};
use crate::state::Event;
use anyhow::{Context, Result};
use base64::Engine;
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

const TYPE_MESSAGES: &str = "kdeconnect.sms.messages";
const TYPE_ATTACHMENT_FILE: &str = "kdeconnect.sms.attachment_file";
pub(crate) const TYPE_REQUEST: &str = "kdeconnect.sms.request";
pub(crate) const TYPE_REQUEST_CONVERSATIONS: &str = "kdeconnect.sms.request_conversations";
pub(crate) const TYPE_REQUEST_CONVERSATION: &str = "kdeconnect.sms.request_conversation";
const TYPE_REQUEST_ATTACHMENT: &str = "kdeconnect.sms.request_attachment";
const SMS_REQUEST_VERSION: u32 = 2;
const MAX_ATTACHMENT_BYTES: u64 = 25 << 20;
/// Images demandées automatiquement par paquet de messages.
const MAX_AUTO_ATTACHMENTS: usize = 10;
const NOTIFICATION_LIFETIME: Duration = Duration::from_secs(600);

pub struct SmsPlugin;

impl Plugin for SmsPlugin {
    fn name(&self) -> &'static str {
        "sms"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_MESSAGES, TYPE_ATTACHMENT_FILE]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[
            TYPE_REQUEST,
            TYPE_REQUEST_CONVERSATIONS,
            TYPE_REQUEST_CONVERSATION,
            TYPE_REQUEST_ATTACHMENT,
        ]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        let _ = ctx.send(NetworkPacket::new(TYPE_REQUEST_CONVERSATIONS, json!({})));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        match packet.kind.as_str() {
            TYPE_MESSAGES => on_messages(ctx, &packet.body),
            TYPE_ATTACHMENT_FILE => on_attachment_file(ctx, packet),
            _ => Ok(()),
        }
    }
}

/// Corps du paquet d'envoi d'un SMS (format version 2).
pub(crate) fn send_body(addresses: &[String], text: &str) -> Value {
    json!({
        "version": SMS_REQUEST_VERSION,
        "addresses": addresses.iter().map(|a| json!({ "address": a })).collect::<Vec<_>>(),
        "messageBody": text,
    })
}

pub(crate) fn conversation_request_body(thread: i64, count: i64) -> Value {
    json!({ "threadID": thread, "rangeStartTimestamp": -1, "numberToRequest": count })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_image(mime: &str) -> bool {
    mime.starts_with("image/")
}

fn on_messages(ctx: &PluginCtx, body: &Value) -> Result<()> {
    let messages = sms::parse_messages(body);
    if messages.is_empty() {
        return Ok(());
    }
    let dir = ctx.state.sms_dir(&ctx.device_id);
    let store = &ctx.state.sms;

    // Vignettes et pièces jointes déjà en cache : on les rattache avant d'avertir l'interface.
    for m in &messages {
        for a in &m.attachments {
            let name = share::sanitize(&a.unique_identifier);
            if let Some(b64) = &a.thumbnail_b64 {
                if let Err(e) = write_thumbnail(&dir, &name, b64) {
                    debug!("vignette ignorée : {e:#}");
                }
            }
        }
    }
    let added = store.insert(&ctx.device_id, messages.clone());
    for m in &messages {
        for a in &m.attachments {
            let name = share::sanitize(&a.unique_identifier);
            let thumb = dir.join(format!("thumb-{name}"));
            if thumb.exists() {
                store.set_thumbnail(&ctx.device_id, &a.unique_identifier, &thumb.display().to_string());
            }
            let full = dir.join(&name);
            if full.exists() {
                store.set_attachment_path(&ctx.device_id, &a.unique_identifier, &full.display().to_string());
            }
        }
    }

    let threads: BTreeSet<i64> = messages.iter().map(|m| m.thread_id).collect();
    for thread in threads {
        ctx.emit(Event::SmsChanged {
            device: ctx.device_id.clone(),
            thread,
        });
    }

    let now = now_ms();
    for m in added.iter().filter(|m| sms::is_fresh_incoming(m, now)) {
        notify_incoming(ctx, m);
    }
    request_images(ctx, &added, &dir);
    Ok(())
}

fn write_thumbnail(dir: &std::path::Path, name: &str, b64: &str) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("vignette illisible")?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("thumb-{name}")), bytes)?;
    Ok(())
}

/// Demande au téléphone les images des messages reçus, sauf celles qu'on a déjà.
fn request_images(ctx: &PluginCtx, added: &[Message], dir: &std::path::Path) {
    let wanted = added
        .iter()
        .flat_map(|m| &m.attachments)
        .filter(|a| is_image(&a.mime_type) && !dir.join(share::sanitize(&a.unique_identifier)).exists())
        .take(MAX_AUTO_ATTACHMENTS);
    for a in wanted {
        let _ = ctx.send(NetworkPacket::new(
            TYPE_REQUEST_ATTACHMENT,
            json!({ "part_id": a.part_id, "unique_identifier": a.unique_identifier }),
        ));
    }
}

fn on_attachment_file(ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
    let filename = packet
        .body
        .get("filename")
        .and_then(Value::as_str)
        .context("nom de fichier manquant")?;
    let thread = packet.body.get("thread_id").and_then(Value::as_i64).unwrap_or(0);
    let port = packet
        .payload_transfer_info
        .as_ref()
        .and_then(|i| i.get("port"))
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .context("port de transfert manquant")?;
    let (ctx, unique) = (ctx.clone(), filename.to_owned());
    tokio::spawn(async move {
        let result: Result<()> = async {
            let data = share::fetch_bytes(&ctx, port, MAX_ATTACHMENT_BYTES).await?;
            let dir = ctx.state.sms_dir(&ctx.device_id);
            tokio::fs::create_dir_all(&dir).await?;
            let path = dir.join(share::sanitize(&unique));
            tokio::fs::write(&path, data).await?;
            ctx.state
                .sms
                .set_attachment_path(&ctx.device_id, &unique, &path.display().to_string());
            ctx.emit(Event::SmsChanged {
                device: ctx.device_id.clone(),
                thread,
            });
            Ok(())
        }
        .await;
        if let Err(e) = result {
            warn!(file = %unique, "pièce jointe : {e:#}");
        }
    });
    Ok(())
}

fn notify_incoming(ctx: &PluginCtx, m: &Message) {
    let state = ctx.state.clone();
    let (device, thread) = (ctx.device_id.clone(), m.thread_id);
    let name = state.display_name(&device, m.sender());
    let text = if !m.body.is_empty() {
        m.body.clone()
    } else if m.attachments.is_empty() {
        String::new()
    } else {
        "Pièce jointe".to_owned()
    };
    tokio::spawn(async move {
        let shown = ActionNotification::show(
            &name,
            &escape_markup(&text),
            &[("default", ""), ("reply", "Répondre")],
            "chat-bubble-text-symbolic",
            false,
        )
        .await;
        let mut notif = match shown {
            Ok(n) => n,
            Err(e) => return warn!("notification de SMS : {e:#}"),
        };
        let clicked = tokio::time::timeout(NOTIFICATION_LIFETIME, notif.wait_action()).await;
        if let Ok(Some(_)) = clicked {
            if let Err(e) = desktop::open_gui_thread(&device, thread).await {
                warn!("ouverture de la conversation : {e:#}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paquet_d_envoi() {
        let body = send_body(&["+33612345678".into(), "0698765432".into()], "Salut !");
        assert_eq!(
            body,
            json!({
                "version": 2,
                "addresses": [{"address": "+33612345678"}, {"address": "0698765432"}],
                "messageBody": "Salut !"
            })
        );
    }

    #[test]
    fn demande_de_conversation() {
        assert_eq!(
            conversation_request_body(132, 100),
            json!({"threadID": 132, "rangeStartTimestamp": -1, "numberToRequest": 100})
        );
    }
}
