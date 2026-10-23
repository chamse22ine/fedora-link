<div align="center">

<img src="packaging/org.fedoralink.Gui.svg" alt="Fedora Link" width="112" height="112">

# Fedora Link

**Your Android phone, integrated into your Fedora desktop.**

Notifications, clipboard, files, SMS, screen mirroring and media players,
all local, with no cloud and no account.

![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)
![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)
![Fedora](https://img.shields.io/badge/Fedora-GNOME%20%7C%20Wayland-51A2DA?logo=fedora&logoColor=white)
![GTK4](https://img.shields.io/badge/GTK4-libadwaita-4A86CF)
![Protocol](https://img.shields.io/badge/protocol-KDE%20Connect-1D99F3)

**English** · [Français](docs/README.fr.md)

</div>

---

Fedora Link is a **native** application for Fedora (GNOME, Wayland) that speaks the
**KDE Connect protocol**: the [KDE Connect](https://kdeconnect.kde.org/) Android app acts as the
mobile client, so nothing extra needs to be installed on the phone.

A user daemon keeps the link alive in the background, even with the window closed. The
GTK4 / libadwaita interface is just a D-Bus client of that daemon.

> **Privacy**: everything stays on your local network. Traffic is encrypted with TLS and every
> device must be explicitly paired.

## Features

| | Feature | Details |
|---|---|---|
| 🔔 | **Notifications** | Phone notifications show up on the desktop, with quick reply |
| 💬 | **SMS and contacts** | Read and send SMS from the PC, with notifications for new messages |
| 📞 | **Calls** | Incoming call alert on the PC (telephony) |
| 🔋 | **Battery and network** | Phone battery level and mobile network, PC battery sent to the phone |
| 📋 | **Clipboard** | Copy on one device, paste on the other |
| 📁 | **Files** | Send and receive, "Send to phone" entry in Nautilus, SFTP browsing |
| 🔍 | **Find my phone** | Make it ring remotely |
| 🖥️ | **Screen mirroring** | Driven through [scrcpy](https://github.com/Genymobile/scrcpy) |
| 🔊 | **Listen on the PC** | Phone audio on the PC speakers (supervised scrcpy audio) |
| 🎵 | **Media players** | Phone players appear in GNOME's media menu (MPRIS), and the other way around |
| 🖱️ | **Touchpad and keyboard** | The phone controls the PC's mouse and keyboard |
| 🔈 | **System volume** | Adjust the PC volume from the phone |
| 📽️ | **Presenter** | Use the phone as a slideshow remote |
| ⚙️ | **Commands** | Run predefined PC commands from the phone |
| 🏓 | **Ping** | Quick link test |

Each feature is an independent plugin that can be enabled separately.

## Installation

### From the RPM

```bash
sudo dnf install rpm-build cargo rust gtk4-devel libadwaita-devel desktop-file-utils appstream
packaging/build-rpm.sh              # RPMBUILD_ARGS=--nodeps if Rust comes from rustup
sudo dnf install ~/rpmbuild/RPMS/x86_64/fedora-link-*.rpm
```

Recommended packages (installed automatically depending on your dnf configuration): `firewalld`,
`nautilus-python`, `gvfs`, `openssh-clients`, `pipewire-pulseaudio`, `xdg-desktop-portal-gnome`,
`scrcpy`, `android-tools`.

### Firewall

Ports **1714-1764** (TCP and UDP) must be open in your network's firewall zone:

```bash
sudo firewall-cmd --permanent --add-service=fedora-link && sudo firewall-cmd --reload
```

## Pairing

1. Install **KDE Connect** on your phone, connected to the same Wi-Fi network as the PC.
2. Open Fedora Link, then pick the PC in the mobile app
   (or use "Add a device by IP").
3. Check that **the same code** is shown on both sides, then accept.

> ⚠️ KDE Connect and GSConnect must not run on the PC at the same time: they use the same ports.

## Architecture

```
┌──────────────┐   TLS / KDE Connect protocol    ┌──────────────────────────────┐
│    Phone     │ ◄─────────────────────────────► │  fedora-linkd  (daemon)      │
│ KDE Connect  │         local network           │  discovery · pairing ·       │
└──────────────┘                                 │  plugins                     │
                                                 └──────────────┬───────────────┘
                                                                │ D-Bus
                                                                │ org.fedoralink.Daemon1
                                                 ┌──────────────▼───────────────┐
                                                 │  fedora-link  (GTK4 + Adw)   │
                                                 └──────────────────────────────┘
```

| Crate | Role |
|---|---|
| [`crates/core`](crates/core) | Protocol packets, certificates, TLS |
| [`crates/daemon`](crates/daemon) | `fedora-linkd`: discovery, pairing, plugins, D-Bus API |
| [`crates/gui`](crates/gui) | `fedora-link`: GTK4 / libadwaita window, a plain D-Bus client |
| [`packaging`](packaging) | RPM spec, systemd user service, D-Bus activation, firewalld service, Nautilus extension, icon, AppStream metadata |

The daemon runs as a systemd user service (`fedora-link.service`). The interface starts it through
D-Bus activation when needed.

## Development

Build dependencies: `gtk4-devel libadwaita-devel`.

```bash
cargo test --workspace
cargo run -p fedora-link-daemon -- --console   # answer pairing requests in the terminal
cargo run -p fedora-link-gui
```

### Troubleshooting

If you see "Unknown method" or "UDP port 1716 already in use", an old daemon is still running
(D-Bus activation restarts it whenever the GUI is open):

```bash
systemctl --user stop fedora-link.service
pkill -f /usr/bin/fedora-linkd
```

Then start the version you want.

## Project status

Version **0.1.0**. The core (pairing, notifications, clipboard, files, MPRIS) works. Some features
still need to be validated on real hardware: scrcpy audio, PC battery to phone, mobile network,
SFTP, touchpad and volume.

Ideas for later: microphone, webcam (v4l2loopback), Bluetooth speaker.

## License

[GPL-3.0-or-later](LICENSE)
