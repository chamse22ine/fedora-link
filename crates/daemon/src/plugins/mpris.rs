//! Contrôle multimédia : pilote les lecteurs du téléphone (lecture, volume, position)
//! et récupère titre, artiste et pochette. Protocole : `kdeconnect.mpris(.request)`.

use super::{share, Plugin, PluginCtx};
use anyhow::{Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use tracing::{debug, warn};

const TYPE_MPRIS: &str = "kdeconnect.mpris";
const TYPE_REQUEST: &str = "kdeconnect.mpris.request";
/// Une pochette JPEG dépasse rarement quelques centaines de Ko.
const MAX_ART_BYTES: u64 = 8 << 20;

pub struct MprisPlugin;

fn request(ctx: &PluginCtx, body: Value) {
    if let Err(e) = ctx.send(NetworkPacket::new(TYPE_REQUEST, body)) {
        debug!("requête multimédia : {e:#}");
    }
}

impl Plugin for MprisPlugin {
    fn name(&self) -> &'static str {
        "mpris"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_MPRIS]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        request(ctx, json!({ "requestPlayerList": true }));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let body = &packet.body;

        if let Some(list) = body.get("playerList").and_then(Value::as_array) {
            let names: Vec<String> = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            ctx.state.set_players(&ctx.device_id, &names);
            for name in &names {
                request(ctx, json!({ "player": name, "requestNowPlaying": true }));
            }
            return Ok(());
        }

        let player = body
            .get("player")
            .and_then(Value::as_str)
            .context("lecteur manquant")?
            .to_owned();

        // Pochette : l'image arrive dans un transfert séparé.
        if body.get("transferringAlbumArt").and_then(Value::as_bool) == Some(true) {
            let url = body
                .get("albumArtUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let port = packet
                .payload_transfer_info
                .as_ref()
                .and_then(|i| i.get("port"))
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .context("port de transfert manquant")?;
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = save_art(&ctx, &player, &url, port).await {
                    warn!(%player, "pochette : {e:#}");
                }
            });
            return Ok(());
        }

        // Métadonnées : on fusionne, puis on demande la pochette si elle a changé.
        let url = body
            .get("albumArtUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut need_art = false;
        ctx.state.update_player(&ctx.device_id, &player, |m| {
            let (Some(target), Some(src)) = (m.as_object_mut(), body.as_object()) else {
                return;
            };
            for (k, v) in src {
                target.insert(k.clone(), v.clone());
            }
            let current = target
                .get("artRequested")
                .and_then(Value::as_str)
                .unwrap_or("");
            if url.is_empty() {
                target.remove("artPath");
                target.remove("artRequested");
            } else if current != url {
                target.remove("artPath");
                target.insert("artRequested".into(), json!(url));
                need_art = true;
            }
        });
        if need_art {
            request(ctx, json!({ "player": player, "albumArtUrl": url }));
        }
        Ok(())
    }
}

async fn save_art(ctx: &PluginCtx, player: &str, url: &str, port: u16) -> Result<()> {
    let data = share::fetch_bytes(ctx, port, MAX_ART_BYTES).await?;
    let dir = ctx.state.art_dir();
    tokio::fs::create_dir_all(&dir).await?;

    // Le nom de fichier dépend de l'URL : une nouvelle pochette = un nouveau fichier.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&ctx.device_id, player, url).hash(&mut hasher);
    let path = dir.join(format!("{:016x}.jpg", hasher.finish()));
    tokio::fs::write(&path, &data).await?;

    let path = path.display().to_string();
    ctx.state.update_player(&ctx.device_id, player, |m| {
        // Ignore une pochette arrivée après un changement de morceau.
        if m.get("artRequested").and_then(Value::as_str) == Some(url) {
            m["artPath"] = json!(path);
        }
    });
    Ok(())
}
