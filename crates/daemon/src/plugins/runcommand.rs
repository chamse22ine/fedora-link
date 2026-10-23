//! Commandes à distance : le téléphone lance, sur le PC, des commandes que l'utilisateur a
//! lui-même déclarées. Rien d'autre ne s'exécute jamais : le téléphone n'envoie qu'une clé.
//! Protocole : `kdeconnect.runcommand.request` → `kdeconnect.runcommand` (liste).

use super::{Plugin, PluginCtx};
use crate::settings::RemoteCommand;
use anyhow::{bail, Result};
use fedora_link_core::packet::NetworkPacket;
use serde_json::{json, Map, Value};
use std::process::Stdio;
use tracing::{info, warn};

const TYPE_LIST: &str = "kdeconnect.runcommand";
const TYPE_REQUEST: &str = "kdeconnect.runcommand.request";

pub struct RunCommandPlugin;

impl Plugin for RunCommandPlugin {
    fn name(&self) -> &'static str {
        "runcommand"
    }
    fn incoming(&self) -> &'static [&'static str] {
        &[TYPE_REQUEST]
    }
    fn outgoing(&self) -> &'static [&'static str] {
        &[TYPE_LIST]
    }

    fn on_link_ready(&self, ctx: &PluginCtx) {
        let _ = ctx.send(command_list_packet(&ctx.state.commands()));
    }

    fn handle_packet(&self, ctx: &PluginCtx, packet: &NetworkPacket) -> Result<()> {
        let body = &packet.body;
        if body.get("requestCommandList").and_then(Value::as_bool) == Some(true) {
            return ctx.send(command_list_packet(&ctx.state.commands()));
        }
        // « setup » (éditer la liste) et « stop » ne sont pas pris en charge.
        let Some(key) = body.get("key").and_then(Value::as_str) else {
            return Ok(());
        };
        match find(&ctx.state.commands(), key) {
            Some(c) => run(ctx, c),
            None => bail!("commande inconnue demandée par {} : {key:?}", ctx.device_name),
        }
    }
}

/// Seule façon de désigner une commande : sa clé, dans la liste de l'utilisateur.
pub(crate) fn find<'a>(commands: &'a [RemoteCommand], key: &str) -> Option<&'a RemoteCommand> {
    commands.iter().find(|c| c.key == key)
}

/// `commandList` est une chaîne JSON {clé: {name, command}}, comme dans KDE Connect.
pub(crate) fn command_list_packet(commands: &[RemoteCommand]) -> NetworkPacket {
    let map: Map<String, Value> = commands
        .iter()
        .map(|c| (c.key.clone(), json!({ "name": c.name, "command": c.command })))
        .collect();
    NetworkPacket::new(
        TYPE_LIST,
        json!({ "commandList": Value::Object(map).to_string(), "canAddCommand": false }),
    )
}

fn run(ctx: &PluginCtx, c: &RemoteCommand) -> Result<()> {
    info!(device = %ctx.device_name, name = %c.name, "commande lancée depuis le téléphone");
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&c.command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let name = c.name.clone();
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if !status.success() => warn!(%name, %status, "commande terminée en erreur"),
            Err(e) => warn!(%name, "attente de la commande : {e}"),
            _ => {}
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_commands;

    #[test]
    fn liste_au_format_kde_connect() {
        let packet = command_list_packet(&default_commands());
        assert_eq!(packet.kind, "kdeconnect.runcommand");
        assert_eq!(packet.body["canAddCommand"], false);
        // La liste voyage comme une chaîne JSON, pas comme un objet.
        let list: Value = serde_json::from_str(packet.body["commandList"].as_str().unwrap()).unwrap();
        assert_eq!(list["lock"]["name"], "Verrouiller l'écran");
        assert_eq!(list["poweroff"]["command"], "systemctl poweroff");
        assert_eq!(list.as_object().unwrap().len(), 3);
    }

    #[test]
    fn seules_les_commandes_de_la_liste_existent() {
        let list = default_commands();
        assert!(find(&list, "suspend").is_some());
        // Une commande envoyée en clair à la place d'une clé n'est jamais trouvée.
        assert!(find(&list, "rm -rf ~").is_none());
        assert!(find(&list, "").is_none());
    }
}
