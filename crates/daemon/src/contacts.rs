//! Cache local des contacts du téléphone : une fiche vCard par contact, synchronisée
//! de façon différentielle (horodatage `X-KDECONNECT-TIMESTAMP`).

use anyhow::{Context, Result};
use base64::Engine;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const VCARD_EXT: &str = "vcf";
const AVATAR_EXT: &str = "photo";
const TIMESTAMP_FIELD: &str = "X-KDECONNECT-TIMESTAMP";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contact {
    pub uid: String,
    pub name: String,
    pub numbers: Vec<String>,
    pub avatar: Option<String>,
    #[serde(skip)]
    pub timestamp: i64,
}

/// Résultat brut de l'analyse d'une vCard.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ParsedCard {
    pub name: String,
    pub numbers: Vec<String>,
    pub photo: Option<Vec<u8>>,
    pub timestamp: i64,
}

/// L'identifiant sert de nom de fichier : on refuse tout ce qui pourrait en sortir.
pub(crate) fn valid_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 64
        && uid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut continued_qp = false;
    for raw in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let starts_blank = raw.starts_with(' ') || raw.starts_with('\t');
        match lines.last_mut() {
            // vCard 3.0 : une ligne repliée commence par un espace.
            Some(last) if starts_blank => last.push_str(&raw[1..]),
            // vCard 2.1 « quoted-printable » : « = » en fin de ligne = suite.
            Some(last) if continued_qp => last.push_str(raw),
            _ => lines.push(raw.to_owned()),
        }
        continued_qp = lines
            .last()
            .is_some_and(|l| l.to_ascii_uppercase().contains("QUOTED-PRINTABLE") && l.ends_with('='));
        if continued_qp {
            if let Some(last) = lines.last_mut() {
                last.pop();
            }
        }
    }
    lines
}

fn decode_qp(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unescape(v: &str) -> String {
    v.replace("\\n", " ")
        .replace("\\N", " ")
        .replace("\\,", ",")
        .replace(r"\;", ";")
        .replace("\\\\", "\\")
}

pub(crate) fn parse_vcard(text: &str) -> ParsedCard {
    let mut card = ParsedCard::default();
    let (mut formatted, mut structured) = (String::new(), String::new());
    for line in unfold(text) {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = head.split(';');
        let prop = parts.next().unwrap_or("").to_ascii_uppercase();
        let prop = prop.rsplit('.').next().unwrap_or("").to_owned(); // « item1.TEL »
        let params: Vec<String> = parts.map(|p| p.to_ascii_uppercase()).collect();
        let is_qp = params.iter().any(|p| p.contains("QUOTED-PRINTABLE"));
        let is_b64 = params
            .iter()
            .any(|p| p.contains("BASE64") || p == "ENCODING=B" || p == "B");
        let text_value = || {
            let v = if is_qp { decode_qp(value) } else { value.to_owned() };
            unescape(v.trim())
        };
        match prop.as_str() {
            "FN" => formatted = text_value(),
            "N" => {
                // Famille;Prénom;Autres;Préfixe;Suffixe
                let v = text_value();
                let f: Vec<&str> = v.split(';').collect();
                structured = [f.get(1), f.first()]
                    .into_iter()
                    .flatten()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            "TEL" => {
                let n: String = value.trim().chars().filter(|c| !c.is_whitespace()).collect();
                if !n.is_empty() && !card.numbers.contains(&n) {
                    card.numbers.push(n);
                }
            }
            "PHOTO" if is_b64 => {
                let clean: String = value.chars().filter(|c| !c.is_whitespace()).collect();
                card.photo = base64::engine::general_purpose::STANDARD.decode(clean).ok();
            }
            _ if prop == TIMESTAMP_FIELD => card.timestamp = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    card.name = if !formatted.is_empty() { formatted } else { structured };
    card
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Deux numéros désignent la même ligne s'ils ont les mêmes 9 derniers chiffres
/// (`+33 6 12 34 56 78` = `06 12 34 56 78`).
pub(crate) fn same_number(a: &str, b: &str) -> bool {
    let (da, db) = (digits(a), digits(b));
    if da.len() < 5 || db.len() < 5 {
        return !da.is_empty() && da == db;
    }
    let n = da.len().min(db.len()).min(9);
    da[da.len() - n..] == db[db.len() - n..]
}

/// Compare l'état local et celui du téléphone : (à télécharger, à supprimer).
pub(crate) fn plan_sync(
    local: &HashMap<String, i64>,
    remote: &[(String, i64)],
) -> (Vec<String>, Vec<String>) {
    let wanted: HashSet<&str> = remote.iter().map(|(u, _)| u.as_str()).collect();
    let fetch = remote
        .iter()
        .filter(|(uid, ts)| local.get(uid) != Some(ts))
        .map(|(uid, _)| uid.clone())
        .collect();
    let mut delete: Vec<String> = local
        .keys()
        .filter(|u| !wanted.contains(u.as_str()))
        .cloned()
        .collect();
    delete.sort();
    (fetch, delete)
}

pub struct ContactStore {
    root: PathBuf,
    by_device: Mutex<HashMap<String, Vec<Contact>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ContactStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            by_device: Mutex::new(HashMap::new()),
        }
    }

    fn dir(&self, device: &str) -> PathBuf {
        self.root.join(device)
    }

    /// Horodatages des fiches déjà en cache.
    pub(crate) fn local_timestamps(&self, device: &str) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        for contact in self.read_dir(device) {
            out.insert(contact.uid, contact.timestamp);
        }
        out
    }

    fn read_dir(&self, device: &str) -> Vec<Contact> {
        let dir = self.dir(device);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().and_then(|e| e.to_str()) != Some(VCARD_EXT) {
                continue;
            }
            let Some(uid) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let card = parse_vcard(&text);
            let avatar = dir.join(format!("{uid}.{AVATAR_EXT}"));
            let name = if card.name.is_empty() {
                card.numbers.first().cloned().unwrap_or_default()
            } else {
                card.name
            };
            out.push(Contact {
                avatar: avatar.exists().then(|| avatar.display().to_string()),
                uid,
                name,
                numbers: card.numbers,
                timestamp: card.timestamp,
            });
        }
        out.sort_by_key(|c| c.name.to_lowercase());
        out
    }

    /// Relit le cache disque (après une synchro ou au démarrage du lien).
    pub fn reload(&self, device: &str) {
        let list = self.read_dir(device);
        lock(&self.by_device).insert(device.to_owned(), list);
    }

    pub fn list(&self, device: &str) -> Vec<Contact> {
        let mut map = lock(&self.by_device);
        if !map.contains_key(device) {
            map.insert(device.to_owned(), self.read_dir(device));
        }
        map.get(device).cloned().unwrap_or_default()
    }

    pub fn lookup(&self, device: &str, number: &str) -> Option<Contact> {
        self.list(device)
            .into_iter()
            .find(|c| c.numbers.iter().any(|n| same_number(n, number)))
    }

    /// Enregistre une fiche reçue (et sa photo) ; refuse un identifiant douteux.
    pub(crate) fn save(&self, device: &str, uid: &str, vcard: &str) -> Result<()> {
        anyhow::ensure!(valid_uid(uid), "identifiant de contact refusé : {uid:?}");
        let dir = self.dir(device);
        std::fs::create_dir_all(&dir).context("création du dossier de contacts")?;
        std::fs::write(dir.join(format!("{uid}.{VCARD_EXT}")), vcard)?;
        let photo_path = dir.join(format!("{uid}.{AVATAR_EXT}"));
        match parse_vcard(vcard).photo {
            Some(bytes) if !bytes.is_empty() => std::fs::write(photo_path, bytes)?,
            _ => remove_if_exists(&photo_path),
        }
        Ok(())
    }

    pub(crate) fn delete(&self, device: &str, uid: &str) {
        if !valid_uid(uid) {
            return;
        }
        let dir = self.dir(device);
        remove_if_exists(&dir.join(format!("{uid}.{VCARD_EXT}")));
        remove_if_exists(&dir.join(format!("{uid}.{AVATAR_EXT}")));
    }
}

fn remove_if_exists(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    // vCard 3.0 telle qu'envoyée par KDE Connect Android (photo repliée sur 2 lignes).
    const CARD_30: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alice Martin\r\nN:Martin;Alice;;;\r\nTEL;TYPE=CELL:+33 6 12 34 56 78\r\nTEL;TYPE=HOME:01 23 45 67 89\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ\r\n SkZJRg==\r\nX-KDECONNECT-ID-DEV-1234:4\r\nX-KDECONNECT-TIMESTAMP:1729504200123\r\nEND:VCARD\r\n";
    // vCard 2.1 avec accents en quoted-printable.
    const CARD_21: &str = "BEGIN:VCARD\nVERSION:2.1\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=A9tivier;Jos=C3=A9;;;\nTEL;CELL:0612345678\nX-KDECONNECT-TIMESTAMP:99\nEND:VCARD\n";

    #[test]
    fn lit_une_vcard_3_0() {
        let c = parse_vcard(CARD_30);
        assert_eq!(c.name, "Alice Martin");
        assert_eq!(c.numbers, vec!["+33612345678", "0123456789"]);
        assert_eq!(c.photo.as_deref(), Some(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46][..]));
        assert_eq!(c.timestamp, 1729504200123);
    }

    #[test]
    fn lit_une_vcard_2_1_quoted_printable() {
        let c = parse_vcard(CARD_21);
        assert_eq!(c.name, "José Métivier");
        assert_eq!(c.numbers, vec!["0612345678"]);
        assert_eq!(c.timestamp, 99);
    }

    #[test]
    fn numeros_equivalents() {
        assert!(same_number("+33 6 12 34 56 78", "06 12 34 56 78"));
        assert!(same_number("0612345678", "+33612345678"));
        assert!(!same_number("0612345678", "0612345679"));
        assert!(same_number("3631", "3631"));
        assert!(!same_number("3631", "3632"));
        assert!(!same_number("", ""));
    }

    #[test]
    fn synchro_differentielle() {
        let local = HashMap::from([("1".to_string(), 10), ("2".to_string(), 20), ("9".to_string(), 5)]);
        let remote = vec![("1".to_string(), 10), ("2".to_string(), 21), ("3".to_string(), 30)];
        let (fetch, delete) = plan_sync(&local, &remote);
        assert_eq!(fetch, vec!["2", "3"]);
        assert_eq!(delete, vec!["9"]);
    }

    #[test]
    fn identifiants_sans_danger() {
        assert!(valid_uid("1234"));
        assert!(valid_uid("abc-DEF_9"));
        for bad in ["", "../x", "a/b", "a.b", "é"] {
            assert!(!valid_uid(bad), "{bad:?}");
        }
    }

    #[test]
    fn cache_sur_disque() {
        let dir = std::env::temp_dir().join(format!("fl-contacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ContactStore::new(dir.clone());
        store.save("tel1", "4", CARD_30).unwrap();
        assert!(store.save("tel1", "../evil", CARD_30).is_err());
        assert_eq!(store.local_timestamps("tel1").get("4"), Some(&1729504200123));
        let alice = store.lookup("tel1", "06 12 34 56 78").unwrap();
        assert_eq!(alice.name, "Alice Martin");
        assert!(alice.avatar.is_some());
        store.delete("tel1", "4");
        store.reload("tel1");
        assert!(store.list("tel1").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
