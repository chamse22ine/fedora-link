//! Notifications du téléphone : affichage GNOME, historique, fermeture synchronisée.

use super::{desktop_notify, Plugin, PluginCtx};
use crate::state::Event;
use anyhow::{Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};

const TYPE_NOTIFICATION: &str = "kdeconnect.notification";
const TYPE_REQUEST: &str = "kdeconnect.notification.request";
const TYPE_REPLY: &str = "kdeconnect.notification.reply";

pub struct NotificationPlugin;

impl Plugin for NotificationPlugin {
    fn name(&self) -> &'static str {
        "notification"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_NOTIFICATION]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST, TYPE_REPLY]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        // Demande les notifications déjà affichées sur le téléphone.
        let _ = ctx.send(NetworkPacket::new(TYPE_REQUEST, json!({ "request": true })));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let body = &packet.body;
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .context("id manquant")?;

        if body
            .get("isCancel")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            ctx.state.remove_notification(&ctx.device_id, id);
            ctx.emit(Event::NotificationDismissed {
                device: ctx.device_id.clone(),
                id: id.to_owned(),
            });
            return Ok(());
        }

        let text_of = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let (app, title, text) = (text_of("appName"), text_of("title"), text_of("text"));

        ctx.state.push_notification(&ctx.device_id, body.clone());
        ctx.emit(Event::NotificationPosted {
            device: ctx.device_id.clone(),
            json: body.to_string(),
        });

        // Les notifications de la synchro initiale sont marquées `silent`.
        let silent = body.get("silent").and_then(Value::as_bool).unwrap_or(false);
        if !silent {
            let summary = if title.is_empty() {
                app.clone()
            } else {
                format!("{app} — {title}")
            };
            desktop_notify(app, summary, text);
        }
        Ok(())
    }
}
