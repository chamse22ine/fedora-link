//! Préférences : plugins activés et dossier de réception (`config.toml`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Que faire du son du PC quand le téléphone sonne ou qu'un appel est en cours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallMedia {
    #[default]
    None,
    Pause,
    Lower,
}

impl CallMedia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pause => "pause",
            Self::Lower => "lower",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "pause" => Some(Self::Pause),
            "lower" => Some(Self::Lower),
            _ => None,
        }
    }
}

/// Commande que le téléphone peut lancer sur le PC. Seules celles de cette liste s'exécutent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteCommand {
    /// Identifiant stable envoyé au téléphone.
    pub key: String,
    pub name: String,
    pub command: String,
}

pub fn default_commands() -> Vec<RemoteCommand> {
    let cmd = |key: &str, name: &str, command: &str| RemoteCommand {
        key: key.into(),
        name: name.into(),
        command: command.into(),
    };
    vec![
        cmd(
            "lock",
            "Verrouiller l'écran",
            "gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver --method org.gnome.ScreenSaver.Lock",
        ),
        cmd("suspend", "Mettre en veille", "systemctl suspend"),
        cmd("poweroff", "Éteindre", "systemctl poweroff"),
    ]
}

/// Valide la liste saisie par l'utilisateur et attribue une clé aux nouvelles commandes.
pub fn normalize_commands(list: Vec<RemoteCommand>) -> Result<Vec<RemoteCommand>> {
    let mut out: Vec<RemoteCommand> = Vec::with_capacity(list.len());
    for mut c in list {
        c.name = c.name.trim().to_owned();
        c.command = c.command.trim().to_owned();
        anyhow::ensure!(!c.name.is_empty() && c.name.chars().count() <= 80, "nom de commande invalide");
        anyhow::ensure!(!c.command.is_empty() && c.command.len() <= 2000, "commande vide ou trop longue pour « {} »", c.name);
        let key_ok = !c.key.is_empty()
            && c.key.len() <= 64
            && c.key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
        if !key_ok || out.iter().any(|o| o.key == c.key) {
            c.key = (1..)
                .map(|n| format!("cmd-{n}"))
                .find(|k| !out.iter().any(|o| &o.key == k))
                .unwrap_or_default();
        }
        out.push(c);
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// Plugin → activé. Un plugin absent est considéré comme activé.
    #[serde(default)]
    pub plugins: BTreeMap<String, bool>,
    /// Comportement du son du PC pendant un appel.
    #[serde(default)]
    pub call_media: CallMedia,
    /// Commandes à distance (préremplies à la première utilisation).
    #[serde(default = "default_commands")]
    pub commands: Vec<RemoteCommand>,
    /// Publie les lecteurs du téléphone sur le bus MPRIS (menu multimédia de GNOME).
    #[serde(default = "default_true")]
    pub mpris_proxy: bool,
    /// « Écouter sur le PC » : garde aussi le son sur le téléphone (`--audio-dup`, Android 13+).
    #[serde(default)]
    pub listen_keep_on_phone: bool,
    /// « Écouter sur le PC » : met en pause les lecteurs du PC pendant l'écoute.
    #[serde(default = "default_true")]
    pub listen_pause_players: bool,
}

fn default_true() -> bool {
    true
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).context("config.toml invalide"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("lecture de config.toml"),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, toml::to_string_pretty(self)?).context("écriture de config.toml")
    }

    pub fn is_enabled(&self, plugin: &str) -> bool {
        self.plugins.get(plugin).copied().unwrap_or(true)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: dirs::download_dir().unwrap_or_else(|| PathBuf::from(".")),
            plugins: BTreeMap::new(),
            call_media: CallMedia::None,
            commands: default_commands(),
            mpris_proxy: true,
            listen_keep_on_phone: false,
            listen_pause_players: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_absent_est_actif() {
        let mut s = Settings::default();
        assert!(s.is_enabled("battery"));
        s.plugins.insert("battery".into(), false);
        assert!(!s.is_enabled("battery"));
    }

    #[test]
    fn validation_des_commandes() {
        let cmd = |key: &str, name: &str, command: &str| RemoteCommand {
            key: key.into(),
            name: name.into(),
            command: command.into(),
        };
        let ok = normalize_commands(vec![
            cmd("lock", " Verrouiller ", " loginctl lock-session "),
            cmd("", "Nouvelle", "echo salut"),
            cmd("lock", "Doublon de clé", "true"),
            cmd("../x", "Clé douteuse", "true"),
        ])
        .unwrap();
        assert_eq!(ok[0].name, "Verrouiller");
        assert_eq!(ok[0].command, "loginctl lock-session");
        let keys: Vec<_> = ok.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["lock", "cmd-1", "cmd-2", "cmd-3"]);
        assert!(normalize_commands(vec![cmd("a", "", "true")]).is_err());
        assert!(normalize_commands(vec![cmd("a", "Nom", "  ")]).is_err());
    }

    #[test]
    fn commandes_par_defaut_puis_liste_vide_respectee() {
        let s: Settings = toml::from_str("download_dir = \"/tmp\"\n").unwrap();
        let keys: Vec<_> = s.commands.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["lock", "suspend", "poweroff"]);
        let s: Settings = toml::from_str("download_dir = \"/tmp\"\ncommands = []\n").unwrap();
        assert!(s.commands.is_empty(), "une liste volontairement vide reste vide");
    }

    #[test]
    fn options_multimedia_par_defaut() {
        let s: Settings = toml::from_str("download_dir = \"/tmp\"\n").unwrap();
        assert!(s.mpris_proxy);
        assert!(!s.listen_keep_on_phone);
        assert!(s.listen_pause_players);
    }

    #[test]
    fn ancien_fichier_sans_option_d_appel() {
        let s: Settings = toml::from_str("download_dir = \"/tmp\"\n").unwrap();
        assert_eq!(s.call_media, CallMedia::None);
        let s: Settings = toml::from_str("download_dir = \"/tmp\"\ncall_media = \"pause\"\n").unwrap();
        assert_eq!(s.call_media, CallMedia::Pause);
        assert_eq!(CallMedia::parse(CallMedia::Lower.as_str()), Some(CallMedia::Lower));
        assert_eq!(CallMedia::parse("nimporte"), None);
    }
}
