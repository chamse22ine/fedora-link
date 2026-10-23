//! Fenêtre libadwaita : barre latérale + pages (tableau de bord, notifications, multimédia,
//! fichiers, écran, paramètres). Tout passe par le client D-Bus ; aucune logique réseau ici.

use crate::client::{service_wanted, systemctl, Client, Device, ListenStatus, SERVICE};
use crate::cover_color;
use crate::messages::Messages;
use crate::presenter::Presenter;
use crate::widgets::{a11y, icon_bubble, phone_art, round_button, BatteryRing};
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsStr;
use std::future::Future;
use std::rc::Rc;

/// Point d'entrée de la traduction : remplacé par gettext quand les catalogues existeront.
fn t(s: &str) -> String {
    s.to_owned()
}

fn spawn(f: impl Future<Output = ()> + 'static) {
    glib::MainContext::default().spawn_local(f);
}

struct Page {
    name: &'static str,
    title: &'static str,
    icon: &'static str,
    soon: bool,
}

const fn page(name: &'static str, title: &'static str, icon: &'static str) -> Page {
    Page {
        name,
        title,
        icon,
        soon: false,
    }
}

const PAGES: [Page; 7] = [
    page("dashboard", "Tableau de bord", "phone-symbolic"),
    page(
        "notifications",
        "Notifications",
        "preferences-system-notifications-symbolic",
    ),
    page("messages", "Messages", "chat-bubble-text-symbolic"),
    page("media", "Multimédia", "multimedia-player-symbolic"),
    page("files", "Fichiers", "folder-symbolic"),
    page("mirror", "Écran du téléphone", "video-display-symbolic"),
    page("settings", "Paramètres", "emblem-system-symbolic"),
];

fn plugin_label(name: &str) -> &str {
    match name {
        "ping" => "Ping",
        "battery" => "Batterie du téléphone et du PC",
        "connectivity" => "Réseau mobile du téléphone",
        "notification" => "Notifications",
        "clipboard" => "Presse-papiers partagé",
        "findmyphone" => "Trouver mon téléphone",
        "share" => "Partage de fichiers, liens et texte",
        "telephony" => "Appels entrants",
        "contacts" => "Contacts du téléphone",
        "sms" => "SMS et MMS",
        "sftp" => "Parcourir les fichiers du téléphone",
        "mousepad" => "Pavé tactile et clavier à distance",
        "systemvolume" => "Volume du PC",
        "mprispc" => "Lecteurs du PC",
        "runcommand" => "Commandes à distance",
        "presenter" => "Télécommande de présentation",
        "mpris" => "Contrôle multimédia",
        other => other,
    }
}

fn plugin_icon(name: &str) -> &'static str {
    match name {
        "battery" => "battery-symbolic",
        "connectivity" => "network-cellular-signal-good-symbolic",
        "notification" => "preferences-system-notifications-symbolic",
        "clipboard" => "edit-paste-symbolic",
        "findmyphone" => "alarm-symbolic",
        "share" => "document-send-symbolic",
        "mpris" => "multimedia-player-symbolic",
        "telephony" => "call-start-symbolic",
        "contacts" => "avatar-default-symbolic",
        "sms" => "chat-bubble-text-symbolic",
        "sftp" => "folder-remote-symbolic",
        "mousepad" => "input-mouse-symbolic",
        "systemvolume" => "audio-volume-high-symbolic",
        "mprispc" => "multimedia-player-symbolic",
        "runcommand" => "utilities-terminal-symbolic",
        "presenter" => "view-presentation-symbolic",
        "ping" => "network-transmit-receive-symbolic",
        _ => "applications-system-symbolic",
    }
}

/// Ligne de la section « Récemment ».
#[derive(Clone)]
struct Recent {
    icon: &'static str,
    /// Image à afficher à la place de l'icône (fichiers reçus).
    preview: Option<String>,
    text: String,
    action_label: &'static str,
    action: RecentAction,
}

#[derive(Clone)]
enum RecentAction {
    OpenFile(String),
    OpenUrl(String),
    Page(&'static str),
}

/// Widgets du tableau de bord.
struct Dash {
    stack: gtk::Stack,
    dot: gtk::Box,
    status: gtk::Label,
    name: gtk::Label,
    sub: gtk::Label,
    net: gtk::Label,
    ring: BatteryRing,
    ring_label: gtk::Box,
    empty: adw::StatusPage,
    discovered: gtk::Box,
    ring_tile: gtk::Button,
    file_tile: gtk::Button,
    link_tile: gtk::Button,
    clip_tile: gtk::Button,
    mirror_tile: gtk::Button,
    recent: gtk::Box,
}

/// Carte « Écouter sur le PC » de la page Multimédia.
struct ListenCard {
    root: gtk::Box,
    status: gtk::Label,
    button: gtk::Button,
}

/// Carte du lecteur actif, mise à jour sur place.
struct Hero {
    root: gtk::Box,
    cover: gtk::Picture,
    source: gtk::Label,
    title: gtk::Label,
    subtitle: gtk::Label,
    progress_box: gtk::Box,
    progress: gtk::ProgressBar,
    elapsed: gtk::Label,
    total: gtk::Label,
    previous: gtk::Button,
    play: gtk::Button,
    next: gtk::Button,
    volume: gtk::Scale,
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    messages: Rc<Messages>,
    root: gtk::Stack,
    split: adw::NavigationSplitView,
    content_page: adw::NavigationPage,
    stack: gtk::Stack,
    nav: gtk::ListBox,
    notif_badge: gtk::Label,
    client: RefCell<Option<Client>>,
    active: RefCell<Option<Device>>,
    dash: Dash,
    recent: RefCell<VecDeque<Recent>>,
    notif_box: gtk::Box,
    clear_all: gtk::Button,
    browse: gtk::Button,
    received_section: gtk::Box,
    received_list: gtk::ListBox,
    transfers_section: gtk::Box,
    transfers_list: gtk::ListBox,
    transfers: RefCell<HashMap<String, (adw::ActionRow, gtk::ProgressBar)>>,
    mirror_entry: adw::EntryRow,
    settings_group: adw::PreferencesGroup,
    settings_rows: RefCell<Vec<adw::PreferencesRow>>,
    commands_group: adw::PreferencesGroup,
    commands_rows: RefCell<Vec<gtk::Widget>>,
    presenter: Rc<Presenter>,
    devices_group: adw::PreferencesGroup,
    devices_rows: RefCell<Vec<adw::PreferencesRow>>,
    media_hero: Hero,
    media_others: gtk::Box,
    media_empty: adw::StatusPage,
    listen: ListenCard,
    hero_player: Rc<RefCell<String>>,
    cover_provider: gtk::CssProvider,
    cover_path: RefCell<Option<String>>,
}

fn page_scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .margin_top(8)
        .margin_bottom(32)
        .margin_start(32)
        .margin_end(32)
        .child(child)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    scroller.add_css_class("fl-page");
    scroller
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn label(text: &str, css: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .build();
    for c in css {
        l.add_css_class(c);
    }
    l
}

fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

fn card() -> gtk::Box {
    let c = vbox(0);
    c.add_css_class("fl-card");
    c
}

fn is_image(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        ext.as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
    )
}

/// Aperçu carré d'une image, sinon une pastille d'icône selon le type de fichier.
fn file_thumbnail(path: &str) -> gtk::Widget {
    if is_image(path) {
        let pic = gtk::Picture::for_filename(path);
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_size_request(48, 48);
        pic.set_overflow(gtk::Overflow::Hidden);
        pic.add_css_class("fl-thumb");
        pic.set_valign(gtk::Align::Center);
        return pic.upcast();
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let icon = match ext.as_str() {
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "video-x-generic-symbolic",
        "mp3" | "ogg" | "flac" | "wav" | "m4a" | "opus" => "audio-x-generic-symbolic",
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ppt" | "pptx" => {
            "x-office-document-symbolic"
        }
        "zip" | "7z" | "rar" | "tar" | "gz" => "package-x-generic-symbolic",
        _ => "text-x-generic-symbolic",
    };
    icon_bubble(icon, false).upcast()
}

fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

fn link_button(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.add_css_class("fl-link");
    b.set_valign(gtk::Align::Center);
    b
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    round_button(icon, tooltip)
}

fn action_row(title: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_prefix(&icon_bubble(icon, false));
    row
}

fn tile(icon: &str, text: &str) -> gtk::Button {
    let column = vbox(10);
    column.set_halign(gtk::Align::Center);
    column.append(&icon_bubble(icon, false));
    let l = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    l.add_css_class("fl-tile-label");
    column.append(&l);
    let b = gtk::Button::builder().child(&column).hexpand(true).build();
    b.add_css_class("fl-tile");
    a11y(&b, text);
    b
}

fn format_time(ms: f64) -> String {
    let secs = (ms / 1000.0).max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl Ui {
    pub fn build(app: &adw::Application) -> Rc<Self> {
        let toasts = adw::ToastOverlay::new();
        let messages = Messages::new({
            let toasts = toasts.clone();
            move |text| toasts.add_toast(adw::Toast::new(text))
        });
        // --- tableau de bord -----------------------------------------------
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("fl-dot");
        dot.set_valign(gtk::Align::Center);
        let status = label("", &["fl-hero-status"]);
        let status_row = hbox(8);
        status_row.append(&dot);
        status_row.append(&status);
        let name = label("", &["fl-hero-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_width_chars(1);
        let sub = label("", &["fl-hero-sub"]);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sub.set_width_chars(1);
        let texts = vbox(4);
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        texts.append(&status_row);
        texts.append(&name);
        texts.append(&sub);
        let net = label("", &["fl-hero-sub"]);
        net.set_xalign(0.0);
        net.set_visible(false);
        texts.append(&net);
        let ring = BatteryRing::new();
        let ring_label = hbox(0);
        ring_label.append(&ring.root);
        let hero = hbox(28);
        hero.add_css_class("fl-hero");
        hero.append(&phone_art());
        hero.append(&texts);
        hero.append(&ring_label);

        let ring_tile = tile("alarm-symbolic", &t("Faire sonner"));
        let file_tile = tile("document-send-symbolic", &t("Envoyer un fichier"));
        let link_tile = tile("insert-link-symbolic", &t("Envoyer un lien"));
        let clip_tile = tile("edit-paste-symbolic", &t("Presse-papiers"));
        let mirror_tile = tile("video-display-symbolic", &t("Recopier l'écran"));
        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .column_homogeneous(true)
            .valign(gtk::Align::Start)
            .build();
        for (i, tl) in [&ring_tile, &file_tile, &link_tile, &clip_tile, &mirror_tile]
            .into_iter()
            .enumerate()
        {
            grid.attach(tl, i as i32, 0, 1, 1);
        }
        let recent = card();

        let device_view = vbox(20);
        device_view.append(&hero);
        device_view.append(&grid);
        device_view.append(&label(&t("Récemment"), &["fl-section-title"]));
        device_view.append(&recent);

        let search = gtk::Button::with_label(&t("Rechercher"));
        search.add_css_class("pill");
        search.add_css_class("suggested-action");
        search.set_halign(gtk::Align::Center);
        let empty = adw::StatusPage::builder()
            .icon_name("phone-symbolic")
            .title(t("Aucun téléphone détecté"))
            .description(t("Ouvrez KDE Connect sur le téléphone, sur le même Wi-Fi"))
            .child(&search)
            .build();
        let discovered = vbox(8);
        let empty_view = vbox(8);
        empty_view.append(&empty);
        empty_view.append(&discovered);
        empty_view.append(&samsung_tip());

        let dash_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .build();
        dash_stack.add_named(&device_view, Some("device"));
        dash_stack.add_named(&empty_view, Some("empty"));
        dash_stack.set_visible_child_name("empty");

        // --- notifications ---------------------------------------------------
        let clear_all = gtk::Button::with_label(&t("Tout effacer"));
        clear_all.add_css_class("fl-grey");
        clear_all.set_halign(gtk::Align::End);
        clear_all.set_visible(false);
        let notif_box = vbox(20);
        let notif_page = vbox(12);
        notif_page.append(&clear_all);
        notif_page.append(&notif_box);

        // --- multimédia ------------------------------------------------------
        let media_hero = build_hero();
        let media_others = vbox(8);
        let media_empty = adw::StatusPage::builder()
            .icon_name("multimedia-player-symbolic")
            .title(t("Aucun lecteur actif sur le téléphone"))
            .build();
        let media_page = vbox(16);
        media_page.append(&media_hero.root);
        media_page.append(&media_others);
        media_page.append(&media_empty);
        let listen = build_listen_card();
        media_page.append(&listen.root);
        media_hero.root.set_visible(false);

        // --- fichiers --------------------------------------------------------
        let transfers_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        transfers_list.add_css_class("boxed-list");
        let drop_hint = adw::StatusPage::builder()
            .icon_name("folder-download-symbolic")
            .title(t("Glissez un fichier ici"))
            .description(t("Il sera envoyé au téléphone appairé."))
            .build();
        drop_hint.add_css_class("compact");
        let choose = gtk::Button::with_label(&t("Choisir un fichier…"));
        choose.add_css_class("pill");
        choose.add_css_class("suggested-action");
        choose.set_halign(gtk::Align::Center);
        let browse = gtk::Button::with_label(&t("Parcourir le téléphone"));
        browse.add_css_class("pill");
        browse.set_halign(gtk::Align::Center);
        browse.set_tooltip_text(Some(&t("Ouvre les fichiers du téléphone dans le gestionnaire de fichiers")));
        let files = vbox(18);
        files.append(&drop_hint);
        let file_buttons = hbox(12);
        file_buttons.set_halign(gtk::Align::Center);
        file_buttons.append(&choose);
        file_buttons.append(&browse);
        files.append(&file_buttons);
        let transfers_section = vbox(8);
        transfers_section.append(&label(&t("Transferts"), &["fl-section-title"]));
        transfers_section.append(&transfers_list);
        transfers_section.set_visible(false);
        files.append(&transfers_section);
        let received_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        received_list.add_css_class("boxed-list");
        let received_section = vbox(8);
        received_section.append(&label(&t("Reçus du téléphone"), &["fl-section-title"]));
        received_section.append(&received_list);
        received_section.set_visible(false);
        files.append(&received_section);

        // --- écran du téléphone ----------------------------------------------
        let mirror_status = action_row(&t("scrcpy"), "video-display-symbolic");
        let usb_button = gtk::Button::with_label(&t("Lancer par USB"));
        usb_button.set_valign(gtk::Align::Center);
        usb_button.add_css_class("fl-grey");
        let wifi_entry = adw::EntryRow::builder()
            .title(t("Adresse du téléphone (IP:port)"))
            .build();
        let prepare_button = gtk::Button::with_label(&t("Activer le Wi-Fi"));
        prepare_button.set_valign(gtk::Align::Center);
        prepare_button.add_css_class("fl-grey");
        prepare_button.set_tooltip_text(Some(&t(
            "Téléphone branché en USB : active adb sur le port 5555 (une seule fois)",
        )));
        wifi_entry.add_suffix(&prepare_button);
        let wifi_button = gtk::Button::with_label(&t("Connecter et lancer"));
        wifi_button.set_valign(gtk::Align::Center);
        wifi_button.add_css_class("suggested-action");
        wifi_entry.add_suffix(&wifi_button);
        let mirror_group = adw::PreferencesGroup::builder()
            .title(t("Recopie d'écran"))
            .description(t("Utilise scrcpy et adb. Activez le débogage USB dans les options pour développeurs. En partage de connexion : branchez le téléphone une fois, cliquez sur « Activer le Wi-Fi », puis débranchez."))
            .build();
        mirror_group.add(&mirror_status);
        let usb_row = action_row(&t("Téléphone branché en USB"), "media-removable-symbolic");
        usb_row.add_suffix(&usb_button);
        mirror_group.add(&usb_row);
        mirror_group.add(&wifi_entry);
        let mirror_page = vbox(18);
        mirror_page.append(&mirror_group);

        // --- paramètres ------------------------------------------------------
        let devices_group = adw::PreferencesGroup::builder()
            .title(t("Appareils"))
            .build();
        let settings_group = adw::PreferencesGroup::builder()
            .title(t("Fonctionnalités"))
            .build();
        let settings_page = vbox(24);
        settings_page.append(&devices_group);
        settings_page.append(&settings_group);
        let commands_group = adw::PreferencesGroup::builder()
            .title(t("Commandes à distance"))
            .description(t("Le téléphone ne peut lancer que les commandes de cette liste."))
            .build();
        settings_page.append(&commands_group);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&page_scroller(&dash_stack), Some("dashboard"));
        stack.add_named(&page_scroller(&notif_page), Some("notifications"));
        stack.add_named(&page_scroller(&media_page), Some("media"));
        stack.add_named(&messages.root, Some("messages"));
        stack.add_named(&page_scroller(&files), Some("files"));
        stack.add_named(&page_scroller(&mirror_page), Some("mirror"));
        stack.add_named(&page_scroller(&settings_page), Some("settings"));

        // --- barre latérale -----------------------------------------------
        let nav = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        nav.add_css_class("fl-nav");
        let notif_badge = gtk::Label::new(None);
        notif_badge.add_css_class("fl-badge");
        notif_badge.set_visible(false);
        for p in &PAGES {
            let content = hbox(12);
            content.append(&gtk::Image::from_icon_name(p.icon));
            let title = gtk::Label::builder()
                .label(t(p.title))
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            content.append(&title);
            if p.name == "notifications" {
                notif_badge.set_valign(gtk::Align::Center);
                content.append(&notif_badge);
            }
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&content));
            if p.soon {
                let tag = gtk::Label::new(Some(&t("bientôt")));
                tag.add_css_class("fl-soon-tag");
                tag.set_valign(gtk::Align::Center);
                content.append(&tag);
                row.add_css_class("soon");
                row.set_selectable(false);
                row.set_activatable(false);
            }
            nav.append(&row);
        }

        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.add_css_class("fl-header");
        sidebar_header.set_show_start_title_buttons(false);
        sidebar_header.set_show_end_title_buttons(false);
        sidebar_header.set_title_widget(Some(&label("Fedora Link", &["fl-sidebar-title"])));
        let app_menu = gtk::gio::Menu::new();
        app_menu.append(Some(&t("Écouter sur le PC")), Some("app.listen"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&app_menu)
            .tooltip_text(t("Menu"))
            .build();
        a11y(&menu_button, &t("Menu"));
        sidebar_header.pack_end(&menu_button);
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_css_class("fl-sidebar");
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.set_content(Some(&nav));
        let sidebar_page = adw::NavigationPage::builder()
            .title("Fedora Link")
            .child(&sidebar_view)
            .build();

        // Contenu : pas de titre, seulement le bouton fermer à droite.
        let content_header = adw::HeaderBar::new();
        content_header.add_css_class("fl-header");
        content_header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        content_header.set_decoration_layout(Some(":close"));
        let content_view = adw::ToolbarView::new();
        content_view.add_css_class("fl-page");
        content_view.add_top_bar(&content_header);
        content_view.set_content(Some(&stack));
        let content_page = adw::NavigationPage::builder()
            .title(t(PAGES[0].title))
            .child(&content_view)
            .build();

        let split = adw::NavigationSplitView::new();
        split.set_min_sidebar_width(232.0);
        split.set_max_sidebar_width(232.0);
        split.set_sidebar_width_fraction(0.0);
        split.set_sidebar(Some(&sidebar_page));
        split.set_content(Some(&content_page));

        // --- état « daemon injoignable » -----------------------------------
        let offline = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title(t("Fedora Link est en veille"))
            .description(t("Le service d'arrière-plan est arrêté : plus de notifications ni de liaison avec le téléphone, et aucune batterie consommée. La fenêtre se reconnectera seule dès qu'il redémarre."))
            .build();
        let wake = gtk::Button::with_label(&t("Réactiver"));
        wake.add_css_class("pill");
        wake.add_css_class("suggested-action");
        wake.set_halign(gtk::Align::Center);
        wake.connect_clicked(|b| {
            b.set_sensitive(false);
            let b = b.clone();
            spawn(async move {
                systemctl(&["enable", "--now", SERVICE]).await;
                b.set_sensitive(true);
            });
        });
        offline.set_child(Some(&wake));
        let root = gtk::Stack::new();
        root.add_named(&split, Some("main"));
        root.add_named(&offline, Some("offline"));
        root.set_visible_child_name("offline");

        toasts.set_child(Some(&root));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Fedora Link")
            .default_width(940)
            .default_height(660)
            .content(&toasts)
            .build();
        window.add_css_class("fl-window");

        // Petites largeurs : vue unique, et la barre latérale reprend le bouton fermer.
        if let Ok(cond) = adw::BreakpointCondition::parse("max-width: 640sp") {
            let bp = adw::Breakpoint::new(cond);
            bp.add_setter(&split, "collapsed", Some(&true.to_value()));
            bp.add_setter(
                &sidebar_header,
                "show-end-title-buttons",
                Some(&true.to_value()),
            );
            window.add_breakpoint(bp);
        }

        // Couleur de la pochette : fournisseur dédié, repli bleu nuit.
        let cover_provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &cover_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }

        let ui = Rc::new(Self {
            window,
            toasts,
            messages,
            root,
            split,
            content_page,
            stack,
            nav: nav.clone(),
            notif_badge,
            client: RefCell::new(None),
            active: RefCell::new(None),
            dash: Dash {
                stack: dash_stack,
                dot,
                status,
                name,
                sub,
                net,
                ring,
                ring_label,
                empty,
                discovered,
                ring_tile,
                file_tile,
                link_tile,
                clip_tile,
                mirror_tile,
                recent,
            },
            recent: RefCell::new(VecDeque::new()),
            notif_box,
            clear_all: clear_all.clone(),
            browse,
            received_section,
            received_list,
            transfers_section,
            transfers_list,
            transfers: RefCell::new(HashMap::new()),
            mirror_entry: wifi_entry.clone(),
            settings_group,
            settings_rows: RefCell::new(Vec::new()),
            commands_group,
            commands_rows: RefCell::new(Vec::new()),
            presenter: Presenter::new(),
            devices_group,
            devices_rows: RefCell::new(Vec::new()),
            media_hero,
            media_others,
            media_empty,
            listen,
            hero_player: Rc::new(RefCell::new(String::new())),
            cover_provider,
            cover_path: RefCell::new(None),
        });

        ui.set_cover_css(None);
        ui.render_recent();
        ui.wire(&nav, &clear_all, &choose, &search);
        ui.wire_media();
        ui.listen.button.connect_clicked({
            let ui = ui.clone();
            move |_| ui.toggle_listen()
        });
        ui.wire_mirror(
            &mirror_status,
            &usb_button,
            &wifi_button,
            &wifi_entry,
            &prepare_button,
        );
        ui.connect_daemon();
        ui.setup_clipboard();
        nav.select_row(nav.row_at_index(0).as_ref());
        ui
    }

    /// Ouvre une conversation (demandé par la notification d'un nouveau SMS).
    pub fn open_thread(&self, thread: i64) {
        self.navigate("messages");
        self.messages.open_thread(thread);
    }

    fn navigate(&self, name: &str) {
        if let Some(i) = PAGES.iter().position(|p| p.name == name) {
            self.nav
                .select_row(self.nav.row_at_index(i as i32).as_ref());
            self.split.set_show_content(true);
        }
    }

    // ---- câblage des widgets ---------------------------------------------

    fn wire(
        self: &Rc<Self>,
        sidebar: &gtk::ListBox,
        clear_all: &gtk::Button,
        choose: &gtk::Button,
        search: &gtk::Button,
    ) {
        let ui = self.clone();
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let p = &PAGES[row.index() as usize];
            ui.stack.set_visible_child_name(p.name);
            ui.content_page.set_title(&t(p.title));
        });
        let ui = self.clone();
        sidebar.connect_row_activated(move |_, _| ui.split.set_show_content(true));

        let ui = self.clone();
        search.connect_clicked(move |_| {
            ui.toast(&t("Recherche en cours…"));
            ui.refresh_all();
        });

        let ui = self.clone();
        self.dash
            .ring_tile
            .connect_clicked(move |_| ui.ring_phone());
        let ui = self.clone();
        self.dash.file_tile.connect_clicked(move |_| ui.pick_file());
        let ui = self.clone();
        self.dash
            .mirror_tile
            .connect_clicked(move |_| ui.navigate("mirror"));
        let ui = self.clone();
        self.dash.link_tile.connect_clicked(move |_| ui.ask_link());
        let ui = self.clone();
        choose.connect_clicked(move |_| ui.pick_file());
        let ui = self.clone();
        self.browse.connect_clicked(move |b| {
            b.set_sensitive(false);
            let (ui, b) = (ui.clone(), b.clone());
            spawn(async move {
                ui.browse_phone().await;
                b.set_sensitive(true);
            });
        });

        let ui = self.clone();
        self.dash.clip_tile.connect_clicked(move |_| {
            let ui = ui.clone();
            spawn(async move {
                let clipboard = ui.window.clipboard();
                let text = clipboard.read_text_future().await.ok().flatten();
                match (ui.client(), text) {
                    (Some(c), Some(text)) => {
                        ui.report(c.send_clipboard(&text).await, &t("Presse-papiers envoyé."))
                    }
                    _ => ui.toast(&t("Le presse-papiers ne contient pas de texte.")),
                }
            });
        });

        let ui = self.clone();
        clear_all.connect_clicked(move |_| {
            let ui = ui.clone();
            spawn(async move {
                let (Some(c), Some(d)) = (ui.client(), ui.active_device()) else {
                    return;
                };
                for n in c.notifications(&d.id).await.unwrap_or_default() {
                    if let Some(id) = n["id"].as_str() {
                        let _ = c.dismiss_notification(&d.id, id).await;
                    }
                }
                ui.refresh_notifications().await;
            });
        });

        // Glisser-déposer d'un fichier n'importe où dans la fenêtre.
        let drop = gtk::DropTarget::new(gtk::gio::File::static_type(), gtk::gdk::DragAction::COPY);
        let ui = self.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(file) = value.get::<gtk::gio::File>() else {
                return false;
            };
            ui.send_file(file);
            true
        });
        self.window.add_controller(drop);
    }

    /// « Faire sonner » : toast avec « Arrêter » (un second signal arrête la sonnerie côté téléphone).
    fn ring_phone(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            let (Some(c), Some(d)) = (ui.client(), ui.active_device()) else {
                return;
            };
            match c.ring_phone(&d.id).await {
                Err(e) => ui.toast(e.message()),
                Ok(()) => {
                    let toast = adw::Toast::new(&t("Le téléphone sonne"));
                    toast.set_button_label(Some(&t("Arrêter")));
                    let ui2 = ui.clone();
                    toast.connect_button_clicked(move |_| {
                        let ui = ui2.clone();
                        spawn(async move {
                            if let (Some(c), Some(d)) = (ui.client(), ui.active_device()) {
                                ui.report(c.ring_phone(&d.id).await, "");
                            }
                        });
                    });
                    ui.toasts.add_toast(toast);
                }
            }
        });
    }

    // ---- recopie d'écran (scrcpy) ------------------------------------------

    fn wire_mirror(
        self: &Rc<Self>,
        status: &adw::ActionRow,
        usb: &gtk::Button,
        wifi: &gtk::Button,
        entry: &adw::EntryRow,
        prepare: &gtk::Button,
    ) {
        let installed = glib::find_program_in_path("scrcpy").is_some();
        let adb = glib::find_program_in_path("adb").is_some();
        status.set_subtitle(&match (installed, adb) {
            (true, true) => t("Prêt"),
            (false, _) => t("scrcpy est introuvable : installez-le (paquet, COPR ou Flatpak)."),
            (_, false) => t("adb est introuvable : sudo dnf install android-tools"),
        });
        usb.set_sensitive(installed);
        wifi.set_sensitive(installed && adb);
        prepare.set_sensitive(adb);

        // `adb tcpip` : le téléphone écoute sur le port 5555 du réseau local (USB requis une fois).
        let ui = self.clone();
        prepare.connect_clicked(move |_| {
            let ui = ui.clone();
            spawn(async move {
                let process = gtk::gio::Subprocess::newv(
                    &[OsStr::new("adb"), OsStr::new("tcpip"), OsStr::new("5555")],
                    gtk::gio::SubprocessFlags::STDOUT_PIPE | gtk::gio::SubprocessFlags::STDERR_MERGE,
                );
                let output = match process {
                    Ok(p) => p.communicate_utf8_future(None::<String>).await.ok().and_then(|(out, _)| out),
                    Err(e) => return ui.toast(e.message()),
                };
                let output = output.map(|s| s.to_string()).unwrap_or_default();
                if output.contains("restarting in TCP mode") {
                    ui.toast(&t("Wi-Fi activé : vous pouvez débrancher et cliquer sur « Connecter et lancer »."));
                } else {
                    ui.toast(&format!("adb : {}", output.trim()));
                }
            });
        });

        let ui = self.clone();
        usb.connect_clicked(move |_| {
            let ui = ui.clone();
            spawn(async move { ui.start_mirror(None).await });
        });
        let (ui, entry) = (self.clone(), entry.clone());
        wifi.connect_clicked(move |_| {
            let addr = entry.text().trim().to_owned();
            let ui = ui.clone();
            spawn(async move {
                if addr.is_empty() {
                    ui.toast(&t(
                        "Saisissez l'adresse affichée dans « Débogage sans fil ».",
                    ));
                } else {
                    ui.start_mirror(Some(addr)).await;
                }
            });
        });
    }

    /// Lance scrcpy, après `adb connect` pour le mode sans fil.
    async fn start_mirror(self: &Rc<Self>, wifi_addr: Option<String>) {
        use gtk::gio::{Subprocess, SubprocessFlags};
        if let Some(addr) = &wifi_addr {
            let adb = Subprocess::newv(
                &[OsStr::new("adb"), OsStr::new("connect"), OsStr::new(addr)],
                SubprocessFlags::STDOUT_PIPE | SubprocessFlags::STDERR_MERGE,
            );
            let output = match adb {
                Ok(p) => p
                    .communicate_utf8_future(None::<String>)
                    .await
                    .ok()
                    .and_then(|(out, _)| out),
                Err(e) => return self.toast(e.message()),
            };
            let output = output.map(|s| s.to_string()).unwrap_or_default();
            if !output.contains("connected to") {
                return self.toast(&format!("adb : {}", output.trim()));
            }
        }
        let mut args = vec!["scrcpy".to_owned(), "--window-title=Fedora Link".to_owned()];
        if let Some(addr) = wifi_addr {
            args.push(format!("--serial={addr}"));
        }
        let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        match Subprocess::newv(&argv, SubprocessFlags::STDERR_PIPE) {
            Ok(process) => {
                self.toast(&t("Recopie d'écran lancée."));
                if let Ok((_, stderr)) = process.communicate_utf8_future(None::<String>).await {
                    if !process.is_successful() {
                        let detail = stderr.map(|s| s.to_string()).unwrap_or_default();
                        let last = detail.trim().lines().last().unwrap_or("").to_owned();
                        self.toast(&format!("scrcpy : {last}"));
                    }
                }
            }
            Err(e) => self.toast(e.message()),
        }
    }

    fn pick_file(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            let dialog = gtk::FileDialog::new();
            if let Ok(file) = dialog.open_future(Some(&ui.window)).await {
                ui.send_file(file);
            }
        });
    }

    fn send_file(self: &Rc<Self>, file: gtk::gio::File) {
        let ui = self.clone();
        spawn(async move {
            let (Some(c), Some(d)) = (ui.client(), ui.active_device()) else {
                ui.toast(&t("Aucun téléphone appairé et connecté."));
                return;
            };
            let Some(path) = file.path() else { return };
            ui.report(
                c.send_file(&d.id, &path.to_string_lossy()).await,
                &t("Envoi démarré."),
            );
            ui.navigate("files");
        });
    }

    // ---- connexion au daemon ---------------------------------------------

    fn client(&self) -> Option<Client> {
        self.client.borrow().clone()
    }

    fn active_device(&self) -> Option<Device> {
        self.active.borrow().clone()
    }

    /// Nom affiché d'un appareil pour les messages (celui du téléphone actif, sinon un terme neutre).
    fn device_name(&self, id: &str) -> String {
        self.active_device()
            .filter(|d| d.id == id)
            .map_or_else(|| t("votre téléphone"), |d| d.name)
    }

    /// « Envoyer un lien » : demande l'adresse (pré-remplie si le presse-papiers en contient une).
    fn ask_link(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            let entry = gtk::Entry::builder()
                .placeholder_text("https://")
                .activates_default(true)
                .input_purpose(gtk::InputPurpose::Url)
                .build();
            if let Some(text) = ui
                .window
                .clipboard()
                .read_text_future()
                .await
                .ok()
                .flatten()
            {
                let text = text.trim();
                if text.starts_with("https://") || text.starts_with("http://") {
                    entry.set_text(text);
                }
            }
            let dialog = adw::AlertDialog::new(
                Some(&t("Envoyer un lien")),
                Some(&t("Le lien s'ouvrira dans le navigateur du téléphone.")),
            );
            dialog.set_extra_child(Some(&entry));
            dialog.add_responses(&[("cancel", &t("Annuler")), ("send", &t("Envoyer"))]);
            dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("send"));
            dialog.set_close_response("cancel");
            if dialog.choose_future(Some(&ui.window)).await != "send" {
                return;
            }
            let (Some(c), Some(d)) = (ui.client(), ui.active_device()) else {
                return ui.toast(&t("Aucun téléphone appairé et connecté."));
            };
            ui.report(
                c.share_url(&d.id, entry.text().trim()).await,
                &t("Lien envoyé."),
            );
        });
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn report(&self, result: Result<(), glib::Error>, ok: &str) {
        match result {
            Ok(()) if ok.is_empty() => {}
            Ok(()) => self.toast(ok),
            Err(e) => self.toast(e.message()),
        }
    }

    fn connect_daemon(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            match Client::connect(service_wanted().await).await {
                Ok(client) => {
                    let u = ui.clone();
                    client.on_owner_changed(move |_| u.sync_daemon_state());
                    let u = ui.clone();
                    client.on_signal(move |name, params| u.on_signal(name, params));
                    *ui.client.borrow_mut() = Some(client);
                    ui.sync_daemon_state();
                }
                Err(e) => tracing::error!("D-Bus : {e}"),
            }
        });
    }

    fn sync_daemon_state(self: &Rc<Self>) {
        let running = self.client().is_some_and(|c| c.is_running());
        self.root
            .set_visible_child_name(if running { "main" } else { "offline" });
        if running {
            self.refresh_all();
        }
    }

    fn refresh_all(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            ui.refresh_received().await;
            ui.refresh_devices().await;
            ui.refresh_notifications().await;
            ui.refresh_media().await;
            ui.refresh_listen().await;
            ui.refresh_settings().await;
        });
    }

    fn on_signal(self: &Rc<Self>, name: &str, params: &glib::Variant) {
        let s = |i: usize| params.child_value(i).str().unwrap_or("").to_owned();
        let ui = self.clone();
        match name {
            "DeviceChanged" => spawn(async move {
                ui.refresh_devices().await;
                ui.refresh_notifications().await;
                ui.refresh_media().await;
                ui.refresh_settings().await;
            }),
            "MediaChanged" => spawn(async move { ui.refresh_media().await }),
            "ListenChanged" => {
                let status = ListenStatus { state: s(0), detail: s(1) };
                self.show_listen(&status);
                if status.state == "error" {
                    self.toast(&format!("{} {}", t("Écoute impossible :"), status.detail));
                }
            }
            "NotificationPosted" => {
                if let Ok(n) = serde_json::from_str::<serde_json::Value>(&s(1)) {
                    let app = n["appName"].as_str().unwrap_or("");
                    let title = n["title"].as_str().unwrap_or("");
                    let text = [app, title]
                        .iter()
                        .filter(|p| !p.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    self.push_recent(Recent {
                        preview: None,
                        icon: "preferences-system-notifications-symbolic",
                        text,
                        action_label: "Voir",
                        action: RecentAction::Page("notifications"),
                    });
                }
                spawn(async move { ui.refresh_notifications().await })
            }
            "NotificationDismissed" => spawn(async move { ui.refresh_notifications().await }),
            "PairingRequested" => {
                let (id, device, code) = (s(0), s(1), s(2));
                spawn(async move { ui.ask_pairing(id, device, code).await });
            }
            "SmsChanged" => {
                let thread = params.child_value(1).get::<i64>().unwrap_or(0);
                spawn(async move { ui.messages.on_sms_changed(thread).await });
            }
            "PresenterMoved" => {
                let dx = params.child_value(0).get::<f64>().unwrap_or(0.0);
                let dy = params.child_value(1).get::<f64>().unwrap_or(0.0);
                self.presenter.moved(dx, dy);
            }
            "PresenterStopped" => self.presenter.stop(),
            "ContactsChanged" => spawn(async move { ui.messages.on_sms_changed(-1).await }),
            "ClipboardReceived" => self.window.clipboard().set_text(&s(1)),
            "LinkReceived" => {
                let url = s(1);
                self.toast(&format!(
                    "{} {}",
                    t("Lien reçu de"),
                    self.device_name(&s(0))
                ));
                self.push_recent(Recent {
                    preview: None,
                    icon: "insert-link-symbolic",
                    text: url.clone(),
                    action_label: "Ouvrir",
                    action: RecentAction::OpenUrl(url),
                });
            }
            "TextReceived" => {
                let text = s(1);
                self.window.clipboard().set_text(&text);
                self.toast(&format!(
                    "{} {} — {}",
                    t("Texte reçu de"),
                    self.device_name(&s(0)),
                    t("copié dans le presse-papiers")
                ));
            }
            "TransferProgress" => {
                let done = params.child_value(2).get::<u64>().unwrap_or(0);
                let total = params.child_value(3).get::<u64>().unwrap_or(0);
                let incoming = params.child_value(4).get::<bool>().unwrap_or(true);
                self.show_progress(&s(1), done, total, incoming);
            }
            "FileReceived" => {
                let ui2 = self.clone();
                spawn(async move { ui2.refresh_received().await });
                let path = s(1);
                let file_name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                self.toast(&format!("{} {file_name}", t("Fichier reçu :")));
                self.push_recent(Recent {
                    preview: Some(path.clone()),
                    icon: "folder-download-symbolic",
                    text: format!("{} {file_name}", t("Fichier reçu :")),
                    action_label: "Ouvrir",
                    action: RecentAction::OpenFile(path),
                });
            }
            _ => {}
        }
    }

    async fn ask_pairing(self: &Rc<Self>, id: String, device: String, code: String) {
        let body = format!(
            "« {device} » {}\n\n{} {code}\n{}",
            t("souhaite s'appairer avec cet ordinateur."),
            t("Code de vérification :"),
            t("Vérifiez que le même code s'affiche sur le téléphone."),
        );
        let dialog = adw::AlertDialog::new(Some(&t("Demande d'appairage")), Some(&body));
        dialog.add_responses(&[("reject", &t("Refuser")), ("accept", &t("Accepter"))]);
        dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("reject"));
        dialog.set_close_response("reject");
        let answer = dialog.choose_future(Some(&self.window)).await;
        if let Some(c) = self.client() {
            self.report(c.answer_pairing(&id, answer == "accept").await, "");
        }
    }

    // ---- tableau de bord -------------------------------------------------

    fn push_recent(self: &Rc<Self>, item: Recent) {
        {
            let mut list = self.recent.borrow_mut();
            list.push_front(item);
            list.truncate(4);
        }
        self.render_recent();
    }

    fn render_recent(self: &Rc<Self>) {
        let recent = &self.dash.recent;
        clear_box(recent);
        let items = self.recent.borrow().clone();
        if items.is_empty() {
            let row = hbox(12);
            row.set_margin_top(14);
            row.set_margin_bottom(14);
            row.set_margin_start(16);
            row.set_margin_end(16);
            let hint = label(
                &t("Rien pour l'instant : fichiers reçus et notifications apparaîtront ici."),
                &["fl-dim"],
            );
            hint.set_wrap(true);
            row.append(&hint);
            recent.append(&row);
            return;
        }
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                recent.append(&separator());
            }
            let row = hbox(12);
            row.set_margin_top(6);
            row.set_margin_bottom(6);
            row.set_margin_start(16);
            row.set_margin_end(8);
            match item.preview.as_deref().filter(|p| is_image(p)) {
                Some(path) => row.append(&file_thumbnail(path)),
                None => row.append(&icon_bubble(item.icon, true)),
            }
            let text = gtk::Label::builder()
                .label(&item.text)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            row.append(&text);
            let link = link_button(&t(item.action_label));
            let ui = self.clone();
            link.connect_clicked(move |_| match &item.action {
                RecentAction::Page(p) => ui.navigate(p),
                RecentAction::OpenUrl(url) => {
                    let launcher = gtk::UriLauncher::new(url);
                    let ui = ui.clone();
                    spawn(async move {
                        if let Err(e) = launcher.launch_future(Some(&ui.window)).await {
                            ui.toast(e.message());
                        }
                    });
                }
                RecentAction::OpenFile(path) => ui.open_received(path.clone()),
            });
            row.append(&link);
            recent.append(&row);
        }
    }

    /// Monte le serveur SFTP du téléphone avec gvfs, puis l'ouvre dans le gestionnaire de fichiers.
    async fn browse_phone(self: &Rc<Self>) {
        let (Some(c), Some(d)) = (self.client(), self.active_device()) else {
            return self.toast(&t("Aucun téléphone appairé et connecté."));
        };
        self.toast(&t("Connexion au téléphone…"));
        let info = match c.browse_phone(&d.id).await {
            Ok(i) => i,
            Err(e) => return self.toast(e.message()),
        };
        let (Some(uri), Some(user), Some(password)) = (
            info["uri"].as_str().map(str::to_owned),
            info["user"].as_str().map(str::to_owned),
            info["password"].as_str().map(str::to_owned),
        ) else {
            return self.toast(&t("Réponse du téléphone incomplète."));
        };

        // Le mot de passe est à usage unique : gvfs le demande, nous le fournissons sans rien afficher.
        let op = gtk::gio::MountOperation::new();
        op.connect_ask_password(move |op, _, _, _, flags| {
            if flags.contains(gtk::gio::AskPasswordFlags::NEED_USERNAME) {
                op.set_username(Some(&user));
            }
            op.set_password(Some(&password));
            op.reply(gtk::gio::MountOperationResult::Handled);
        });
        // Le téléphone change de clé SSH à chaque session : KDE Connect ne la vérifie pas non plus.
        op.connect_local("ask-question", false, |args| {
            if let Ok(op) = args[0].get::<gtk::gio::MountOperation>() {
                op.set_choice(0);
                op.reply(gtk::gio::MountOperationResult::Handled);
            }
            None
        });
        let location = gtk::gio::File::for_uri(&uri);
        match location
            .mount_enclosing_volume_future(gtk::gio::MountMountFlags::NONE, Some(&op))
            .await
        {
            Ok(()) => {}
            Err(e) if e.matches(gtk::gio::IOErrorEnum::AlreadyMounted) => {}
            Err(e) => {
                tracing::warn!("montage SFTP : {}", e.message());
                return self.toast(&format!("{} {}", t("Montage impossible :"), e.message()));
            }
        }
        // Aucune application n'est associée aux liens sftp:// : on passe par le gestionnaire de dossiers.
        let opened = gtk::gio::AppInfo::default_for_type("inode/directory", false)
            .map(|app| app.launch_uris(&[uri.as_str()], None::<&gtk::gio::AppLaunchContext>));
        match opened {
            Some(Ok(())) => {}
            Some(Err(e)) => self.toast(e.message()),
            None => self.toast(&t("Aucun gestionnaire de fichiers trouvé.")),
        }
    }

    /// Liste « Reçus du téléphone » : un clic ouvre le fichier, sans passer par le dossier.
    async fn refresh_received(self: &Rc<Self>) {
        let Some(c) = self.client() else { return };
        let files = c.received_files().await.unwrap_or_default();
        while let Some(row) = self.received_list.first_child() {
            self.received_list.remove(&row);
        }
        self.received_section.set_visible(!files.is_empty());
        for f in files.iter().take(30) {
            let (name, path) = (
                f["name"].as_str().unwrap_or(""),
                f["path"].as_str().unwrap_or(""),
            );
            let when = glib::DateTime::from_unix_local(f["time"].as_i64().unwrap_or(0) / 1000)
                .ok()
                .and_then(|d| d.format("%d/%m %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let size = glib::format_size(f["size"].as_u64().unwrap_or(0));
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(name))
                .subtitle(format!("{size} · {when}"))
                .activatable(true)
                .build();
            row.add_prefix(&file_thumbnail(path));
            let folder = icon_button("folder-open-symbolic", &t("Afficher dans le dossier"));
            folder.set_valign(gtk::Align::Center);
            let (ui, p) = (self.clone(), path.to_owned());
            folder.connect_clicked(move |_| {
                let (ui, file) = (ui.clone(), gtk::gio::File::for_path(&p));
                spawn(async move {
                    let launcher = gtk::FileLauncher::new(Some(&file));
                    if let Err(e) = launcher
                        .open_containing_folder_future(Some(&ui.window))
                        .await
                    {
                        ui.toast(e.message());
                    }
                });
            });
            row.add_suffix(&folder);
            let open = link_button(&t("Ouvrir"));
            let (ui, p) = (self.clone(), path.to_owned());
            open.connect_clicked(move |_| ui.open_received(p.clone()));
            row.add_suffix(&open);
            let (ui, p) = (self.clone(), path.to_owned());
            row.connect_activated(move |_| ui.open_received(p.clone()));
            self.received_list.append(&row);
        }
    }

    /// Images : visionneuse intégrée ; autres fichiers : application par défaut.
    fn open_received(self: &Rc<Self>, path: String) {
        let file = gtk::gio::File::for_path(&path);
        let ui = self.clone();
        if is_image(&path) {
            let picture = gtk::Picture::for_filename(&path);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_can_shrink(true);
            let open_with = gtk::Button::with_label(&t("Ouvrir avec…"));
            open_with.add_css_class("fl-grey");
            let (ui2, file2) = (ui.clone(), file.clone());
            open_with.connect_clicked(move |_| {
                let (ui, file) = (ui2.clone(), file2.clone());
                spawn(async move {
                    if let Err(e) = gtk::FileLauncher::new(Some(&file))
                        .launch_future(Some(&ui.window))
                        .await
                    {
                        ui.toast(e.message());
                    }
                });
            });
            let header = adw::HeaderBar::new();
            header.pack_start(&open_with);
            let view = adw::ToolbarView::new();
            view.add_top_bar(&header);
            view.set_content(Some(&picture));
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dialog = adw::Dialog::builder()
                .title(name)
                .content_width(900)
                .content_height(650)
                .child(&view)
                .build();
            dialog.present(Some(&self.window));
            return;
        }
        spawn(async move {
            if let Err(e) = gtk::FileLauncher::new(Some(&file))
                .launch_future(Some(&ui.window))
                .await
            {
                ui.toast(e.message());
            }
        });
    }

    async fn refresh_devices(self: &Rc<Self>) {
        let Some(c) = self.client() else { return };
        let devices = match c.devices().await {
            Ok(d) => d,
            Err(e) => return self.toast(e.message()),
        };
        let active = devices.iter().find(|d| d.paired && d.connected).cloned();
        let shown = active
            .clone()
            .or_else(|| devices.iter().find(|d| d.paired).cloned());

        let dash = &self.dash;
        for tl in [
            &dash.ring_tile,
            &dash.file_tile,
            &dash.link_tile,
            &dash.clip_tile,
        ] {
            tl.set_sensitive(active.is_some());
        }
        match &shown {
            Some(d) => {
                dash.stack.set_visible_child_name("device");
                if d.connected {
                    dash.dot.add_css_class("online");
                } else {
                    dash.dot.remove_css_class("online");
                }
                dash.status.set_label(&if d.connected {
                    t("Connecté")
                } else {
                    t("Hors ligne")
                });
                dash.name.set_label(&d.name);
                dash.sub.set_label(&match (&d.ip, d.connected) {
                    (Some(ip), true) => format!("{} · {ip}", t("Wi-Fi")),
                    (_, true) => t("Appairé"),
                    _ => t("Appairé · injoignable pour le moment"),
                });
                match network_markup(d) {
                    Some(markup) if d.connected => {
                        dash.net.set_markup(&markup);
                        dash.net.set_visible(true);
                    }
                    _ => dash.net.set_visible(false),
                }
                match d.battery {
                    Some(level) => {
                        dash.ring.set(level, d.charging);
                        dash.ring_label.set_visible(true);
                    }
                    None => dash.ring_label.set_visible(false),
                }
            }
            None => {
                dash.stack.set_visible_child_name("empty");
                let unpaired: Vec<&Device> = devices.iter().filter(|d| !d.paired).collect();
                if unpaired.is_empty() {
                    dash.empty.set_title(&t("Aucun téléphone détecté"));
                    dash.empty.set_description(Some(&t(
                        "Ouvrez KDE Connect sur le téléphone, sur le même Wi-Fi",
                    )));
                } else {
                    dash.empty.set_title(&t("Aucun téléphone appairé"));
                    dash.empty.set_description(Some(&t(
                        "Appairez l'un des appareils détectés ci-dessous.",
                    )));
                }
            }
        }
        clear_box(&dash.discovered);
        for d in devices.iter().filter(|d| !d.paired && d.connected) {
            let row = hbox(12);
            row.add_css_class("fl-card");
            row.set_margin_top(0);
            let inner = hbox(12);
            inner.set_margin_top(10);
            inner.set_margin_bottom(10);
            inner.set_margin_start(16);
            inner.set_margin_end(10);
            inner.set_hexpand(true);
            inner.append(&icon_bubble("phone-symbolic", false));
            let name = gtk::Label::builder()
                .label(&d.name)
                .xalign(0.0)
                .hexpand(true)
                .build();
            inner.append(&name);
            inner.append(&self.pair_button(&c, d));
            row.append(&inner);
            dash.discovered.append(&row);
        }
        *self.active.borrow_mut() = active.clone();
        self.messages
            .set_device(Some(c.clone()), active.as_ref().map(|d| d.id.clone()))
            .await;

        if let Some(ip) = active.as_ref().and_then(|d| d.ip.as_deref()) {
            if self.mirror_entry.text().is_empty() {
                self.mirror_entry.set_text(&format!("{ip}:5555"));
            }
        }
    }

    /// Bouton Appairer / Désappairer d'un appareil.
    fn pair_button(self: &Rc<Self>, c: &Client, d: &Device) -> gtk::Button {
        let (text, css) = if d.paired {
            (t("Désappairer"), "destructive-action")
        } else {
            (t("Appairer"), "suggested-action")
        };
        let button = gtk::Button::with_label(&text);
        button.set_valign(gtk::Align::Center);
        button.add_css_class(css);
        button.add_css_class("pill");
        let (ui, c, id, paired) = (self.clone(), c.clone(), d.id.clone(), d.paired);
        button.connect_clicked(move |_| {
            let (ui, c, id) = (ui.clone(), c.clone(), id.clone());
            spawn(async move {
                let result = if paired {
                    c.unpair(&id).await
                } else {
                    c.pair(&id).await
                };
                let msg = if paired {
                    ""
                } else {
                    "Demande envoyée, confirmez sur le téléphone."
                };
                ui.report(result, &t(msg));
            });
        });
        button
    }

    // ---- notifications ---------------------------------------------------

    fn set_notification_count(&self, n: usize) {
        self.notif_badge.set_visible(n > 0);
        self.notif_badge.set_label(&n.to_string());
        self.clear_all.set_visible(n > 0);
    }

    async fn refresh_notifications(self: &Rc<Self>) {
        clear_box(&self.notif_box);
        let (Some(c), Some(device)) = (self.client(), self.active_device()) else {
            self.set_notification_count(0);
            return self
                .notif_box
                .append(&empty_page(&t("Aucun téléphone connecté")));
        };
        let list = c.notifications(&device.id).await.unwrap_or_default();
        self.set_notification_count(list.len());
        if list.is_empty() {
            return self
                .notif_box
                .append(&empty_page(&t("Aucune notification")));
        }

        // Groupées par application, les plus récentes d'abord dans chaque groupe.
        let mut by_app: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for n in list.into_iter().rev() {
            let app = n["appName"].as_str().unwrap_or("?").to_owned();
            by_app.entry(app).or_default().push(n);
        }
        for (app, items) in by_app {
            let group = vbox(8);
            group.append(&label(&app, &["fl-section-title"]));
            let list = card();
            for (i, n) in items.iter().enumerate() {
                if i > 0 {
                    list.append(&separator());
                }
                list.append(&self.notification_row(&c, &device.id, n));
            }
            group.append(&list);
            self.notif_box.append(&group);
        }
    }

    fn notification_row(
        self: &Rc<Self>,
        c: &Client,
        device: &str,
        n: &serde_json::Value,
    ) -> gtk::Box {
        let text = |k: &str| n[k].as_str().unwrap_or("").to_owned();
        let row = hbox(12);
        row.set_margin_top(10);
        row.set_margin_bottom(10);
        row.set_margin_start(16);
        row.set_margin_end(8);
        row.append(&icon_bubble(
            "preferences-system-notifications-symbolic",
            true,
        ));

        let body = vbox(2);
        body.set_hexpand(true);
        body.set_valign(gtk::Align::Center);
        let title = label(&text("title"), &[]);
        title.add_css_class("heading");
        title.set_wrap(true);
        body.append(&title);
        let details = label(&text("text"), &["fl-dim"]);
        details.set_wrap(true);
        details.set_lines(3);
        details.set_ellipsize(gtk::pango::EllipsizeMode::End);
        body.append(&details);
        row.append(&body);

        if let Some(reply_id) = n["requestReplyId"].as_str() {
            let button = icon_button("mail-reply-sender-symbolic", &t("Répondre"));
            let (ui, c, device, reply_id) = (
                self.clone(),
                c.clone(),
                device.to_owned(),
                reply_id.to_owned(),
            );
            button.connect_clicked(move |_| {
                let (ui, c, device, reply_id) =
                    (ui.clone(), c.clone(), device.clone(), reply_id.clone());
                spawn(async move { ui.ask_reply(c, device, reply_id).await });
            });
            row.append(&button);
        }
        if n["isClearable"].as_bool().unwrap_or(true) {
            let button = icon_button("window-close-symbolic", &t("Effacer"));
            let (ui, c, device, id) = (self.clone(), c.clone(), device.to_owned(), text("id"));
            button.connect_clicked(move |_| {
                let (ui, c, device, id) = (ui.clone(), c.clone(), device.clone(), id.clone());
                spawn(async move {
                    ui.report(c.dismiss_notification(&device, &id).await, "");
                    ui.refresh_notifications().await;
                });
            });
            row.append(&button);
        }
        row
    }

    async fn ask_reply(self: &Rc<Self>, c: Client, device: String, reply_id: String) {
        let entry = gtk::Entry::builder().activates_default(true).build();
        let dialog = adw::AlertDialog::new(Some(&t("Répondre")), None);
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", &t("Annuler")), ("send", &t("Envoyer"))]);
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("send"));
        dialog.set_close_response("cancel");
        if dialog.choose_future(Some(&self.window)).await == "send" {
            let message = entry.text();
            if !message.is_empty() {
                self.report(
                    c.reply_notification(&device, &reply_id, &message).await,
                    &t("Réponse envoyée."),
                );
            }
        }
    }

    // ---- multimédia ------------------------------------------------------

    fn wire_media(self: &Rc<Self>) {
        let hero = &self.media_hero;
        for (button, action) in [
            (&hero.previous, "Previous"),
            (&hero.play, "PlayPause"),
            (&hero.next, "Next"),
        ] {
            let ui = self.clone();
            button.connect_clicked(move |_| {
                let player = ui.hero_player.borrow().clone();
                ui.media_action(player, action);
            });
        }
        // `change-value` ne part que d'une action de l'utilisateur, pas des mises à jour du téléphone.
        let ui = self.clone();
        hero.volume.connect_change_value(move |_, _, value| {
            let (ui, player) = (ui.clone(), ui.hero_player.borrow().clone());
            let level = value.clamp(0.0, 100.0).round() as i32;
            spawn(async move {
                if let (Some(c), Some(d)) = (ui.client(), ui.active_device()) {
                    ui.report(c.set_media_volume(&d.id, &player, level).await, "");
                }
            });
            glib::Propagation::Proceed
        });
    }

    fn media_action(self: &Rc<Self>, player: String, action: &'static str) {
        let ui = self.clone();
        spawn(async move {
            if let (Some(c), Some(d)) = (ui.client(), ui.active_device()) {
                ui.report(c.media_action(&d.id, &player, action).await, "");
            }
        });
    }

    async fn refresh_media(self: &Rc<Self>) {
        let (Some(c), Some(device)) = (self.client(), self.active_device()) else {
            return self.show_media_placeholder(&t("Aucun téléphone connecté"));
        };
        let players = c.media(&device.id).await.unwrap_or_default();
        if players.is_empty() {
            return self.show_media_placeholder(&t("Aucun lecteur actif sur le téléphone"));
        }
        self.media_empty.set_visible(false);

        // Le lecteur en lecture (sinon le premier) prend la grande carte, les autres sont compacts.
        let main = players
            .iter()
            .position(|p| p["isPlaying"].as_bool().unwrap_or(false))
            .unwrap_or(0);
        let player_name = players[main]["player"].as_str().unwrap_or("").to_owned();
        *self.hero_player.borrow_mut() = player_name;
        update_hero(&self.media_hero, &players[main]);
        self.media_hero.root.set_visible(true);
        self.apply_cover(players[main]["artPath"].as_str().map(str::to_owned));

        clear_box(&self.media_others);
        for (i, p) in players.iter().enumerate() {
            if i != main {
                self.media_others.append(&self.media_row(p));
            }
        }
    }

    /// Démarre ou arrête l'écoute du son du téléphone sur le PC (bouton et menu de l'appli).
    pub fn toggle_listen(self: &Rc<Self>) {
        let ui = self.clone();
        spawn(async move {
            let Some(c) = ui.client() else { return };
            let busy = c
                .listen_status()
                .await
                .is_ok_and(|s| matches!(s.state.as_str(), "starting" | "active"));
            let result = if busy {
                c.stop_listening().await
            } else {
                // Sans lien KDE Connect, l'écoute reste possible par USB : l'appareil est facultatif.
                let id = ui.active_device().map(|d| d.id).unwrap_or_default();
                c.start_listening(&id).await
            };
            if let Err(e) = result {
                ui.toast(e.message());
            }
            ui.refresh_listen().await;
        });
    }

    async fn refresh_listen(self: &Rc<Self>) {
        let Some(c) = self.client() else { return };
        if let Ok(status) = c.listen_status().await {
            self.show_listen(&status);
        }
    }

    fn show_listen(&self, status: &ListenStatus) {
        let l = &self.listen;
        l.status.remove_css_class("error");
        l.button.remove_css_class("suggested-action");
        l.button.remove_css_class("destructive-action");
        let (text, button) = match status.state.as_str() {
            "starting" if status.detail.is_empty() => (t("Démarrage…"), t("Annuler")),
            "starting" => (format!("{} {}", t("Démarrage…"), status.detail), t("Annuler")),
            "active" => (
                t("Actif : le son du téléphone sort sur le PC"),
                t("Arrêter l'écoute"),
            ),
            "error" => {
                l.status.add_css_class("error");
                (format!("{} {}", t("Erreur :"), status.detail), t("Réessayer"))
            }
            _ => (t("Arrêtée"), t("Écouter")),
        };
        l.status.set_label(&text);
        l.button.set_label(&button);
        let busy = matches!(status.state.as_str(), "starting" | "active");
        l.button
            .add_css_class(if busy { "destructive-action" } else { "suggested-action" });
        // La coche du menu de l'appli suit l'état réel du daemon.
        if let Some(action) = self
            .window
            .application()
            .and_then(|app| app.lookup_action("listen"))
        {
            action.change_state(&busy.to_variant());
        }
    }

    fn show_media_placeholder(&self, text: &str) {
        self.media_hero.root.set_visible(false);
        clear_box(&self.media_others);
        self.media_empty.set_title(text);
        self.media_empty.set_visible(true);
    }

    /// Ligne compacte d'un lecteur secondaire.
    fn media_row(self: &Rc<Self>, p: &serde_json::Value) -> gtk::Box {
        let text = |k: &str| p[k].as_str().unwrap_or("").to_owned();
        let row = hbox(12);
        row.add_css_class("fl-card");
        let inner = hbox(12);
        inner.set_hexpand(true);
        inner.set_margin_top(12);
        inner.set_margin_bottom(12);
        inner.set_margin_start(16);
        inner.set_margin_end(16);

        let thumb = gtk::Overlay::new();
        thumb.add_css_class("fl-thumb");
        thumb.set_overflow(gtk::Overflow::Hidden);
        thumb.set_valign(gtk::Align::Center);
        let note = gtk::Image::from_icon_name("multimedia-player-symbolic");
        note.set_halign(gtk::Align::Center);
        note.set_valign(gtk::Align::Center);
        thumb.set_child(Some(&note));
        if let Some(path) = p["artPath"].as_str() {
            let picture = gtk::Picture::for_filename(path);
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_can_shrink(true);
            thumb.add_overlay(&picture);
        }
        inner.append(&thumb);

        let title = text("title");
        let body = vbox(2);
        body.set_hexpand(true);
        body.set_valign(gtk::Align::Center);
        let title_label = label(
            &if title.is_empty() {
                t("Aucun titre")
            } else {
                title
            },
            &[],
        );
        title_label.add_css_class("heading");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        body.append(&title_label);
        let state = if p["isPlaying"].as_bool().unwrap_or(false) {
            t("en lecture")
        } else {
            t("en pause")
        };
        body.append(&label(
            &format!("{} · {state}", text("player")),
            &["fl-dim"],
        ));
        inner.append(&body);

        let play = round_button(
            if p["isPlaying"].as_bool().unwrap_or(false) {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            },
            &t("Lecture / pause"),
        );
        let (ui, player) = (self.clone(), text("player"));
        play.connect_clicked(move |_| ui.media_action(player.clone(), "PlayPause"));
        inner.append(&play);
        row.append(&inner);
        row
    }

    /// Recalcule la couleur de la carte quand la pochette change (hors du thread principal).
    fn apply_cover(self: &Rc<Self>, path: Option<String>) {
        if *self.cover_path.borrow() == path {
            return;
        }
        *self.cover_path.borrow_mut() = path.clone();
        let ui = self.clone();
        spawn(async move {
            let color = match path.clone() {
                Some(p) => gtk::gio::spawn_blocking(move || cover_color::from_path(&p))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            // Une autre pochette est arrivée pendant le calcul : on laisse faire la suivante.
            if *ui.cover_path.borrow() == path {
                ui.set_cover_css(color);
            }
        });
    }

    fn set_cover_css(&self, color: Option<String>) {
        let c = color.as_deref().unwrap_or(cover_color::FALLBACK);
        self.cover_provider.load_from_string(&format!(
            ".player-card {{ background-color: {c}; }}\n\
             .player-card button.player-play {{ color: {c}; }}"
        ));
    }

    // ---- fichiers --------------------------------------------------------

    fn show_progress(&self, name: &str, done: u64, total: u64, incoming: bool) {
        self.transfers_section.set_visible(true);
        let mut rows = self.transfers.borrow_mut();
        let (row, bar) = rows.entry(name.to_owned()).or_insert_with(|| {
            let bar = gtk::ProgressBar::builder()
                .valign(gtk::Align::Center)
                .width_request(160)
                .build();
            let row = action_row(
                &glib::markup_escape_text(name),
                if incoming {
                    "folder-download-symbolic"
                } else {
                    "document-send-symbolic"
                },
            );
            row.add_suffix(&bar);
            self.transfers_list.prepend(&row);
            (row, bar)
        });
        let direction = if incoming {
            t("Réception")
        } else {
            t("Envoi")
        };
        if total > 0 {
            bar.set_fraction(done as f64 / total as f64);
        } else {
            bar.pulse();
        }
        let finished = total > 0 && done >= total;
        row.set_subtitle(&if finished {
            format!("{direction} · {}", t("terminé"))
        } else {
            direction
        });
    }

    // ---- paramètres ------------------------------------------------------

    async fn refresh_settings(self: &Rc<Self>) {
        let Some(c) = self.client() else { return };

        for row in self.devices_rows.borrow_mut().drain(..) {
            self.devices_group.remove(&row);
        }
        let mut device_rows = Vec::new();
        for d in c.devices().await.unwrap_or_default() {
            let state = if d.connected {
                t("Connecté")
            } else {
                t("Hors ligne")
            };
            let row = action_row(&d.name, "phone-symbolic");
            row.set_subtitle(&format!(
                "{state} · {}",
                if d.paired {
                    t("Appairé")
                } else {
                    t("Non appairé")
                }
            ));
            if d.paired || d.connected {
                row.add_suffix(&self.pair_button(&c, &d));
            }
            self.devices_group.add(&row);
            device_rows.push(row.upcast());
        }
        *self.devices_rows.borrow_mut() = device_rows;

        for row in self.settings_rows.borrow_mut().drain(..) {
            self.settings_group.remove(&row);
        }
        let mut rows = Vec::new();
        for p in c.plugins().await.unwrap_or_default() {
            let row = adw::SwitchRow::builder()
                .title(t(plugin_label(&p.name)))
                .active(p.enabled)
                .build();
            row.add_prefix(&icon_bubble(plugin_icon(&p.name), false));
            let (ui, c, name) = (self.clone(), c.clone(), p.name);
            row.connect_active_notify(move |r| {
                let (ui, c, name, on) = (ui.clone(), c.clone(), name.clone(), r.is_active());
                spawn(async move { ui.report(c.set_plugin_enabled(&name, on).await, "") });
            });
            self.settings_group.add(&row);
            rows.push(row.upcast());
        }
        let sleep = adw::SwitchRow::builder()
            .title(t("Fedora Link actif en arrière-plan"))
            .subtitle(t(
                "Désactivez pour arrêter le service et économiser la batterie",
            ))
            .active(true)
            .build();
        sleep.add_prefix(&icon_bubble("battery-symbolic", false));
        sleep.connect_active_notify(|r| {
            if !r.is_active() {
                spawn(async {
                    systemctl(&["disable", "--now", SERVICE]).await;
                });
            }
        });
        self.settings_group.add(&sleep);
        rows.push(sleep.upcast());
        if let Ok(mode) = c.call_media_mode().await {
            const MODES: [&str; 3] = ["none", "pause", "lower"];
            let row = adw::ComboRow::builder()
                .title(t("Son du PC pendant un appel"))
                .model(&gtk::StringList::new(&[
                    &t("Ne rien faire"),
                    &t("Mettre les lecteurs en pause"),
                    &t("Baisser le volume"),
                ]))
                .selected(MODES.iter().position(|m| *m == mode).unwrap_or(0) as u32)
                .build();
            row.add_prefix(&icon_bubble("audio-volume-medium-symbolic", false));
            let (ui, c) = (self.clone(), c.clone());
            row.connect_selected_notify(move |r| {
                let Some(mode) = MODES.get(r.selected() as usize) else {
                    return;
                };
                let (ui, c) = (ui.clone(), c.clone());
                spawn(async move { ui.report(c.set_call_media_mode(mode).await, "") });
            });
            self.settings_group.add(&row);
            rows.push(row.upcast());
        }
        if let Ok(on) = c.mpris_proxy_enabled().await {
            let row = adw::SwitchRow::builder()
                .title(t("Afficher les lecteurs du téléphone dans GNOME"))
                .subtitle(t("Menu multimédia, touches média et playerctl"))
                .active(on)
                .build();
            row.add_prefix(&icon_bubble("multimedia-player-symbolic", false));
            let (ui, c) = (self.clone(), c.clone());
            row.connect_active_notify(move |r| {
                let (ui, c, on) = (ui.clone(), c.clone(), r.is_active());
                spawn(async move { ui.report(c.set_mpris_proxy_enabled(on).await, "") });
            });
            self.settings_group.add(&row);
            rows.push(row.upcast());
        }
        if let Ok(opts) = c.listen_options().await {
            let keep = adw::SwitchRow::builder()
                .title(t("Garder le son sur le téléphone"))
                .subtitle(t("Pendant « Écouter sur le PC » (Android 13 ou plus récent)"))
                .active(opts.keep_on_phone)
                .build();
            let pause = adw::SwitchRow::builder()
                .title(t("Mettre en pause les lecteurs du PC pendant l'écoute"))
                .active(opts.pause_players)
                .build();
            keep.add_prefix(&icon_bubble("audio-headphones-symbolic", false));
            pause.add_prefix(&icon_bubble("media-playback-pause-symbolic", false));
            for row in [&keep, &pause] {
                let (ui, c, keep, pause) = (self.clone(), c.clone(), keep.clone(), pause.clone());
                row.connect_active_notify(move |_| {
                    let (ui, c) = (ui.clone(), c.clone());
                    let (k, p) = (keep.is_active(), pause.is_active());
                    spawn(async move { ui.report(c.set_listen_options(k, p).await, "") });
                });
            }
            self.settings_group.add(&keep);
            self.settings_group.add(&pause);
            rows.push(keep.upcast());
            rows.push(pause.upcast());
        }
        if let Ok(dir) = c.download_dir().await {
            let row = action_row(&t("Dossier de réception"), "folder-symbolic");
            row.set_subtitle(&dir);
            self.settings_group.add(&row);
            rows.push(row.upcast());
        }
        *self.settings_rows.borrow_mut() = rows;
        self.refresh_commands(&c).await;
    }

    /// Liste éditable des commandes que le téléphone a le droit de lancer.
    async fn refresh_commands(self: &Rc<Self>, c: &Client) {
        let commands = c.commands().await.unwrap_or_default();
        for w in self.commands_rows.borrow_mut().drain(..) {
            self.commands_group.remove(&w);
        }
        let mut rows: Vec<gtk::Widget> = Vec::new();
        for (i, cmd) in commands.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(cmd["name"].as_str().unwrap_or("")))
                .subtitle(glib::markup_escape_text(cmd["command"].as_str().unwrap_or("")))
                .subtitle_lines(1)
                .activatable(true)
                .build();
            row.add_prefix(&icon_bubble("utilities-terminal-symbolic", false));
            let remove = icon_button("user-trash-symbolic", &t("Supprimer"));
            remove.set_valign(gtk::Align::Center);
            let (ui, list) = (self.clone(), commands.clone());
            remove.connect_clicked(move |_| {
                let (ui, mut list) = (ui.clone(), list.clone());
                list.remove(i);
                spawn(async move { ui.save_commands(list).await });
            });
            row.add_suffix(&remove);
            let (ui, list, current) = (self.clone(), commands.clone(), cmd.clone());
            row.connect_activated(move |_| ui.edit_command(list.clone(), Some(i), current.clone()));
            self.commands_group.add(&row);
            rows.push(row.upcast());
        }
        let add = action_row(&t("Ajouter une commande"), "list-add-symbolic");
        add.set_activatable(true);
        let (ui, list) = (self.clone(), commands);
        add.connect_activated(move |_| ui.edit_command(list.clone(), None, serde_json::json!({})));
        self.commands_group.add(&add);
        rows.push(add.upcast());
        *self.commands_rows.borrow_mut() = rows;
    }

    async fn save_commands(self: &Rc<Self>, list: Vec<serde_json::Value>) {
        let Some(c) = self.client() else { return };
        match c.set_commands(&list).await {
            Ok(()) => self.refresh_commands(&c).await,
            Err(e) => self.toast(e.message()),
        }
    }

    /// Fenêtre d'édition d'une commande (`index` : celle à remplacer, sinon ajout).
    fn edit_command(self: &Rc<Self>, list: Vec<serde_json::Value>, index: Option<usize>, current: serde_json::Value) {
        let name = adw::EntryRow::builder().title(t("Nom affiché sur le téléphone")).build();
        name.set_text(current["name"].as_str().unwrap_or(""));
        let command = adw::EntryRow::builder().title(t("Commande")).build();
        command.set_text(current["command"].as_str().unwrap_or(""));
        let fields = adw::PreferencesGroup::new();
        fields.add(&name);
        fields.add(&command);
        let dialog = adw::AlertDialog::new(
            Some(&if index.is_some() { t("Modifier la commande") } else { t("Nouvelle commande") }),
            Some(&t("Exécutée dans un shell, avec vos droits d'utilisateur.")),
        );
        dialog.set_extra_child(Some(&fields));
        dialog.add_responses(&[("cancel", &t("Annuler")), ("save", &t("Enregistrer"))]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let ui = self.clone();
        spawn(async move {
            if dialog.choose_future(Some(&ui.window)).await != "save" {
                return;
            }
            let mut entry = current.clone();
            entry["name"] = name.text().to_string().into();
            entry["command"] = command.text().to_string().into();
            // Les nouvelles commandes n'ont pas de clé : le daemon en attribue une.
            if index.is_none() {
                entry["key"] = "".into();
            }
            let mut list = list;
            match index {
                Some(i) if i < list.len() => list[i] = entry,
                _ => list.push(entry),
            }
            ui.save_commands(list).await;
        });
    }

    // ---- presse-papiers --------------------------------------------------

    /// Le daemon ne peut pas observer le presse-papiers sous GNOME/Wayland :
    /// c'est la fenêtre qui détecte les changements locaux et les lui transmet.
    fn setup_clipboard(self: &Rc<Self>) {
        let ui = self.clone();
        self.window.clipboard().connect_changed(move |cb| {
            // Un changement fait par nous-mêmes (texte reçu du téléphone) n'est pas renvoyé.
            if cb.is_local() {
                return;
            }
            let (ui, cb) = (ui.clone(), cb.clone());
            spawn(async move {
                let Some(text) = cb.read_text_future().await.ok().flatten() else {
                    return;
                };
                if let (Some(c), Some(_)) = (ui.client(), ui.active_device()) {
                    // Désactivé côté daemon ou pas de téléphone : on ignore silencieusement.
                    let _ = c.send_clipboard(&text).await;
                }
            });
        });
    }
}

/// Réseau mobile du téléphone : « 4G ▂▄▆█ », les barres au-delà du signal sont atténuées.
fn network_markup(d: &Device) -> Option<String> {
    let signal = d.signal?;
    let kind = d.network_type.as_deref().unwrap_or("");
    let bars: String = ["▂", "▄", "▆", "█"]
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (i as i32) < signal {
                (*b).to_owned()
            } else {
                format!("<span alpha=\"35%\">{b}</span>")
            }
        })
        .collect();
    Some(if kind.is_empty() {
        bars
    } else {
        format!("{} {bars}", glib::markup_escape_text(kind))
    })
}

/// Conseil d'installation : sur Samsung, l'optimisation de batterie coupe KDE Connect.
fn samsung_tip() -> gtk::Box {
    let texts = vbox(2);
    texts.set_hexpand(true);
    let title = label(&t("Sur Samsung, désactivez l'optimisation de batterie"), &["heading"]);
    title.set_wrap(true);
    title.set_xalign(0.0);
    let body = label(
        &t("Sinon Android endort KDE Connect et le téléphone disparaît du réseau. Paramètres › Applications › KDE Connect › Batterie › Sans restriction."),
        &["dim-label"],
    );
    body.set_wrap(true);
    body.set_xalign(0.0);
    texts.append(&title);
    texts.append(&body);
    let inner = hbox(12);
    inner.set_margin_top(12);
    inner.set_margin_bottom(12);
    inner.set_margin_start(16);
    inner.set_margin_end(16);
    inner.append(&icon_bubble("battery-symbolic", false));
    inner.append(&texts);
    let tip = card();
    tip.set_margin_top(12);
    tip.append(&inner);
    tip
}

fn build_listen_card() -> ListenCard {
    let title = label(&t("Écouter sur le PC"), &["heading"]);
    title.set_xalign(0.0);
    let status = label(&t("Arrêtée"), &["dim-label"]);
    status.set_xalign(0.0);
    status.set_wrap(true);
    let texts = vbox(2);
    texts.set_hexpand(true);
    texts.set_valign(gtk::Align::Center);
    texts.append(&title);
    texts.append(&status);
    let button = gtk::Button::with_label(&t("Écouter"));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("suggested-action");
    button.set_tooltip_text(Some(&t(
        "Joue le son du téléphone sur le PC (scrcpy, USB ou ADB Wi-Fi)",
    )));
    let inner = hbox(12);
    inner.set_margin_top(12);
    inner.set_margin_bottom(12);
    inner.set_margin_start(16);
    inner.set_margin_end(16);
    inner.append(&icon_bubble("audio-headphones-symbolic", false));
    inner.append(&texts);
    inner.append(&button);
    let root = card();
    root.append(&inner);
    ListenCard { root, status, button }
}

fn empty_page(title: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name("preferences-system-notifications-symbolic")
        .title(title)
        .build()
}

/// Carte du lecteur actif : fond = couleur de la pochette (CSS dédié), texte blanc.
fn build_hero() -> Hero {
    let cover = gtk::Picture::builder()
        .width_request(184)
        .height_request(184)
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .build();
    cover.add_css_class("player-cover");
    cover.set_overflow(gtk::Overflow::Hidden);
    // Les Clamp fixent 184×184 quelle que soit la taille naturelle de l'image.
    let clamp = |orientation, child: &gtk::Widget| {
        adw::Clamp::builder()
            .orientation(orientation)
            .maximum_size(184)
            .tightening_threshold(184)
            .child(child)
            .build()
    };
    let inner = clamp(gtk::Orientation::Vertical, cover.upcast_ref());
    let cover_frame = clamp(gtk::Orientation::Horizontal, inner.upcast_ref());
    cover_frame.add_css_class("player-cover-shadow");
    cover_frame.set_valign(gtk::Align::End);
    cover_frame.set_halign(gtk::Align::Start);

    let source = label("", &["player-source"]);
    source.set_width_chars(1);
    source.set_wrap(true);
    let title = label("", &["player-title"]);
    title.set_width_chars(1);
    title.set_wrap(true);
    title.set_lines(2);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let subtitle = label("", &["player-sub"]);
    subtitle.set_wrap(true);
    subtitle.set_width_chars(1);
    let info = vbox(6);
    info.set_hexpand(true);
    info.set_valign(gtk::Align::End);
    info.append(&source);
    info.append(&title);
    info.append(&subtitle);

    let top = hbox(24);
    top.append(&cover_frame);
    top.append(&info);

    let progress = gtk::ProgressBar::new();
    progress.add_css_class("player-progress");
    let elapsed = label("0:00", &["player-time"]);
    let total = label("0:00", &["player-time"]);
    total.set_halign(gtk::Align::End);
    let times = gtk::CenterBox::new();
    times.set_start_widget(Some(&elapsed));
    times.set_end_widget(Some(&total));
    let progress_box = vbox(6);
    progress_box.append(&progress);
    progress_box.append(&times);
    progress_box.set_visible(false);

    let skip = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("player-skip");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(tip));
        a11y(&b, tip);
        b
    };
    let previous = skip("media-skip-backward-symbolic", &t("Précédent"));
    let next = skip("media-skip-forward-symbolic", &t("Suivant"));
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.add_css_class("player-play");
    play.set_valign(gtk::Align::Center);
    play.set_tooltip_text(Some(&t("Lecture / pause")));
    a11y(&play, &t("Lecture / pause"));
    let controls = hbox(18);
    controls.set_halign(gtk::Align::Center);
    controls.set_valign(gtk::Align::Center);
    controls.append(&previous);
    controls.append(&play);
    controls.append(&next);

    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_draw_value(false);
    volume.set_width_request(120);
    volume.set_tooltip_text(Some(&t("Volume du téléphone")));
    a11y(&volume, &t("Volume du téléphone"));
    let volume_row = hbox(8);
    volume_row.set_valign(gtk::Align::Center);
    volume_row.append(&gtk::Image::from_icon_name("audio-volume-high-symbolic"));
    volume_row.append(&volume);

    volume_row.set_halign(gtk::Align::End);
    let bottom = gtk::Overlay::new();
    bottom.set_child(Some(&controls));
    bottom.add_overlay(&volume_row);

    let root = vbox(22);
    root.add_css_class("player-card");
    root.append(&top);
    root.append(&progress_box);
    root.append(&bottom);
    Hero {
        root,
        cover,
        source,
        title,
        subtitle,
        progress_box,
        progress,
        elapsed,
        total,
        previous,
        play,
        next,
        volume,
    }
}

fn update_hero(hero: &Hero, p: &serde_json::Value) {
    let text = |k: &str| p[k].as_str().unwrap_or("").to_owned();
    let flag = |k: &str| p[k].as_bool().unwrap_or(true);
    let playing = p["isPlaying"].as_bool().unwrap_or(false);

    hero.source.set_label(
        &format!(
            "{} · {}",
            text("player"),
            if playing {
                t("en lecture")
            } else {
                t("en pause")
            }
        )
        .to_uppercase(),
    );
    let title = text("title");
    hero.title.set_label(&if title.is_empty() {
        t("Aucun titre")
    } else {
        title
    });
    let (artist, album) = (text("artist"), text("album"));
    hero.subtitle.set_label(
        &[artist, album]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · "),
    );

    hero.play.set_icon_name(if playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    });
    hero.previous.set_sensitive(flag("canGoPrevious"));
    hero.next.set_sensitive(flag("canGoNext"));
    if let Some(volume) = p["volume"].as_f64() {
        hero.volume.set_value(volume);
    }
    match p["artPath"].as_str() {
        Some(path) => hero.cover.set_filename(Some(path)),
        None => hero.cover.set_paintable(None::<&gtk::gdk::Paintable>),
    }

    // Barre de progression : seulement si le téléphone envoie position et durée.
    match (p["pos"].as_f64(), p["length"].as_f64()) {
        (Some(pos), Some(len)) if len > 0.0 => {
            hero.progress.set_fraction((pos / len).clamp(0.0, 1.0));
            hero.elapsed.set_label(&format_time(pos));
            hero.total.set_label(&format_time(len));
            hero.progress_box.set_visible(true);
        }
        _ => hero.progress_box.set_visible(false),
    }
}
