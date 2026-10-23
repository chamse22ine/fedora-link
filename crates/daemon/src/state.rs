//! État partagé du daemon : appareils, appairage, historique, événements.
//! Toutes les actions (D-Bus, tests) passent par ici.

use crate::contacts::ContactStore;
use crate::listen::{Listener, Options as ListenOptions};
use crate::input::RemoteInput;
use crate::plugins::{self, Plugin, PluginCtx};
use crate::settings::{normalize_commands, CallMedia, RemoteCommand, Settings};
use crate::received::ReceivedLog;
use crate::sms::SmsStore;
use crate::trust::TrustStore;
use anyhow::{anyhow, bail, Result};
use fedora_link_core::cert::DeviceIdentity;
use fedora_link_core::packet::{IdentityBody, NetworkPacket};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use crate::plugins::sftp::SftpInfo;
use tracing::{debug, warn};

const MAX_HISTORY: usize = 200;
const PAIR_PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
const SFTP_TIMEOUT: Duration = Duration::from_secs(20);

/// Événements publiés par le daemon ; la couche D-Bus les transforme en signaux.
#[derive(Debug, Clone)]
pub enum Event {
    DeviceChanged(String),
    MediaChanged(String),
    PairingRequested {
        id: String,
        name: String,
        code: String,
    },
    NotificationPosted {
        device: String,
        json: String,
    },
    NotificationDismissed {
        device: String,
        id: String,
    },
    ClipboardReceived {
        device: String,
        text: String,
    },
    TransferProgress {
        device: String,
        name: String,
        done: u64,
        total: u64,
        incoming: bool,
    },
    FileReceived {
        device: String,
        path: String,
    },
    /// Lien web partagé par le téléphone (déjà ouvert si l'option est active).
    LinkReceived {
        device: String,
        url: String,
    },
    ContactsChanged(String),
    /// Messages d'une conversation ajoutés ou mis à jour.
    SmsChanged {
        device: String,
        thread: i64,
    },
    /// Le pointeur de présentation bouge (déplacement relatif, sans unité).
    PresenterMoved {
        dx: f64,
        dy: f64,
    },
    PresenterStopped,
    /// Un réglage qui concerne les services en arrière-plan a changé.
    SettingsChanged,
    /// L'état de « Écouter sur le PC » a changé (voir `listen`).
    ListenChanged,
    /// Texte partagé par le téléphone : l'interface le copie dans le presse-papiers.
    TextReceived {
        device: String,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<i32>,
    pub charging: bool,
    /// Génération du réseau mobile du téléphone (« 4G »), vide si inconnue.
    pub network_type: Option<String>,
    /// Force du signal mobile, de 0 à 4.
    pub signal: Option<i32>,
    /// Adresse IP du lien actif (utile pour la recopie d'écran en Wi-Fi).
    pub ip: Option<String>,
}

struct Entry {
    info: DeviceInfo,
    ctx: Option<PluginCtx>,
    link_id: u64,
}

pub struct State {
    pub identity: DeviceIdentity,
    pub identity_body: IdentityBody,
    pub trust: TrustStore,
    pub auto_accept: bool,
    /// Ouvre les liens reçus dans le navigateur.
    pub open_links: bool,
    pub contacts: ContactStore,
    pub sms: SmsStore,
    pub received: ReceivedLog,
    pub input: RemoteInput,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    events: broadcast::Sender<Event>,
    devices: Mutex<HashMap<String, Entry>>,
    history: Mutex<HashMap<String, Vec<Value>>>,
    /// Lecteurs multimédias du téléphone : appareil → lecteur → métadonnées.
    media: Mutex<HashMap<String, BTreeMap<String, Value>>>,
    pending_pairs: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    sftp_waiters: Mutex<HashMap<String, oneshot::Sender<Result<SftpInfo, String>>>>,
    outgoing_pairs: Mutex<HashSet<String>>,
    plugins: Vec<Arc<dyn Plugin>>,
    next_link: AtomicU64,
    pub listen: Arc<Listener>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Un mutex empoisonné ne rend pas ces données invalides : on continue.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: DeviceIdentity,
        identity_body: IdentityBody,
        trust: TrustStore,
        settings: Settings,
        settings_path: PathBuf,
        auto_accept: bool,
        open_links: bool,
        data_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        let events = broadcast::channel(256).0;
        Self {
            identity,
            identity_body,
            trust,
            auto_accept,
            open_links,
            contacts: ContactStore::new(data_dir.join("contacts")),
            sms: SmsStore::default(),
            received: ReceivedLog::load(data_dir.join("received.json")),
            input: RemoteInput::new(data_dir.join("input-token")),
            data_dir,
            cache_dir,
            settings: Mutex::new(settings),
            settings_path,
            listen: Arc::new(Listener::new(events.clone())),
            events,
            devices: Mutex::new(HashMap::new()),
            history: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
            pending_pairs: Mutex::new(HashMap::new()),
            sftp_waiters: Mutex::new(HashMap::new()),
            outgoing_pairs: Mutex::new(HashSet::new()),
            plugins: plugins::all(),
            next_link: AtomicU64::new(1),
        }
    }

    // ---- événements ------------------------------------------------------

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn emit(&self, event: Event) {
        // Aucun abonné n'est pas une erreur.
        let _ = self.events.send(event);
    }

    // ---- plugins et réglages ----------------------------------------------

    pub fn plugin_enabled(&self, name: &str) -> bool {
        lock(&self.settings).is_enabled(name)
    }

    pub fn download_dir(&self) -> PathBuf {
        lock(&self.settings).download_dir.clone()
    }

    pub fn call_media(&self) -> CallMedia {
        lock(&self.settings).call_media
    }

    pub fn set_call_media(&self, mode: CallMedia) -> Result<()> {
        let mut s = lock(&self.settings);
        s.call_media = mode;
        s.save(&self.settings_path)
    }

    pub fn mpris_proxy_enabled(&self) -> bool {
        lock(&self.settings).mpris_proxy
    }

    pub fn set_mpris_proxy_enabled(&self, enabled: bool) -> Result<()> {
        self.update_settings(|s| s.mpris_proxy = enabled)
    }

    /// Options de « Écouter sur le PC » : (garder le son sur le téléphone, pauser les lecteurs).
    pub fn listen_options(&self) -> (bool, bool) {
        let s = lock(&self.settings);
        (s.listen_keep_on_phone, s.listen_pause_players)
    }

    pub fn set_listen_options(&self, keep_on_phone: bool, pause_players: bool) -> Result<()> {
        self.update_settings(|s| {
            s.listen_keep_on_phone = keep_on_phone;
            s.listen_pause_players = pause_players;
        })
    }

    fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> Result<()> {
        {
            let mut s = lock(&self.settings);
            f(&mut s);
            s.save(&self.settings_path)?;
        }
        self.emit(Event::SettingsChanged);
        Ok(())
    }

    /// Lance « Écouter sur le PC » pour l'appareil donné (son IP aide à choisir l'entrée ADB).
    pub fn start_listening(&self, device: &str) -> Result<()> {
        let ip = lock(&self.devices)
            .get(device)
            .and_then(|e| e.info.ip.clone());
        let (keep_on_phone, pause_players) = self.listen_options();
        self.listen
            .start(ListenOptions::new(ip, keep_on_phone, pause_players))
    }

    pub fn commands(&self) -> Vec<RemoteCommand> {
        lock(&self.settings).commands.clone()
    }

    /// Remplace la liste des commandes à distance et prévient les téléphones connectés.
    pub fn set_commands(&self, list: Vec<RemoteCommand>) -> Result<Vec<RemoteCommand>> {
        let list = normalize_commands(list)?;
        {
            let mut s = lock(&self.settings);
            s.commands = list.clone();
            s.save(&self.settings_path)?;
        }
        self.broadcast("runcommand", || plugins::runcommand::command_list_packet(&list));
        Ok(list)
    }

    pub fn list_plugins(&self) -> Vec<(String, bool)> {
        let s = lock(&self.settings);
        self.plugins
            .iter()
            .map(|p| (p.name().to_owned(), s.is_enabled(p.name())))
            .collect()
    }

    pub fn set_plugin_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        if !self.plugins.iter().any(|p| p.name() == name) {
            bail!("plugin inconnu : {name}");
        }
        let mut s = lock(&self.settings);
        s.plugins.insert(name.to_owned(), enabled);
        s.save(&self.settings_path)
    }

    /// Types de paquets annoncés au téléphone, selon les plugins activés.
    pub(crate) fn capabilities(settings: &Settings) -> (Vec<String>, Vec<String>) {
        let mut incoming = Vec::new();
        let mut outgoing = Vec::new();
        for p in plugins::all() {
            if settings.is_enabled(p.name()) {
                incoming.extend(p.incoming().iter().map(|s| s.to_string()));
                outgoing.extend(p.outgoing().iter().map(|s| s.to_string()));
            }
        }
        (incoming, outgoing)
    }

    pub(crate) fn dispatch(&self, ctx: &PluginCtx, packet: &NetworkPacket) {
        for p in &self.plugins {
            if p.incoming().contains(&packet.kind.as_str()) {
                if !self.plugin_enabled(p.name()) {
                    debug!(plugin = p.name(), "plugin désactivé, paquet ignoré");
                } else if let Err(e) = p.handle_packet(ctx, packet) {
                    warn!(plugin = p.name(), "paquet {} : {e:#}", packet.kind);
                }
                return;
            }
        }
        debug!(kind = %packet.kind, "type de paquet non géré");
    }

    pub(crate) fn link_ready(&self, ctx: &PluginCtx) {
        for p in &self.plugins {
            if self.plugin_enabled(p.name()) {
                p.on_link_ready(ctx);
            }
        }
    }

    // ---- appareils --------------------------------------------------------

    pub(crate) fn next_link_id(&self) -> u64 {
        self.next_link.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn register_link(
        &self,
        ctx: &PluginCtx,
        peer: &IdentityBody,
        paired: bool,
        link_id: u64,
    ) {
        let mut devices = lock(&self.devices);
        let previous = devices.get(&peer.device_id).map(|e| e.info.clone());
        devices.insert(
            peer.device_id.clone(),
            Entry {
                info: DeviceInfo {
                    id: peer.device_id.clone(),
                    name: peer.device_name.clone(),
                    device_type: peer.device_type.clone(),
                    paired,
                    connected: true,
                    battery: previous.as_ref().and_then(|i| i.battery),
                    charging: previous.is_some_and(|i| i.charging),
                    network_type: None,
                    signal: None,
                    ip: Some(ctx.ip.to_string()),
                },
                ctx: Some(ctx.clone()),
                link_id,
            },
        );
        drop(devices);
        self.emit(Event::DeviceChanged(peer.device_id.clone()));
    }

    pub(crate) fn link_closed(&self, id: &str, link_id: u64) {
        let mut devices = lock(&self.devices);
        if let Some(e) = devices.get_mut(id) {
            // Une reconnexion a pu remplacer ce lien : on ne touche alors à rien.
            if e.link_id == link_id {
                e.info.connected = false;
                e.ctx = None;
                drop(devices);
                lock(&self.media).remove(id);
                self.emit(Event::DeviceChanged(id.to_owned()));
            }
        }
    }

    pub(crate) fn update_device(&self, id: &str, f: impl FnOnce(&mut DeviceInfo)) {
        if let Some(e) = lock(&self.devices).get_mut(id) {
            f(&mut e.info);
        }
        self.emit(Event::DeviceChanged(id.to_owned()));
    }

    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        let devices = lock(&self.devices);
        let mut out: Vec<DeviceInfo> = devices.values().map(|e| e.info.clone()).collect();
        for (id, name) in self.trust.list() {
            if !devices.contains_key(&id) {
                out.push(DeviceInfo {
                    id,
                    name,
                    device_type: "phone".into(),
                    paired: true,
                    connected: false,
                    battery: None,
                    charging: false,
                    network_type: None,
                    signal: None,
                    ip: None,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Envoie un paquet à chaque téléphone appairé et connecté dont le plugin est activé.
    pub fn broadcast(&self, plugin: &str, packet: impl Fn() -> NetworkPacket) {
        if !self.plugin_enabled(plugin) {
            return;
        }
        let ctxs: Vec<PluginCtx> = lock(&self.devices)
            .values()
            .filter_map(|e| e.ctx.clone())
            .collect();
        for ctx in ctxs {
            if self.trust.is_trusted(&ctx.device_id, &ctx.peer_cert) {
                let _ = ctx.send(packet());
            }
        }
    }

    pub fn is_connected(&self, id: &str) -> bool {
        lock(&self.devices).get(id).is_some_and(|e| e.ctx.is_some())
    }

    pub fn connected_ctx(&self, id: &str) -> Result<PluginCtx> {
        lock(&self.devices)
            .get(id)
            .and_then(|e| e.ctx.clone())
            .ok_or_else(|| anyhow!("appareil {id} non connecté"))
    }

    fn paired_ctx(&self, id: &str) -> Result<PluginCtx> {
        let ctx = self.connected_ctx(id)?;
        if !self.trust.is_trusted(id, &ctx.peer_cert) {
            bail!("appareil {id} non appairé");
        }
        Ok(ctx)
    }

    // ---- appairage --------------------------------------------------------

    /// Appairage lancé depuis le PC.
    pub fn pair(&self, id: &str) -> Result<()> {
        let ctx = self.connected_ctx(id)?;
        lock(&self.outgoing_pairs).insert(id.to_owned());
        ctx.send(NetworkPacket::pair(true))
    }

    pub fn unpair(&self, id: &str) -> Result<()> {
        if let Ok(ctx) = self.connected_ctx(id) {
            let _ = ctx.send(NetworkPacket::pair(false));
        }
        self.trust.remove(id)?;
        self.update_device(id, |i| i.paired = false);
        Ok(())
    }

    pub fn answer_pairing(&self, id: &str, accept: bool) -> Result<()> {
        let tx = lock(&self.pending_pairs)
            .remove(id)
            .ok_or_else(|| anyhow!("aucune demande d'appairage en attente pour {id}"))?;
        let _ = tx.send(accept);
        Ok(())
    }

    pub(crate) fn take_outgoing_pair(&self, id: &str) -> bool {
        lock(&self.outgoing_pairs).remove(id)
    }

    /// Publie la demande et attend la réponse de l'interface (ou le délai).
    pub(crate) async fn confirm_pairing(&self, id: &str, name: &str, code: &str) -> bool {
        if self.auto_accept {
            return true;
        }
        let (tx, rx) = oneshot::channel();
        lock(&self.pending_pairs).insert(id.to_owned(), tx);
        self.emit(Event::PairingRequested {
            id: id.to_owned(),
            name: name.to_owned(),
            code: code.to_owned(),
        });
        let answer = tokio::time::timeout(PAIR_PROMPT_TIMEOUT, rx).await;
        lock(&self.pending_pairs).remove(id);
        matches!(answer, Ok(Ok(true)))
    }

    // ---- actions sur les appareils ---------------------------------------

    pub fn ring_phone(&self, id: &str) -> Result<()> {
        self.require_plugin("findmyphone")?;
        self.paired_ctx(id)?.send(NetworkPacket::new(
            "kdeconnect.findmyphone.request",
            json!({}),
        ))
    }

    pub fn send_clipboard(&self, text: &str) -> Result<()> {
        self.require_plugin("clipboard")?;
        for d in self
            .list_devices()
            .into_iter()
            .filter(|d| d.connected && d.paired)
        {
            if let Ok(ctx) = self.paired_ctx(&d.id) {
                let _ = ctx.send(NetworkPacket::new(
                    "kdeconnect.clipboard",
                    json!({ "content": text }),
                ));
            }
        }
        Ok(())
    }

    pub fn send_file(self: &Arc<Self>, id: &str, path: PathBuf) -> Result<()> {
        self.require_plugin("share")?;
        let ctx = self.paired_ctx(id)?;
        tokio::spawn(async move {
            if let Err(e) = plugins::share::send_file(ctx, path).await {
                warn!("envoi de fichier : {e:#}");
            }
        });
        Ok(())
    }

    /// Envoie un lien web au téléphone, qui l'ouvre dans son navigateur.
    pub fn share_url(&self, id: &str, url: &str) -> Result<()> {
        self.require_plugin("share")?;
        let url = crate::desktop::web_url(url)
            .ok_or_else(|| anyhow!("ce n'est pas un lien http(s) valide"))?;
        self.paired_ctx(id)?
            .send(NetworkPacket::new("kdeconnect.share.request", json!({ "url": url })))
    }

    /// Envoie un texte au téléphone (il le met dans son presse-papiers).
    pub fn share_text(&self, id: &str, text: &str) -> Result<()> {
        self.require_plugin("share")?;
        if text.trim().is_empty() {
            bail!("texte vide");
        }
        self.paired_ctx(id)?
            .send(NetworkPacket::new("kdeconnect.share.request", json!({ "text": text })))
    }

    pub fn dismiss_notification(&self, id: &str, notif_id: &str) -> Result<()> {
        self.require_plugin("notification")?;
        self.paired_ctx(id)?.send(NetworkPacket::new(
            "kdeconnect.notification.request",
            json!({ "cancel": notif_id }),
        ))?;
        self.remove_notification(id, notif_id);
        Ok(())
    }

    pub fn reply_notification(&self, id: &str, reply_id: &str, message: &str) -> Result<()> {
        self.require_plugin("notification")?;
        self.paired_ctx(id)?.send(NetworkPacket::new(
            "kdeconnect.notification.reply",
            json!({ "requestReplyId": reply_id, "message": message }),
        ))
    }

    fn require_plugin(&self, name: &str) -> Result<()> {
        if self.plugin_enabled(name) {
            Ok(())
        } else {
            bail!("la fonctionnalité « {name} » est désactivée")
        }
    }

    // ---- parcourir le téléphone ---------------------------------------------

    /// Demande au téléphone d'ouvrir son serveur SFTP et attend sa réponse.
    pub async fn browse_phone(&self, id: &str) -> Result<SftpInfo> {
        self.require_plugin("sftp")?;
        let ctx = self.paired_ctx(id)?;
        let (tx, rx) = oneshot::channel();
        lock(&self.sftp_waiters).insert(id.to_owned(), tx);
        let sent = ctx.send(NetworkPacket::new(
            plugins::sftp::TYPE_REQUEST,
            json!({ "startBrowsing": true }),
        ));
        if let Err(e) = sent {
            lock(&self.sftp_waiters).remove(id);
            return Err(e);
        }
        let answer = tokio::time::timeout(SFTP_TIMEOUT, rx).await;
        lock(&self.sftp_waiters).remove(id);
        match answer {
            Ok(Ok(Ok(info))) => Ok(info),
            Ok(Ok(Err(message))) => bail!("{message}"),
            Ok(Err(_)) => bail!("demande remplacée par une autre"),
            Err(_) => bail!("le téléphone n'a pas répondu (autorisation de stockage accordée ?)"),
        }
    }

    pub(crate) fn finish_sftp(&self, id: &str, answer: Result<SftpInfo, String>) {
        if let Some(tx) = lock(&self.sftp_waiters).remove(id) {
            let _ = tx.send(answer);
        }
    }

    // ---- contacts et SMS ----------------------------------------------------

    /// Cache des pièces jointes MMS d'un appareil.
    pub fn sms_dir(&self, device: &str) -> PathBuf {
        self.data_dir.join("sms").join(device)
    }

    /// Nom du contact pour un numéro, sinon le numéro lui-même.
    pub fn display_name(&self, device: &str, number: &str) -> String {
        self.contacts
            .lookup(device, number)
            .map_or_else(|| number.to_owned(), |c| c.name)
    }

    /// Conversations (dernier message de chacune) avec nom et avatar résolus.
    pub fn sms_conversations(&self, device: &str) -> Vec<Value> {
        self.sms
            .conversations(device)
            .into_iter()
            .map(|m| {
                let title = m
                    .addresses
                    .iter()
                    .map(|a| self.display_name(device, a))
                    .collect::<Vec<_>>()
                    .join(", ");
                let avatar = m
                    .addresses
                    .first()
                    .and_then(|a| self.contacts.lookup(device, a))
                    .and_then(|c| c.avatar);
                json!({
                    "thread_id": m.thread_id,
                    "addresses": m.addresses,
                    "title": title,
                    "avatar": avatar,
                    "body": m.body,
                    "has_attachment": !m.attachments.is_empty(),
                    "date": m.date,
                    "incoming": m.incoming,
                    "unread": self.sms.thread_has_unread(device, m.thread_id),
                })
            })
            .collect()
    }

    pub fn sms_messages(&self, device: &str, thread: i64) -> Vec<Value> {
        self.sms
            .thread(device, thread)
            .into_iter()
            .map(|m| {
                let sender = self.display_name(device, m.sender());
                let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
                v["sender"] = json!(sender);
                v
            })
            .collect()
    }

    pub fn request_sms_conversations(&self, device: &str) -> Result<()> {
        self.require_plugin("sms")?;
        self.paired_ctx(device)?.send(NetworkPacket::new(
            plugins::sms::TYPE_REQUEST_CONVERSATIONS,
            json!({}),
        ))
    }

    /// Demande au téléphone les derniers messages d'une conversation.
    pub fn request_sms_thread(&self, device: &str, thread: i64) -> Result<()> {
        self.require_plugin("sms")?;
        self.paired_ctx(device)?.send(NetworkPacket::new(
            plugins::sms::TYPE_REQUEST_CONVERSATION,
            plugins::sms::conversation_request_body(thread, 100),
        ))
    }

    /// Envoie un SMS ; le téléphone renvoie ensuite le message envoyé, qu'on redemande au besoin.
    pub fn send_sms(self: &Arc<Self>, device: &str, addresses: &[String], text: &str) -> Result<()> {
        self.require_plugin("sms")?;
        if text.trim().is_empty() {
            bail!("message vide");
        }
        if addresses.is_empty() || !addresses.iter().all(|a| crate::sms::valid_recipient(a)) {
            bail!("destinataire invalide");
        }
        self.paired_ctx(device)?.send(NetworkPacket::new(
            plugins::sms::TYPE_REQUEST,
            plugins::sms::send_body(addresses, text),
        ))?;
        // Le message envoyé apparaît dans la base du téléphone un instant plus tard.
        let (state, device) = (self.clone(), device.to_owned());
        tokio::spawn(async move {
            for delay in [2, 5] {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                let _ = state.request_sms_conversations(&device);
            }
        });
        Ok(())
    }

    // ---- historique des notifications --------------------------------------

    pub fn notifications(&self, device: &str) -> Vec<Value> {
        lock(&self.history).get(device).cloned().unwrap_or_default()
    }

    pub(crate) fn push_notification(&self, device: &str, notif: Value) {
        let mut h = lock(&self.history);
        let list = h.entry(device.to_owned()).or_default();
        let id = notif.get("id").cloned();
        list.retain(|n| n.get("id") != id.as_ref());
        list.push(notif);
        if list.len() > MAX_HISTORY {
            list.remove(0);
        }
    }

    pub(crate) fn remove_notification(&self, device: &str, notif_id: &str) {
        if let Some(list) = lock(&self.history).get_mut(device) {
            list.retain(|n| n.get("id").and_then(Value::as_str) != Some(notif_id));
        }
    }
}

// ---- multimédia ------------------------------------------------------------

/// Actions de lecture acceptées par le téléphone.
const MEDIA_ACTIONS: [&str; 6] = ["Play", "Pause", "PlayPause", "Next", "Previous", "Stop"];

impl State {
    /// Dossier des pochettes téléchargées depuis le téléphone.
    pub fn art_dir(&self) -> PathBuf {
        self.cache_dir.join("art")
    }

    /// Lecteurs connus de l'appareil, avec leurs métadonnées.
    pub fn media(&self, device: &str) -> Vec<Value> {
        lock(&self.media)
            .get(device)
            .map(|players| players.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Remplace la liste des lecteurs (ceux qui ont disparu sont oubliés).
    pub(crate) fn set_players(&self, device: &str, names: &[String]) {
        let mut media = lock(&self.media);
        let players = media.entry(device.to_owned()).or_default();
        players.retain(|name, _| names.contains(name));
        for name in names {
            players
                .entry(name.clone())
                .or_insert_with(|| json!({ "player": name }));
        }
        drop(media);
        self.emit(Event::MediaChanged(device.to_owned()));
    }

    /// Fusionne des métadonnées reçues ; renvoie la valeur résultante.
    pub(crate) fn update_player(
        &self,
        device: &str,
        player: &str,
        f: impl FnOnce(&mut Value),
    ) -> Value {
        let mut media = lock(&self.media);
        let entry = media
            .entry(device.to_owned())
            .or_default()
            .entry(player.to_owned())
            .or_insert_with(|| json!({ "player": player }));
        f(entry);
        let value = entry.clone();
        drop(media);
        self.emit(Event::MediaChanged(device.to_owned()));
        value
    }

    fn media_request(&self, device: &str, body: Value) -> Result<()> {
        self.require_plugin("mpris")?;
        self.paired_ctx(device)?
            .send(NetworkPacket::new("kdeconnect.mpris.request", body))
    }

    pub fn media_action(&self, device: &str, player: &str, action: &str) -> Result<()> {
        if !MEDIA_ACTIONS.contains(&action) {
            bail!("action multimédia inconnue : {action}");
        }
        self.media_request(device, json!({ "player": player, "action": action }))
    }

    /// Volume du lecteur, de 0 à 100.
    pub fn media_volume(&self, device: &str, player: &str, volume: i32) -> Result<()> {
        self.media_request(
            device,
            json!({ "player": player, "setVolume": volume.clamp(0, 100) }),
        )
    }

    /// Position dans le morceau, en millisecondes.
    pub fn media_seek(&self, device: &str, player: &str, position_ms: i64) -> Result<()> {
        self.media_request(
            device,
            json!({ "player": player, "SetPosition": position_ms.max(0) }),
        )
    }
}
