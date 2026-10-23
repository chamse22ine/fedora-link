//! Une session = un lien TCP+TLS avec un appareil : handshake, appairage, paquets.

use crate::plugins::PluginCtx;
use crate::state::State;
use anyhow::{bail, Context as _, Result};
use fedora_link_core::cert::{common_name, verification_code};
use fedora_link_core::packet::{IdentityBody, NetworkPacket, TYPE_IDENTITY, TYPE_PAIR};
use fedora_link_core::tls;
use rustls::pki_types::ServerName;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{debug, info, warn};

/// Taille maximale d'une ligne de paquet (protège contre un pair malveillant).
const MAX_LINE: u64 = 1 << 20;

/// Le téléphone s'est connecté à nous : il envoie son identité en clair puis
/// nous jouons le rôle de client TLS (les rôles TLS sont inversés dans KDE Connect).
pub async fn serve_incoming(state: Arc<State>, mut tcp: TcpStream, addr: SocketAddr) -> Result<()> {
    let line = tokio::time::timeout(Duration::from_secs(10), read_plain_line(&mut tcp))
        .await
        .context("identité non reçue à temps")??;
    let peer = NetworkPacket::from_line(&line)?.into_identity()?;

    let connector = TlsConnector::from(tls::client_config(&state.identity)?);
    let server_name = ServerName::IpAddress(addr.ip().into());
    let stream = connector
        .connect(server_name, tcp)
        .await
        .context("handshake TLS (client)")?;
    let cert = peer_cert(stream.get_ref().1.peer_certificates())?;
    run_session(state, stream, peer, cert, addr).await
}

/// Nous avons vu le broadcast du téléphone et nous nous connectons à lui :
/// nous envoyons notre identité en clair puis jouons le serveur TLS.
pub async fn serve_outgoing(
    state: Arc<State>,
    mut tcp: TcpStream,
    addr: SocketAddr,
    peer: IdentityBody,
) -> Result<()> {
    let mut mine = state.identity_body.clone();
    mine.tcp_port = None;
    tcp.write_all(&NetworkPacket::identity(&mine)?.to_line()?)
        .await?;
    let acceptor = TlsAcceptor::from(tls::server_config(&state.identity)?);
    let stream = acceptor
        .accept(tcp)
        .await
        .context("handshake TLS (serveur)")?;
    let cert = peer_cert(stream.get_ref().1.peer_certificates())?;
    run_session(state, stream, peer, cert, addr).await
}

fn peer_cert(certs: Option<&[rustls::pki_types::CertificateDer<'_>]>) -> Result<Vec<u8>> {
    certs
        .and_then(|c| c.first())
        .map(|c| c.to_vec())
        .context("le pair n'a présenté aucun certificat")
}

/// Lit la ligne d'identité octet par octet : aucun octet TLS ne doit être
/// consommé par un tampon avant le handshake.
async fn read_plain_line(tcp: &mut TcpStream) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        tcp.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() as u64 > MAX_LINE {
            bail!("ligne d'identité trop longue");
        }
    }
    Ok(String::from_utf8(line)?)
}

/// Lit les paquets du lien et les transmet à la session (annulable sans perte).
async fn read_packets<R: AsyncRead + Unpin>(read: R, out: mpsc::Sender<NetworkPacket>) {
    let mut read = BufReader::new(read);
    loop {
        let mut line = String::new();
        match (&mut read).take(MAX_LINE).read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => match NetworkPacket::from_line(&line) {
                Ok(p) => {
                    if out.send(p).await.is_err() {
                        return;
                    }
                }
                Err(e) => warn!("paquet ignoré : {e:#}"),
            },
        }
    }
}

async fn run_session<S>(
    state: Arc<State>,
    stream: S,
    peer: IdentityBody,
    cert: Vec<u8>,
    addr: SocketAddr,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Le CN du certificat doit correspondre à l'identifiant annoncé.
    if common_name(&cert)? != peer.device_id {
        bail!(
            "le certificat ne correspond pas à l'identifiant {}",
            peer.device_id
        );
    }
    let mut trusted = state.trust.is_trusted(&peer.device_id, &cert);
    info!(device = %peer.device_name, %addr, trusted, "lien établi");
    debug!(
        incoming = ?peer.incoming_capabilities,
        outgoing = ?peer.outgoing_capabilities,
        "capacités annoncées par l'appareil"
    );

    let (read, mut write) = tokio::io::split(stream);
    let (in_tx, mut in_rx) = mpsc::channel(64);
    let reader = tokio::spawn(read_packets(read, in_tx));
    let (out_tx, mut out_rx) = mpsc::channel(64);

    let ctx = PluginCtx {
        state: state.clone(),
        device_id: peer.device_id.clone(),
        device_name: peer.device_name.clone(),
        ip: addr.ip(),
        peer_cert: cert.clone(),
        tx: out_tx,
    };
    let link_id = state.next_link_id();
    state.register_link(&ctx, &peer, trusted, link_id);
    if trusted {
        remember_ip(&state, &peer.device_id, addr);
        state.link_ready(&ctx);
    }

    let result = async {
        loop {
            tokio::select! {
                packet = in_rx.recv() => {
                    let Some(packet) = packet else {
                        info!(device = %peer.device_name, "lien fermé");
                        return Ok(());
                    };
                    match (packet.kind.as_str(), packet.pair_flag()) {
                        (TYPE_PAIR, Some(true)) => {
                            if !trusted {
                                // Si nous avions demandé l'appairage, cette réponse le confirme.
                                let ours = state.take_outgoing_pair(&peer.device_id);
                                let code = verification_code(&state.identity.cert_der, &cert)?;
                                if !ours && !state.confirm_pairing(&peer.device_id, &peer.device_name, &code).await {
                                    info!(device = %peer.device_name, "appairage refusé");
                                    send(&mut write, &NetworkPacket::pair(false)).await?;
                                    continue;
                                }
                                state.trust.add(&peer.device_id, &peer.device_name, &cert)?;
                                trusted = true;
                                state.update_device(&peer.device_id, |i| i.paired = true);
                                remember_ip(&state, &peer.device_id, addr);
                                info!(device = %peer.device_name, "appairage accepté");
                                if !ours {
                                    send(&mut write, &NetworkPacket::pair(true)).await?;
                                }
                                state.link_ready(&ctx);
                            } else {
                                send(&mut write, &NetworkPacket::pair(true)).await?;
                            }
                        }
                        (TYPE_PAIR, Some(false)) => {
                            state.take_outgoing_pair(&peer.device_id);
                            state.trust.remove(&peer.device_id)?;
                            trusted = false;
                            state.update_device(&peer.device_id, |i| i.paired = false);
                            info!(device = %peer.device_name, "désappairé par l'appareil");
                        }
                        // Un appareil non appairé ne reçoit ni n'émet rien d'autre.
                        _ if !trusted => {
                            debug!(kind = %packet.kind, "paquet d'un appareil non appairé");
                            send(&mut write, &NetworkPacket::pair(false)).await?;
                        }
                        (TYPE_IDENTITY, _) => {}
                        _ => {
                            debug!(kind = %packet.kind, "paquet reçu");
                            state.dispatch(&ctx, &packet)
                        }
                    }
                }
                Some(packet) = out_rx.recv() => {
                    if trusted || packet.kind == TYPE_PAIR {
                        send(&mut write, &packet).await?;
                    }
                }
            }
        }
    }
    .await;

    reader.abort();
    state.link_closed(&peer.device_id, link_id);
    // Lien perdu (Wi-Fi coupé, veille…) : on tente de le rétablir quelques instants.
    if state.trust.is_trusted(&peer.device_id, &cert) {
        crate::spawn_reconnect(state.clone(), peer.device_id.clone());
    }
    result
}

fn remember_ip(state: &State, id: &str, addr: SocketAddr) {
    if let Err(e) = state.trust.set_last_ip(id, addr.ip()) {
        warn!("adresse non mémorisée : {e:#}");
    }
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, packet: &NetworkPacket) -> Result<()> {
    w.write_all(&packet.to_line()?).await?;
    w.flush().await?;
    Ok(())
}
