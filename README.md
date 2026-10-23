# Fedora Link

Application native pour Fedora (GNOME, Wayland) qui relie un téléphone Android au PC, en parlant le
protocole KDE Connect : l'appli **KDE Connect** pour Android sert de client mobile.

Fonctionnalités : notifications (avec réponse rapide), batterie, presse-papiers, envoi/réception de
fichiers, « trouver mon téléphone », recopie d'écran et « Écouter sur le PC » (scrcpy), lecteurs du
téléphone dans le menu multimédia de GNOME (MPRIS). Aucune donnée ne quitte le réseau local.

## Architecture

| Crate | Rôle |
|---|---|
| `crates/core` | paquets du protocole, certificats, TLS |
| `crates/daemon` | `fedora-linkd` : découverte, appairage, plugins, API D-Bus `org.fedoralink.Daemon1` |
| `crates/gui` | `fedora-link` : fenêtre GTK4 / libadwaita, simple client D-Bus |

Le daemon tourne en service systemd utilisateur (`fedora-link.service`) ; l'interface le démarre
par activation D-Bus si besoin.

## Compiler et tester

    cargo test --workspace
    cargo run -p fedora-link-daemon      # --console pour répondre aux appairages au terminal
    cargo run -p fedora-link-gui

Dépendances de build : `gtk4-devel libadwaita-devel`.

## Construire le .rpm

    sudo dnf install rpm-build cargo rust gtk4-devel libadwaita-devel desktop-file-utils appstream
    packaging/build-rpm.sh          # RPMBUILD_ARGS=--nodeps si Rust vient de rustup
    sudo dnf install ~/rpmbuild/RPMS/x86_64/fedora-link-*.rpm

## Pare-feu

Les ports 1714-1764 (TCP et UDP) doivent être ouverts dans la zone du réseau :

    sudo firewall-cmd --permanent --add-service=fedora-link && sudo firewall-cmd --reload

## Appairage

1. Installez KDE Connect sur le téléphone, même réseau Wi-Fi.
2. Ouvrez Fedora Link, choisissez le PC dans l'appli mobile (ou « Ajouter un appareil par IP »).
3. Vérifiez que le même code s'affiche des deux côtés, puis acceptez.

KDE Connect / GSConnect ne doit pas tourner en même temps (mêmes ports).

## Licence

GPL-3.0-or-later.
# fedora-link
# fedora-link
# fedora-link
# fedora-link
# fedora-link
