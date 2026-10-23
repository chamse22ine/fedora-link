//! API D-Bus du daemon (bus de session) : `org.fedoralink.Daemon1`.
//! Les listes complexes sont renvoyées en JSON (chaîne) pour garder le contrat simple.

use crate::settings::{CallMedia, RemoteCommand};
use crate::state::{Event, State};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

pub const BUS_NAME: &str = "org.fedoralink.Daemon";
pub const OBJECT_PATH: &str = "/org/fedoralink/Daemon";

pub struct DbusService {
    state: Arc<State>,
}

fn fail(e: anyhow::Error) -> fdo::Error {
    fdo::Error::Failed(format!("{e:#}"))
}

#[interface(name = "org.fedoralink.Daemon1")]
impl DbusService {
    /// JSON : tableau d'objets {id, name, device_type, paired, connected, battery, charging}.
    async fn list_devices(&self) -> fdo::Result<String> {
        serde_json::to_string(&self.state.list_devices())
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// JSON : tableau de notifications (corps `kdeconnect.notification` bruts).
    async fn list_notifications(&self, device: &str) -> String {
        serde_json::Value::Array(self.state.notifications(device)).to_string()
    }

    /// JSON : tableau d'objets {name, enabled}.
    async fn list_plugins(&self) -> String {
        let list: Vec<_> = self
            .state
            .list_plugins()
            .into_iter()
            .map(|(name, enabled)| serde_json::json!({ "name": name, "enabled": enabled }))
            .collect();
        serde_json::Value::Array(list).to_string()
    }

    /// JSON : lecteurs multimédias du téléphone (titre, artiste, volume, chemin de la pochette…).
    async fn list_media(&self, device: &str) -> String {
        serde_json::Value::Array(self.state.media(device)).to_string()
    }

    /// `action` : Play, Pause, PlayPause, Next, Previous ou Stop.
    async fn media_action(&self, device: &str, player: &str, action: &str) -> fdo::Result<()> {
        self.state
            .media_action(device, player, action)
            .map_err(fail)
    }

    async fn set_media_volume(&self, device: &str, player: &str, volume: i32) -> fdo::Result<()> {
        self.state
            .media_volume(device, player, volume)
            .map_err(fail)
    }

    async fn seek_media(&self, device: &str, player: &str, position_ms: i64) -> fdo::Result<()> {
        self.state
            .media_seek(device, player, position_ms)
            .map_err(fail)
    }

    /// Envoie un lien http(s) au téléphone.
    async fn share_url(&self, device: &str, url: &str) -> fdo::Result<()> {
        self.state.share_url(device, url).map_err(fail)
    }

    /// Envoie un texte au téléphone.
    async fn share_text(&self, device: &str, text: &str) -> fdo::Result<()> {
        self.state.share_text(device, text).map_err(fail)
    }

    /// Comportement du son du PC pendant un appel : none, pause ou lower.
    async fn call_media_mode(&self) -> String {
        self.state.call_media().as_str().to_owned()
    }

    async fn set_call_media_mode(&self, mode: &str) -> fdo::Result<()> {
        let mode = CallMedia::parse(mode)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("mode inconnu : {mode}")))?;
        self.state.set_call_media(mode).map_err(fail)
    }

    /// Publier les lecteurs du téléphone sur le bus MPRIS (menu multimédia de GNOME).
    async fn mpris_proxy_enabled(&self) -> bool {
        self.state.mpris_proxy_enabled()
    }

    async fn set_mpris_proxy_enabled(&self, enabled: bool) -> fdo::Result<()> {
        self.state.set_mpris_proxy_enabled(enabled).map_err(fail)
    }

    /// JSON : {state: stopped|starting|active|error, detail} de « Écouter sur le PC ».
    async fn listen_status(&self) -> String {
        serde_json::to_string(&self.state.listen.status()).unwrap_or_default()
    }

    async fn start_listening(&self, device: &str) -> fdo::Result<()> {
        self.state.start_listening(device).map_err(fail)
    }

    async fn stop_listening(&self) {
        self.state.listen.stop();
    }

    /// JSON : {keep_on_phone, pause_players} — garder le son sur le téléphone, pauser les lecteurs du PC.
    async fn listen_options(&self) -> String {
        let (keep_on_phone, pause_players) = self.state.listen_options();
        serde_json::json!({ "keep_on_phone": keep_on_phone, "pause_players": pause_players })
            .to_string()
    }

    async fn set_listen_options(&self, keep_on_phone: bool, pause_players: bool) -> fdo::Result<()> {
        self.state
            .set_listen_options(keep_on_phone, pause_players)
            .map_err(fail)
    }

    #[zbus(signal)]
    async fn listen_changed(
        emitter: &SignalEmitter<'_>,
        state: &str,
        detail: &str,
    ) -> zbus::Result<()>;

    /// JSON : contacts en cache {uid, name, numbers, avatar}.
    async fn list_contacts(&self, device: &str) -> String {
        serde_json::to_string(&self.state.contacts.list(device)).unwrap_or_else(|_| "[]".into())
    }

    /// JSON : conversations {thread_id, addresses, title, avatar, body, date, incoming, unread}.
    async fn list_conversations(&self, device: &str) -> String {
        serde_json::Value::Array(self.state.sms_conversations(device)).to_string()
    }

    /// JSON : messages d'une conversation, du plus ancien au plus récent.
    async fn list_messages(&self, device: &str, thread_id: i64) -> String {
        serde_json::Value::Array(self.state.sms_messages(device, thread_id)).to_string()
    }

    async fn request_conversations(&self, device: &str) -> fdo::Result<()> {
        self.state.request_sms_conversations(device).map_err(fail)
    }

    async fn request_conversation(&self, device: &str, thread_id: i64) -> fdo::Result<()> {
        self.state.request_sms_thread(device, thread_id).map_err(fail)
    }

    async fn send_sms(&self, device: &str, addresses: Vec<String>, text: &str) -> fdo::Result<()> {
        self.state.send_sms(device, &addresses, text).map_err(fail)
    }

    /// JSON : fichiers reçus {device, name, path, size, time}, du plus récent au plus ancien.
    async fn list_received_files(&self) -> String {
        serde_json::to_string(&self.state.received.list()).unwrap_or_else(|_| "[]".into())
    }

    /// JSON : {uri, user, password, ip, port, path, storages} du serveur SFTP du téléphone.
    async fn browse_phone(&self, device: &str) -> fdo::Result<String> {
        let info = self.state.browse_phone(device).await.map_err(fail)?;
        serde_json::to_string(&info).map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// JSON : commandes à distance {key, name, command}.
    async fn list_commands(&self) -> String {
        serde_json::to_string(&self.state.commands()).unwrap_or_else(|_| "[]".into())
    }

    /// Remplace la liste des commandes (JSON) ; renvoie la liste validée, clés comprises.
    async fn set_commands(&self, commands_json: &str) -> fdo::Result<String> {
        let list: Vec<RemoteCommand> = serde_json::from_str(commands_json)
            .map_err(|e| fdo::Error::InvalidArgs(format!("liste de commandes invalide : {e}")))?;
        let saved = self.state.set_commands(list).map_err(fail)?;
        serde_json::to_string(&saved).map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn download_dir(&self) -> String {
        self.state.download_dir().display().to_string()
    }

    async fn set_plugin_enabled(&self, name: &str, enabled: bool) -> fdo::Result<()> {
        self.state.set_plugin_enabled(name, enabled).map_err(fail)
    }

    async fn pair(&self, device: &str) -> fdo::Result<()> {
        self.state.pair(device).map_err(fail)
    }

    async fn unpair(&self, device: &str) -> fdo::Result<()> {
        self.state.unpair(device).map_err(fail)
    }

    async fn answer_pairing(&self, device: &str, accept: bool) -> fdo::Result<()> {
        self.state.answer_pairing(device, accept).map_err(fail)
    }

    async fn ring_phone(&self, device: &str) -> fdo::Result<()> {
        self.state.ring_phone(device).map_err(fail)
    }

    async fn send_file(&self, device: &str, path: &str) -> fdo::Result<()> {
        self.state
            .send_file(device, PathBuf::from(path))
            .map_err(fail)
    }

    /// Envoie le texte à tous les appareils appairés connectés.
    async fn send_clipboard(&self, text: &str) -> fdo::Result<()> {
        self.state.send_clipboard(text).map_err(fail)
    }

    async fn dismiss_notification(&self, device: &str, notification_id: &str) -> fdo::Result<()> {
        self.state
            .dismiss_notification(device, notification_id)
            .map_err(fail)
    }

    async fn reply_notification(
        &self,
        device: &str,
        reply_id: &str,
        message: &str,
    ) -> fdo::Result<()> {
        self.state
            .reply_notification(device, reply_id, message)
            .map_err(fail)
    }

    #[zbus(signal)]
    async fn media_changed(emitter: &SignalEmitter<'_>, device: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn device_changed(emitter: &SignalEmitter<'_>, device: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn pairing_requested(
        emitter: &SignalEmitter<'_>,
        device: &str,
        name: &str,
        code: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_posted(
        emitter: &SignalEmitter<'_>,
        device: &str,
        json: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_dismissed(
        emitter: &SignalEmitter<'_>,
        device: &str,
        notification_id: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn clipboard_received(
        emitter: &SignalEmitter<'_>,
        device: &str,
        text: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn transfer_progress(
        emitter: &SignalEmitter<'_>,
        device: &str,
        name: &str,
        done: u64,
        total: u64,
        incoming: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn link_received(emitter: &SignalEmitter<'_>, device: &str, url: &str)
        -> zbus::Result<()>;

    #[zbus(signal)]
    async fn text_received(
        emitter: &SignalEmitter<'_>,
        device: &str,
        text: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn contacts_changed(emitter: &SignalEmitter<'_>, device: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn sms_changed(
        emitter: &SignalEmitter<'_>,
        device: &str,
        thread_id: i64,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn presenter_moved(emitter: &SignalEmitter<'_>, dx: f64, dy: f64) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn presenter_stopped(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn file_received(
        emitter: &SignalEmitter<'_>,
        device: &str,
        path: &str,
    ) -> zbus::Result<()>;
}

/// Publie le service sur le bus de session et relaie les événements en signaux.
pub async fn serve(state: Arc<State>) -> Result<zbus::Connection> {
    let conn = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            DbusService {
                state: state.clone(),
            },
        )?
        .build()
        .await?;

    let mut rx = state.subscribe();
    let relay = conn.clone();
    let relay_state = state.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(e) = emit(&relay, &relay_state, event).await {
                        tracing::warn!("signal D-Bus : {e}");
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
    Ok(conn)
}

async fn emit(conn: &zbus::Connection, state: &State, event: Event) -> zbus::Result<()> {
    let iface = conn
        .object_server()
        .interface::<_, DbusService>(OBJECT_PATH)
        .await?;
    let em = iface.signal_emitter();
    match event {
        Event::DeviceChanged(id) => DbusService::device_changed(em, &id).await,
        Event::PairingRequested { id, name, code } => {
            DbusService::pairing_requested(em, &id, &name, &code).await
        }
        Event::NotificationPosted { device, json } => {
            DbusService::notification_posted(em, &device, &json).await
        }
        Event::NotificationDismissed { device, id } => {
            DbusService::notification_dismissed(em, &device, &id).await
        }
        Event::ClipboardReceived { device, text } => {
            DbusService::clipboard_received(em, &device, &text).await
        }
        Event::TransferProgress {
            device,
            name,
            done,
            total,
            incoming,
        } => DbusService::transfer_progress(em, &device, &name, done, total, incoming).await,
        Event::MediaChanged(device) => DbusService::media_changed(em, &device).await,
        Event::FileReceived { device, path } => {
            DbusService::file_received(em, &device, &path).await
        }
        Event::ContactsChanged(device) => DbusService::contacts_changed(em, &device).await,
        Event::SmsChanged { device, thread } => {
            DbusService::sms_changed(em, &device, thread).await
        }
        Event::PresenterMoved { dx, dy } => DbusService::presenter_moved(em, dx, dy).await,
        Event::PresenterStopped => DbusService::presenter_stopped(em).await,
        Event::LinkReceived { device, url } => {
            DbusService::link_received(em, &device, &url).await
        }
        Event::SettingsChanged => Ok(()),
        Event::ListenChanged => {
            let status = state.listen.status();
            DbusService::listen_changed(em, status.state, &status.detail).await
        }
        Event::TextReceived { device, text } => {
            DbusService::text_received(em, &device, &text).await
        }
    }
}
