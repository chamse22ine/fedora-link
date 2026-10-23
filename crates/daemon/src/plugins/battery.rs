//! Batterie du téléphone : niveau, charge et alerte de batterie faible.
//! Sens inverse : la batterie du PC (UPower) est envoyée au téléphone s'il en a une.

use super::{desktop_notify, Plugin, PluginCtx};
use anyhow::{Context, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::debug;
use zbus::fdo::PropertiesProxy;
use zbus::Connection;

const TYPE_BATTERY: &str = "kdeconnect.battery";
const TYPE_BATTERY_REQUEST: &str = "kdeconnect.battery.request";
const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";
/// UPower : états « en charge » et « pleine ».
const STATE_CHARGING: u32 = 1;
const STATE_FULL: u32 = 4;
const LOW_LEVEL: i32 = 15;
const RECOVERED_LEVEL: i32 = 20;

#[derive(Default)]
pub struct BatteryPlugin {
    /// Appareils déjà alertés (évite de répéter l'alerte à chaque paquet).
    alerted: Mutex<HashSet<String>>,
}

impl Plugin for BatteryPlugin {
    fn name(&self) -> &'static str {
        "battery"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_BATTERY, TYPE_BATTERY_REQUEST]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_BATTERY, TYPE_BATTERY_REQUEST]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        let _ = ctx.send(NetworkPacket::new(
            TYPE_BATTERY_REQUEST,
            json!({ "request": true }),
        ));
        tokio::spawn(watch_pc_battery(ctx.clone()));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        if packet.kind == TYPE_BATTERY_REQUEST {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Some(body) = pc_battery().await {
                    let _ = ctx.send(NetworkPacket::new(TYPE_BATTERY, body));
                }
            });
            return Ok(());
        }
        let level = packet
            .body
            .get("currentCharge")
            .and_then(Value::as_i64)
            .context("currentCharge manquant")? as i32;
        let charging = packet
            .body
            .get("isCharging")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let threshold = packet
            .body
            .get("thresholdEvent")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        ctx.state.update_device(&ctx.device_id, |i| {
            i.battery = Some(level);
            i.charging = charging;
        });

        let mut alerted = self.alerted.lock().unwrap_or_else(|e| e.into_inner());
        if charging || level > RECOVERED_LEVEL {
            alerted.remove(&ctx.device_id);
        } else if (threshold == 1 || level <= LOW_LEVEL) && alerted.insert(ctx.device_id.clone()) {
            desktop_notify(
                "Fedora Link".into(),
                format!("Batterie faible — {}", ctx.device_name),
                format!("Il reste {level} %."),
            );
        }
        Ok(())
    }
}

/// Corps du paquet `kdeconnect.battery` pour la batterie du PC.
pub(crate) fn pc_battery_body(percentage: f64, state: u32) -> Value {
    json!({
        "currentCharge": percentage.round().clamp(0.0, 100.0) as i32,
        "isCharging": state == STATE_CHARGING || state == STATE_FULL,
        "thresholdEvent": 0,
    })
}

/// Lit la batterie agrégée d'UPower ; `None` sur un PC fixe ou sans UPower.
async fn pc_battery() -> Option<Value> {
    let conn = Connection::system().await.ok()?;
    read_pc_battery(&conn).await
}

async fn read_pc_battery(conn: &Connection) -> Option<Value> {
    let proxy = zbus::Proxy::new(conn, UPOWER, DISPLAY_DEVICE, DEVICE_IFACE)
        .await
        .ok()?;
    if !proxy.get_property::<bool>("IsPresent").await.ok()? {
        return None;
    }
    let percentage: f64 = proxy.get_property("Percentage").await.ok()?;
    let state: u32 = proxy.get_property("State").await.ok()?;
    Some(pc_battery_body(percentage, state))
}

/// Envoie la batterie du PC au branchement du lien, puis à chaque changement.
async fn watch_pc_battery(ctx: PluginCtx) {
    if !ctx.state.plugin_enabled("battery") {
        return;
    }
    let Ok(conn) = Connection::system().await else {
        return;
    };
    let mut last: Option<Value> = None;
    let mut send_if_changed = |body: Option<Value>| match body {
        Some(b) if last.as_ref() != Some(&b) => {
            let sent = ctx.send(NetworkPacket::new(TYPE_BATTERY, b.clone())).is_ok();
            last = Some(b);
            sent
        }
        _ => !ctx.tx.is_closed(),
    };
    if !send_if_changed(read_pc_battery(&conn).await) {
        return;
    }
    let Ok(props) = PropertiesProxy::new(&conn, UPOWER, DISPLAY_DEVICE).await else {
        return;
    };
    let Ok(mut changes) = props.receive_properties_changed().await else {
        return;
    };
    // Le lien peut se fermer sans qu'UPower émette : on le vérifie régulièrement.
    let mut tick = interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            change = futures_util::StreamExt::next(&mut changes) => {
                if change.is_none() { return; }
            }
            _ = tick.tick() => {}
        }
        if !send_if_changed(read_pc_battery(&conn).await) {
            debug!("lien fermé, fin du suivi de la batterie du PC");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batterie_du_pc_en_charge() {
        let b = pc_battery_body(72.4, 1);
        assert_eq!(b["currentCharge"], 72);
        assert_eq!(b["isCharging"], true);
    }

    #[test]
    fn batterie_du_pc_sur_secteur_pleine_ou_sur_batterie() {
        assert_eq!(pc_battery_body(100.0, 4)["isCharging"], true);
        assert_eq!(pc_battery_body(40.0, 2)["isCharging"], false);
        assert_eq!(pc_battery_body(130.0, 2)["currentCharge"], 100);
    }
}
