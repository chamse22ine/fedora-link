//! Lecteurs du PC (Spotify, Firefox…) pilotables depuis le téléphone, avec pochette.
//! Protocole : `kdeconnect.mpris.request` (téléphone → PC), `kdeconnect.mpris` (PC → téléphone).
//! Le plugin « mpris » existant fait l'inverse : il pilote les lecteurs du téléphone.

use super::{share, Plugin, PluginCtx};
use crate::desktop::session;
use crate::state::State;
use anyhow::{bail, Context, Result};
use fedora_link_core::packet::NetworkPacket;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Once};
use std::time::Duration;
use tracing::{debug, warn};
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::InterfaceName;
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, MessageStream, Proxy};

const TYPE_MPRIS: &str = "kdeconnect.mpris";
const TYPE_REQUEST: &str = "kdeconnect.mpris.request";
const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const MAX_ART_BYTES: usize = 8 << 20;
const ACTIONS: [&str; 6] = ["Play", "Pause", "PlayPause", "Next", "Previous", "Stop"];
const LOOP_STATUS: [&str; 3] = ["None", "Track", "Playlist"];

/// Ce que le téléphone a besoin de savoir d'un lecteur.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PlayerState {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub url: String,
    pub length_ms: i64,
    pub pos_ms: i64,
    pub playing: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub loop_status: Option<String>,
    pub shuffle: Option<bool>,
    pub volume: i32,
}

/// Corps du paquet « en lecture », au format de KDE Connect.
pub(crate) fn now_playing_body(player: &str, s: &PlayerState) -> Value {
    let mut body = json!({
        "player": player,
        "title": s.title,
        "artist": s.artist,
        "album": s.album,
        "albumArtUrl": s.art_url,
        "length": s.length_ms,
        "url": s.url,
        "pos": s.pos_ms,
        "isPlaying": s.playing,
        "canPause": s.can_pause,
        "canPlay": s.can_play,
        "canGoNext": s.can_next,
        "canGoPrevious": s.can_previous,
        "canSeek": s.can_seek,
        "volume": s.volume,
    });
    if let Some(l) = &s.loop_status {
        body["loopStatus"] = json!(l);
    }
    if let Some(sh) = s.shuffle {
        body["shuffle"] = json!(sh);
    }
    body
}

pub(crate) fn player_list_body(names: &[String]) -> Value {
    json!({ "playerList": names, "supportAlbumArtPayload": true })
}

/// Noms affichés : l'identité du lecteur, suivie de « [n] » en cas de doublon.
pub(crate) fn unique_names(identities: &[(String, String)]) -> Vec<(String, String)> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    identities
        .iter()
        .map(|(bus, identity)| {
            let n = seen.entry(identity.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { identity.clone() } else { format!("{identity} [{n}]") };
            (name, bus.clone())
        })
        .collect()
}

fn get<T: TryFrom<OwnedValue>>(map: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    map.get(key)?.try_clone().ok()?.try_into().ok()
}

async fn players() -> Result<Vec<(String, String)>> {
    let conn = session().await?;
    let mut buses: Vec<String> = DBusProxy::new(&conn)
        .await?
        .list_names()
        .await?
        .iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(BUS_PREFIX))
        // Lecteurs relayés par KDE Connect / playerctld : ce serait un écho.
        .filter(|n| !n.starts_with("org.mpris.MediaPlayer2.kdeconnect.") && !n.ends_with(".playerctld"))
        .collect();
    buses.sort();
    let mut identities = Vec::new();
    for bus in buses {
        let identity = async {
            let p = Proxy::new(&conn, bus.clone(), PATH, ROOT_IFACE).await?;
            p.get_property::<String>("Identity").await
        }
        .await
        .unwrap_or_else(|_| bus.trim_start_matches(BUS_PREFIX).to_owned());
        identities.push((bus, identity));
    }
    Ok(unique_names(&identities))
}

async fn read_state(bus: &str) -> Result<PlayerState> {
    let conn = session().await?;
    let props = PropertiesProxy::builder(&conn)
        .destination(bus.to_owned())?
        .path(PATH)?
        .build()
        .await?;
    let all = props
        .get_all(InterfaceName::from_static_str_unchecked(PLAYER_IFACE))
        .await?;
    let meta: HashMap<String, OwnedValue> = get(&all, "Metadata").unwrap_or_default();
    let mut s = PlayerState {
        title: get(&meta, "xesam:title").unwrap_or_default(),
        album: get(&meta, "xesam:album").unwrap_or_default(),
        art_url: get(&meta, "mpris:artUrl").unwrap_or_default(),
        url: get(&meta, "xesam:url").unwrap_or_default(),
        artist: get::<Vec<String>>(&meta, "xesam:artist").unwrap_or_default().join(", "),
        length_ms: get::<i64>(&meta, "mpris:length")
            .or_else(|| get::<u64>(&meta, "mpris:length").map(|v| v as i64))
            .map_or(-1, |us| us / 1000),
        pos_ms: get::<i64>(&all, "Position").unwrap_or(0) / 1000,
        playing: get::<String>(&all, "PlaybackStatus").as_deref() == Some("Playing"),
        can_play: get(&all, "CanPlay").unwrap_or(false),
        can_pause: get(&all, "CanPause").unwrap_or(false),
        can_next: get(&all, "CanGoNext").unwrap_or(false),
        can_previous: get(&all, "CanGoPrevious").unwrap_or(false),
        can_seek: get(&all, "CanSeek").unwrap_or(false),
        loop_status: get(&all, "LoopStatus"),
        shuffle: get(&all, "Shuffle"),
        volume: (get::<f64>(&all, "Volume").unwrap_or(1.0) * 100.0).round() as i32,
    };
    // Lecteur sans titre (fichier local) : le nom du fichier fait l'affaire, comme dans KDE Connect.
    if s.title.is_empty() && s.artist.is_empty() {
        if let Some(name) = s.url.rsplit('/').next().filter(|n| s.url.starts_with("file://") && !n.is_empty()) {
            s.title = name.to_owned();
        }
    }
    Ok(s)
}

pub struct MprisPcPlugin {
    watcher: Once,
}

impl Default for MprisPcPlugin {
    fn default() -> Self {
        Self { watcher: Once::new() }
    }
}

impl Plugin for MprisPcPlugin {
    fn name(&self) -> &'static str {
        "mprispc"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_MPRIS]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        self.watcher.call_once(|| {
            tokio::spawn(watch(ctx.state.clone()));
        });
        let ctx = ctx.clone();
        tokio::spawn(async move { send_player_list(&ctx).await });
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let (ctx, body) = (ctx.clone(), packet.body.clone());
        tokio::spawn(async move {
            if let Err(e) = handle_request(&ctx, &body).await {
                debug!("lecteur du PC : {e:#}");
            }
        });
        Ok(())
    }
}

async fn send_player_list(ctx: &PluginCtx) {
    match players().await {
        Ok(list) => {
            let names: Vec<String> = list.into_iter().map(|(n, _)| n).collect();
            let _ = ctx.send(NetworkPacket::new(TYPE_MPRIS, player_list_body(&names)));
        }
        Err(e) => warn!("lecteurs du PC : {e:#}"),
    }
}

async fn handle_request(ctx: &PluginCtx, body: &Value) -> Result<()> {
    let str_of = |k: &str| body.get(k).and_then(Value::as_str);
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
    let list = players().await?;
    let player = str_of("player").unwrap_or("");
    let bus = list.iter().find(|(n, _)| n == player).map(|(_, b)| b.clone());

    // Demande d'une pochette : on ne sert que celle du lecteur, jamais une adresse choisie par le téléphone.
    if let (Some(requested), Some(bus)) = (str_of("albumArtUrl"), &bus) {
        return send_art(ctx, player, bus, requested).await;
    }
    let Some(bus) = bus else {
        send_player_list(ctx).await;
        return Ok(());
    };
    if flag("requestPlayerList") {
        send_player_list(ctx).await;
    }

    let conn = session().await?;
    let proxy = Proxy::new(&conn, bus.clone(), PATH, PLAYER_IFACE).await?;
    if let Some(action) = str_of("action") {
        if !ACTIONS.contains(&action) {
            bail!("action refusée : {action}");
        }
        proxy.call_method(action, &()).await?;
    }
    if let Some(l) = str_of("setLoopStatus").filter(|l| LOOP_STATUS.contains(l)) {
        proxy.set_property("LoopStatus", l).await?;
    }
    if let Some(sh) = body.get("setShuffle").and_then(Value::as_bool) {
        proxy.set_property("Shuffle", sh).await?;
    }
    if let Some(v) = body.get("setVolume").and_then(Value::as_i64) {
        proxy.set_property("Volume", v.clamp(0, 100) as f64 / 100.0).await?;
    }
    if let Some(offset) = body.get("Seek").and_then(Value::as_i64) {
        proxy.call_method("Seek", &(offset,)).await?;
    }
    if let Some(ms) = body.get("SetPosition").and_then(Value::as_i64) {
        // Le TrackId n'est pas toujours connu : on se déplace de l'écart, comme KDE Connect.
        let now: i64 = proxy.get_property("Position").await.unwrap_or(0);
        proxy.call_method("Seek", &(ms * 1000 - now,)).await?;
    }

    if flag("requestNowPlaying") {
        let state = read_state(&bus).await?;
        ctx.send(NetworkPacket::new(TYPE_MPRIS, now_playing_body(player, &state)))?;
    }
    if flag("requestVolume") {
        let state = read_state(&bus).await?;
        ctx.send(NetworkPacket::new(
            TYPE_MPRIS,
            json!({ "player": player, "volume": state.volume }),
        ))?;
    }
    Ok(())
}

/// Octets de la pochette : fichier local, ou adresse web récupérée avec `curl` (taille et durée bornées).
async fn fetch_art(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        let data = tokio::fs::read(path).await.context("pochette illisible")?;
        if data.len() > MAX_ART_BYTES {
            bail!("pochette trop volumineuse");
        }
        return Ok(data);
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("pochette : adresse non prise en charge");
    }
    let out = tokio::process::Command::new("curl")
        .args(["--silent", "--fail", "--location", "--proto", "=http,https"])
        .args(["--max-filesize", &MAX_ART_BYTES.to_string(), "--max-time", "10", "--", url])
        .output()
        .await
        .context("curl introuvable")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("pochette non téléchargée");
    }
    Ok(out.stdout)
}

async fn send_art(ctx: &PluginCtx, player: &str, bus: &str, requested: &str) -> Result<()> {
    let current = read_state(bus).await?.art_url;
    if requested.is_empty() || requested != current {
        bail!("pochette demandée différente de celle du lecteur");
    }
    let data = fetch_art(&current).await?;
    let packet = NetworkPacket::new(
        TYPE_MPRIS,
        json!({ "transferringAlbumArt": true, "player": player, "albumArtUrl": requested }),
    );
    share::offer_payload(ctx, packet, data).await
}

/// Prévient les téléphones quand un lecteur du PC change (titre, lecture, volume) ou apparaît/disparaît.
async fn watch(state: Arc<State>) {
    if let Err(e) = watch_inner(state).await {
        warn!("surveillance des lecteurs du PC arrêtée : {e:#}");
    }
}

async fn watch_inner(state: Arc<State>) -> Result<()> {
    let conn = session().await?;
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path(PATH)?
        .build();
    let mut changes = MessageStream::for_match_rule(rule, &conn, None).await?;
    let mut owners = DBusProxy::new(&conn).await?.receive_name_owner_changed().await?;

    loop {
        tokio::select! {
            Some(msg) = changes.next() => {
                let Ok(msg) = msg else { continue };
                let Some(sender) = msg.header().sender().map(|s| s.to_string()) else { continue };
                // Les propriétés changent par rafales (position, métadonnées) : on regroupe.
                tokio::time::sleep(Duration::from_millis(150)).await;
                if let Err(e) = push_player(&conn, &state, &sender).await {
                    debug!("lecteur du PC : {e:#}");
                }
            }
            Some(signal) = owners.next() => {
                let Ok(args) = signal.args() else { continue };
                if args.name().starts_with(BUS_PREFIX) {
                    if let Ok(list) = players().await {
                        let names: Vec<String> = list.into_iter().map(|(n, _)| n).collect();
                        state.broadcast("mprispc", || NetworkPacket::new(TYPE_MPRIS, player_list_body(&names)));
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

/// Envoie l'état du lecteur dont `sender` (nom unique D-Bus) est le propriétaire.
async fn push_player(conn: &zbus::Connection, state: &State, sender: &str) -> Result<()> {
    let dbus = DBusProxy::new(conn).await?;
    for (name, bus) in players().await? {
        let owner = dbus
            .get_name_owner(zbus::names::BusName::try_from(bus.as_str())?)
            .await?;
        if owner.as_str() == sender {
            let s = read_state(&bus).await?;
            let body = now_playing_body(&name, &s);
            state.broadcast("mprispc", || NetworkPacket::new(TYPE_MPRIS, body.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lit les vrais lecteurs du bureau : `cargo test -- --ignored lecteurs_reels --nocapture`.
    #[tokio::test]
    #[ignore = "nécessite un bureau avec bus de session"]
    async fn lecteurs_reels() {
        let list = players().await.expect("liste des lecteurs");
        for (name, bus) in &list {
            let s = read_state(bus).await.expect("état du lecteur");
            println!("{name} ({bus}) : {s:?}");
            println!("{}", now_playing_body(name, &s));
        }
    }

    #[test]
    fn paquet_en_lecture() {
        let s = PlayerState {
            title: "Roygbiv".into(),
            artist: "Boards of Canada".into(),
            album: "Music Has the Right to Children".into(),
            art_url: "file:///tmp/cover.jpg".into(),
            length_ms: 151_000,
            pos_ms: 42_000,
            playing: true,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_previous: true,
            can_seek: true,
            loop_status: Some("None".into()),
            shuffle: Some(false),
            volume: 80,
            ..Default::default()
        };
        let b = now_playing_body("Spotify", &s);
        assert_eq!(b["player"], "Spotify");
        assert_eq!((b["title"].as_str(), b["length"].as_i64(), b["pos"].as_i64()), (Some("Roygbiv"), Some(151_000), Some(42_000)));
        assert_eq!((b["isPlaying"].as_bool(), b["canGoNext"].as_bool(), b["volume"].as_i64()), (Some(true), Some(true), Some(80)));
        assert_eq!(b["loopStatus"], "None");
        // Sans boucle ni lecture aléatoire, ces champs sont absents (champs optionnels).
        let plain = now_playing_body("mpv", &PlayerState::default());
        assert!(plain.get("loopStatus").is_none() && plain.get("shuffle").is_none());
    }

    #[test]
    fn liste_et_doublons() {
        assert_eq!(
            player_list_body(&["Spotify".into(), "Firefox".into()]),
            json!({"playerList": ["Spotify", "Firefox"], "supportAlbumArtPayload": true})
        );
        let named = unique_names(&[
            ("org.mpris.MediaPlayer2.firefox.instance1".into(), "Mozilla Firefox".into()),
            ("org.mpris.MediaPlayer2.firefox.instance2".into(), "Mozilla Firefox".into()),
            ("org.mpris.MediaPlayer2.spotify".into(), "Spotify".into()),
        ]);
        let names: Vec<&str> = named.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Mozilla Firefox", "Mozilla Firefox [2]", "Spotify"]);
    }
}
