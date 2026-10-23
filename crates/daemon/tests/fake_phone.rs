//! Test d'intégration : un faux téléphone se connecte au daemon, s'appaire et
//! échange un ping, en suivant la séquence réelle de KDE Connect.

use fedora_link_core::cert::DeviceIdentity;
use fedora_link_core::packet::NetworkPacket;
use fedora_link_core::tls;
use fedora_link_daemon::{start, Config, RunningDaemon};
use rustls::pki_types::ServerName;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fl-it-{tag}-{}", std::process::id()))
}

async fn daemon(tag: &str, auto_accept: bool) -> RunningDaemon {
    let dir = temp_dir(tag);
    let _ = std::fs::remove_dir_all(&dir);
    start(Config {
        config_dir: dir.clone(),
        device_name: "PC de test".into(),
        udp_port: 0,
        tcp_port_range: (20000, 20999),
        broadcast: false,
        auto_accept,
        open_links: false,
        download_dir: Some(dir.join("downloads")),
        data_dir: dir.join("data"),
        cache_dir: dir.join("cache"),
    })
    .await
    .expect("démarrage du daemon")
}

fn phone_identity(tcp_port: Option<u16>, id: &DeviceIdentity) -> NetworkPacket {
    use fedora_link_core::packet::IdentityBody;
    NetworkPacket::identity(&IdentityBody {
        device_id: id.device_id.clone(),
        device_name: "Faux téléphone".into(),
        device_type: "phone".into(),
        protocol_version: 7,
        incoming_capabilities: vec!["kdeconnect.ping".into()],
        outgoing_capabilities: vec!["kdeconnect.ping".into()],
        tcp_port,
    })
    .expect("identité")
}

async fn read_packet<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> NetworkPacket {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line))
        .await
        .expect("délai dépassé")
        .expect("lecture");
    NetworkPacket::from_line(&line).expect("paquet")
}

/// Le téléphone ouvre le TCP (donc serveur TLS), comme après un broadcast du PC.
async fn connect_phone(
    port: u16,
    id: &DeviceIdentity,
) -> (
    BufReader<tokio::io::ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>>,
    tokio::io::WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>,
) {
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.expect("tcp");
    tcp.write_all(&phone_identity(None, id).to_line().unwrap())
        .await
        .unwrap();
    let acceptor = TlsAcceptor::from(tls::server_config(id).unwrap());
    let tls = acceptor.accept(tcp).await.expect("tls");
    let (r, w) = tokio::io::split(tls);
    (BufReader::new(r), w)
}

#[tokio::test]
async fn appairage_puis_ping() {
    let d = daemon("pair", true).await;
    let phone = DeviceIdentity::generate().unwrap();
    let (mut r, mut w) = connect_phone(d.tcp_port, &phone).await;

    w.write_all(&NetworkPacket::pair(true).to_line().unwrap())
        .await
        .unwrap();
    assert_eq!(read_packet(&mut r).await.pair_flag(), Some(true));
    // Une fois appairé, le PC demande batterie et notifications existantes.
    assert_eq!(read_packet(&mut r).await.kind, "kdeconnect.battery.request");
    assert_eq!(
        read_packet(&mut r).await.kind,
        "kdeconnect.notification.request"
    );

    // Reconnexion : l'appareil est connu, les demandes arrivent sans nouvel appairage.
    let (mut r2, _w2) = connect_phone(d.tcp_port, &phone).await;
    assert_eq!(
        read_packet(&mut r2).await.kind,
        "kdeconnect.battery.request"
    );
    d.stop();
}

#[tokio::test]
async fn un_appareil_non_appaire_ne_recoit_rien() {
    let d = daemon("unpaired", true).await;
    let phone = DeviceIdentity::generate().unwrap();
    let (mut r, mut w) = connect_phone(d.tcp_port, &phone).await;

    w.write_all(&NetworkPacket::ping().to_line().unwrap())
        .await
        .unwrap();
    // Seule réponse : « pair:false », jamais de ping ni de données.
    assert_eq!(read_packet(&mut r).await.pair_flag(), Some(false));
    d.stop();
}

/// Le PC voit un broadcast UDP du téléphone, se connecte à lui et s'annonce.
#[tokio::test]
async fn decouverte_udp_connexion_sortante() {
    let d = daemon("udp", true).await;
    let phone = DeviceIdentity::generate().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let phone_port = listener.local_addr().unwrap().port();

    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp.send_to(
        &phone_identity(Some(phone_port), &phone).to_line().unwrap(),
        ("127.0.0.1", d.udp_port),
    )
    .await
    .unwrap();

    let (mut tcp, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("le PC ne s'est pas connecté")
        .unwrap();
    // Le PC envoie son identité en clair, puis attend le handshake TLS (rôle client).
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        tokio::io::AsyncReadExt::read_exact(&mut tcp, &mut b)
            .await
            .unwrap();
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    let id = NetworkPacket::from_line(std::str::from_utf8(&line).unwrap())
        .unwrap()
        .into_identity()
        .unwrap();
    assert_eq!(id.device_id, d.device_id);

    // Le téléphone a accepté le TCP : il est client TLS.
    let connector = TlsConnector::from(tls::client_config(&phone).unwrap());
    let ip = ServerName::IpAddress("127.0.0.1".parse::<std::net::IpAddr>().unwrap().into());
    let tls = connector.connect(ip, tcp).await.expect("handshake TLS");
    let (r, mut w) = tokio::io::split(tls);
    let mut r = BufReader::new(r);
    w.write_all(&NetworkPacket::pair(true).to_line().unwrap())
        .await
        .unwrap();
    assert_eq!(read_packet(&mut r).await.pair_flag(), Some(true));
    d.stop();
}

type PhoneReader = BufReader<tokio::io::ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>>;
type PhoneWriter = tokio::io::WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>;

/// Téléphone déjà appairé, prêt à échanger (les demandes initiales du PC sont consommées).
async fn paired_phone(d: &RunningDaemon) -> (DeviceIdentity, PhoneReader, PhoneWriter) {
    let phone = DeviceIdentity::generate().unwrap();
    let (mut r, mut w) = connect_phone(d.tcp_port, &phone).await;
    w.write_all(&NetworkPacket::pair(true).to_line().unwrap())
        .await
        .unwrap();
    assert_eq!(read_packet(&mut r).await.pair_flag(), Some(true));
    // Demandes initiales du PC (batterie, notifications, contacts, SMS, volume, commandes…) :
    // leur nombre et leur ordre dépendent de la machine, on attend simplement le silence.
    let mut line = String::new();
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(500), r.read_line(&mut line)).await
    {
        if n == 0 {
            break;
        }
        line.clear();
    }
    (phone, r, w)
}

async fn send_packet(w: &mut PhoneWriter, kind: &str, body: serde_json::Value) {
    w.write_all(&NetworkPacket::new(kind, body).to_line().unwrap())
        .await
        .unwrap();
}

async fn next_event(
    rx: &mut tokio::sync::broadcast::Receiver<fedora_link_daemon::Event>,
    f: impl Fn(&fedora_link_daemon::Event) -> bool,
) -> fedora_link_daemon::Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.expect("événements");
            if f(&e) {
                return e;
            }
        }
    })
    .await
    .expect("événement attendu")
}

#[tokio::test]
async fn reseau_mobile_du_telephone() {
    let d = daemon("connectivity", true).await;
    let (_phone, _r, mut w) = paired_phone(&d).await;

    send_packet(
        &mut w,
        "kdeconnect.connectivity_report",
        serde_json::json!({"signalStrengths": {"1": {"networkType": "LTE", "signalStrength": 3}}}),
    )
    .await;
    let dev = loop {
        let dev = d.state.list_devices().into_iter().next().unwrap();
        if dev.network_type.is_some() {
            break dev;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!((dev.network_type.as_deref(), dev.signal), (Some("4G"), Some(3)));
    d.stop();
}

#[tokio::test]
async fn batterie_et_notification() {
    let d = daemon("battery", true).await;
    let mut events = d.state.subscribe();
    let (_phone, _r, mut w) = paired_phone(&d).await;

    send_packet(
        &mut w,
        "kdeconnect.battery",
        serde_json::json!({"currentCharge": 42, "isCharging": true, "thresholdEvent": 0}),
    )
    .await;
    next_event(&mut events, |e| {
        matches!(e, fedora_link_daemon::Event::DeviceChanged(_))
    })
    .await;
    let dev = loop {
        let dev = d.state.list_devices().into_iter().next().unwrap();
        if dev.battery.is_some() {
            break dev;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        (dev.battery, dev.charging, dev.paired, dev.connected),
        (Some(42), true, true, true)
    );

    send_packet(&mut w, "kdeconnect.notification", serde_json::json!({"id": "n1", "appName": "Messages", "title": "Alice", "text": "Salut", "silent": true})).await;
    next_event(&mut events, |e| {
        matches!(e, fedora_link_daemon::Event::NotificationPosted { .. })
    })
    .await;
    assert_eq!(d.state.notifications(&dev.id).len(), 1);

    send_packet(
        &mut w,
        "kdeconnect.notification",
        serde_json::json!({"id": "n1", "isCancel": true}),
    )
    .await;
    next_event(&mut events, |e| {
        matches!(e, fedora_link_daemon::Event::NotificationDismissed { .. })
    })
    .await;
    assert!(d.state.notifications(&dev.id).is_empty());
    d.stop();
}

#[tokio::test]
async fn presse_papiers_sonnerie_et_fermeture() {
    let d = daemon("clip", true).await;
    let mut events = d.state.subscribe();
    let (phone, mut r, mut w) = paired_phone(&d).await;

    send_packet(
        &mut w,
        "kdeconnect.clipboard",
        serde_json::json!({"content": "bonjour"}),
    )
    .await;
    let ev = next_event(&mut events, |e| {
        matches!(e, fedora_link_daemon::Event::ClipboardReceived { .. })
    })
    .await;
    assert!(
        matches!(ev, fedora_link_daemon::Event::ClipboardReceived { text, .. } if text == "bonjour")
    );

    d.state.send_clipboard("depuis le PC").unwrap();
    let p = read_packet(&mut r).await;
    assert_eq!(
        (p.kind.as_str(), p.body["content"].as_str()),
        ("kdeconnect.clipboard", Some("depuis le PC"))
    );

    d.state.ring_phone(&phone.device_id).unwrap();
    assert_eq!(
        read_packet(&mut r).await.kind,
        "kdeconnect.findmyphone.request"
    );

    // Plugin désactivé : plus d'envoi.
    d.state.set_plugin_enabled("findmyphone", false).unwrap();
    assert!(d.state.ring_phone(&phone.device_id).is_err());
    d.stop();
}

#[tokio::test]
async fn reception_de_fichier() {
    let d = daemon("recv", true).await;
    let mut events = d.state.subscribe();
    let (_phone, _r, mut w) = paired_phone(&d).await;
    let phone = _phone;

    // Le téléphone ouvre un port de transfert (serveur TLS) puis l'annonce.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut pkt = NetworkPacket::new(
        "kdeconnect.share.request",
        serde_json::json!({"filename": "../../salut.txt"}),
    );
    pkt.payload_size = Some(11);
    pkt.payload_transfer_info = Some(serde_json::json!({"port": port}));
    w.write_all(&pkt.to_line().unwrap()).await.unwrap();

    let (tcp, _) = listener.accept().await.unwrap();
    let tls = TlsAcceptor::from(tls::server_config(&phone).unwrap())
        .accept(tcp)
        .await
        .unwrap();
    let (_r2, mut w2) = tokio::io::split(tls);
    w2.write_all(b"hello world").await.unwrap();
    w2.shutdown().await.unwrap();

    let ev = next_event(&mut events, |e| {
        matches!(e, fedora_link_daemon::Event::FileReceived { .. })
    })
    .await;
    let fedora_link_daemon::Event::FileReceived { path, .. } = ev else {
        unreachable!()
    };
    // Le nom est nettoyé : le fichier reste dans le dossier de réception.
    assert!(path.ends_with("downloads/salut.txt"), "{path}");
    assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    // Le fichier est journalisé pour la liste « Reçus » de l'interface.
    let received = d.state.received.list();
    assert_eq!(received.len(), 1);
    assert_eq!((received[0].name.as_str(), received[0].size), ("salut.txt", 11));
    d.stop();
}

#[tokio::test]
async fn envoi_de_fichier() {
    let d = daemon("send", true).await;
    let (phone, mut r, _w) = paired_phone(&d).await;

    let src = temp_dir("send-src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("photo.bin");
    std::fs::write(&file, vec![7u8; 200_000]).unwrap();
    d.state.send_file(&phone.device_id, file).unwrap();

    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.share.request");
    assert_eq!(p.body["filename"], "photo.bin");
    assert_eq!(p.payload_size, Some(200_000));
    let port = p.payload_transfer_info.unwrap()["port"].as_u64().unwrap() as u16;

    // Le téléphone se connecte au PC pour télécharger (client TLS).
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let ip = ServerName::IpAddress("127.0.0.1".parse::<std::net::IpAddr>().unwrap().into());
    let mut tls = TlsConnector::from(tls::client_config(&phone).unwrap())
        .connect(ip, tcp)
        .await
        .unwrap();
    let mut data = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut tls, &mut data)
        .await
        .unwrap();
    assert_eq!(data.len(), 200_000);
    d.stop();
}

#[tokio::test]
async fn multimedia_metadonnees_pochette_et_commandes() {
    let d = daemon("mpris", true).await;
    let (phone, mut r, mut w) = paired_phone(&d).await;

    send_packet(
        &mut w,
        "kdeconnect.mpris",
        serde_json::json!({"playerList": ["Spotify"]}),
    )
    .await;
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.mpris.request");
    assert_eq!(p.body["player"], "Spotify");
    assert_eq!(p.body["requestNowPlaying"], true);

    send_packet(
        &mut w,
        "kdeconnect.mpris",
        serde_json::json!({
            "player": "Spotify", "title": "Titre", "artist": "Artiste", "isPlaying": true,
            "volume": 40, "length": 200000, "pos": 1000, "albumArtUrl": "kdeconnect://art/1"
        }),
    )
    .await;
    // Nouvelle pochette : le PC la réclame une seule fois.
    let p = read_packet(&mut r).await;
    assert_eq!(p.body["albumArtUrl"], "kdeconnect://art/1");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut art = NetworkPacket::new(
        "kdeconnect.mpris",
        serde_json::json!({
            "player": "Spotify", "transferringAlbumArt": true, "albumArtUrl": "kdeconnect://art/1"
        }),
    );
    art.payload_size = Some(4);
    art.payload_transfer_info = Some(serde_json::json!({"port": port}));
    w.write_all(&art.to_line().unwrap()).await.unwrap();
    let (tcp, _) = listener.accept().await.unwrap();
    let tls = TlsAcceptor::from(tls::server_config(&phone).unwrap())
        .accept(tcp)
        .await
        .unwrap();
    let (_r2, mut w2) = tokio::io::split(tls);
    w2.write_all(b"JPEG").await.unwrap();
    w2.shutdown().await.unwrap();

    let media = loop {
        let m = d.state.media(&phone.device_id);
        if m.first().is_some_and(|p| p.get("artPath").is_some()) {
            break m;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(media[0]["title"], "Titre");
    assert_eq!(
        std::fs::read(media[0]["artPath"].as_str().unwrap()).unwrap(),
        b"JPEG"
    );

    d.state
        .media_action(&phone.device_id, "Spotify", "Next")
        .unwrap();
    assert_eq!(read_packet(&mut r).await.body["action"], "Next");
    d.state.set_plugin_enabled("mpris", true).unwrap();
    d.state
        .media_volume(&phone.device_id, "Spotify", 250)
        .unwrap();
    assert_eq!(read_packet(&mut r).await.body["setVolume"], 100);
    d.state
        .media_seek(&phone.device_id, "Spotify", 42_000)
        .unwrap();
    assert_eq!(read_packet(&mut r).await.body["SetPosition"], 42_000);
    assert!(d
        .state
        .media_action(&phone.device_id, "Spotify", "Format")
        .is_err());
    d.stop();
}

#[tokio::test]
async fn partage_de_liens_et_de_texte() {
    use fedora_link_daemon::Event;
    let d = daemon("share-text", true).await;
    let mut events = d.state.subscribe();
    let (phone, mut r, mut w) = paired_phone(&d).await;

    // Reçus du téléphone.
    send_packet(
        &mut w,
        "kdeconnect.share.request",
        serde_json::json!({"url": "https://fr.wikipedia.org/wiki/Fedora"}),
    )
    .await;
    let ev = next_event(&mut events, |e| matches!(e, Event::LinkReceived { .. })).await;
    assert!(matches!(ev, Event::LinkReceived { url, .. } if url == "https://fr.wikipedia.org/wiki/Fedora"));

    send_packet(
        &mut w,
        "kdeconnect.share.request",
        serde_json::json!({"text": "RDV 18h\nsalle 3"}),
    )
    .await;
    let ev = next_event(&mut events, |e| matches!(e, Event::TextReceived { .. })).await;
    assert!(matches!(ev, Event::TextReceived { text, .. } if text == "RDV 18h\nsalle 3"));

    // Envoyés au téléphone.
    d.state
        .share_url(&phone.device_id, "https://exemple.org/a?b=1")
        .unwrap();
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.share.request");
    assert_eq!(p.body["url"], "https://exemple.org/a?b=1");

    d.state.share_text(&phone.device_id, "bonjour").unwrap();
    assert_eq!(read_packet(&mut r).await.body["text"], "bonjour");

    // Schémas dangereux refusés.
    assert!(d.state.share_url(&phone.device_id, "file:///etc/passwd").is_err());
    assert!(d.state.share_text(&phone.device_id, "   ").is_err());
    d.stop();
}

#[tokio::test]
async fn appel_entrant_sans_effet_si_plugin_desactive() {
    let d = daemon("telephony-off", true).await;
    let (_phone, _r, mut w) = paired_phone(&d).await;
    d.state.set_plugin_enabled("telephony", false).unwrap();
    // Paquet réel d'une sonnerie : ignoré sans erreur ni blocage.
    send_packet(
        &mut w,
        "kdeconnect.telephony",
        serde_json::json!({"event": "ringing", "phoneNumber": "+33612345678", "contactName": "Alice"}),
    )
    .await;
    send_packet(&mut w, "kdeconnect.ping", serde_json::json!({})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(d.state.is_connected(&_phone.device_id));
    d.stop();
}

#[tokio::test]
async fn synchro_des_contacts_puis_sms() {
    use fedora_link_daemon::Event;
    let d = daemon("sms", true).await;
    let mut events = d.state.subscribe();
    let (phone, mut r, mut w) = paired_phone(&d).await;
    let id = phone.device_id.clone();

    // Le téléphone liste ses contacts : le PC demande les fiches qu'il n'a pas.
    send_packet(
        &mut w,
        "kdeconnect.contacts.response_uids_timestamps",
        serde_json::json!({"uids": ["4", "5", "../x"], "4": 1729504200123i64, "5": 1729504200999i64, "../x": 1}),
    )
    .await;
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.contacts.request_vcards_by_uid");
    assert_eq!(p.body["uids"], serde_json::json!(["4", "5"]), "identifiant douteux écarté");

    let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alice Martin\r\nTEL;TYPE=CELL:+33 6 12 34 56 78\r\nX-KDECONNECT-TIMESTAMP:1729504200123\r\nEND:VCARD\r\n";
    send_packet(
        &mut w,
        "kdeconnect.contacts.response_vcards",
        serde_json::json!({"uids": ["4"], "4": vcard}),
    )
    .await;
    next_event(&mut events, |e| matches!(e, Event::ContactsChanged(_))).await;
    assert_eq!(d.state.display_name(&id, "0612345678"), "Alice Martin");

    // Deuxième synchro : la fiche 4 est à jour, seule la 5 est redemandée.
    send_packet(
        &mut w,
        "kdeconnect.contacts.response_uids_timestamps",
        serde_json::json!({"uids": ["4", "5"], "4": 1729504200123i64, "5": 1729504200999i64}),
    )
    .await;
    assert_eq!(read_packet(&mut r).await.body["uids"], serde_json::json!(["5"]));

    // Conversations reçues du téléphone.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    send_packet(
        &mut w,
        "kdeconnect.sms.messages",
        serde_json::json!({"version": 2, "messages": [
            {"event": 1, "body": "On se retrouve à 18h ?", "addresses": [{"address": "+33612345678"}],
             "date": now - 400_000, "type": 1, "thread_id": 132, "read": true, "_id": 901},
            {"event": 1, "body": "Ok", "addresses": [{"address": "0123456789"}],
             "date": now - 900_000, "type": 2, "thread_id": 7, "read": true, "_id": 850}
        ]}),
    )
    .await;
    let ev = next_event(&mut events, |e| matches!(e, Event::SmsChanged { .. })).await;
    assert!(matches!(ev, Event::SmsChanged { thread: 7, .. } | Event::SmsChanged { thread: 132, .. }));
    let conv = d.state.sms_conversations(&id);
    assert_eq!(conv.len(), 2);
    assert_eq!(conv[0]["title"], "Alice Martin");
    assert_eq!(conv[0]["body"], "On se retrouve à 18h ?");

    // Envoi d'un SMS et demande d'une conversation.
    d.state.send_sms(&id, &["+33612345678".to_string()], "Oui, 18h").unwrap();
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.sms.request");
    assert_eq!(
        p.body,
        serde_json::json!({"version": 2, "addresses": [{"address": "+33612345678"}], "messageBody": "Oui, 18h"})
    );
    assert!(d.state.send_sms(&id, &["pas un numéro".to_string()], "x").is_err());
    assert!(d.state.send_sms(&id, &["0612345678".to_string()], "  ").is_err());

    d.state.request_sms_thread(&id, 132).unwrap();
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.sms.request_conversation");
    assert_eq!(p.body["threadID"], 132);
    d.stop();
}

#[tokio::test]
async fn parcourir_le_telephone_sftp() {
    let d = daemon("sftp", true).await;
    let (phone, mut r, mut w) = paired_phone(&d).await;
    let id = phone.device_id.clone();

    let state = d.state.clone();
    let browse = tokio::spawn({
        let id = id.clone();
        async move { state.browse_phone(&id).await }
    });
    let p = read_packet(&mut r).await;
    assert_eq!(p.kind, "kdeconnect.sftp.request");
    assert_eq!(p.body["startBrowsing"], true);
    send_packet(
        &mut w,
        "kdeconnect.sftp",
        serde_json::json!({"ip": "192.168.1.20", "port": 1740, "user": "kdeconnect", "password": "Xy12AbCd34",
            "path": "/", "multiPaths": ["/storage/emulated/0"], "pathNames": ["Stockage interne"]}),
    )
    .await;
    let info = browse.await.unwrap().expect("réponse SFTP");
    assert_eq!(info.uri, "sftp://kdeconnect@127.0.0.1:1740/storage/emulated/0");
    assert_eq!(info.password, "Xy12AbCd34");

    // Erreur côté téléphone (permission refusée) : le message arrive tel quel.
    let state = d.state.clone();
    let browse = tokio::spawn(async move { state.browse_phone(&id).await });
    read_packet(&mut r).await;
    send_packet(
        &mut w,
        "kdeconnect.sftp",
        serde_json::json!({"errorMessage": "Autorisation de stockage manquante"}),
    )
    .await;
    let err = browse.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("Autorisation de stockage manquante"));
    d.stop();
}

#[tokio::test]
async fn commandes_a_distance_seulement_celles_de_la_liste() {
    let d = daemon("runcmd", true).await;
    let (_phone, mut r, mut w) = paired_phone(&d).await;
    let marker = temp_dir("runcmd-marker");
    let _ = std::fs::remove_file(&marker);

    // Le téléphone demande la liste : les trois commandes préremplies.
    send_packet(&mut w, "kdeconnect.runcommand.request", serde_json::json!({"requestCommandList": true})).await;
    let p = loop {
        let p = read_packet(&mut r).await;
        if p.kind == "kdeconnect.runcommand" {
            break p;
        }
    };
    let list: serde_json::Value = serde_json::from_str(p.body["commandList"].as_str().unwrap()).unwrap();
    assert_eq!(list.as_object().unwrap().len(), 3);

    // Une commande ajoutée par l'utilisateur s'exécute ; une commande brute envoyée par le téléphone, jamais.
    let saved = d
        .state
        .set_commands(vec![fedora_link_daemon::settings::RemoteCommand {
            key: String::new(),
            name: "Marque".into(),
            command: format!("touch {}", marker.display()),
        }])
        .unwrap();
    assert_eq!(saved[0].key, "cmd-1");
    let pushed = loop {
        let p = read_packet(&mut r).await;
        if p.kind == "kdeconnect.runcommand" {
            break p;
        }
    };
    assert!(pushed.body["commandList"].as_str().unwrap().contains("Marque"));

    send_packet(&mut w, "kdeconnect.runcommand.request", serde_json::json!({"key": format!("touch {}", marker.display())})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!marker.exists(), "une commande hors liste ne doit jamais s'exécuter");

    send_packet(&mut w, "kdeconnect.runcommand.request", serde_json::json!({"key": "cmd-1"})).await;
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(marker.exists(), "la commande de la liste a été exécutée");
    let _ = std::fs::remove_file(&marker);
    d.stop();
}

#[tokio::test]
async fn pointeur_de_presentation() {
    use fedora_link_daemon::Event;
    let d = daemon("presenter", true).await;
    let mut events = d.state.subscribe();
    let (_phone, _r, mut w) = paired_phone(&d).await;
    send_packet(&mut w, "kdeconnect.presenter", serde_json::json!({"dx": 0.5, "dy": -0.25})).await;
    let ev = next_event(&mut events, |e| matches!(e, Event::PresenterMoved { .. })).await;
    assert!(matches!(ev, Event::PresenterMoved { dx, dy } if dx == 0.5 && dy == -0.25));
    send_packet(&mut w, "kdeconnect.presenter", serde_json::json!({"stop": true})).await;
    next_event(&mut events, |e| matches!(e, Event::PresenterStopped)).await;
    d.stop();
}

/// Noms MPRIS de nos lecteurs actuellement sur le bus de session.
async fn fedoralink_players(conn: &zbus::Connection) -> Vec<String> {
    zbus::fdo::DBusProxy::new(conn)
        .await
        .unwrap()
        .list_names()
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2.fedoralink_"))
        .collect()
}

/// Cycle de vie complet sur le vrai bus de session : publication, commande, retrait.
#[tokio::test]
#[ignore = "utilise le bus de session réel"]
async fn lecteur_du_telephone_publie_sur_mpris() {
    let d = daemon("mprisproxy", true).await;
    let proxy_task = tokio::spawn(fedora_link_daemon::mprisproxy::run(d.state.clone()));
    let bus = zbus::Connection::session().await.unwrap();
    let before = fedoralink_players(&bus).await;
    let (_phone, mut r, mut w) = paired_phone(&d).await;

    send_packet(&mut w, "kdeconnect.mpris", serde_json::json!({"playerList": ["Spotify"]})).await;
    send_packet(
        &mut w,
        "kdeconnect.mpris",
        serde_json::json!({
            "player": "Spotify", "title": "Titre", "artist": "Artiste", "isPlaying": true,
            "canPause": true, "canPlay": true, "volume": 40, "length": 200000, "pos": 1000
        }),
    )
    .await;

    let name = loop {
        let now = fedoralink_players(&bus).await;
        if let Some(n) = now.into_iter().find(|n| !before.contains(n)) {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let player = |iface: &'static str| {
        let bus = bus.clone();
        let name = name.clone();
        async move {
            zbus::Proxy::new(&bus, name, "/org/mpris/MediaPlayer2", iface).await.unwrap()
        }
    };
    let root = player("org.mpris.MediaPlayer2").await;
    let identity: String = root.get_property("Identity").await.unwrap();
    assert_eq!(identity, "Spotify sur Faux téléphone");
    let p = player("org.mpris.MediaPlayer2.Player").await;
    let status: String = p.get_property("PlaybackStatus").await.unwrap();
    assert_eq!(status, "Playing");

    // Une commande GNOME arrive au téléphone sous forme de paquet KDE Connect.
    p.call_method("PlayPause", &()).await.unwrap();
    let request = loop {
        let pkt = read_packet(&mut r).await;
        if pkt.kind == "kdeconnect.mpris.request" && pkt.body.get("action").is_some() {
            break pkt;
        }
    };
    assert_eq!(request.body["action"], "PlayPause");
    assert_eq!(request.body["player"], "Spotify");

    // Le téléphone se déconnecte : le service disparaît du bus.
    drop((r, w));
    for _ in 0..200 {
        if !fedoralink_players(&bus).await.contains(&name) {
            proxy_task.abort();
            d.stop();
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("le service {name} est resté sur le bus après la déconnexion");
}
