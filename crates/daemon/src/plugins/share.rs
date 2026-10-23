//! Partage de fichiers. Le paquet `share.request` annonce un transfert ; les données
//! passent par une connexion TCP+TLS séparée, ouverte par celui qui reçoit.

use super::{Plugin, PluginCtx};
use crate::state::Event;
use anyhow::{bail, Context, Result};
use fedora_link_core::packet::NetworkPacket;
use fedora_link_core::tls;
use rustls::pki_types::ServerName;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{debug, info, warn};

const TYPE_SHARE: &str = "kdeconnect.share.request";
const TYPE_SHARE_UPDATE: &str = "kdeconnect.share.request.update";
const PAYLOAD_PORT_MIN: u16 = 1739;
const PAYLOAD_PORT_MAX: u16 = 1764;
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

pub struct SharePlugin;

impl Plugin for SharePlugin {
    fn name(&self) -> &'static str {
        "share"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_SHARE, TYPE_SHARE_UPDATE]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_SHARE]
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        if packet.kind != TYPE_SHARE {
            return Ok(());
        }
        let filename = match classify(&packet.body) {
            Share::File(name) => name,
            Share::Url(url) => return receive_link(ctx, url),
            Share::Text(text) => return receive_text(ctx, text),
            Share::Nothing => {
                debug!(device = %ctx.device_name, "partage sans contenu");
                return Ok(());
            }
        };
        let port = packet
            .payload_transfer_info
            .as_ref()
            .and_then(|i| i.get("port"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .context("port de transfert manquant")?;
        let size = packet.payload_size.and_then(|s| u64::try_from(s).ok());

        let (ctx, filename) = (ctx.clone(), filename);
        tokio::spawn(async move {
            if let Err(e) = receive_file(&ctx, &filename, size, port).await {
                warn!(file = %filename, "réception échouée : {e:#}");
            }
        });
        Ok(())
    }
}

/// Ce que contient un paquet `share.request`.
#[derive(Debug, PartialEq)]
enum Share {
    File(String),
    Url(String),
    Text(String),
    Nothing,
}

fn classify(body: &Value) -> Share {
    let field = |k: &str| body.get(k).and_then(Value::as_str);
    if let Some(name) = field("filename") {
        Share::File(name.to_owned())
    } else if let Some(url) = field("url").filter(|u| !u.trim().is_empty()) {
        Share::Url(url.trim().to_owned())
    } else if let Some(text) = field("text").filter(|t| !t.trim().is_empty()) {
        Share::Text(text.to_owned())
    } else {
        Share::Nothing
    }
}

fn receive_link(ctx: &PluginCtx, url: String) -> Result<()> {
    info!(device = %ctx.device_name, "lien reçu");
    super::desktop_notify(
        "Fedora Link".into(),
        format!("Lien reçu de {}", ctx.device_name),
        url.clone(),
    );
    ctx.emit(Event::LinkReceived {
        device: ctx.device_id.clone(),
        url: url.clone(),
    });
    if ctx.state.open_links {
        tokio::spawn(async move {
            if let Err(e) = crate::desktop::open_url(&url).await {
                warn!("ouverture du lien : {e:#}");
            }
        });
    }
    Ok(())
}

/// Un texte : copié par l'interface. S'il se réduit à un lien web, on l'ouvre aussi (comme KDE Connect).
fn receive_text(ctx: &PluginCtx, text: String) -> Result<()> {
    if let Some(url) = crate::desktop::web_url(&text) {
        return receive_link(ctx, url);
    }
    info!(device = %ctx.device_name, "texte reçu");
    ctx.emit(Event::TextReceived {
        device: ctx.device_id.clone(),
        text,
    });
    Ok(())
}

/// Garde uniquement le nom du fichier (jamais de chemin fourni par le pair).
pub(crate) fn sanitize(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .unwrap_or("fichier")
        .to_owned()
}

/// `a.txt` → `a (1).txt` si le fichier existe déjà.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str());
    (1..)
        .map(|n| match ext {
            Some(e) => dir.join(format!("{stem} ({n}).{e}")),
            None => dir.join(format!("{stem} ({n})")),
        })
        .find(|c| !c.exists())
        .unwrap_or(candidate)
}

/// Ouvre la connexion de transfert vers l'appareil (nous sommes client TLS) et vérifie son certificat.
async fn connect_payload(
    ctx: &PluginCtx,
    port: u16,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect((ctx.ip, port))
        .await
        .context("connexion de transfert")?;
    let connector = TlsConnector::from(tls::client_config(&ctx.state.identity)?);
    let stream = connector
        .connect(ServerName::IpAddress(ctx.ip.into()), tcp)
        .await
        .context("TLS de transfert")?;
    check_peer(&ctx.peer_cert, stream.get_ref().1.peer_certificates())?;
    Ok(stream)
}

/// Télécharge un petit contenu (pochette…) en mémoire, avec une taille maximale.
pub(crate) async fn fetch_bytes(ctx: &PluginCtx, port: u16, max: u64) -> Result<Vec<u8>> {
    let mut stream = connect_payload(ctx, port).await?;
    let mut data = Vec::new();
    (&mut stream)
        .take(max + 1)
        .read_to_end(&mut data)
        .await
        .context("lecture du contenu")?;
    if data.len() as u64 > max {
        bail!("contenu trop volumineux (plus de {max} octets)");
    }
    Ok(data)
}

async fn receive_file(ctx: &PluginCtx, filename: &str, size: Option<u64>, port: u16) -> Result<()> {
    let mut stream = connect_payload(ctx, port).await?;

    let name = sanitize(filename);
    let dir = ctx.state.download_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .context("création du dossier de réception")?;
    let path = unique_path(&dir, &name);
    let part = path.with_extension("part");
    let mut file = tokio::fs::File::create(&part).await?;

    let total = size.unwrap_or(0);
    let (mut done, mut last_report) = (0u64, Instant::now());
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        done += n as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            progress(ctx, &name, done, total, true);
        }
    }
    file.flush().await?;
    drop(file);
    if size.is_some_and(|s| s != done) {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("transfert incomplet : {done} octets sur {total}");
    }
    tokio::fs::rename(&part, &path).await?;
    progress(ctx, &name, done, done, true);
    info!(file = %path.display(), "fichier reçu");
    super::desktop_notify(
        "Fedora Link".into(),
        format!("Fichier reçu de {}", ctx.device_name),
        name.clone(),
    );
    let entry = crate::received::ReceivedFile {
        device: ctx.device_id.clone(),
        name: name.clone(),
        path: path.display().to_string(),
        size: done,
        time: now_millis(),
    };
    if let Err(e) = ctx.state.received.add(entry) {
        warn!("journal des fichiers reçus : {e:#}");
    }
    ctx.emit(Event::FileReceived {
        device: ctx.device_id.clone(),
        path: path.display().to_string(),
    });
    Ok(())
}

pub async fn send_file(ctx: PluginCtx, path: PathBuf) -> Result<()> {
    let meta = tokio::fs::metadata(&path)
        .await
        .context("fichier illisible")?;
    if !meta.is_file() {
        bail!("{} n'est pas un fichier", path.display());
    }
    let size = meta.len();
    let name = sanitize(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("fichier"),
    );

    let listener = bind_payload().await?;
    let port = listener.local_addr()?.port();
    let modified = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or_else(now_millis, |d| d.as_millis() as i64);

    let mut packet = NetworkPacket::new(
        TYPE_SHARE,
        json!({
            "filename": name,
            "lastModified": modified,
            "numberOfFiles": 1,
            "totalPayloadSize": size,
        }),
    );
    packet.payload_size = Some(size as i64);
    packet.payload_transfer_info = Some(json!({ "port": port }));
    ctx.send(packet)?;

    let (tcp, _) = tokio::time::timeout(ACCEPT_TIMEOUT, listener.accept())
        .await
        .context("le téléphone ne s'est pas connecté")??;
    let acceptor = TlsAcceptor::from(tls::server_config(&ctx.state.identity)?);
    let mut stream = acceptor.accept(tcp).await.context("TLS de transfert")?;
    check_peer(&ctx.peer_cert, stream.get_ref().1.peer_certificates())?;

    let mut file = tokio::fs::File::open(&path).await?;
    let (mut done, mut last_report) = (0u64, Instant::now());
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
        done += n as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            progress(&ctx, &name, done, size, false);
        }
    }
    stream.shutdown().await?;
    progress(&ctx, &name, done, size, false);
    info!(file = %path.display(), "fichier envoyé");
    Ok(())
}

/// Annonce un paquet avec contenu (pochette…) et l'envoie à qui se connecte pour le récupérer.
pub(crate) async fn offer_payload(ctx: &PluginCtx, mut packet: NetworkPacket, data: Vec<u8>) -> Result<()> {
    let listener = bind_payload().await?;
    packet.payload_size = Some(data.len() as i64);
    packet.payload_transfer_info = Some(json!({ "port": listener.local_addr()?.port() }));
    ctx.send(packet)?;
    let (tcp, _) = tokio::time::timeout(ACCEPT_TIMEOUT, listener.accept())
        .await
        .context("le téléphone ne s'est pas connecté")??;
    let acceptor = TlsAcceptor::from(tls::server_config(&ctx.state.identity)?);
    let mut stream = acceptor.accept(tcp).await.context("TLS de transfert")?;
    check_peer(&ctx.peer_cert, stream.get_ref().1.peer_certificates())?;
    stream.write_all(&data).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn bind_payload() -> Result<TcpListener> {
    for port in PAYLOAD_PORT_MIN..=PAYLOAD_PORT_MAX {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", port)).await {
            return Ok(l);
        }
    }
    bail!("aucun port de transfert libre ({PAYLOAD_PORT_MIN}-{PAYLOAD_PORT_MAX})")
}

/// Le certificat présenté sur la connexion de transfert doit être celui de l'appareil appairé.
fn check_peer(
    expected: &[u8],
    presented: Option<&[rustls::pki_types::CertificateDer<'_>]>,
) -> Result<()> {
    match presented.and_then(|c| c.first()) {
        Some(c) if c.as_ref() == expected => Ok(()),
        _ => bail!("certificat de transfert inattendu"),
    }
}

fn progress(ctx: &PluginCtx, name: &str, done: u64, total: u64, incoming: bool) {
    ctx.emit(Event::TransferProgress {
        device: ctx.device_id.clone(),
        name: name.to_owned(),
        done,
        total,
        incoming,
    });
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nettoie_les_chemins() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize(".."), "fichier");
        assert_eq!(sanitize(""), "fichier");
    }

    // Paquets tels qu'émis par KDE Connect Android.
    fn body(line: &str) -> Value {
        NetworkPacket::from_line(line).unwrap().body
    }

    #[test]
    fn reconnait_lien_texte_et_fichier() {
        let url = body(r#"{"id":1729504200001,"type":"kdeconnect.share.request","body":{"url":"https://fr.wikipedia.org/wiki/Fedora"}}"#);
        assert_eq!(classify(&url), Share::Url("https://fr.wikipedia.org/wiki/Fedora".into()));
        let text = body(r#"{"id":1729504200002,"type":"kdeconnect.share.request","body":{"text":"RDV 18h\nsalle 3"}}"#);
        assert_eq!(classify(&text), Share::Text("RDV 18h\nsalle 3".into()));
        let file = body(r#"{"id":1729504200003,"type":"kdeconnect.share.request","body":{"filename":"IMG_0001.jpg","creationTime":1729504100000,"lastModified":1729504100000,"open":false},"payloadSize":2048,"payloadTransferInfo":{"port":1739}}"#);
        assert_eq!(classify(&file), Share::File("IMG_0001.jpg".into()));
        let empty = body(r#"{"id":1,"type":"kdeconnect.share.request","body":{"text":"  "}}"#);
        assert_eq!(classify(&empty), Share::Nothing);
    }

    #[test]
    fn nom_unique() {
        let dir = std::env::temp_dir().join(format!("fl-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
