//! Plugins : une fonctionnalité = un module, activable séparément.

pub mod battery;
pub mod clipboard;
pub mod connectivity;
pub mod contacts;
pub mod findmyphone;
pub mod mousepad;
pub mod mpris;
pub mod mprispc;
pub mod notification;
pub mod ping;
pub mod presenter;
pub mod runcommand;
pub mod share;
pub mod sftp;
pub mod sms;
pub mod systemvolume;
pub mod telephony;

use crate::state::{Event, State};
use anyhow::{anyhow, Result};
use fedora_link_core::packet::NetworkPacket;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Contexte d'un lien actif avec un appareil, remis aux plugins.
#[derive(Clone)]
pub struct PluginCtx {
    pub state: Arc<State>,
    pub device_id: String,
    pub device_name: String,
    pub ip: IpAddr,
    /// Certificat DER présenté par l'appareil (pour authentifier les transferts).
    pub peer_cert: Vec<u8>,
    pub(crate) tx: mpsc::Sender<NetworkPacket>,
}

impl PluginCtx {
    /// Met un paquet en file d'envoi (non bloquant).
    pub fn send(&self, packet: NetworkPacket) -> Result<()> {
        self.tx
            .try_send(packet)
            .map_err(|e| anyhow!("envoi impossible vers {} : {e}", self.device_name))
    }

    pub fn emit(&self, event: Event) {
        self.state.emit(event);
    }
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    /// Types de paquets que ce plugin sait recevoir.
    fn incoming(&self) -> &'static [&'static str];
    /// Types de paquets que ce plugin peut envoyer.
    fn outgoing(&self) -> &'static [&'static str];
    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()>;
    /// Appelé quand le lien avec un appareil appairé est prêt.
    fn on_link_ready(&self, _ctx: &PluginCtx) {}
}

pub fn all() -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(ping::PingPlugin),
        Arc::new(battery::BatteryPlugin::default()),
        Arc::new(notification::NotificationPlugin),
        Arc::new(clipboard::ClipboardPlugin::default()),
        Arc::new(findmyphone::FindMyPhonePlugin),
        Arc::new(share::SharePlugin),
        Arc::new(mpris::MprisPlugin),
        Arc::new(telephony::TelephonyPlugin::default()),
        Arc::new(contacts::ContactsPlugin),
        Arc::new(sms::SmsPlugin),
        Arc::new(sftp::SftpPlugin),
        Arc::new(mousepad::MousepadPlugin::default()),
        Arc::new(mprispc::MprisPcPlugin::default()),
        Arc::new(systemvolume::SystemVolumePlugin::default()),
        Arc::new(runcommand::RunCommandPlugin),
        Arc::new(presenter::PresenterPlugin),
        Arc::new(connectivity::ConnectivityPlugin),
    ]
}

/// Affiche une notification GNOME sans bloquer le runtime ; échec = simple avertissement.
pub(crate) fn desktop_notify(app: String, summary: String, body: String) {
    tokio::task::spawn_blocking(move || {
        let result = notify_rust::Notification::new()
            .appname(&app)
            .summary(&summary)
            .body(&body)
            .show();
        if let Err(e) = result {
            tracing::warn!("notification de bureau impossible : {e}");
        }
    });
}
