//! Intégration au bureau via D-Bus : notifications avec boutons d'action, ouverture
//! d'un lien par le portail OpenURI.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::{debug, warn};
use zbus::zvariant::Value;
use zbus::Connection;

static SESSION: OnceCell<Connection> = OnceCell::const_new();

/// Connexion au bus de session, partagée par tous les appels du daemon.
pub(crate) async fn session() -> Result<Connection> {
    SESSION
        .get_or_try_init(|| async { Connection::session().await })
        .await
        .cloned()
        .context("bus de session indisponible")
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// Notification GNOME avec boutons ; `wait_action` rend la clé du bouton cliqué.
pub(crate) struct ActionNotification {
    pub id: u32,
    proxy: NotificationsProxy<'static>,
    actions: ActionInvokedStream,
}

/// Échappe le balisage que les serveurs de notification interprètent dans le corps.
pub(crate) fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl ActionNotification {
    /// `actions` : paires (clé, libellé). Une notification `urgent` reste affichée tant qu'on ne la ferme pas.
    pub async fn show(
        summary: &str,
        body: &str,
        actions: &[(&str, &str)],
        icon: &str,
        urgent: bool,
    ) -> Result<Self> {
        let conn = session().await?;
        let proxy = NotificationsProxy::new(&conn).await?;
        // Abonnement avant l'affichage : aucun clic ne peut être manqué.
        let stream = proxy.receive_action_invoked().await?;
        let flat: Vec<&str> = actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
        let hints = HashMap::from([
            ("urgency", Value::U8(if urgent { 2 } else { 1 })),
            ("desktop-entry", Value::from("org.fedoralink.Gui")),
        ]);
        let id = proxy
            .notify(
                "Fedora Link",
                0,
                icon,
                summary,
                body,
                &flat,
                hints,
                if urgent { 0 } else { -1 },
            )
            .await
            .context("affichage de la notification")?;
        Ok(Self { id, proxy, actions: stream })
    }

    /// Attend un clic sur un bouton de cette notification.
    pub async fn wait_action(&mut self) -> Option<String> {
        while let Some(signal) = self.actions.next().await {
            match signal.args() {
                Ok(a) if a.id == self.id => return Some(a.action_key.to_owned()),
                Ok(_) => {}
                Err(e) => debug!("signal d'action illisible : {e}"),
            }
        }
        None
    }

    pub async fn close(&self) {
        if let Err(e) = self.proxy.close_notification(self.id).await {
            debug!("fermeture de la notification : {e}");
        }
    }
}

/// Ferme une notification déjà affichée (identifiant rendu par `show`).
pub(crate) async fn close_notification(id: u32) {
    let result = async {
        let proxy = NotificationsProxy::new(&session().await?).await?;
        proxy.close_notification(id).await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        debug!("fermeture de la notification {id} : {e:#}");
    }
}

/// Un lien web que l'on accepte d'ouvrir : http(s), sans espace ni caractère de contrôle.
pub(crate) fn web_url(s: &str) -> Option<String> {
    let s = s.trim();
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let valid = !host.is_empty()
        && !host.starts_with(['.', ':', '@'])
        && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then(|| s.to_owned())
}

/// Ouvre un lien web avec le navigateur par défaut (portail OpenURI, puis `xdg-open`).
pub(crate) async fn open_url(url: &str) -> Result<()> {
    let Some(url) = web_url(url) else {
        bail!("lien refusé (seuls http et https sont ouverts)");
    };
    match open_with_portal(&url).await {
        Ok(()) => return Ok(()),
        Err(e) => warn!("portail OpenURI indisponible ({e:#}), repli sur xdg-open"),
    }
    let status = tokio::process::Command::new("xdg-open")
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("xdg-open introuvable")?;
    if !status.success() {
        bail!("xdg-open a échoué ({status})");
    }
    Ok(())
}

const GUI_NAME: &str = "org.fedoralink.Gui";
const GUI_PATH: &str = "/org/fedoralink/Gui";

async fn activate_gui_action(action: &str, argument: &str) -> Result<()> {
    let conn = session().await?;
    let proxy = zbus::Proxy::new(&conn, GUI_NAME, GUI_PATH, "org.freedesktop.Application").await?;
    let params = vec![Value::from(argument)];
    let platform: HashMap<&str, Value<'_>> = HashMap::new();
    proxy
        .call_method("ActivateAction", &(action, params, platform))
        .await?;
    Ok(())
}

/// Ouvre l'interface sur une conversation SMS ; la lance d'abord si elle n'est pas ouverte.
pub(crate) async fn open_gui_thread(device: &str, thread: i64) -> Result<()> {
    let argument = format!("{device}|{thread}");
    if activate_gui_action("open-thread", &argument).await.is_ok() {
        return Ok(());
    }
    tokio::process::Command::new("fedora-link")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("lancement de l'interface")?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if activate_gui_action("open-thread", &argument).await.is_ok() {
            return Ok(());
        }
    }
    bail!("l'interface n'a pas répondu")
}

async fn open_with_portal(url: &str) -> Result<()> {
    let conn = session().await?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI",
    )
    .await?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let _: zbus::zvariant::OwnedObjectPath = proxy.call("OpenURI", &("", url, options)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seuls_les_liens_web_sont_ouverts() {
        assert_eq!(
            web_url(" https://exemple.org/a?b=1#c \n").as_deref(),
            Some("https://exemple.org/a?b=1#c")
        );
        assert!(web_url("http://192.168.1.2:8080").is_some());
        for refuse in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "tel:+33612345678",
            "https://",
            "https:///chemin",
            "https://exemple.org/un lien",
            "https://exemple.org/\u{7}",
            "bonjour",
            "",
        ] {
            assert!(web_url(refuse).is_none(), "{refuse:?} aurait dû être refusé");
        }
    }

    /// Vérifie le contrat avec le vrai serveur de notifications : `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "nécessite un bureau avec bus de session"]
    async fn notification_reelle_avec_bouton() {
        let notif = ActionNotification::show("Fedora Link (test)", "Couper la sonnerie ?", &[("mute", "Couper la sonnerie")], "call-start-symbolic", true)
            .await
            .expect("notification affichée");
        assert!(notif.id > 0);
        notif.close().await;
    }

    #[test]
    fn echappe_le_balisage() {
        assert_eq!(escape_markup("A & <b>B</b>"), "A &amp; &lt;b&gt;B&lt;/b&gt;");
    }
}
