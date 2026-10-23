//! Palette claire/sombre et feuille de style, rechargées quand le thème système change.

use gtk4 as gtk;
use libadwaita as adw;

const STYLE: &str = include_str!("style.css");

/// (nom, clair, sombre)
const PALETTE: [(&str, &str, &str); 13] = [
    ("fl_bg", "#fafafb", "#222226"),
    ("fl_sidebar", "#ebebed", "#2e2e32"),
    ("fl_card", "#ffffff", "#353539"),
    ("fl_border", "#deddda", "#46464b"),
    ("fl_fg", "#1f1f23", "#ffffff"),
    ("fl_dim", "#5e5c64", "#c0bfc4"),
    ("fl_accent", "#1c71d8", "#3584e4"),
    ("fl_accent_soft", "#dbe9fb", "rgba(53, 132, 228, 0.25)"),
    ("fl_accent_fg", "#1a5fb4", "#99c1f1"),
    ("fl_btn_grey", "#e8e8ea", "#46464b"),
    ("fl_banner", "#dbe9fb", "#1c3a5e"),
    ("fl_banner_ink", "#0f3a6b", "#dbe9fb"),
    ("fl_connected", "#26a269", "#57e389"),
];

fn source(dark: bool) -> String {
    let mut css = String::new();
    for (name, light, night) in PALETTE {
        css.push_str(&format!(
            "@define-color {name} {};\n",
            if dark { night } else { light }
        ));
    }
    // Les widgets libadwaita (curseurs, interrupteurs, fond de fenêtre) suivent la même palette.
    let get = |name: &str| {
        let (_, light, night) = PALETTE.iter().find(|(n, ..)| *n == name).unwrap();
        if dark {
            night
        } else {
            light
        }
    };
    for (adw_name, ours) in [
        ("window_bg_color", "fl_bg"),
        ("window_fg_color", "fl_fg"),
        ("headerbar_bg_color", "fl_bg"),
        ("headerbar_fg_color", "fl_fg"),
        ("card_bg_color", "fl_card"),
        ("accent_bg_color", "fl_accent"),
        ("accent_color", "fl_accent_fg"),
    ] {
        css.push_str(&format!("@define-color {adw_name} {};\n", get(ours)));
    }
    css.push_str(STYLE);
    css
}

/// Charge la feuille de style au démarrage et la recharge à chaque bascule clair/sombre.
pub fn install() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, err| {
        tracing::warn!("CSS {} : {err}", section.start_location().lines());
    });
    let manager = adw::StyleManager::default();
    provider.load_from_string(&source(manager.is_dark()));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    manager.connect_dark_notify(move |m| provider.load_from_string(&source(m.is_dark())));
}
