//! Identité de l'appareil : identifiant, certificat auto-signé et clé privée.
//!
//! Étape 1 : stockage en fichiers (clé en 0600) dans le dossier de config.
//! Le passage au trousseau GNOME (libsecret) est prévu à l'étape du daemon final.

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use x509_parser::prelude::*;

#[derive(Clone)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl DeviceIdentity {
    /// Charge l'identité depuis `dir`, ou la génère au premier lancement.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
        let id_path = dir.join("device_id");
        let cert_path = dir.join("cert.der");
        let key_path = dir.join("key.der");

        if id_path.exists() && cert_path.exists() && key_path.exists() {
            return Ok(Self {
                device_id: fs::read_to_string(&id_path)?.trim().to_owned(),
                cert_der: fs::read(&cert_path)?,
                key_der: fs::read(&key_path)?,
            });
        }

        let identity = Self::generate()?;
        fs::write(&id_path, &identity.device_id)?;
        fs::write(&cert_path, &identity.cert_der)?;
        write_private(&key_path, &identity.key_der)?;
        Ok(identity)
    }

    /// Génère un identifiant (UUID avec `_`, comme KDE Connect) et un certificat
    /// auto-signé dont le CN est l'identifiant de l'appareil.
    pub fn generate() -> Result<Self> {
        let device_id = uuid::Uuid::new_v4().to_string().replace('-', "_");
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, device_id.clone());
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationName, "KDE");
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationalUnitName, "Kde connect");
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2120, 1, 1);

        let key = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        Ok(Self {
            device_id,
            cert_der: cert.der().to_vec(),
            key_der: key.serialize_der(),
        })
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    fs::write(path, data)?;
    Ok(())
}

/// Nom commun (CN) du certificat, censé être l'identifiant de l'appareil.
pub fn common_name(cert_der: &[u8]) -> Result<String> {
    let (_, cert) =
        X509Certificate::from_der(cert_der).map_err(|e| anyhow!("certificat invalide : {e}"))?;
    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .ok_or_else(|| anyhow!("certificat sans CN"))?
        .as_str()
        .map_err(|e| anyhow!("CN illisible : {e}"))?
        .to_owned();
    Ok(cn)
}

fn public_key(cert_der: &[u8]) -> Result<Vec<u8>> {
    let (_, cert) =
        X509Certificate::from_der(cert_der).map_err(|e| anyhow!("certificat invalide : {e}"))?;
    Ok(cert.public_key().raw.to_vec())
}

/// Code de vérification affiché des deux côtés lors de l'appairage :
/// SHA-256 des deux clés publiques triées, 8 premiers caractères hexadécimaux.
/// TODO : à confirmer contre la doc du protocole v7/v8 avec un vrai téléphone.
pub fn verification_code(cert_a: &[u8], cert_b: &[u8]) -> Result<String> {
    let mut keys = [public_key(cert_a)?, public_key(cert_b)?];
    keys.sort();
    let mut hasher = Sha256::new();
    hasher.update(&keys[0]);
    hasher.update(&keys[1]);
    Ok(hex::encode(hasher.finalize())[..8].to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_cn_est_l_identifiant() {
        let id = DeviceIdentity::generate().unwrap();
        assert_eq!(common_name(&id.cert_der).unwrap(), id.device_id);
        assert!(!id.device_id.contains('-'));
    }

    #[test]
    fn le_code_est_symetrique() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let ab = verification_code(&a.cert_der, &b.cert_der).unwrap();
        let ba = verification_code(&b.cert_der, &a.cert_der).unwrap();
        assert_eq!(ab, ba);
        assert_eq!(ab.len(), 8);
    }

    #[test]
    fn l_identite_est_persistee() {
        let dir = std::env::temp_dir().join(format!("fl-test-{}", uuid::Uuid::new_v4()));
        let a = DeviceIdentity::load_or_create(&dir).unwrap();
        let b = DeviceIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.cert_der, b.cert_der);
        let _ = fs::remove_dir_all(dir);
    }
}
