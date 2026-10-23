//! Lecteurs du téléphone publiés sur le bus MPRIS du PC : ils apparaissent dans le menu
//! multimédia de GNOME, au clavier (touches média) et dans `playerctl`.
//!
//! Chaque lecteur reçu par le plugin `mpris` devient un service
//! `org.mpris.MediaPlayer2.fedoralink_<appareil>_<n>` sur sa propre connexion (MPRIS impose
//! un seul objet `/org/mpris/MediaPlayer2` par service).

use crate::state::{Event, State};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{Array, ObjectPath, OwnedValue, Value as ZValue};
use zbus::{fdo, interface, Connection};

const PATH: &str = "/org/mpris/MediaPlayer2";
const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.fedoralink_";
/// Nom du fichier `.desktop` (sans extension) pour que GNOME affiche l'icône de l'appli.
const DESKTOP_ENTRY: &str = "org.fedoralink.Gui";
/// Un écart de position plus grand que celui-ci n'est pas de la lecture normale : on signale un saut.
const SEEK_JUMP_MS: i64 = 1500;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- données d'un lecteur ------------------------------------------------

/// Ce que MPRIS expose d'un lecteur, déduit des métadonnées reçues du téléphone.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Snapshot {
    pub identity: String,
    pub status: &'static str,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub url: String,
    pub art_url: String,
    pub length_us: i64,
    pub pos_ms: i64,
    pub volume: f64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub loop_status: &'static str,
    pub shuffle: bool,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn millis(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|n| n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
        .max(0)
}

/// Un lecteur « actif » a un morceau ou joue ; les autres restent invisibles dans GNOME.
pub(crate) fn is_active(player: &Value) -> bool {
    flag(player, "isPlaying") || !text(player, "title").is_empty()
}

pub(crate) fn snapshot_from(player: &Value, identity: String) -> Snapshot {
    let title = text(player, "title");
    let playing = flag(player, "isPlaying");
    let loop_status = match player.get("loopStatus").and_then(Value::as_str) {
        Some("Track") => "Track",
        Some("Playlist") => "Playlist",
        _ => "None",
    };
    Snapshot {
        identity,
        status: match (playing, title.is_empty()) {
            (true, _) => "Playing",
            (false, false) => "Paused",
            (false, true) => "Stopped",
        },
        artist: text(player, "artist"),
        album: text(player, "album"),
        url: text(player, "url"),
        art_url: player
            .get("artPath")
            .and_then(Value::as_str)
            .map(|p| file_url(Path::new(p)))
            .unwrap_or_default(),
        length_us: millis(player, "length") * 1000,
        pos_ms: millis(player, "pos"),
        volume: (player.get("volume").and_then(Value::as_f64).unwrap_or(0.0) / 100.0)
            .clamp(0.0, 1.0),
        can_play: flag(player, "canPlay"),
        can_pause: flag(player, "canPause"),
        can_next: flag(player, "canGoNext"),
        can_previous: flag(player, "canGoPrevious"),
        can_seek: flag(player, "canSeek"),
        loop_status,
        shuffle: flag(player, "shuffle"),
        title,
    }
}

/// `file:///chemin` avec les octets spéciaux encodés (le dossier personnel peut contenir des espaces).
pub(crate) fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Nom de service MPRIS : seuls `[A-Za-z0-9_]` sont sûrs dans un identifiant d'appareil.
pub(crate) fn bus_name(device_id: &str, n: u32) -> String {
    let id: String = device_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    format!("{BUS_PREFIX}{id}_{n}")
}

fn track_id(n: u32) -> String {
    format!("/org/fedoralink/track/{n}")
}

fn owned(v: impl Into<ZValue<'static>>) -> OwnedValue {
    // Aucune de ces valeurs ne porte de descripteur de fichier : la conversion ne peut pas échouer.
    OwnedValue::try_from(v.into()).expect("valeur D-Bus sans descripteur")
}

pub(crate) fn metadata(s: &Snapshot, n: u32) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    let path = ObjectPath::try_from(track_id(n)).expect("chemin d'objet valide");
    m.insert("mpris:trackid".into(), owned(path.into_owned()));
    m.insert("mpris:length".into(), owned(s.length_us));
    if !s.title.is_empty() {
        m.insert("xesam:title".into(), owned(s.title.clone()));
    }
    if !s.artist.is_empty() {
        m.insert("xesam:artist".into(), owned(Array::from(vec![s.artist.clone()])));
    }
    if !s.album.is_empty() {
        m.insert("xesam:album".into(), owned(s.album.clone()));
    }
    if !s.art_url.is_empty() {
        m.insert("mpris:artUrl".into(), owned(s.art_url.clone()));
    }
    if !s.url.is_empty() {
        m.insert("xesam:url".into(), owned(s.url.clone()));
    }
    m
}

/// Quelles propriétés ont changé, pour n'émettre `PropertiesChanged` que sur celles-là.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Changes {
    pub metadata: bool,
    pub status: bool,
    pub volume: bool,
    pub caps: bool,
    pub loop_shuffle: bool,
}

pub(crate) fn diff(old: &Snapshot, new: &Snapshot) -> Changes {
    Changes {
        metadata: (&old.title, &old.artist, &old.album, &old.url, &old.art_url, old.length_us)
            != (&new.title, &new.artist, &new.album, &new.url, &new.art_url, new.length_us),
        status: old.status != new.status,
        volume: old.volume != new.volume,
        caps: (old.can_play, old.can_pause, old.can_next, old.can_previous, old.can_seek)
            != (new.can_play, new.can_pause, new.can_next, new.can_previous, new.can_seek),
        loop_shuffle: (old.loop_status, old.shuffle) != (new.loop_status, new.shuffle),
    }
}

/// État partagé entre les interfaces D-Bus et le gestionnaire.
struct Shared {
    snap: Snapshot,
    /// Instant où `snap.pos_ms` a été reçu : la position avance seule pendant la lecture.
    pos_at: Instant,
}

impl Shared {
    fn position_ms(&self) -> i64 {
        let elapsed = if self.snap.status == "Playing" {
            self.pos_at.elapsed().as_millis() as i64
        } else {
            0
        };
        let pos = self.snap.pos_ms + elapsed;
        match self.snap.length_us / 1000 {
            0 => pos,
            len => pos.min(len),
        }
    }
}

/// Commandes renvoyées au téléphone ; séparées de zbus pour les tester.
pub(crate) trait Control: Send + Sync {
    fn action(&self, action: &str) -> Result<()>;
    fn volume(&self, percent: i32) -> Result<()>;
    fn seek(&self, position_ms: i64) -> Result<()>;
}

struct PhoneControl {
    state: Arc<State>,
    device: String,
    player: String,
}

impl Control for PhoneControl {
    fn action(&self, action: &str) -> Result<()> {
        self.state.media_action(&self.device, &self.player, action)
    }
    fn volume(&self, percent: i32) -> Result<()> {
        self.state.media_volume(&self.device, &self.player, percent)
    }
    fn seek(&self, position_ms: i64) -> Result<()> {
        self.state.media_seek(&self.device, &self.player, position_ms)
    }
}

fn failed(e: anyhow::Error) -> fdo::Error {
    fdo::Error::Failed(format!("{e:#}"))
}

// ---- interfaces D-Bus ----------------------------------------------------

struct Root {
    shared: Arc<Mutex<Shared>>,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}
    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        lock(&self.shared).snap.identity.clone()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        DESKTOP_ENTRY.into()
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    shared: Arc<Mutex<Shared>>,
    control: Arc<dyn Control>,
    n: u32,
}

impl Player {
    fn snap<R>(&self, f: impl FnOnce(&Snapshot) -> R) -> R {
        f(&lock(&self.shared).snap)
    }

    fn seek_to(&self, position_ms: i64) -> fdo::Result<()> {
        self.control.seek(position_ms.max(0)).map_err(failed)?;
        // Optimiste : la position affichée suit sans attendre le prochain paquet du téléphone.
        let mut s = lock(&self.shared);
        s.snap.pos_ms = position_ms.max(0);
        s.pos_at = Instant::now();
        Ok(())
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) -> fdo::Result<()> {
        self.control.action("Next").map_err(failed)
    }
    fn previous(&self) -> fdo::Result<()> {
        self.control.action("Previous").map_err(failed)
    }
    fn pause(&self) -> fdo::Result<()> {
        self.control.action("Pause").map_err(failed)
    }
    fn play_pause(&self) -> fdo::Result<()> {
        self.control.action("PlayPause").map_err(failed)
    }
    fn stop(&self) -> fdo::Result<()> {
        self.control.action("Stop").map_err(failed)
    }
    fn play(&self) -> fdo::Result<()> {
        self.control.action("Play").map_err(failed)
    }

    /// Saut relatif, en microsecondes.
    fn seek(&self, offset_us: i64) -> fdo::Result<()> {
        let target = lock(&self.shared).position_ms() + offset_us / 1000;
        self.seek_to(target)
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position_us: i64) -> fdo::Result<()> {
        // Selon la spécification, une demande pour un autre morceau est ignorée.
        if track_id.as_str() != self::track_id(self.n) {
            return Ok(());
        }
        self.seek_to(position_us / 1000)
    }

    fn open_uri(&self, _uri: &str) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported("ouverture d'URI non prise en charge".into()))
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position_us: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.snap(|s| s.status).into()
    }
    #[zbus(property)]
    fn loop_status(&self) -> String {
        self.snap(|s| s.loop_status).into()
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.snap(|s| s.shuffle)
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.snap(|s| metadata(s, self.n))
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.snap(|s| s.volume)
    }
    #[zbus(property)]
    fn set_volume(&self, volume: f64) -> zbus::Result<()> {
        let volume = volume.clamp(0.0, 1.0);
        self.control
            .volume((volume * 100.0).round() as i32)
            .map_err(|e| zbus::Error::Failure(format!("{e:#}")))?;
        lock(&self.shared).snap.volume = volume;
        Ok(())
    }
    /// Microsecondes ; MPRIS demande de ne pas émettre de signal pour cette propriété.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        lock(&self.shared).position_ms() * 1000
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.snap(|s| s.can_next)
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.snap(|s| s.can_previous)
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.snap(|s| s.can_play)
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.snap(|s| s.can_pause)
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snap(|s| s.can_seek)
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

// ---- gestionnaire --------------------------------------------------------

struct Published {
    /// Garder la connexion en vie garde le nom sur le bus.
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    player: InterfaceRef<Player>,
}

impl Published {
    async fn update(&self, new: Snapshot) {
        let (changes, jumped) = {
            let mut s = lock(&self.shared);
            let predicted = s.position_ms();
            let changes = diff(&s.snap, &new);
            let jumped = (new.pos_ms - predicted).abs() > SEEK_JUMP_MS
                && new.status == s.snap.status
                && !changes.metadata;
            s.snap = new;
            s.pos_at = Instant::now();
            (changes, jumped)
        };
        let emitter = self.player.signal_emitter();
        let player = self.player.get().await;
        let results = [
            if changes.metadata { Some(player.metadata_changed(emitter).await) } else { None },
            if changes.status { Some(player.playback_status_changed(emitter).await) } else { None },
            if changes.volume { Some(player.volume_changed(emitter).await) } else { None },
            if changes.caps {
                Some(async {
                    player.can_play_changed(emitter).await?;
                    player.can_pause_changed(emitter).await?;
                    player.can_go_next_changed(emitter).await?;
                    player.can_go_previous_changed(emitter).await?;
                    player.can_seek_changed(emitter).await
                }
                .await)
            } else {
                None
            },
            if changes.loop_shuffle {
                Some(async {
                    player.loop_status_changed(emitter).await?;
                    player.shuffle_changed(emitter).await
                }
                .await)
            } else {
                None
            },
        ];
        for e in results.into_iter().flatten().filter_map(Result::err) {
            debug!("PropertiesChanged : {e}");
        }
        if jumped {
            let pos = lock(&self.shared).position_ms() * 1000;
            if let Err(e) = Player::seeked(emitter, pos).await {
                debug!("Seeked : {e}");
            }
        }
    }

    async fn close(self) {
        let Published { conn, shared, player } = self;
        drop((shared, player));
        conn.graceful_shutdown().await;
    }
}

type Key = (String, String);

struct Manager {
    state: Arc<State>,
    published: HashMap<Key, Published>,
    /// Numéro attribué à un lecteur pour toute sa durée de vie, par appareil.
    numbers: HashMap<Key, u32>,
    next_number: HashMap<String, u32>,
    /// Lecteurs dont la publication a échoué : on n'insiste pas avant qu'ils disparaissent.
    failed: HashSet<Key>,
}

impl Manager {
    fn device_name(&self, device: &str) -> String {
        self.state
            .list_devices()
            .into_iter()
            .find(|d| d.id == device)
            .map(|d| d.name)
            .unwrap_or_else(|| device.to_owned())
    }

    /// Lecteurs qui doivent être visibles dans GNOME pour cet appareil.
    fn wanted(&self, device: &str) -> Vec<(String, Value)> {
        let online = self
            .state
            .list_devices()
            .iter()
            .any(|d| d.id == device && d.paired && d.connected);
        if !online || !self.state.mpris_proxy_enabled() || !self.state.plugin_enabled("mpris") {
            return Vec::new();
        }
        self.state
            .media(device)
            .into_iter()
            .filter(is_active)
            .filter_map(|p| Some((p.get("player")?.as_str()?.to_owned(), p)))
            .collect()
    }

    async fn sync_device(&mut self, device: &str) {
        let wanted = self.wanted(device);
        let names: HashSet<&str> = wanted.iter().map(|(n, _)| n.as_str()).collect();

        let gone: Vec<Key> = self
            .published
            .keys()
            .filter(|(d, p)| d == device && !names.contains(p.as_str()))
            .cloned()
            .collect();
        for key in gone {
            if let Some(p) = self.published.remove(&key) {
                info!(device = %key.0, player = %key.1, "lecteur retiré de GNOME");
                p.close().await;
            }
            self.numbers.remove(&key);
        }
        self.failed.retain(|(d, p)| d != device || names.contains(p.as_str()));

        let device_name = self.device_name(device);
        for (player, json) in wanted {
            let key = (device.to_owned(), player.clone());
            let snap = snapshot_from(&json, format!("{player} sur {device_name}"));
            if let Some(p) = self.published.get(&key) {
                p.update(snap).await;
            } else if !self.failed.contains(&key) {
                match self.publish(&key, snap).await {
                    Ok(p) => {
                        self.published.insert(key, p);
                    }
                    Err(e) => {
                        warn!(device, %player, "publication MPRIS impossible : {e:#}");
                        self.failed.insert(key);
                    }
                }
            }
        }
    }

    async fn sync_all(&mut self) {
        let mut devices: HashSet<String> = self.state.list_devices().into_iter().map(|d| d.id).collect();
        devices.extend(self.published.keys().map(|(d, _)| d.clone()));
        for device in devices {
            self.sync_device(&device).await;
        }
    }

    async fn publish(&mut self, key: &Key, snap: Snapshot) -> Result<Published> {
        let n = *self.numbers.entry(key.clone()).or_insert_with(|| {
            let next = self.next_number.entry(key.0.clone()).or_insert(0);
            *next += 1;
            *next
        });
        let name = bus_name(&key.0, n);
        let shared = Arc::new(Mutex::new(Shared { snap, pos_at: Instant::now() }));
        let control = Arc::new(PhoneControl {
            state: self.state.clone(),
            device: key.0.clone(),
            player: key.1.clone(),
        });
        let conn = zbus::connection::Builder::session()?
            .name(name.as_str())?
            .serve_at(PATH, Root { shared: shared.clone() })?
            .serve_at(PATH, Player { shared: shared.clone(), control, n })?
            .build()
            .await
            .with_context(|| format!("service {name}"))?;
        let player = conn.object_server().interface::<_, Player>(PATH).await?;
        info!(%name, "lecteur du téléphone publié sur MPRIS");
        Ok(Published { conn, shared, player })
    }
}

/// Suit les événements du daemon et publie / met à jour / retire les lecteurs.
pub async fn run(state: Arc<State>) {
    let mut events = state.subscribe();
    let mut manager = Manager {
        state,
        published: HashMap::new(),
        numbers: HashMap::new(),
        next_number: HashMap::new(),
        failed: HashSet::new(),
    };
    manager.sync_all().await;
    loop {
        match events.recv().await {
            Ok(Event::MediaChanged(d) | Event::DeviceChanged(d)) => manager.sync_device(&d).await,
            Ok(Event::SettingsChanged) | Err(RecvError::Lagged(_)) => manager.sync_all().await,
            Ok(_) => {}
            Err(RecvError::Closed) => break,
        }
        // Évite de saturer le bus quand le téléphone envoie la position en rafale.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn player() -> Value {
        json!({
            "player": "Spotify", "title": "Titre", "artist": "Artiste", "album": "Album",
            "length": 200000, "pos": 5000, "isPlaying": true, "volume": 40,
            "canPlay": true, "canPause": true, "canGoNext": true, "canGoPrevious": false,
            "canSeek": true, "loopStatus": "Track", "shuffle": true,
            "artPath": "/home/Jean Dupont/.cache/fedora-link/art/ab12.jpg",
        })
    }

    #[test]
    fn snapshot_depuis_les_metadonnees_du_telephone() {
        let s = snapshot_from(&player(), "Spotify sur Pixel".into());
        assert_eq!(s.status, "Playing");
        assert_eq!(s.length_us, 200_000_000);
        assert_eq!(s.volume, 0.4);
        assert_eq!((s.loop_status, s.shuffle), ("Track", true));
        assert_eq!(
            s.art_url,
            "file:///home/Jean%20Dupont/.cache/fedora-link/art/ab12.jpg"
        );
    }

    #[test]
    fn etats_de_lecture_et_lecteurs_actifs() {
        let mut p = player();
        p["isPlaying"] = json!(false);
        assert_eq!(snapshot_from(&p, String::new()).status, "Paused");
        assert!(is_active(&p));
        let vide = json!({ "player": "Firefox" });
        assert_eq!(snapshot_from(&vide, String::new()).status, "Stopped");
        assert!(!is_active(&vide));
        assert!(is_active(&json!({ "player": "x", "isPlaying": true })));
    }

    #[test]
    fn metadonnees_mpris() {
        let m = metadata(&snapshot_from(&player(), String::new()), 3);
        assert_eq!(m.len(), 6);
        let title: String = m["xesam:title"].clone().try_into().unwrap();
        assert_eq!(title, "Titre");
        let artists: Vec<String> = m["xesam:artist"].clone().try_into().unwrap();
        assert_eq!(artists, ["Artiste"]);
        let length: i64 = m["mpris:length"].clone().try_into().unwrap();
        assert_eq!(length, 200_000_000);
        let id: ObjectPath = m["mpris:trackid"].clone().try_into().unwrap();
        assert_eq!(id.as_str(), "/org/fedoralink/track/3");
        let sans = metadata(&snapshot_from(&json!({"player": "x"}), String::new()), 1);
        assert!(!sans.contains_key("xesam:title") && !sans.contains_key("mpris:artUrl"));
    }

    #[test]
    fn noms_de_service_valides() {
        assert_eq!(bus_name("_ab12_cd", 1), "org.mpris.MediaPlayer2.fedoralink__ab12_cd_1");
        assert_eq!(bus_name("a-b.c", 2), "org.mpris.MediaPlayer2.fedoralink_a_b_c_2");
        assert!(zbus::names::WellKnownName::try_from(bus_name("_ab12_cd", 1)).is_ok());
    }

    #[test]
    fn seules_les_proprietes_modifiees_sont_signalees() {
        let a = snapshot_from(&player(), "x".into());
        assert_eq!(diff(&a, &a), Changes::default());
        let mut p = player();
        p["isPlaying"] = json!(false);
        p["title"] = json!("Autre");
        let c = diff(&a, &snapshot_from(&p, "x".into()));
        assert_eq!(c, Changes { metadata: true, status: true, ..Changes::default() });
    }

    #[derive(Default)]
    struct Fake(Mutex<Vec<String>>);
    impl Control for Fake {
        fn action(&self, a: &str) -> Result<()> {
            lock(&self.0).push(a.to_owned());
            Ok(())
        }
        fn volume(&self, p: i32) -> Result<()> {
            lock(&self.0).push(format!("volume {p}"));
            Ok(())
        }
        fn seek(&self, ms: i64) -> Result<()> {
            lock(&self.0).push(format!("seek {ms}"));
            Ok(())
        }
    }

    fn fixture() -> (Player, Arc<Fake>) {
        let shared = Arc::new(Mutex::new(Shared {
            snap: Snapshot {
                status: "Paused",
                length_us: 200_000_000,
                pos_ms: 60_000,
                ..snapshot_from(&player(), String::new())
            },
            pos_at: Instant::now(),
        }));
        let fake = Arc::new(Fake::default());
        (Player { shared, control: fake.clone(), n: 1 }, fake)
    }

    #[test]
    fn commandes_renvoyees_au_telephone() {
        let (p, fake) = fixture();
        p.play_pause().unwrap();
        p.next().unwrap();
        p.previous().unwrap();
        p.stop().unwrap();
        p.seek(10_000_000).unwrap();
        p.seek(-100_000_000).unwrap();
        let id = ObjectPath::try_from("/org/fedoralink/track/1").unwrap();
        p.set_position(id, 30_000_000).unwrap();
        let autre = ObjectPath::try_from("/org/fedoralink/track/9").unwrap();
        p.set_position(autre, 90_000_000).unwrap();
        assert!(p.open_uri("file:///x").is_err());
        assert_eq!(
            *lock(&fake.0),
            ["PlayPause", "Next", "Previous", "Stop", "seek 70000", "seek 0", "seek 30000"]
        );
    }

    #[test]
    fn volume_et_position() {
        let (p, fake) = fixture();
        p.set_volume(0.75).unwrap();
        assert_eq!(*lock(&fake.0), ["volume 75"]);
        assert_eq!(p.volume(), 0.75);
        // En pause la position ne bouge pas ; elle ne dépasse jamais la durée.
        assert_eq!(p.position(), 60_000_000);
        lock(&p.shared).snap.pos_ms = 199_999;
        lock(&p.shared).snap.status = "Playing";
        lock(&p.shared).pos_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(p.position(), 200_000_000);
    }

    /// Publie vraiment un lecteur sur le bus de session (lancé à la main : `--ignored`).
    #[tokio::test]
    #[ignore = "utilise le bus de session réel"]
    async fn publication_reelle_sur_le_bus() {
        let (p, fake) = fixture();
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.mpris.MediaPlayer2.fedoralink_test_1")
            .unwrap()
            .serve_at(PATH, Root { shared: p.shared.clone() })
            .unwrap()
            .serve_at(PATH, p)
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = Connection::session().await.unwrap();
        let proxy = zbus::Proxy::new(
            &client,
            "org.mpris.MediaPlayer2.fedoralink_test_1",
            PATH,
            "org.mpris.MediaPlayer2.Player",
        )
        .await
        .unwrap();
        let status: String = proxy.get_property("PlaybackStatus").await.unwrap();
        assert_eq!(status, "Paused");
        let meta: HashMap<String, OwnedValue> = proxy.get_property("Metadata").await.unwrap();
        assert!(meta.contains_key("xesam:title"));
        proxy.call_method("PlayPause", &()).await.unwrap();
        assert_eq!(*lock(&fake.0), ["PlayPause"]);
        let root = zbus::Proxy::new(
            &client,
            "org.mpris.MediaPlayer2.fedoralink_test_1",
            PATH,
            "org.mpris.MediaPlayer2",
        )
        .await
        .unwrap();
        let desktop: String = root.get_property("DesktopEntry").await.unwrap();
        assert_eq!(desktop, "org.fedoralink.Gui");
        conn.graceful_shutdown().await;
    }
}
