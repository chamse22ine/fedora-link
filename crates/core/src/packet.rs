//! Paquets du protocole KDE Connect : un objet JSON par ligne (terminé par `\n`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub const TYPE_IDENTITY: &str = "kdeconnect.identity";
pub const TYPE_PAIR: &str = "kdeconnect.pair";
pub const TYPE_PING: &str = "kdeconnect.ping";

/// Version du protocole annoncée (la 7 ne demande pas d'échange d'identité après TLS).
pub const PROTOCOL_VERSION: u32 = 7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkPacket {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub body: Value,
    #[serde(
        rename = "payloadSize",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_size: Option<i64>,
    #[serde(
        rename = "payloadTransferInfo",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_transfer_info: Option<Value>,
}

impl NetworkPacket {
    /// Crée un paquet dont l'id est l'horodatage courant en millisecondes.
    pub fn new(kind: impl Into<String>, body: Value) -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            id,
            kind: kind.into(),
            body,
            payload_size: None,
            payload_transfer_info: None,
        }
    }

    /// Sérialise en une ligne JSON terminée par `\n`.
    pub fn to_line(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self).context("sérialisation du paquet")?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        serde_json::from_str(line.trim_end()).context("paquet JSON invalide")
    }

    pub fn identity(identity: &IdentityBody) -> Result<Self> {
        Ok(Self::new(TYPE_IDENTITY, serde_json::to_value(identity)?))
    }

    pub fn pair(pair: bool) -> Self {
        Self::new(TYPE_PAIR, json!({ "pair": pair }))
    }

    pub fn ping() -> Self {
        Self::new(TYPE_PING, json!({}))
    }

    /// Valeur du champ `pair` si c'est un paquet d'appairage.
    pub fn pair_flag(&self) -> Option<bool> {
        if self.kind == TYPE_PAIR {
            self.body.get("pair").and_then(Value::as_bool)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityBody {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub incoming_capabilities: Vec<String>,
    #[serde(default)]
    pub outgoing_capabilities: Vec<String>,
    /// Absent dans les paquets échangés une fois la connexion établie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
}

impl NetworkPacket {
    /// Extrait le corps d'identité d'un paquet `kdeconnect.identity`.
    pub fn into_identity(self) -> Result<IdentityBody> {
        anyhow::ensure!(
            self.kind == TYPE_IDENTITY,
            "ce paquet n'est pas une identité"
        );
        serde_json::from_value(self.body).context("corps d'identité invalide")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exemple réel émis par l'appli KDE Connect Android.
    const IDENTITY_ANDROID: &str = r#"{"id":1698000000123,"type":"kdeconnect.identity","body":{"deviceId":"a1b2c3d4_e5f6_4789_9abc_def012345678","deviceName":"Pixel 8","deviceType":"phone","incomingCapabilities":["kdeconnect.ping","kdeconnect.notification.request"],"outgoingCapabilities":["kdeconnect.ping","kdeconnect.notification"],"protocolVersion":7,"tcpPort":1716}}"#;

    #[test]
    fn lit_une_identite_android() {
        let p = NetworkPacket::from_line(IDENTITY_ANDROID).unwrap();
        assert_eq!(p.kind, TYPE_IDENTITY);
        let id = p.into_identity().unwrap();
        assert_eq!(id.device_name, "Pixel 8");
        assert_eq!(id.device_type, "phone");
        assert_eq!(id.protocol_version, 7);
        assert_eq!(id.tcp_port, Some(1716));
        assert_eq!(id.incoming_capabilities.len(), 2);
    }

    #[test]
    fn aller_retour_identite() {
        let id = NetworkPacket::from_line(IDENTITY_ANDROID)
            .unwrap()
            .into_identity()
            .unwrap();
        let line = NetworkPacket::identity(&id).unwrap().to_line().unwrap();
        assert_eq!(*line.last().unwrap(), b'\n');
        let back = NetworkPacket::from_line(std::str::from_utf8(&line).unwrap())
            .unwrap()
            .into_identity()
            .unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn paquet_pair() {
        let p =
            NetworkPacket::from_line(r#"{"id":1,"type":"kdeconnect.pair","body":{"pair":true}}"#)
                .unwrap();
        assert_eq!(p.pair_flag(), Some(true));
        assert_eq!(NetworkPacket::pair(false).pair_flag(), Some(false));
        assert_eq!(NetworkPacket::ping().pair_flag(), None);
    }

    #[test]
    fn ping_sans_corps_et_champs_optionnels() {
        let p = NetworkPacket::from_line(r#"{"id":2,"type":"kdeconnect.ping"}"#).unwrap();
        assert_eq!(p.kind, TYPE_PING);
        let s = String::from_utf8(p.to_line().unwrap()).unwrap();
        assert!(!s.contains("payloadSize"));
    }

    #[test]
    fn json_invalide_est_une_erreur() {
        assert!(NetworkPacket::from_line("pas du json").is_err());
    }
}
