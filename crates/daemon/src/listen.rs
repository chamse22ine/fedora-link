//! « Écouter sur le PC » : le son du téléphone est capté par `scrcpy --no-video` (USB ou ADB Wi-Fi).
//! Le processus est supervisé : état publié, arrêt propre, relance après une coupure,
//! cause lisible en cas d'échec.

use crate::pcaudio;
use crate::state::Event;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

/// Un scrcpy qui tient ce temps sans se plaindre est considéré comme actif.
const SETTLE: Duration = Duration::from_secs(2);
/// Au-delà, une coupure n'est pas un échec de démarrage : on repart de zéro.
const STABLE_AFTER: Duration = Duration::from_secs(10);
const MAX_QUICK_FAILURES: u32 = 3;
const TERM_GRACE: Duration = Duration::from_secs(3);
const ADB_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListenStatus {
    /// `stopped`, `starting`, `active` ou `error`.
    pub state: &'static str,
    /// Explication affichable (cause de l'erreur, tentative de reconnexion…).
    pub detail: String,
}

impl ListenStatus {
    fn new(state: &'static str, detail: impl Into<String>) -> Self {
        Self { state, detail: detail.into() }
    }
}

/// Programmes lancés ; remplaçables dans les tests.
#[derive(Debug, Clone)]
pub struct Tools {
    pub scrcpy: String,
    pub adb: String,
}

impl Default for Tools {
    fn default() -> Self {
        Self { scrcpy: "scrcpy".into(), adb: "adb".into() }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Adresse du téléphone sur le réseau local, pour choisir l'entrée ADB Wi-Fi.
    pub ip: Option<String>,
    /// `--audio-dup` : le son reste aussi sur le téléphone (Android 13+).
    pub keep_on_phone: bool,
    pub pause_players: bool,
    pub settle: Duration,
    /// Attente avant de relancer scrcpy après une coupure.
    pub retry_delay: Duration,
    pub tools: Tools,
}

impl Options {
    pub fn new(ip: Option<String>, keep_on_phone: bool, pause_players: bool) -> Self {
        Self { ip, keep_on_phone, pause_players, settle: SETTLE, retry_delay: Duration::from_secs(2), tools: Tools::default() }
    }
}

pub struct Listener {
    events: broadcast::Sender<Event>,
    status: Mutex<ListenStatus>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Listener {
    pub(crate) fn new(events: broadcast::Sender<Event>) -> Self {
        Self {
            events,
            status: Mutex::new(ListenStatus::new("stopped", "")),
            stop: Mutex::new(None),
        }
    }

    pub fn status(&self) -> ListenStatus {
        lock(&self.status).clone()
    }

    fn set(&self, state: &'static str, detail: impl Into<String>) {
        *lock(&self.status) = ListenStatus::new(state, detail);
        let _ = self.events.send(Event::ListenChanged);
    }

    pub fn start(self: &Arc<Self>, opts: Options) -> Result<()> {
        let mut slot = lock(&self.stop);
        if slot.is_some() {
            bail!("l'écoute est déjà en cours");
        }
        let (tx, rx) = oneshot::channel();
        *slot = Some(tx);
        drop(slot);
        let this = self.clone();
        tokio::spawn(async move {
            this.run(opts, rx).await;
            lock(&this.stop).take();
        });
        Ok(())
    }

    /// Demande l'arrêt ; sans effet si rien n'est lancé.
    pub fn stop(&self) {
        if let Some(tx) = lock(&self.stop).take() {
            let _ = tx.send(());
        }
    }

    async fn run(&self, opts: Options, mut stop: oneshot::Receiver<()>) {
        self.set("starting", "");
        let paused = if opts.pause_players {
            pcaudio::pause_playing_players().await.unwrap_or_else(|e| {
                debug!("lecteurs du PC non mis en pause : {e:#}");
                Vec::new()
            })
        } else {
            Vec::new()
        };
        let outcome = self.supervise(&opts, &mut stop).await;
        pcaudio::resume_players(&paused).await;
        match outcome {
            Ok(()) => self.set("stopped", ""),
            Err(cause) => {
                warn!("écoute interrompue : {cause}");
                self.set("error", cause);
            }
        }
    }

    /// `Ok` = arrêt demandé ; `Err` = cause affichable d'un échec définitif.
    async fn supervise(
        &self,
        opts: &Options,
        stop: &mut oneshot::Receiver<()>,
    ) -> Result<(), String> {
        let mut quick_failures = 0;
        loop {
            let serial = pick_serial_with(&opts.tools.adb, opts.ip.as_deref()).await?;
            let mut cmd = Command::new(&opts.tools.scrcpy);
            cmd.args(scrcpy_args(serial.as_deref(), opts.keep_on_phone))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    "scrcpy est introuvable : installez-le (paquet, COPR ou Flatpak)".to_owned()
                }
                _ => format!("scrcpy ne démarre pas : {e}"),
            })?;
            info!(?serial, "écoute lancée");

            let (lines_tx, mut lines) = mpsc::unbounded_channel();
            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if lines_tx.send(line).is_err() {
                            break;
                        }
                    }
                });
            }

            let started = Instant::now();
            let mut tail: VecDeque<String> = VecDeque::new();
            let mut active = false;
            let settle = tokio::time::sleep(opts.settle);
            tokio::pin!(settle);
            let exit = loop {
                tokio::select! {
                    _ = &mut *stop => {
                        terminate(&mut child).await;
                        return Ok(());
                    }
                    status = child.wait() => break status.ok().and_then(|s| s.code()),
                    Some(line) = lines.recv() => {
                        debug!("scrcpy : {line}");
                        if let Some(cause) = classify(&line) {
                            terminate(&mut child).await;
                            return Err(cause);
                        }
                        if tail.len() == 8 {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                    _ = &mut settle, if !active => {
                        active = true;
                        self.set("active", "");
                    }
                }
            };
            // Les dernières lignes ont pu arriver avec la sortie du processus.
            while let Ok(line) = lines.try_recv() {
                if let Some(cause) = classify(&line) {
                    return Err(cause);
                }
                tail.push_back(line);
            }

            if started.elapsed() >= STABLE_AFTER {
                quick_failures = 0;
            } else {
                quick_failures += 1;
            }
            if quick_failures >= MAX_QUICK_FAILURES {
                let last = tail
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| match exit {
                        Some(code) => format!("scrcpy s'est arrêté (code {code})"),
                        None => "scrcpy s'est arrêté".to_owned(),
                    });
                return Err(last);
            }
            self.set("starting", "reconnexion…");
            tokio::select! {
                _ = &mut *stop => return Ok(()),
                _ = tokio::time::sleep(opts.retry_delay) => {}
            }
        }
    }
}

/// SIGTERM pour que scrcpy ferme proprement sa session ADB, puis SIGKILL s'il tarde.
async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        // SAFETY : `kill` n'accède à aucune mémoire ; le pid est celui de notre enfant vivant.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(TERM_GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

pub(crate) fn scrcpy_args(serial: Option<&str>, keep_on_phone: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--no-video", "--no-control", "--audio-codec=opus"]
        .map(String::from)
        .into();
    if keep_on_phone {
        args.push("--audio-dup".into());
    }
    if let Some(s) = serial {
        args.push(format!("--serial={s}"));
    }
    args
}

/// Traduit une ligne d'erreur de scrcpy en cause lisible ; `None` si la ligne est anodine.
pub(crate) fn classify(line: &str) -> Option<String> {
    if let Some(rest) = line.split("before Android ").nth(1) {
        let version: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return Some(match version.as_str() {
            "13" => "« Garder le son sur le téléphone » demande Android 13 ou plus récent".into(),
            "11" => "La capture audio demande Android 11 ou plus récent".into(),
            v => format!("Fonction audio indisponible avant Android {v}"),
        });
    }
    let lower = line.to_lowercase();
    if lower.contains("unauthorized") {
        return Some(
            "ADB n'est pas autorisé : acceptez la demande de débogage sur le téléphone".into(),
        );
    }
    if lower.contains("could not find any adb device") || lower.contains("no devices/emulators") {
        return Some("Aucun téléphone ADB : branchez-le en USB ou activez ADB Wi-Fi".into());
    }
    if lower.contains("more than one device") {
        return Some("Plusieurs appareils ADB sont connectés".into());
    }
    if lower.contains("audio disabled") {
        return Some("La capture audio est indisponible sur ce téléphone".into());
    }
    None
}

async fn pick_serial_with(adb: &str, ip: Option<&str>) -> Result<Option<String>, String> {
    let output = tokio::time::timeout(
        ADB_TIMEOUT,
        Command::new(adb).arg("devices").stdin(Stdio::null()).output(),
    )
    .await;
    match output {
        // Sans adb dans le PATH, scrcpy tentera sa propre copie.
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(Err(e)) => Err(format!("adb ne répond pas : {e}")),
        Err(_) => Err("adb ne répond pas (délai dépassé)".into()),
        Ok(Ok(out)) => pick_serial(&String::from_utf8_lossy(&out.stdout), ip).map(Some),
    }
}

/// Choisit l'appareil ADB dans la sortie de `adb devices` : le seul prêt, ou à défaut celui
/// dont l'adresse correspond à celle du téléphone.
pub(crate) fn pick_serial(output: &str, ip: Option<&str>) -> Result<String, String> {
    let mut ready = Vec::new();
    let mut unauthorized = false;
    for line in output.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        match state {
            "device" => ready.push(serial.to_owned()),
            "unauthorized" => unauthorized = true,
            _ => {}
        }
    }
    match ready.len() {
        0 if unauthorized => Err(classify("unauthorized").unwrap_or_default()),
        0 => Err(classify("could not find any adb device").unwrap_or_default()),
        1 => Ok(ready.remove(0)),
        _ => ip
            .and_then(|ip| ready.iter().find(|s| s.starts_with(&format!("{ip}:"))).cloned())
            .ok_or_else(|| classify("more than one device").unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_de_scrcpy() {
        assert_eq!(
            scrcpy_args(None, false),
            ["--no-video", "--no-control", "--audio-codec=opus"]
        );
        let a = scrcpy_args(Some("192.168.1.5:5555"), true);
        assert!(a.contains(&"--audio-dup".to_string()));
        assert_eq!(a.last().unwrap(), "--serial=192.168.1.5:5555");
    }

    #[test]
    fn causes_lisibles() {
        let c = |l| classify(l).unwrap();
        assert!(c("WARN: Audio disabled: it is not supported before Android 11").contains("Android 11"));
        assert!(c("ERROR: Audio duplication is not supported before Android 13").contains("Android 13"));
        assert!(c("adb: device unauthorized.").contains("autorisé"));
        assert!(c("ERROR: Could not find any ADB device").contains("Aucun téléphone"));
        assert!(c("ERROR: ADB: more than one device/emulator").contains("Plusieurs"));
        assert_eq!(classify("INFO: Audio encoder: opus"), None);
    }

    #[test]
    fn choix_de_l_appareil_adb() {
        let one = "List of devices attached\nR58M123\tdevice\n\n";
        assert_eq!(pick_serial(one, None).unwrap(), "R58M123");

        let two = "List of devices attached\nR58M123\tdevice\n192.168.1.5:5555\tdevice\n";
        assert_eq!(pick_serial(two, Some("192.168.1.5")).unwrap(), "192.168.1.5:5555");
        assert!(pick_serial(two, None).unwrap_err().contains("Plusieurs"));

        let unauth = "List of devices attached\nR58M123\tunauthorized\n";
        assert!(pick_serial(unauth, None).unwrap_err().contains("autorisé"));
        assert!(pick_serial("List of devices attached\n\n", None).unwrap_err().contains("Aucun"));
    }

    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    fn fixture(tag: &str, scrcpy_body: &str) -> (Arc<Listener>, Options, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("fl-listen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let adb = script(&dir, "adb", "printf 'List of devices attached\\nSER1\\tdevice\\n'");
        let scrcpy = script(&dir, "scrcpy", scrcpy_body);
        let listener = Arc::new(Listener::new(broadcast::channel(16).0));
        let mut opts = Options::new(None, false, false);
        opts.settle = Duration::from_millis(150);
        opts.retry_delay = Duration::from_millis(50);
        opts.tools = Tools { scrcpy, adb };
        (listener, opts, dir)
    }

    async fn wait_for(l: &Listener, state: &str) -> ListenStatus {
        for _ in 0..200 {
            let s = l.status();
            if s.state == state {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("état « {state} » jamais atteint, dernier : {:?}", l.status());
    }

    #[tokio::test]
    async fn demarre_devient_actif_puis_s_arrete_proprement() {
        let (l, opts, dir) = fixture("ok", "echo $$ > \"$(dirname \"$0\")/pid\"; exec sleep 30");
        l.start(opts.clone()).unwrap();
        assert!(l.start(opts).is_err(), "pas deux écoutes à la fois");
        wait_for(&l, "active").await;
        let pid: i32 = std::fs::read_to_string(dir.join("pid")).unwrap().trim().parse().unwrap();
        l.stop();
        wait_for(&l, "stopped").await;
        // Le processus a bien reçu le signal d'arrêt.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(unsafe { libc::kill(pid, 0) } != 0, "scrcpy tourne encore");
    }

    #[tokio::test]
    async fn android_trop_ancien_donne_une_erreur_claire() {
        let (l, opts, _) = fixture(
            "old",
            "echo 'WARN: Audio disabled: it is not supported before Android 11' >&2; sleep 30",
        );
        l.start(opts).unwrap();
        let s = wait_for(&l, "error").await;
        assert!(s.detail.contains("Android 11"), "{s:?}");
    }

    #[tokio::test]
    async fn coupures_repetees_finissent_en_erreur() {
        let (l, opts, _) = fixture("flaky", "echo 'ERROR: connexion perdue' >&2; exit 1");
        l.start(opts).unwrap();
        let s = wait_for(&l, "error").await;
        assert_eq!(s.detail, "ERROR: connexion perdue");
    }

    #[tokio::test]
    async fn scrcpy_absent() {
        let (l, mut opts, _) = fixture("missing", "true");
        opts.tools.scrcpy = "/nonexistent/scrcpy".into();
        l.start(opts).unwrap();
        let s = wait_for(&l, "error").await;
        assert!(s.detail.contains("introuvable"), "{s:?}");
    }
}
