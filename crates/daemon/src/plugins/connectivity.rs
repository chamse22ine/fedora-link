//! Réseau mobile du téléphone : type de réseau (2G à 5G) et force du signal.
//! Protocole : `kdeconnect.connectivity_report` (téléphone → PC), `.request` (PC → téléphone).

use super::{Plugin, PluginCtx};
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};

const TYPE_REPORT: &str = "kdeconnect.connectivity_report";
const TYPE_REQUEST: &str = "kdeconnect.connectivity_report.request";

pub struct ConnectivityPlugin;

/// Réseau affiché dans le bandeau : « 4G » et un signal de 0 à 4.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Network {
    pub kind: String,
    pub signal: i32,
}

/// Ramène le nom Android du réseau (« LTE », « NR »…) à la génération affichée.
fn generation(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "GPRS" | "EDGE" | "GSM" | "CDMA" | "1XRTT" | "IDEN" => "2G".into(),
        "UMTS" | "HSDPA" | "HSUPA" | "HSPA" | "HSPA+" | "EVDO" | "EHRPD" | "TD-SCDMA" => "3G".into(),
        "LTE" | "LTE_CA" | "4G" => "4G".into(),
        "NR" | "5G" => "5G".into(),
        "" | "UNKNOWN" => String::new(),
        other => other.to_owned(),
    }
}

/// Lit le rapport du téléphone ; avec une double SIM, garde la carte au meilleur signal.
pub(crate) fn parse(body: &Value) -> Option<Network> {
    body.get("signalStrengths")?
        .as_object()?
        .values()
        .filter_map(|sim| {
            Some(Network {
                kind: generation(sim.get("networkType")?.as_str()?),
                signal: sim.get("signalStrength")?.as_i64()?.clamp(0, 4) as i32,
            })
        })
        .max_by_key(|n| n.signal)
}

impl Plugin for ConnectivityPlugin {
    fn name(&self) -> &'static str {
        "connectivity"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_REPORT]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        let _ = ctx.send(NetworkPacket::new(TYPE_REQUEST, json!({})));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let network = parse(&packet.body);
        ctx.state.update_device(&ctx.device_id, |i| {
            i.network_type = network.as_ref().map(|n| n.kind.clone());
            i.signal = network.as_ref().map(|n| n.signal);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn une_sim_en_4g() {
        let b = body(r#"{"signalStrengths":{"1":{"networkType":"LTE","signalStrength":3}}}"#);
        assert_eq!(parse(&b), Some(Network { kind: "4G".into(), signal: 3 }));
    }

    #[test]
    fn double_sim_garde_le_meilleur_signal() {
        let b = body(
            r#"{"signalStrengths":{"1":{"networkType":"GSM","signalStrength":1},
                                   "2":{"networkType":"NR","signalStrength":4}}}"#,
        );
        assert_eq!(parse(&b), Some(Network { kind: "5G".into(), signal: 4 }));
    }

    #[test]
    fn reseau_inconnu_et_signal_hors_bornes() {
        let b = body(r#"{"signalStrengths":{"1":{"networkType":"Unknown","signalStrength":9}}}"#);
        assert_eq!(parse(&b), Some(Network { kind: String::new(), signal: 4 }));
    }

    #[test]
    fn rapport_vide_ou_invalide() {
        assert_eq!(parse(&body(r#"{"signalStrengths":{}}"#)), None);
        assert_eq!(parse(&body(r#"{}"#)), None);
    }
}
