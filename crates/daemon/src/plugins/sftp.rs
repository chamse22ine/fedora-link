//! Parcourir le téléphone : le téléphone ouvre un serveur SFTP et nous en donne l'adresse et
//! un mot de passe à usage temporaire. Le montage (gvfs) est fait par l'interface.
//! Protocole : `kdeconnect.sftp.request` {startBrowsing} → `kdeconnect.sftp`.

use super::{Plugin, PluginCtx};
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;
use serde::Serialize;
use serde_json::Value;
use std::net::IpAddr;

pub(crate) const TYPE_SFTP: &str = "kdeconnect.sftp";
pub(crate) const TYPE_REQUEST: &str = "kdeconnect.sftp.request";

pub struct SftpPlugin;

impl Plugin for SftpPlugin {
    fn name(&self) -> &'static str {
        "sftp"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_SFTP]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        // Le téléphone envoie aussi `serverRunning` : sans demande en cours, rien à faire.
        if packet.body.get("serverRunning").is_some() {
            return Ok(());
        }
        ctx.state
            .finish_sftp(&ctx.device_id, parse(&packet.body, ctx.ip));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Storage {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SftpInfo {
    pub ip: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Dossier à ouvrir d'abord (le premier stockage s'il y en a plusieurs).
    pub path: String,
    pub storages: Vec<Storage>,
    pub uri: String,
}

/// Encode un chemin pour une URI en gardant les `/`.
pub(crate) fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Analyse la réponse du téléphone ; `Err` porte le message d'erreur à montrer à l'utilisateur.
pub(crate) fn parse(body: &Value, peer: IpAddr) -> Result<SftpInfo, String> {
    if let Some(msg) = body.get("errorMessage").and_then(Value::as_str) {
        return Err(msg.to_owned());
    }
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
    let (Some(user), Some(password)) = (text("user"), text("password")) else {
        return Err("réponse SFTP incomplète du téléphone".into());
    };
    let port = body
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .ok_or("port SFTP manquant")?;

    let paths: Vec<String> = body
        .get("multiPaths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let names: Vec<String> = body
        .get("pathNames")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let storages: Vec<Storage> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| Storage {
            name: names.get(i).cloned().unwrap_or_else(|| p.clone()),
            path: p.clone(),
        })
        .collect();
    let path = storages
        .first()
        .map(|s| s.path.clone())
        .or_else(|| text("path"))
        .unwrap_or_else(|| "/".into());
    if !path.starts_with('/') {
        return Err("chemin SFTP invalide".into());
    }
    // L'adresse annoncée par le téléphone peut être celle d'une autre interface : on garde celle du lien.
    let ip = peer.to_string();
    let host = if peer.is_ipv6() { format!("[{ip}]") } else { ip.clone() };
    let uri = format!("sftp://{}@{host}:{port}{}", encode_path(&user), encode_path(&path));
    Ok(SftpInfo {
        ip,
        port,
        user,
        password,
        path,
        storages,
        uri,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_link_core::packet::NetworkPacket;

    fn body(line: &str) -> Value {
        NetworkPacket::from_line(line).unwrap().body
    }
    const PEER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20));

    // Réponse de KDE Connect Android avec deux stockages (interne + carte SD).
    #[test]
    fn deux_stockages() {
        let b = body(r#"{"id":1729504400000,"type":"kdeconnect.sftp","body":{"ip":"192.168.1.20","port":1740,"user":"kdeconnect","password":"Xy12AbCd34","path":"/","multiPaths":["/storage/emulated/0","/storage/1234-ABCD"],"pathNames":["Stockage interne","Carte SD"]}}"#);
        let info = parse(&b, PEER).unwrap();
        assert_eq!(info.uri, "sftp://kdeconnect@192.168.1.20:1740/storage/emulated/0");
        assert_eq!(info.storages.len(), 2);
        assert_eq!(info.storages[1].name, "Carte SD");
        assert_eq!(info.password, "Xy12AbCd34");
    }

    #[test]
    fn un_seul_chemin() {
        let b = body(r#"{"id":1,"type":"kdeconnect.sftp","body":{"ip":"192.168.1.20","port":1741,"user":"kdeconnect","password":"pw","path":"/storage/emulated/0/Mes photos"}}"#);
        let info = parse(&b, PEER).unwrap();
        assert_eq!(info.uri, "sftp://kdeconnect@192.168.1.20:1741/storage/emulated/0/Mes%20photos");
        assert!(info.storages.is_empty());
    }

    #[test]
    fn erreurs_du_telephone() {
        let b = body(r#"{"id":2,"type":"kdeconnect.sftp","body":{"errorMessage":"Autorisation de stockage manquante"}}"#);
        assert_eq!(parse(&b, PEER).unwrap_err(), "Autorisation de stockage manquante");
        let b = body(r#"{"id":3,"type":"kdeconnect.sftp","body":{"port":1740}}"#);
        assert!(parse(&b, PEER).is_err());
        let b = body(r#"{"id":4,"type":"kdeconnect.sftp","body":{"port":1740,"user":"u","password":"p","path":"relatif"}}"#);
        assert!(parse(&b, PEER).is_err(), "chemin non absolu refusé");
    }

    #[test]
    fn encodage_des_chemins() {
        assert_eq!(encode_path("/a b/é#"), "/a%20b/%C3%A9%23");
    }
}
