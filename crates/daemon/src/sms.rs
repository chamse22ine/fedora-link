//! Messages du téléphone : conversations en mémoire (le téléphone fait foi, on les redemande
//! à la demande), pièces jointes MMS en cache disque.

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

/// Valeurs de `Telephony.TextBasedSmsColumns.MESSAGE_TYPE_*` d'Android.
pub const TYPE_INBOX: i32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attachment {
    pub part_id: i64,
    pub mime_type: String,
    pub unique_identifier: String,
    /// Vignette décodée, écrite dans le cache (chemin).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// Fichier complet, s'il a déjà été téléchargé.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip)]
    pub thumbnail_b64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub thread_id: i64,
    pub date: i64,
    #[serde(rename = "type")]
    pub kind: i32,
    pub body: String,
    pub addresses: Vec<String>,
    pub read: bool,
    pub incoming: bool,
    pub attachments: Vec<Attachment>,
}

impl Message {
    /// Un message reçu a pour auteur la première adresse.
    pub fn sender(&self) -> &str {
        self.addresses.first().map_or("", String::as_str)
    }

    fn same_as(&self, other: &Message) -> bool {
        match (self.id, other.id) {
            (Some(a), Some(b)) => a == b,
            _ => (self.date, self.kind, &self.body) == (other.date, other.kind, &other.body),
        }
    }
}

fn parse_attachment(v: &Value) -> Option<Attachment> {
    Some(Attachment {
        part_id: v.get("part_id")?.as_i64()?,
        mime_type: v.get("mime_type").and_then(Value::as_str)?.to_owned(),
        unique_identifier: v.get("unique_identifier").and_then(Value::as_str)?.to_owned(),
        thumbnail: None,
        path: None,
        thumbnail_b64: v
            .get("encoded_thumbnail")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

/// Messages d'un paquet `kdeconnect.sms.messages` ; ignore ceux qui sont illisibles.
pub fn parse_messages(body: &Value) -> Vec<Message> {
    let Some(list) = body.get("messages").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter().filter_map(parse_message).collect()
}

fn parse_message(m: &Value) -> Option<Message> {
    // La date arrive tantôt en nombre, tantôt en chaîne selon la version de l'appli.
    let num = |k: &str| {
        m.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
    };
    let kind = num("type")? as i32;
    let addresses: Vec<String> = m
        .get("addresses")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("address").and_then(Value::as_str).map(str::to_owned))
        .collect();
    Some(Message {
        id: num("_id"),
        thread_id: num("thread_id")?,
        date: num("date")?,
        kind,
        body: m.get("body").and_then(Value::as_str).unwrap_or("").to_owned(),
        addresses,
        read: m.get("read").and_then(Value::as_bool).unwrap_or(true),
        incoming: kind == TYPE_INBOX,
        attachments: m
            .get("attachments")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(parse_attachment).collect())
            .unwrap_or_default(),
    })
}

/// Numéro acceptable comme destinataire d'un SMS.
pub fn valid_recipient(s: &str) -> bool {
    let digits = s.chars().filter(char::is_ascii_digit).count();
    s.len() <= 32
        && digits >= 3
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '.' | '(' | ')'))
}

/// Un message est « nouveau » (à notifier) s'il est reçu, non lu, et récent.
pub fn is_fresh_incoming(m: &Message, now_ms: i64) -> bool {
    m.incoming && !m.read && (now_ms - m.date).abs() < 2 * 60 * 1000
}

#[derive(Default)]
pub struct SmsStore {
    devices: Mutex<HashMap<String, BTreeMap<i64, Vec<Message>>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SmsStore {
    /// Ajoute ou met à jour des messages ; rend ceux qui n'étaient pas connus.
    pub fn insert(&self, device: &str, messages: Vec<Message>) -> Vec<Message> {
        let mut devices = lock(&self.devices);
        let threads = devices.entry(device.to_owned()).or_default();
        let mut added = Vec::new();
        for m in messages {
            let list = threads.entry(m.thread_id).or_default();
            match list.iter_mut().find(|e| e.same_as(&m)) {
                Some(existing) => {
                    // Mise à jour (lu, type après envoi…) sans perdre les vignettes décodées.
                    let attachments = std::mem::take(&mut existing.attachments);
                    *existing = m;
                    if existing.attachments.is_empty() {
                        existing.attachments = attachments;
                    }
                }
                None => {
                    list.push(m.clone());
                    added.push(m);
                }
            }
            list.sort_by_key(|e| e.date);
        }
        added
    }

    pub fn clear(&self, device: &str) {
        lock(&self.devices).remove(device);
    }

    /// Messages d'une conversation, du plus ancien au plus récent.
    pub fn thread(&self, device: &str, thread: i64) -> Vec<Message> {
        lock(&self.devices)
            .get(device)
            .and_then(|t| t.get(&thread))
            .cloned()
            .unwrap_or_default()
    }

    /// Dernier message de chaque conversation, le plus récent en premier.
    pub fn conversations(&self, device: &str) -> Vec<Message> {
        let devices = lock(&self.devices);
        let mut last: Vec<Message> = devices
            .get(device)
            .map(|t| t.values().filter_map(|l| l.last().cloned()).collect())
            .unwrap_or_default();
        last.sort_by_key(|m| std::cmp::Reverse(m.date));
        last
    }

    pub fn thread_has_unread(&self, device: &str, thread: i64) -> bool {
        self.thread(device, thread)
            .iter()
            .any(|m| m.incoming && !m.read)
    }

    /// Chemin d'une pièce jointe dans le cache, une fois téléchargée.
    pub fn set_attachment_path(&self, device: &str, unique_id: &str, path: &str) {
        let mut devices = lock(&self.devices);
        for list in devices.get_mut(device).into_iter().flat_map(|t| t.values_mut()) {
            for a in list.iter_mut().flat_map(|m| m.attachments.iter_mut()) {
                if a.unique_identifier == unique_id {
                    a.path = Some(path.to_owned());
                }
            }
        }
    }

    pub fn set_thumbnail(&self, device: &str, unique_id: &str, path: &str) {
        let mut devices = lock(&self.devices);
        for list in devices.get_mut(device).into_iter().flat_map(|t| t.values_mut()) {
            for a in list.iter_mut().flat_map(|m| m.attachments.iter_mut()) {
                if a.unique_identifier == unique_id {
                    a.thumbnail = Some(path.to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_link_core::packet::NetworkPacket;

    // Réponse à `request_conversations`, telle qu'émise par KDE Connect Android.
    const BATCH: &str = r#"{"id":1729504300000,"type":"kdeconnect.sms.messages","body":{"version":2,"messages":[
      {"event":1,"body":"On se retrouve à 18h ?","addresses":[{"address":"+33612345678"}],"date":1729504290000,"type":1,"thread_id":132,"read":false,"_id":901,"sub_id":3},
      {"event":1,"body":"Ok merci","addresses":[{"address":"0123456789"}],"date":"1729500000000","type":2,"thread_id":7,"read":true,"_id":850},
      {"event":2,"body":"","addresses":[{"address":"+33612345678"},{"address":"+33698765432"}],"date":1729504295000,"type":1,"thread_id":140,"read":false,"_id":902,
       "attachments":[{"part_id":55,"mime_type":"image/jpeg","encoded_thumbnail":"/9j/4AAQ","unique_identifier":"PART_1729504295_IMG.jpg"}]},
      {"event":1,"body":"incomplet"}
    ]}}"#;

    fn batch() -> Vec<Message> {
        parse_messages(&NetworkPacket::from_line(BATCH).unwrap().body)
    }

    #[test]
    fn lit_les_messages_reels() {
        let m = batch();
        assert_eq!(m.len(), 3, "le message incomplet est ignoré");
        assert_eq!((m[0].thread_id, m[0].id, m[0].incoming, m[0].read), (132, Some(901), true, false));
        assert_eq!(m[0].sender(), "+33612345678");
        assert_eq!(m[1].date, 1729500000000, "date en chaîne");
        assert!(!m[1].incoming);
        assert_eq!(m[2].addresses.len(), 2);
        assert_eq!(m[2].attachments[0].part_id, 55);
        assert_eq!(m[2].attachments[0].thumbnail_b64.as_deref(), Some("/9j/4AAQ"));
    }

    #[test]
    fn conversations_triees_et_dedoublonnees() {
        let store = SmsStore::default();
        assert_eq!(store.insert("t", batch()).len(), 3);
        assert!(store.insert("t", batch()).is_empty(), "déjà connus");
        let conv = store.conversations("t");
        assert_eq!(conv.iter().map(|m| m.thread_id).collect::<Vec<_>>(), vec![140, 132, 7]);
        assert!(store.thread_has_unread("t", 132));
        assert!(!store.thread_has_unread("t", 7));
    }

    #[test]
    fn mise_a_jour_garde_le_fil_ordonne() {
        let store = SmsStore::default();
        let mut older = batch().remove(0);
        older.id = Some(800);
        older.date -= 60_000;
        store.insert("t", vec![batch().remove(0), older]);
        let thread = store.thread("t", 132);
        assert_eq!(thread.iter().map(|m| m.id).collect::<Vec<_>>(), vec![Some(800), Some(901)]);
    }

    #[test]
    fn detecte_les_nouveaux_messages() {
        let m = batch().remove(0);
        assert!(is_fresh_incoming(&m, m.date + 30_000));
        assert!(!is_fresh_incoming(&m, m.date + 10 * 60_000), "trop ancien");
        let sent = batch().remove(1);
        assert!(!is_fresh_incoming(&sent, sent.date));
    }

    #[test]
    fn destinataires_valides() {
        for ok in ["+33612345678", "06 12 34 56 78", "(01) 23-45.67"] {
            assert!(valid_recipient(ok), "{ok}");
        }
        for bad in ["", "ab", "12", "06;12", "0612345678\n", "+33612345678901234567890123456789012"] {
            assert!(!valid_recipient(bad), "{bad:?}");
        }
    }
}
