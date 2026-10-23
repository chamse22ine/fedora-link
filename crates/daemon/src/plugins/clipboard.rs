//! Presse-papiers : le daemon relaie le texte ; la lecture/écriture du presse-papiers
//! système se fait côté interface (GNOME/Wayland ne permet pas de l'observer en arrière-plan).

use super::{Plugin, PluginCtx};
use crate::state::Event;
use anyhow::{Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

const TYPE_CLIPBOARD: &str = "kdeconnect.clipboard";
const TYPE_CONNECT: &str = "kdeconnect.clipboard.connect";

#[derive(Default)]
pub struct ClipboardPlugin {
    /// Horodatage du dernier contenu appliqué, par appareil.
    last: Mutex<HashMap<String, i64>>,
}

impl Plugin for ClipboardPlugin {
    fn name(&self) -> &'static str {
        "clipboard"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_CLIPBOARD, TYPE_CONNECT]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_CLIPBOARD, TYPE_CONNECT]
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let text = packet
            .body
            .get("content")
            .and_then(Value::as_str)
            .context("content manquant")?;
        if packet.kind == TYPE_CONNECT {
            // Synchro à la connexion : on n'applique qu'un contenu plus récent que le dernier reçu.
            let ts = packet
                .body
                .get("timestamp")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if ts == 0 || ts <= last.get(&ctx.device_id).copied().unwrap_or(0) {
                return Ok(());
            }
            last.insert(ctx.device_id.clone(), ts);
        }
        ctx.emit(Event::ClipboardReceived {
            device: ctx.device_id.clone(),
            text: text.to_owned(),
        });
        Ok(())
    }
}
