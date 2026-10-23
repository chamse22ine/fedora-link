//! Appareils appairés : identifiant, nom et certificat mémorisé.
//! Désappairer = supprimer l'entrée (donc le certificat).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    /// Certificat DER de l'appareil, en hexadécimal.
    cert: String,
    /// Dernière adresse IP connue, pour se reconnecter sans attendre son annonce.
    #[serde(default)]
    last_ip: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    devices: BTreeMap<String, Entry>,
}

pub struct TrustStore {
    path: PathBuf,
    data: Mutex<File>,
}

impl TrustStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let data = match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).context("devices.toml invalide")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(e).context("lecture de devices.toml"),
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    /// Vrai si l'appareil est appairé *et* présente le certificat mémorisé.
    pub fn is_trusted(&self, device_id: &str, cert_der: &[u8]) -> bool {
        let data = self.lock();
        data.devices
            .get(device_id)
            .is_some_and(|e| e.cert == hex::encode(cert_der))
    }

    /// Appareils appairés : (identifiant, nom).
    pub fn list(&self) -> Vec<(String, String)> {
        let data = self.lock();
        data.devices
            .iter()
            .map(|(id, e)| (id.clone(), e.name.clone()))
            .collect()
    }

    pub fn set_last_ip(&self, device_id: &str, ip: IpAddr) -> Result<()> {
        let mut data = self.lock();
        if let Some(e) = data.devices.get_mut(device_id) {
            let ip = ip.to_string();
            if e.last_ip.as_deref() != Some(ip.as_str()) {
                e.last_ip = Some(ip);
                return self.save(&data);
            }
        }
        Ok(())
    }

    /// Appareils appairés avec une adresse connue : (identifiant, nom, IP).
    pub fn reachable(&self) -> Vec<(String, String, IpAddr)> {
        let data = self.lock();
        data.devices
            .iter()
            .filter_map(|(id, e)| {
                Some((
                    id.clone(),
                    e.name.clone(),
                    e.last_ip.as_deref()?.parse().ok()?,
                ))
            })
            .collect()
    }

    pub fn add(&self, device_id: &str, name: &str, cert_der: &[u8]) -> Result<()> {
        let mut data = self.lock();
        let last_ip = data.devices.get(device_id).and_then(|e| e.last_ip.clone());
        data.devices.insert(
            device_id.to_owned(),
            Entry {
                name: name.to_owned(),
                cert: hex::encode(cert_der),
                last_ip,
            },
        );
        self.save(&data)
    }

    pub fn remove(&self, device_id: &str) -> Result<()> {
        let mut data = self.lock();
        if data.devices.remove(device_id).is_some() {
            self.save(&data)?;
        }
        Ok(())
    }

    // Un mutex empoisonné ne rend pas les données invalides ici : on continue.
    fn lock(&self) -> std::sync::MutexGuard<'_, File> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, data: &File) -> Result<()> {
        let s = toml::to_string_pretty(data)?;
        std::fs::write(&self.path, s).context("écriture de devices.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appairage_puis_desappairage() {
        let path = std::env::temp_dir().join(format!("fl-trust-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = TrustStore::load(path.clone()).unwrap();
        assert!(!store.is_trusted("a", b"cert"));
        store.add("a", "Pixel", b"cert").unwrap();
        assert!(store.is_trusted("a", b"cert"));
        assert!(!store.is_trusted("a", b"autre"));
        // Persistance
        assert!(TrustStore::load(path.clone())
            .unwrap()
            .is_trusted("a", b"cert"));
        store.remove("a").unwrap();
        assert!(!store.is_trusted("a", b"cert"));
        let _ = std::fs::remove_file(path);
    }
}
