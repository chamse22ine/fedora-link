//! Client D-Bus du daemon (via gio, sans runtime async supplémentaire).

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use serde::Deserialize;

const BUS_NAME: &str = "org.fedoralink.Daemon";
const OBJECT_PATH: &str = "/org/fedoralink/Daemon";
const INTERFACE: &str = "org.fedoralink.Daemon1";

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<i32>,
    pub charging: bool,
    #[serde(default)]
    pub network_type: Option<String>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub ip: Option<String>,
}

/// État de « Écouter sur le PC » : `stopped`, `starting`, `active` ou `error`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListenStatus {
    pub state: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListenOptions {
    pub keep_on_phone: bool,
    pub pause_players: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct Client {
    proxy: gio::DBusProxy,
}

impl Client {
    /// Crée le proxy ; démarre le daemon par activation D-Bus si un fichier `.service` existe.
    pub async fn connect(autostart: bool) -> Result<Self, glib::Error> {
        let proxy = gio::DBusProxy::for_bus_future(
            gio::BusType::Session,
            gio::DBusProxyFlags::NONE,
            None,
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
        )
        .await?;
        let client = Self { proxy };
        if autostart && !client.is_running() {
            // Un appel suffit à déclencher l'activation D-Bus (donc le service systemd).
            let _ = client.download_dir().await;
        }
        Ok(client)
    }

    pub fn is_running(&self) -> bool {
        self.proxy.name_owner().is_some()
    }

    /// Appelé quand le daemon apparaît ou disparaît du bus.
    pub fn on_owner_changed(&self, f: impl Fn(bool) + 'static) {
        self.proxy
            .connect_notify_local(Some("g-name-owner"), move |p, _| {
                f(p.name_owner().is_some())
            });
    }

    /// Appelé pour chaque signal du daemon : (nom, paramètres).
    pub fn on_signal(&self, f: impl Fn(&str, &glib::Variant) + 'static) {
        // Signature « g-signal » : (expéditeur, nom du signal, paramètres).
        self.proxy.connect_local("g-signal", false, move |args| {
            let name = args.get(2)?.get::<String>().ok()?;
            let params = args.get(3)?.get::<glib::Variant>().ok()?;
            f(&name, &params);
            None
        });
    }

    async fn call_raw(
        &self,
        method: &str,
        params: Option<glib::Variant>,
    ) -> Result<glib::Variant, glib::Error> {
        self.proxy
            .call_future(method, params.as_ref(), gio::DBusCallFlags::NONE, -1)
            .await
    }

    async fn call(
        &self,
        method: &str,
        params: Option<glib::Variant>,
    ) -> Result<String, glib::Error> {
        let reply = self.call_raw(method, params).await?;
        Ok(reply
            .try_child_value(0)
            .and_then(|v| v.str().map(str::to_owned))
            .unwrap_or_default())
    }

    pub async fn devices(&self) -> Result<Vec<Device>, glib::Error> {
        parse(&self.call("ListDevices", None).await?)
    }

    pub async fn plugins(&self) -> Result<Vec<PluginInfo>, glib::Error> {
        parse(&self.call("ListPlugins", None).await?)
    }

    pub async fn notifications(&self, device: &str) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(
            &self
                .call("ListNotifications", Some((device,).to_variant()))
                .await?,
        )
    }

    pub async fn media(&self, device: &str) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(&self.call("ListMedia", Some((device,).to_variant())).await?)
    }

    pub async fn media_action(
        &self,
        device: &str,
        player: &str,
        action: &str,
    ) -> Result<(), glib::Error> {
        self.call("MediaAction", Some((device, player, action).to_variant()))
            .await
            .map(drop)
    }

    pub async fn set_media_volume(
        &self,
        device: &str,
        player: &str,
        volume: i32,
    ) -> Result<(), glib::Error> {
        self.call(
            "SetMediaVolume",
            Some((device, player, volume).to_variant()),
        )
        .await
        .map(drop)
    }

    pub async fn download_dir(&self) -> Result<String, glib::Error> {
        self.call("DownloadDir", None).await
    }

    pub async fn pair(&self, device: &str) -> Result<(), glib::Error> {
        self.call("Pair", Some((device,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn unpair(&self, device: &str) -> Result<(), glib::Error> {
        self.call("Unpair", Some((device,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn answer_pairing(&self, device: &str, accept: bool) -> Result<(), glib::Error> {
        self.call("AnswerPairing", Some((device, accept).to_variant()))
            .await
            .map(drop)
    }

    pub async fn ring_phone(&self, device: &str) -> Result<(), glib::Error> {
        self.call("RingPhone", Some((device,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn send_file(&self, device: &str, path: &str) -> Result<(), glib::Error> {
        self.call("SendFile", Some((device, path).to_variant()))
            .await
            .map(drop)
    }

    pub async fn send_clipboard(&self, text: &str) -> Result<(), glib::Error> {
        self.call("SendClipboard", Some((text,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn share_url(&self, device: &str, url: &str) -> Result<(), glib::Error> {
        self.call("ShareUrl", Some((device, url).to_variant()))
            .await
            .map(drop)
    }

    pub async fn call_media_mode(&self) -> Result<String, glib::Error> {
        self.call("CallMediaMode", None).await
    }

    pub async fn set_call_media_mode(&self, mode: &str) -> Result<(), glib::Error> {
        self.call("SetCallMediaMode", Some((mode,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn mpris_proxy_enabled(&self) -> Result<bool, glib::Error> {
        let reply = self.call_raw("MprisProxyEnabled", None).await?;
        Ok(reply.try_child_value(0).and_then(|v| v.get::<bool>()).unwrap_or(true))
    }

    pub async fn set_mpris_proxy_enabled(&self, on: bool) -> Result<(), glib::Error> {
        self.call("SetMprisProxyEnabled", Some((on,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn listen_status(&self) -> Result<ListenStatus, glib::Error> {
        parse(&self.call("ListenStatus", None).await?)
    }

    /// Lance l'écoute du son du téléphone ; `device` peut être vide (USB sans lien KDE Connect).
    pub async fn start_listening(&self, device: &str) -> Result<(), glib::Error> {
        self.call("StartListening", Some((device,).to_variant()))
            .await
            .map(drop)
    }

    pub async fn stop_listening(&self) -> Result<(), glib::Error> {
        self.call("StopListening", None).await.map(drop)
    }

    pub async fn listen_options(&self) -> Result<ListenOptions, glib::Error> {
        parse(&self.call("ListenOptions", None).await?)
    }

    pub async fn set_listen_options(
        &self,
        keep_on_phone: bool,
        pause_players: bool,
    ) -> Result<(), glib::Error> {
        self.call(
            "SetListenOptions",
            Some((keep_on_phone, pause_players).to_variant()),
        )
        .await
        .map(drop)
    }

    pub async fn dismiss_notification(&self, device: &str, id: &str) -> Result<(), glib::Error> {
        self.call("DismissNotification", Some((device, id).to_variant()))
            .await
            .map(drop)
    }

    pub async fn reply_notification(
        &self,
        device: &str,
        reply_id: &str,
        message: &str,
    ) -> Result<(), glib::Error> {
        self.call(
            "ReplyNotification",
            Some((device, reply_id, message).to_variant()),
        )
        .await
        .map(drop)
    }

    pub async fn set_plugin_enabled(&self, name: &str, enabled: bool) -> Result<(), glib::Error> {
        self.call("SetPluginEnabled", Some((name, enabled).to_variant()))
            .await
            .map(drop)
    }
}

/// Lance `systemctl --user <args>` et rend sa sortie (vide en cas d'échec).
pub async fn systemctl(args: &[&str]) -> String {
    let mut argv = vec!["systemctl", "--user"];
    argv.extend(args);
    let run = async {
        let process = gio::Subprocess::newv(
            &argv.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>(),
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
        )?;
        let (out, _) = process.communicate_utf8_future(None).await?;
        Ok::<_, glib::Error>(out.map(|s| s.trim().to_owned()).unwrap_or_default())
    };
    run.await.unwrap_or_default()
}

pub const SERVICE: &str = "fedora-link.service";

/// Faux seulement si l'utilisateur a désactivé le service : on ne le relance alors pas à l'ouverture.
pub async fn service_wanted() -> bool {
    systemctl(&["is-enabled", SERVICE]).await != "disabled"
}

fn parse<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, glib::Error> {
    serde_json::from_str(json)
        .map_err(|e| glib::Error::new(gio::IOErrorEnum::InvalidData, &e.to_string()))
}

impl Client {
    pub async fn received_files(&self) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(&self.call("ListReceivedFiles", None).await?)
    }

    /// Adresse et identifiants du serveur SFTP du téléphone.
    pub async fn browse_phone(&self, device: &str) -> Result<serde_json::Value, glib::Error> {
        let json = self
            .call("BrowsePhone", Some((device,).to_variant()))
            .await?;
        serde_json::from_str(&json)
            .map_err(|e| glib::Error::new(gio::IOErrorEnum::InvalidData, &e.to_string()))
    }

    pub async fn commands(&self) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(&self.call("ListCommands", None).await?)
    }

    /// Remplace la liste des commandes à distance (JSON) ; le daemon la valide.
    pub async fn set_commands(&self, list: &[serde_json::Value]) -> Result<(), glib::Error> {
        let json = serde_json::Value::Array(list.to_vec()).to_string();
        self.call("SetCommands", Some((json.as_str(),).to_variant()))
            .await
            .map(drop)
    }

    pub async fn contacts(&self, device: &str) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(
            &self
                .call("ListContacts", Some((device,).to_variant()))
                .await?,
        )
    }

    pub async fn conversations(&self, device: &str) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(
            &self
                .call("ListConversations", Some((device,).to_variant()))
                .await?,
        )
    }

    pub async fn messages(
        &self,
        device: &str,
        thread: i64,
    ) -> Result<Vec<serde_json::Value>, glib::Error> {
        parse(
            &self
                .call("ListMessages", Some((device, thread).to_variant()))
                .await?,
        )
    }

    pub async fn request_conversation(&self, device: &str, thread: i64) -> Result<(), glib::Error> {
        self.call("RequestConversation", Some((device, thread).to_variant()))
            .await
            .map(drop)
    }

    pub async fn send_sms(
        &self,
        device: &str,
        addresses: &[String],
        text: &str,
    ) -> Result<(), glib::Error> {
        self.call("SendSms", Some((device, addresses, text).to_variant()))
            .await
            .map(drop)
    }
}
