//! Souris et clavier virtuels du PC, via le portail XDG RemoteDesktop (Wayland, sans root).
//! L'autorisation est demandée une fois par GNOME, puis mémorisée (jeton de restauration).

use anyhow::{bail, Context, Result};
use ashpd::desktop::remote_desktop::{
    DeviceType, KeyState, NotifyPointerAxisOptions, RemoteDesktop, SelectDevicesOptions,
};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

// Codes evdev (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const KEY_LEFTCTRL: i32 = 29;
const KEY_LEFTSHIFT: i32 = 42;
const KEY_LEFTALT: i32 = 56;
const KEY_LEFTMETA: i32 = 125;
const KEY_ENTER: i32 = 28;
const KEY_TAB: i32 = 15;
const KEY_SPACE: i32 = 57;

/// Après un refus ou un échec, on ne redemande pas l'autorisation avant ce délai.
const RETRY_DELAY: Duration = Duration::from_secs(20);

/// Touches spéciales de KDE Connect (1 à 32) → codes evdev.
const SPECIAL_KEYS: [i32; 33] = [
    0, 14, 15, 101, 105, 103, 106, 108, 104, 109, 102, 107, 28, 111, 1, 99, 70, 29, 56, 42, 125, 59,
    60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88,
];

#[derive(Debug, Clone, PartialEq)]
pub enum InputAction {
    Motion(f64, f64),
    Button { code: u32, pressed: bool },
    Axis(f64, f64),
    Key { code: i32, pressed: bool },
    Text(String),
}

fn click(code: u32) -> [InputAction; 2] {
    [
        InputAction::Button { code, pressed: true },
        InputAction::Button { code, pressed: false },
    ]
}

/// Traduit un paquet `kdeconnect.mousepad.request` en actions, comme le fait KDE Connect sur Wayland.
pub fn plan(body: &Value) -> Vec<InputAction> {
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
    let num = |k: &str| body.get(k).and_then(Value::as_f64).unwrap_or(0.0);

    if flag("singleclick") {
        return click(BTN_LEFT).to_vec();
    }
    if flag("doubleclick") {
        return [click(BTN_LEFT), click(BTN_LEFT)].concat();
    }
    if flag("middleclick") {
        return click(BTN_MIDDLE).to_vec();
    }
    if flag("rightclick") {
        return click(BTN_RIGHT).to_vec();
    }
    // Maintien et relâchement : glisser-déposer.
    if flag("singlehold") {
        return vec![InputAction::Button { code: BTN_LEFT, pressed: true }];
    }
    if flag("singlerelease") {
        return vec![InputAction::Button { code: BTN_LEFT, pressed: false }];
    }
    if flag("scroll") {
        return vec![InputAction::Axis(num("dx"), num("dy"))];
    }

    let special = body
        .get("specialKey")
        .and_then(Value::as_i64)
        .and_then(|k| usize::try_from(k).ok())
        .filter(|k| (1..SPECIAL_KEYS.len()).contains(k));
    let text = body.get("key").and_then(Value::as_str).filter(|k| !k.is_empty());
    if special.is_some() || text.is_some() {
        let modifiers: Vec<i32> = [
            ("ctrl", KEY_LEFTCTRL),
            ("alt", KEY_LEFTALT),
            ("shift", KEY_LEFTSHIFT),
            ("super", KEY_LEFTMETA),
        ]
        .into_iter()
        .filter(|(name, _)| flag(name))
        .map(|(_, code)| code)
        .collect();
        let mut out: Vec<InputAction> = modifiers
            .iter()
            .map(|&code| InputAction::Key { code, pressed: true })
            .collect();
        match (special, text) {
            (Some(k), _) => {
                let code = SPECIAL_KEYS[k];
                out.push(InputAction::Key { code, pressed: true });
                out.push(InputAction::Key { code, pressed: false });
            }
            (None, Some(t)) => out.push(InputAction::Text(t.to_owned())),
            (None, None) => {}
        }
        out.extend(
            modifiers
                .iter()
                .rev()
                .map(|&code| InputAction::Key { code, pressed: false }),
        );
        return out;
    }

    let (dx, dy) = (num("dx"), num("dy"));
    if dx != 0.0 || dy != 0.0 {
        vec![InputAction::Motion(dx, dy)]
    } else {
        Vec::new()
    }
}

/// Fusionne les déplacements consécutifs (un doigt qui glisse envoie des dizaines de paquets par seconde).
pub fn coalesce(actions: Vec<InputAction>) -> Vec<InputAction> {
    let mut out: Vec<InputAction> = Vec::with_capacity(actions.len());
    for a in actions {
        match (out.last_mut(), &a) {
            (Some(InputAction::Motion(x, y)), InputAction::Motion(dx, dy)) => {
                *x += dx;
                *y += dy;
            }
            (Some(InputAction::Axis(x, y)), InputAction::Axis(dx, dy)) => {
                *x += dx;
                *y += dy;
            }
            _ => out.push(a),
        }
    }
    out
}

/// Keysym X11 d'un caractère : Latin-1 tel quel, le reste en `0x01000000 + point de code`.
pub fn keysym_of(c: char) -> i32 {
    let cp = c as u32;
    if (0x20..=0x7E).contains(&cp) || (0xA0..=0xFF).contains(&cp) {
        cp as i32
    } else {
        (0x0100_0000 + cp) as i32
    }
}

struct Active {
    proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

struct Inner {
    active: Option<Active>,
    retry_after: Option<Instant>,
}

pub struct RemoteInput {
    token_path: PathBuf,
    inner: Mutex<Inner>,
}

impl RemoteInput {
    pub fn new(token_path: PathBuf) -> Self {
        Self {
            token_path,
            inner: Mutex::new(Inner {
                active: None,
                retry_after: None,
            }),
        }
    }

    async fn open(&self) -> Result<Active> {
        let proxy = RemoteDesktop::new().await.context("portail RemoteDesktop absent")?;
        let session = proxy.create_session(Default::default()).await?;
        let token = std::fs::read_to_string(&self.token_path)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        proxy
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token.as_deref()),
            )
            .await?;
        let started = proxy
            .start(&session, None, Default::default())
            .await?
            .response()
            .context("autorisation refusée")?;
        let wanted: BitFlags<DeviceType> = DeviceType::Keyboard | DeviceType::Pointer;
        if !started.devices().contains(wanted) {
            bail!("clavier et souris non accordés");
        }
        if let Some(new_token) = started.restore_token() {
            if let Some(dir) = self.token_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&self.token_path, new_token) {
                warn!("jeton du portail non mémorisé : {e}");
            }
        }
        info!("contrôle du clavier et de la souris autorisé");
        Ok(Active { proxy, session })
    }

    /// Exécute les actions ; ouvre la session du portail au premier besoin.
    pub async fn apply(&self, actions: &[InputAction]) -> Result<()> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock().await;
        if inner.active.is_none() {
            if inner.retry_after.is_some_and(|t| Instant::now() < t) {
                bail!("autorisation du clavier et de la souris non accordée");
            }
            match self.open().await {
                Ok(a) => {
                    inner.retry_after = None;
                    inner.active = Some(a);
                }
                Err(e) => {
                    inner.retry_after = Some(Instant::now() + RETRY_DELAY);
                    return Err(e);
                }
            }
        }
        let Some(active) = inner.active.as_ref() else {
            bail!("session de saisie absente");
        };
        let result = send(active, actions).await;
        if result.is_err() {
            // Session fermée par le système : on repartira de zéro à la prochaine action.
            inner.active = None;
        }
        result
    }
}

async fn send(a: &Active, actions: &[InputAction]) -> Result<()> {
    let (p, s) = (&a.proxy, &a.session);
    for action in actions {
        match action {
            InputAction::Motion(dx, dy) => {
                p.notify_pointer_motion(s, *dx, *dy, Default::default()).await?
            }
            InputAction::Button { code, pressed } => {
                p.notify_pointer_button(s, *code as i32, state(*pressed), Default::default())
                    .await?
            }
            InputAction::Axis(dx, dy) => {
                p.notify_pointer_axis(s, *dx, *dy, NotifyPointerAxisOptions::default().set_finish(true))
                    .await?
            }
            InputAction::Key { code, pressed } => {
                p.notify_keyboard_keycode(s, *code, state(*pressed), Default::default())
                    .await?
            }
            InputAction::Text(text) => {
                for c in text.chars() {
                    match c {
                        '\n' | '\r' => tap(a, KEY_ENTER).await?,
                        '\t' => tap(a, KEY_TAB).await?,
                        ' ' => tap(a, KEY_SPACE).await?,
                        c => {
                            let sym = keysym_of(c);
                            p.notify_keyboard_keysym(s, sym, KeyState::Pressed, Default::default())
                                .await?;
                            p.notify_keyboard_keysym(s, sym, KeyState::Released, Default::default())
                                .await?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

async fn tap(a: &Active, code: i32) -> Result<()> {
    for pressed in [true, false] {
        a.proxy
            .notify_keyboard_keycode(&a.session, code, state(pressed), Default::default())
            .await?;
    }
    Ok(())
}

fn state(pressed: bool) -> KeyState {
    if pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_link_core::packet::NetworkPacket;

    fn plan_of(line: &str) -> Vec<InputAction> {
        plan(&NetworkPacket::from_line(line).unwrap().body)
    }

    // Paquets tels qu'émis par KDE Connect Android (MousePadPlugin / KeyListenerView).
    #[test]
    fn deplacement_clic_et_defilement() {
        assert_eq!(
            plan_of(r#"{"id":1,"type":"kdeconnect.mousepad.request","body":{"dx":12.5,"dy":-3.0}}"#),
            vec![InputAction::Motion(12.5, -3.0)]
        );
        assert_eq!(
            plan_of(r#"{"id":2,"type":"kdeconnect.mousepad.request","body":{"singleclick":true}}"#),
            click(BTN_LEFT).to_vec()
        );
        assert_eq!(
            plan_of(r#"{"id":3,"type":"kdeconnect.mousepad.request","body":{"rightclick":true}}"#),
            click(BTN_RIGHT).to_vec()
        );
        assert_eq!(
            plan_of(r#"{"id":4,"type":"kdeconnect.mousepad.request","body":{"doubleclick":true}}"#).len(),
            4
        );
        assert_eq!(
            plan_of(r#"{"id":5,"type":"kdeconnect.mousepad.request","body":{"scroll":true,"dx":0.0,"dy":-18.0}}"#),
            vec![InputAction::Axis(0.0, -18.0)]
        );
        assert_eq!(
            plan_of(r#"{"id":6,"type":"kdeconnect.mousepad.request","body":{"singlehold":true}}"#),
            vec![InputAction::Button { code: BTN_LEFT, pressed: true }]
        );
    }

    #[test]
    fn touches_speciales_et_modificateurs() {
        // Flèche gauche (4) → KEY_LEFT (105).
        assert_eq!(
            plan_of(r#"{"id":7,"type":"kdeconnect.mousepad.request","body":{"specialKey":4}}"#),
            vec![
                InputAction::Key { code: 105, pressed: true },
                InputAction::Key { code: 105, pressed: false }
            ]
        );
        // Alt+F4 (envoyé par « fermer » du téléphone).
        let alt_f4 = plan_of(r#"{"id":8,"type":"kdeconnect.mousepad.request","body":{"alt":true,"specialKey":24}}"#);
        assert_eq!(alt_f4.first(), Some(&InputAction::Key { code: KEY_LEFTALT, pressed: true }));
        assert_eq!(alt_f4.last(), Some(&InputAction::Key { code: KEY_LEFTALT, pressed: false }));
        assert!(alt_f4.contains(&InputAction::Key { code: 62, pressed: true }), "F4 = 62");
        // Ctrl+c
        let ctrl_c = plan_of(r#"{"id":9,"type":"kdeconnect.mousepad.request","body":{"ctrl":true,"key":"c"}}"#);
        assert_eq!(ctrl_c.len(), 3);
        assert_eq!(ctrl_c[1], InputAction::Text("c".into()));
        // Précédent / suivant de présentation : Page haut / bas (8 et 9).
        assert!(plan_of(r#"{"id":10,"type":"kdeconnect.mousepad.request","body":{"specialKey":9}}"#)
            .contains(&InputAction::Key { code: 109, pressed: true }));
        // Touche hors table : ignorée.
        assert!(plan_of(r#"{"id":11,"type":"kdeconnect.mousepad.request","body":{"specialKey":99}}"#).is_empty());
    }

    #[test]
    fn texte_unicode_et_fusion_des_deplacements() {
        assert_eq!(
            plan_of(r#"{"id":12,"type":"kdeconnect.mousepad.request","body":{"key":"é€"}}"#),
            vec![InputAction::Text("é€".into())]
        );
        assert_eq!(keysym_of('a'), 0x61);
        assert_eq!(keysym_of('é'), 0xE9);
        assert_eq!(keysym_of('€'), 0x0100_20AC);
        let merged = coalesce(vec![
            InputAction::Motion(1.0, 2.0),
            InputAction::Motion(3.0, -1.0),
            InputAction::Button { code: BTN_LEFT, pressed: true },
            InputAction::Motion(1.0, 1.0),
        ]);
        assert_eq!(
            merged,
            vec![
                InputAction::Motion(4.0, 1.0),
                InputAction::Button { code: BTN_LEFT, pressed: true },
                InputAction::Motion(1.0, 1.0)
            ]
        );
    }
}
