Name:           fedora-link
Version:        0.1.0
Release:        7%{?dist}
Summary:        Liaison entre un téléphone Android et Fedora (protocole KDE Connect)

# Les dépendances Rust sont embarquées (vendor/) : leurs licences s'ajoutent à celle du projet.
License:        GPL-3.0-or-later
URL:            https://github.com/chamse22ine/fedora-link
Source0:        %{name}-%{version}.tar.gz

# Le débogage Rust est volumineux et inutile ici.
%global debug_package %{nil}

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  systemd-rpm-macros
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  python3

Requires:       gtk4
Requires:       libadwaita
Requires:       dbus-common
Recommends:     firewalld
Recommends:     nautilus-python
Recommends:     gvfs
Recommends:     openssh-clients
Recommends:     pipewire-pulseaudio
Recommends:     xdg-desktop-portal-gnome
Recommends:     scrcpy
Recommends:     android-tools

%description
Fedora Link affiche les notifications du téléphone sur le bureau GNOME, partage
le presse-papiers, échange des fichiers, fait sonner le téléphone et pilote sa
recopie d'écran. Il parle le protocole KDE Connect : l'application KDE Connect
Android sert de client mobile. Un daemon utilisateur garde le lien même fenêtre
fermée ; l'interface libadwaita s'y connecte par D-Bus.

%prep
%autosetup -n %{name}-%{version}

%build
export CARGO_HOME=%{_builddir}/.cargo
cargo build --release --locked --offline

%install
install -Dpm 0755 target/release/fedora-linkd %{buildroot}%{_bindir}/fedora-linkd
install -Dpm 0755 target/release/fedora-link  %{buildroot}%{_bindir}/fedora-link
install -Dpm 0644 packaging/fedora-link.service %{buildroot}%{_userunitdir}/fedora-link.service
install -Dpm 0644 packaging/90-fedora-link.preset %{buildroot}%{_prefix}/lib/systemd/user-preset/90-fedora-link.preset
install -Dpm 0644 packaging/org.fedoralink.Daemon.service %{buildroot}%{_datadir}/dbus-1/services/org.fedoralink.Daemon.service
install -Dpm 0644 packaging/fedora-link.xml %{buildroot}%{_prefix}/lib/firewalld/services/fedora-link.xml
install -Dpm 0644 packaging/org.fedoralink.Gui.desktop %{buildroot}%{_datadir}/applications/org.fedoralink.Gui.desktop
install -Dpm 0644 packaging/org.fedoralink.Gui.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/org.fedoralink.Gui.svg
install -Dpm 0644 packaging/org.fedoralink.Gui.metainfo.xml %{buildroot}%{_metainfodir}/org.fedoralink.Gui.metainfo.xml
install -Dpm 0644 packaging/nautilus/fedora-link-send.py %{buildroot}%{_datadir}/nautilus-python/extensions/fedora-link-send.py

%check
python3 -m py_compile packaging/nautilus/fedora-link-send.py
desktop-file-validate %{buildroot}%{_datadir}/applications/org.fedoralink.Gui.desktop
appstreamcli validate --no-net %{buildroot}%{_metainfodir}/org.fedoralink.Gui.metainfo.xml
cargo test --release --locked --offline --workspace --exclude fedora-link-gui

%post
%systemd_user_post fedora-link.service

%preun
%systemd_user_preun fedora-link.service

%files
%license LICENSE
%doc README.md
%{_bindir}/fedora-linkd
%{_bindir}/fedora-link
%{_userunitdir}/fedora-link.service
%{_prefix}/lib/systemd/user-preset/90-fedora-link.preset
%{_datadir}/dbus-1/services/org.fedoralink.Daemon.service
%{_prefix}/lib/firewalld/services/fedora-link.xml
%{_datadir}/applications/org.fedoralink.Gui.desktop
%{_datadir}/icons/hicolor/scalable/apps/org.fedoralink.Gui.svg
%{_metainfodir}/org.fedoralink.Gui.metainfo.xml
%{_datadir}/nautilus-python/extensions/fedora-link-send.py

%changelog
* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-7
- Nouvelle interface ; réseau mobile et batterie du PC ; lecteurs du téléphone dans GNOME (MPRIS) ; « Écouter sur le PC » (scrcpy audio)

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-6
- Le téléphone pilote le PC : pavé tactile et clavier, volume, lecteurs, commandes à distance, pointeur de présentation

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-5
- Liste des fichiers reçus avec aperçu, parcourir le téléphone en SFTP (gvfs)

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-4
- Contacts (cache vCard synchronisé) et page Messages (SMS/MMS, notification de nouveau message)

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-3
- Partage de liens et de texte, appels entrants, menu clic droit Nautilus

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-2
- Contrôle multimédia (lecture, volume, pochette), reconnexion automatique, recopie d'écran Wi-Fi

* Wed Oct 21 2026 chamse22ine <124091776+chamse22ine@users.noreply.github.com> - 0.1.0-1
- Première version : daemon, interface, plugins MVP, recopie d'écran via scrcpy
