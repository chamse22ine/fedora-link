//! Télécommande de présentation : le téléphone déplace un pointeur laser à l'écran.
//! Les touches (page suivante/précédente, F5, Échap) arrivent par le plugin pavé tactile.
//! Protocole : `kdeconnect.presenter` {dx, dy} ou {stop: true}.

use super::{Plugin, PluginCtx};
use crate::state::Event;
use anyhow::Result;
use fedora_link_core::packet::NetworkPacket;
use serde_json::Value;

const TYPE_PRESENTER: &str = "kdeconnect.presenter";

pub struct PresenterPlugin;

/// Ce que demande le téléphone.
#[derive(Debug, PartialEq)]
pub(crate) enum Pointer {
    Move(f64, f64),
    Stop,
}

pub(crate) fn parse(body: &Value) -> Option<Pointer> {
    if body.get("stop").and_then(Value::as_bool) == Some(true) {
        return Some(Pointer::Stop);
    }
    let (dx, dy) = (body.get("dx")?.as_f64()?, body.get("dy")?.as_f64()?);
    Some(Pointer::Move(dx, dy))
}

impl Plugin for PresenterPlugin {
    fn name(&self) -> &'static str {
        "presenter"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_PRESENTER]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[]
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        match parse(&packet.body) {
            Some(Pointer::Move(dx, dy)) => ctx.emit(Event::PresenterMoved { dx, dy }),
            Some(Pointer::Stop) => ctx.emit(Event::PresenterStopped),
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_link_core::packet::NetworkPacket;

    #[test]
    fn deplacement_et_arret() {
        let b = |l: &str| NetworkPacket::from_line(l).unwrap().body;
        assert_eq!(
            parse(&b(r#"{"id":1,"type":"kdeconnect.presenter","body":{"dx":-0.0125,"dy":0.31}}"#)),
            Some(Pointer::Move(-0.0125, 0.31))
        );
        assert_eq!(
            parse(&b(r#"{"id":2,"type":"kdeconnect.presenter","body":{"stop":true}}"#)),
            Some(Pointer::Stop)
        );
        assert_eq!(parse(&b(r#"{"id":3,"type":"kdeconnect.presenter","body":{}}"#)), None);
    }
}
