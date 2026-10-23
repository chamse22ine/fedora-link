//! Journal des fichiers reçus du téléphone, pour les lister dans l'application
//! sans parcourir tout le dossier de téléchargement.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// Au-delà, les plus anciennes entrées sont oubliées (les fichiers, eux, restent sur le disque).
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceivedFile {
    pub device: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    /// Réception, en millisecondes depuis 1970.
    pub time: i64,
}

pub struct ReceivedLog {
    path: PathBuf,
    entries: Mutex<Vec<ReceivedFile>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ReceivedLog {
    /// Un journal illisible est ignoré : il ne doit jamais empêcher de recevoir un fichier.
    pub fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn add(&self, file: ReceivedFile) -> Result<()> {
        let mut entries = lock(&self.entries);
        entries.retain(|e| e.path != file.path);
        entries.push(file);
        let excess = entries.len().saturating_sub(MAX_ENTRIES);
        entries.drain(..excess);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).context("création du dossier de données")?;
        }
        std::fs::write(&self.path, serde_json::to_string(&*entries)?)
            .context("écriture du journal des fichiers reçus")
    }

    /// Fichiers encore présents sur le disque, du plus récent au plus ancien.
    pub fn list(&self) -> Vec<ReceivedFile> {
        let mut out: Vec<ReceivedFile> = lock(&self.entries)
            .iter()
            .filter(|e| std::path::Path::new(&e.path).is_file())
            .cloned()
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.time));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_persistant_sans_fichiers_disparus() {
        let dir = std::env::temp_dir().join(format!("fl-received-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.jpg"), dir.join("b.pdf"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"yy").unwrap();
        let entry = |name: &str, path: &PathBuf, time| ReceivedFile {
            device: "tel".into(),
            name: name.into(),
            path: path.display().to_string(),
            size: 1,
            time,
        };

        let log = ReceivedLog::load(dir.join("received.json"));
        log.add(entry("a.jpg", &a, 100)).unwrap();
        log.add(entry("b.pdf", &b, 200)).unwrap();
        log.add(entry("a.jpg", &a, 300)).unwrap(); // même chemin : remplace, ne double pas

        let reloaded = ReceivedLog::load(dir.join("received.json"));
        let names: Vec<_> = reloaded.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["a.jpg", "b.pdf"]);

        std::fs::remove_file(&b).unwrap();
        assert_eq!(reloaded.list().len(), 1, "fichier supprimé : plus listé");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn journal_corrompu_ignore() {
        let dir = std::env::temp_dir().join(format!("fl-received-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("received.json"), "{pas du json").unwrap();
        assert!(ReceivedLog::load(dir.join("received.json")).list().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
