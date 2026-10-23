<div align="center">

<img src="../packaging/org.fedoralink.Gui.svg" alt="Fedora Link" width="112" height="112">

# Fedora Link

**Votre téléphone Android, intégré à votre bureau Fedora.**

Notifications, presse-papiers, fichiers, SMS, recopie d'écran et lecteurs multimédia,
le tout en local, sans cloud ni compte.

![Licence](https://img.shields.io/badge/licence-GPL--3.0--or--later-blue)
![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)
![Fedora](https://img.shields.io/badge/Fedora-GNOME%20%7C%20Wayland-51A2DA?logo=fedora&logoColor=white)
![GTK4](https://img.shields.io/badge/GTK4-libadwaita-4A86CF)
![Protocole](https://img.shields.io/badge/protocole-KDE%20Connect-1D99F3)

[English](../README.md) · **Français**

</div>

---

Fedora Link est une application **native** pour Fedora (GNOME, Wayland) qui parle le
**protocole KDE Connect** : l'application [KDE Connect](https://kdeconnect.kde.org/) pour Android
sert de client mobile, aucune appli supplémentaire n'est à installer sur le téléphone.

Un daemon utilisateur garde le lien en arrière-plan, fenêtre fermée. L'interface GTK4 / libadwaita
n'est qu'un client D-Bus de ce daemon.

> **Vie privée** : tout reste sur le réseau local. Les échanges sont chiffrés en TLS et chaque
> appareil doit être appairé explicitement.

## Fonctionnalités

| | Fonctionnalité | Détail |
|---|---|---|
| 🔔 | **Notifications** | Celles du téléphone s'affichent sur le bureau, avec réponse rapide |
| 💬 | **SMS et contacts** | Lire et envoyer des SMS depuis le PC, notification des nouveaux messages |
| 📞 | **Appels** | Alerte d'appel entrant côté PC (téléphonie) |
| 🔋 | **Batterie et réseau** | Niveau de batterie et réseau mobile du téléphone, batterie du PC vers le téléphone |
| 📋 | **Presse-papiers** | Copier sur l'un, coller sur l'autre |
| 📁 | **Fichiers** | Envoi et réception, entrée « Envoyer vers le téléphone » dans Nautilus, navigation SFTP |
| 🔍 | **Trouver mon téléphone** | Le fait sonner à distance |
| 🖥️ | **Recopie d'écran** | Pilotée via [scrcpy](https://github.com/Genymobile/scrcpy) |
| 🔊 | **Écouter sur le PC** | Le son du téléphone sur les haut-parleurs du PC (audio scrcpy, supervisé) |
| 🎵 | **Lecteurs multimédia** | Les lecteurs du téléphone apparaissent dans le menu multimédia de GNOME (MPRIS), et inversement |
| 🖱️ | **Pavé tactile et clavier** | Le téléphone pilote la souris et le clavier du PC |
| 🔈 | **Volume système** | Réglage du volume du PC depuis le téléphone |
| 📽️ | **Présentation** | Le téléphone sert de télécommande de diaporama |
| ⚙️ | **Commandes** | Lancer des commandes prédéfinies du PC depuis le téléphone |
| 🏓 | **Ping** | Test rapide du lien |

Chaque fonctionnalité est un plugin indépendant, activable séparément.

## Installation

### Depuis le RPM

```bash
sudo dnf install rpm-build cargo rust gtk4-devel libadwaita-devel desktop-file-utils appstream
packaging/build-rpm.sh              # RPMBUILD_ARGS=--nodeps si Rust vient de rustup
sudo dnf install ~/rpmbuild/RPMS/x86_64/fedora-link-*.rpm
```

Paquets recommandés (installés automatiquement selon la configuration de dnf) : `firewalld`,
`nautilus-python`, `gvfs`, `openssh-clients`, `pipewire-pulseaudio`, `xdg-desktop-portal-gnome`,
`scrcpy`, `android-tools`.

### Pare-feu

Les ports **1714-1764** (TCP et UDP) doivent être ouverts dans la zone du réseau :

```bash
sudo firewall-cmd --permanent --add-service=fedora-link && sudo firewall-cmd --reload
```

## Appairage

1. Installez **KDE Connect** sur le téléphone, connecté au même réseau Wi-Fi que le PC.
2. Ouvrez Fedora Link, puis choisissez le PC dans l'appli mobile
   (ou utilisez « Ajouter un appareil par IP »).
3. Vérifiez que **le même code** s'affiche des deux côtés, puis acceptez.

> ⚠️ KDE Connect et GSConnect ne doivent pas tourner en même temps sur le PC : ils utilisent les mêmes ports.

## Architecture

```
┌──────────────┐   TLS / protocole KDE Connect   ┌──────────────────────────────┐
│  Téléphone   │ ◄─────────────────────────────► │  fedora-linkd  (daemon)      │
│ KDE Connect  │        réseau local             │  découverte · appairage ·    │
└──────────────┘                                 │  plugins                     │
                                                 └──────────────┬───────────────┘
                                                                │ D-Bus
                                                                │ org.fedoralink.Daemon1
                                                 ┌──────────────▼───────────────┐
                                                 │  fedora-link  (GTK4 + Adw)   │
                                                 └──────────────────────────────┘
```

| Crate | Rôle |
|---|---|
| [`crates/core`](../crates/core) | Paquets du protocole, certificats, TLS |
| [`crates/daemon`](../crates/daemon) | `fedora-linkd` : découverte, appairage, plugins, API D-Bus |
| [`crates/gui`](../crates/gui) | `fedora-link` : fenêtre GTK4 / libadwaita, simple client D-Bus |
| [`packaging`](../packaging) | Spec RPM, service systemd utilisateur, activation D-Bus, service firewalld, extension Nautilus, icône, métadonnées AppStream |

Le daemon tourne en service systemd utilisateur (`fedora-link.service`). L'interface le démarre
par activation D-Bus si besoin.

## Développement

Dépendances de build : `gtk4-devel libadwaita-devel`.

```bash
cargo test --workspace
cargo run -p fedora-link-daemon -- --console   # répondre aux appairages au terminal
cargo run -p fedora-link-gui
```

### Dépannage

Si vous voyez « Unknown method » ou « port UDP 1716 déjà utilisé », un ancien daemon tourne encore
(l'activation D-Bus le relance quand la GUI est ouverte) :

```bash
systemctl --user stop fedora-link.service
pkill -f /usr/bin/fedora-linkd
```

Puis relancez la version voulue.

## État du projet

Version **0.1.0**. Le cœur (appairage, notifications, presse-papiers, fichiers, MPRIS) est
fonctionnel. Certaines fonctions restent à valider sur du matériel réel : écoute scrcpy,
batterie du PC vers le téléphone, réseau mobile, SFTP, pavé tactile et volume.

Idées pour la suite : micro, webcam (v4l2loopback), enceinte Bluetooth.

## Licence

[GPL-3.0-or-later](../LICENSE)
