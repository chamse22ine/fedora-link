//! Contacts : synchronisation différentielle des fiches vCard du téléphone vers le cache local.
//! Protocole : `kdeconnect.contacts.request_all_uids_timestamps` → `response_uids_timestamps`,
//! puis `request_vcards_by_uid` → `response_vcards`.

use super::{Plugin, PluginCtx};
use crate::contacts::{plan_sync, valid_uid};
use crate::state::Event;
use anyhow::{Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};
use tracing::{info, warn};

const TYPE_REQUEST_TIMESTAMPS: &str = "kdeconnect.contacts.request_all_uids_timestamps";
const TYPE_REQUEST_VCARDS: &str = "kdeconnect.contacts.request_vcards_by_uid";
const TYPE_RESPONSE_TIMESTAMPS: &str = "kdeconnect.contacts.response_uids_timestamps";
const TYPE_RESPONSE_VCARDS: &str = "kdeconnect.contacts.response_vcards";
/// Nombre de fiches demandées par paquet (une fiche avec photo pèse vite plusieurs dizaines de Ko).
const BATCH: usize = 50;

pub struct ContactsPlugin;

impl Plugin for ContactsPlugin {
    fn name(&self) -> &'static str {
        "contacts"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_RESPONSE_TIMESTAMPS, TYPE_RESPONSE_VCARDS]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST_TIMESTAMPS, TYPE_REQUEST_VCARDS]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        ctx.state.contacts.reload(&ctx.device_id);
        let _ = ctx.send(NetworkPacket::new(TYPE_REQUEST_TIMESTAMPS, json!({})));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        match packet.kind.as_str() {
            TYPE_RESPONSE_TIMESTAMPS => on_timestamps(ctx, &packet.body),
            TYPE_RESPONSE_VCARDS => on_vcards(ctx, &packet.body),
            _ => Ok(()),
        }
    }
}

fn uids_of(body: &Value) -> Result<Vec<String>> {
    Ok(body
        .get("uids")
        .and_then(Value::as_array)
        .context("« uids » manquant")?
        .iter()
        .filter_map(|u| u.as_str().map(str::to_owned))
        .collect())
}

fn on_timestamps(ctx: &PluginCtx, body: &Value) -> Result<()> {
    let remote: Vec<(String, i64)> = uids_of(body)?
        .into_iter()
        .filter(|uid| valid_uid(uid))
        .map(|uid| {
            let ts = body
                .get(&uid)
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                .unwrap_or(0);
            (uid, ts)
        })
        .collect();
    let store = &ctx.state.contacts;
    let (fetch, delete) = plan_sync(&store.local_timestamps(&ctx.device_id), &remote);
    info!(device = %ctx.device_name, total = remote.len(), fetch = fetch.len(), delete = delete.len(), "synchro des contacts");

    for uid in &delete {
        store.delete(&ctx.device_id, uid);
    }
    if !delete.is_empty() {
        store.reload(&ctx.device_id);
        ctx.emit(Event::ContactsChanged(ctx.device_id.clone()));
    }
    for chunk in fetch.chunks(BATCH) {
        ctx.send(NetworkPacket::new(TYPE_REQUEST_VCARDS, json!({ "uids": chunk })))?;
    }
    Ok(())
}

fn on_vcards(ctx: &PluginCtx, body: &Value) -> Result<()> {
    let store = &ctx.state.contacts;
    let mut saved = 0;
    for uid in uids_of(body)? {
        let Some(vcard) = body.get(&uid).and_then(Value::as_str) else {
            continue;
        };
        match store.save(&ctx.device_id, &uid, vcard) {
            Ok(()) => saved += 1,
            Err(e) => warn!("contact {uid} : {e:#}"),
        }
    }
    store.reload(&ctx.device_id);
    info!(device = %ctx.device_name, saved, "fiches de contact reçues");
    ctx.emit(Event::ContactsChanged(ctx.device_id.clone()));
    Ok(())
}
