//! Pavé tactile et clavier : le téléphone pilote la souris et le clavier du PC.
//! Protocole : `kdeconnect.mousepad.request` (téléphone → PC), `kdeconnect.mousepad.keyboardstate`.

use super::{Plugin, PluginCtx};
use crate::input::{coalesce, plan, InputAction};
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use tracing::warn;

const TYPE_REQUEST: &str = "kdeconnect.mousepad.request";
const TYPE_KEYBOARDSTATE: &str = "kdeconnect.mousepad.keyboardstate";

#[derive(Default)]
pub struct MousepadPlugin {
    worker: OnceLock<mpsc::UnboundedSender<Vec<InputAction>>>,
}

impl Plugin for MousepadPlugin {
    fn name(&self) -> &'static str {
        "mousepad"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_KEYBOARDSTATE]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        // Le clavier du PC est disponible : le téléphone peut afficher son champ de saisie.
        let _ = ctx.send(NetworkPacket::new(TYPE_KEYBOARDSTATE, json!({ "state": true })));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let actions = plan(&packet.body);
        if actions.is_empty() {
            return Ok(());
        }
        // Un seul travailleur : l'ordre des touches et des clics doit être respecté.
        let tx = self.worker.get_or_init(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(run_worker(ctx.state.clone(), rx));
            tx
        });
        if tx.send(actions).is_err() {
            warn!("saisie à distance arrêtée");
        }
        Ok(())
    }
}

async fn run_worker(
    state: Arc<crate::state::State>,
    mut rx: mpsc::UnboundedReceiver<Vec<InputAction>>,
) {
    while let Some(mut batch) = rx.recv().await {
        // Rattrape le retard : tout ce qui attend est traité d'un coup, déplacements fusionnés.
        while let Ok(more) = rx.try_recv() {
            batch.extend(more);
        }
        if let Err(e) = state.input.apply(&coalesce(batch)).await {
            warn!("saisie à distance : {e:#}");
        }
    }
}
