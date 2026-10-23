//! « Trouver mon téléphone » : on n'envoie que `findmyphone.request`
//! (voir `State::ring_phone`), rien à recevoir.

use super::{Plugin, PluginCtx};
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;

pub struct FindMyPhonePlugin;

impl Plugin for FindMyPhonePlugin {
    fn name(&self) -> &'static str {
        "findmyphone"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &["kdeconnect.findmyphone.request"]
    }
    fn handle_packet(&self, _ctx: &PluginCtx, _packet: &NetworkPacket) -> Result<()> {
        Ok(())
    }
}
