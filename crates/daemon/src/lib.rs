//! Daemon Fedora Link : découverte UDP, liens TCP+TLS, appairage, ping.

pub mod contacts;
pub mod dbus;
mod desktop;
mod input;
pub mod listen;
pub mod mprisproxy;
mod pcaudio;
pub mod plugins;
pub mod received;
mod session;
pub mod settings;
pub mod sms;
pub mod state;
pub mod trust;

pub use state::{DeviceInfo, Event, State};

use anyhow::{bail, Context, Result};
use fedora_link_core::cert::DeviceIdentity;
use fedora_link_core::packet::{IdentityBody, NetworkPacket, PROTOCOL_VERSION, TYPE_IDENTITY};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub const DEFAULT_UDP_PORT: u16 = 1716;
pub const TCP_PORT_MIN: u16 = 1714;
pub const TCP_PORT_MAX: u16 = 1764;

pub struct Config {
    pub config_dir: PathBuf,
    pub device_name: String,
    /// Port UDP de découverte (0 = choisi par le système, utile en test).
    pub udp_port: u16,
    pub tcp_port_range: (u16, u16),
    /// Annonce l'appareil en broadcast au démarrage.
    pub broadcast: bool,
    /// Accepte les demandes d'appairage sans demander (tests uniquement).
    pub auto_accept: bool,
    /// Ouvre réellement les liens reçus (désactivé en test).
    pub open_links: bool,
    /// Dossier de réception des fichiers (par défaut : celui de `config.toml`).
    pub download_dir: Option<PathBuf>,
    /// Données durables (contacts, pièces jointes) : `~/.local/share/fedora-link`.
    pub data_dir: PathBuf,
    /// Caches reconstructibles (pochettes) : `~/.cache/fedora-link`.
    pub cache_dir: PathBuf,
}

pub struct RunningDaemon {
    pub state: Arc<State>,
    pub device_id: String,
    pub tcp_port: u16,
    pub udp_port: u16,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDaemon {
    /// Attend la fin des tâches (jamais, sauf arrêt).
    pub async fn wait(self) {
        for t in self.tasks {
            let _ = t.await;
        }
    }

    pub fn stop(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Lie les sockets, publie l'identité et lance les tâches d'écoute.
pub async fn start(config: Config) -> Result<RunningDaemon> {
    let identity = DeviceIdentity::load_or_create(&config.config_dir)?;
    let trust = trust::TrustStore::load(config.config_dir.join("devices.toml"))?;
    let settings_path = config.config_dir.join("config.toml");
    let mut settings = settings::Settings::load(&settings_path)?;
    if let Some(dir) = config.download_dir.clone() {
        settings.download_dir = dir;
    }
    let (incoming_capabilities, outgoing_capabilities) = State::capabilities(&settings);

    let udp = bind_udp(config.udp_port).await?;
    let udp_port = udp.local_addr()?.port();
    let listener = bind_tcp(config.tcp_port_range).await?;
    let tcp_port = listener.local_addr()?.port();

    let identity_body = IdentityBody {
        device_id: identity.device_id.clone(),
        device_name: config.device_name.clone(),
        device_type: "desktop".into(),
        protocol_version: PROTOCOL_VERSION,
        incoming_capabilities,
        outgoing_capabilities,
        tcp_port: Some(tcp_port),
    };
    info!(device_id = %identity.device_id, tcp_port, udp_port, "daemon démarré");

    let ctx = Arc::new(State::new(
        identity,
        identity_body,
        trust,
        settings,
        settings_path,
        config.auto_accept,
        config.open_links,
        config.data_dir.clone(),
        config.cache_dir.clone(),
    ));
    let device_id = ctx.identity.device_id.clone();

    // Reconnexion immédiate aux appareils déjà appairés, sans attendre leur annonce.
    for (id, _, _) in ctx.trust.reachable() {
        spawn_reconnect(ctx.clone(), id);
    }

    let tasks = vec![
        tokio::spawn(accept_loop(ctx.clone(), listener)),
        tokio::spawn(discovery_loop(ctx.clone(), udp, config.broadcast)),
    ];
    Ok(RunningDaemon {
        state: ctx,
        device_id,
        tcp_port,
        udp_port,
        tasks,
    })
}

async fn bind_udp(port: u16) -> Result<UdpSocket> {
    match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == ErrorKind::AddrInUse => bail!(
            "le port UDP {port} est déjà utilisé : KDE Connect ou GSConnect est probablement \
             lancé. Fermez-le avant de démarrer Fedora Link."
        ),
        Err(e) => Err(e).context("ouverture du socket UDP"),
    }
}

async fn bind_tcp((min, max): (u16, u16)) -> Result<TcpListener> {
    for port in min..=max {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e).context("ouverture du port TCP"),
        }
    }
    bail!("aucun port TCP libre entre {min} et {max}")
}

/// Connexions entrantes : le téléphone a reçu notre broadcast et se connecte.
async fn accept_loop(ctx: Arc<State>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = session::serve_incoming(ctx, stream, addr).await {
                        warn!(%addr, "connexion entrante terminée : {e:#}");
                    }
                });
            }
            Err(e) => warn!("accept() a échoué : {e}"),
        }
    }
}

/// Annonce notre identité puis écoute celles des autres appareils.
async fn discovery_loop(ctx: Arc<State>, udp: UdpSocket, broadcast: bool) {
    if broadcast {
        if let Err(e) = send_broadcast(&ctx, &udp).await {
            warn!("broadcast d'identité impossible : {e:#}");
        }
    }
    let mut buf = vec![0u8; 8192];
    loop {
        let (n, from) = match udp.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                warn!("lecture UDP : {e}");
                continue;
            }
        };
        let Ok(text) = std::str::from_utf8(&buf[..n]) else {
            continue;
        };
        let Ok(packet) = NetworkPacket::from_line(text) else {
            debug!(%from, "datagramme UDP ignoré");
            continue;
        };
        if packet.kind != TYPE_IDENTITY {
            continue;
        }
        let Ok(peer) = packet.into_identity() else {
            continue;
        };
        if peer.device_id == ctx.identity.device_id {
            continue; // notre propre broadcast
        }
        let Some(port) = peer.tcp_port else { continue };
        if ctx.is_connected(&peer.device_id) {
            continue; // déjà relié : l'annonce du téléphone n'apporte rien
        }
        info!(device = %peer.device_name, %from, "appareil détecté");
        let ctx = ctx.clone();
        let addr = SocketAddr::new(from.ip(), port);
        tokio::spawn(async move {
            let result = async {
                let stream = TcpStream::connect(addr).await?;
                session::serve_outgoing(ctx, stream, addr, peer).await
            }
            .await;
            if let Err(e) = result {
                warn!(%addr, "connexion sortante terminée : {e:#}");
            }
        });
    }
}

async fn send_broadcast(ctx: &State, udp: &UdpSocket) -> Result<()> {
    udp.set_broadcast(true)?;
    let line = NetworkPacket::identity(&ctx.identity_body)?.to_line()?;
    udp.send_to(&line, (Ipv4Addr::BROADCAST, DEFAULT_UDP_PORT))
        .await
        .context("envoi du broadcast")?;
    Ok(())
}

/// Délais entre deux tentatives de reconnexion : borné, puis on attend l'annonce du téléphone.
const RECONNECT_DELAYS: [Duration; 5] = [
    Duration::from_millis(0),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Se reconnecte à un appareil appairé à sa dernière IP connue.
pub(crate) fn spawn_reconnect(state: Arc<State>, id: String) {
    tokio::spawn(async move {
        for delay in RECONNECT_DELAYS {
            tokio::time::sleep(delay).await;
            if state.is_connected(&id) {
                return;
            }
            let Some((_, name, ip)) = state
                .trust
                .reachable()
                .into_iter()
                .find(|(d, _, _)| *d == id)
            else {
                return; // désappairé entre-temps
            };
            let addr = SocketAddr::new(ip, DEFAULT_UDP_PORT);
            let attempt = tokio::time::timeout(RECONNECT_TIMEOUT, TcpStream::connect(addr)).await;
            let Ok(Ok(stream)) = attempt else {
                debug!(%addr, "reconnexion : appareil injoignable");
                continue;
            };
            let peer = IdentityBody {
                device_id: id.clone(),
                device_name: name,
                device_type: "phone".into(),
                protocol_version: PROTOCOL_VERSION,
                incoming_capabilities: vec![],
                outgoing_capabilities: vec![],
                tcp_port: None,
            };
            info!(%addr, "reconnexion à l'appareil appairé");
            if let Err(e) = session::serve_outgoing(state.clone(), stream, addr, peer).await {
                debug!(%addr, "reconnexion terminée : {e:#}");
            }
            return;
        }
    });
}
