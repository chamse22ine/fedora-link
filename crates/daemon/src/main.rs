use anyhow::{Context, Result};
use fedora_link_daemon::{
    dbus, start, Config, Event, State, DEFAULT_UDP_PORT, TCP_PORT_MAX, TCP_PORT_MIN,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::EnvFilter;

/// Mode `--console` : répond aux demandes d'appairage depuis le terminal
/// (utile sans interface graphique).
async fn console_pairing(state: Arc<State>) {
    let mut events = state.subscribe();
    let mut stdin = BufReader::new(tokio::io::stdin());
    while let Ok(event) = events.recv().await {
        if let Event::PairingRequested { id, name, code } = event {
            println!("Demande d'appairage de « {name} » — code : {code}\nAccepter ? [o/N]");
            let mut answer = String::new();
            if stdin.read_line(&mut answer).await.is_err() {
                continue;
            }
            let accept = matches!(
                answer.trim().to_lowercase().as_str(),
                "o" | "oui" | "y" | "yes"
            );
            if let Err(e) = state.answer_pairing(&id, accept) {
                eprintln!("{e:#}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Les logs partent sur stderr : journald les capte sous systemd.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let config_dir = dirs::config_dir()
        .context("dossier de configuration introuvable")?
        .join("fedora-link");
    // `HOSTNAME` n'est pas défini sous systemd : on lit le noyau.
    let device_name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Fedora".into());

    let daemon = start(Config {
        config_dir,
        device_name,
        udp_port: DEFAULT_UDP_PORT,
        tcp_port_range: (TCP_PORT_MIN, TCP_PORT_MAX),
        broadcast: true,
        auto_accept: false,
        open_links: true,
        download_dir: None,
        data_dir: dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("fedora-link"),
        cache_dir: dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("fedora-link"),
    })
    .await?;

    // Sans bus de session le daemon reste utilisable en mode console.
    let _dbus = match dbus::serve(daemon.state.clone()).await {
        Ok(conn) => Some(conn),
        Err(e) => {
            tracing::warn!("D-Bus indisponible : {e:#}");
            None
        }
    };
    if _dbus.is_some() {
        tokio::spawn(fedora_link_daemon::mprisproxy::run(daemon.state.clone()));
    }
    if std::env::args().any(|a| a == "--console") {
        tokio::spawn(console_pairing(daemon.state.clone()));
    }

    tokio::select! {
        _ = daemon.wait() => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("arrêt demandé"),
    }
    Ok(())
}
