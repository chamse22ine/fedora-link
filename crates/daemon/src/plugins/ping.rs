use super::{Plugin, PluginCtx};
use anyhow::Result;
use fedora_link_core::packet::{NetworkPacket, TYPE_PING};
use tracing::info;

pub struct PingPlugin;

impl Plugin for PingPlugin {
    fn name(&self) -> &'static str {
        "ping"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_PING]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_PING]
    }
    fn handle_packet(&self, ctx: &PluginCtx, _packet: &NetworkPacket) -> Result<()> {
        info!(device = %ctx.device_name, "ping reçu");
        super::desktop_notify(
            "Fedora Link".into(),
            ctx.device_name.clone(),
            "Ping !".into(),
        );
        Ok(())
    }
}
