//! Interface Fedora Link : affiche et pilote le daemon via D-Bus, rien de plus.

mod client;
mod cover_color;
mod messages;
mod presenter;
mod theme;
mod ui;
mod widgets;

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "org.fedoralink.Gui";

fn main() -> gtk::glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let app = adw::Application::builder().application_id(APP_ID).build();
    let ui_cell: Rc<RefCell<Option<Rc<ui::Ui>>>> = Rc::new(RefCell::new(None));

    // Action appelée par la notification d'un nouveau SMS : « <appareil>|<conversation> ».
    let open_thread =
        gtk::gio::SimpleAction::new("open-thread", Some(gtk::glib::VariantTy::STRING));
    let cell = ui_cell.clone();
    open_thread.connect_activate(glib_clone_app(&app, move |app, param| {
        app.activate();
        let thread = param
            .and_then(|p| p.str())
            .and_then(|s| s.rsplit_once('|'))
            .and_then(|(_, id)| id.parse::<i64>().ok());
        if let (Some(ui), Some(thread)) = (cell.borrow().clone(), thread) {
            ui.open_thread(thread);
        }
    }));
    app.add_action(&open_thread);

    // Entrée du menu de l'appli : coche = écoute du téléphone en cours.
    let listen = gtk::gio::SimpleAction::new_stateful("listen", None, &false.to_variant());
    let cell = ui_cell.clone();
    listen.connect_activate(move |_, _| {
        if let Some(ui) = cell.borrow().clone() {
            ui.toggle_listen();
        }
    });
    app.add_action(&listen);

    let cell = ui_cell.clone();
    app.connect_activate(move |app| {
        // Une seule fenêtre : on la ramène au premier plan si elle existe.
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        theme::install();
        let ui = ui::Ui::build(app);
        ui.window.present();
        *cell.borrow_mut() = Some(ui);
    });
    app.run()
}

/// Adapte une fermeture `(application, paramètre)` au format des actions GIO.
fn glib_clone_app(
    app: &adw::Application,
    f: impl Fn(&adw::Application, Option<&gtk::glib::Variant>) + 'static,
) -> impl Fn(&gtk::gio::SimpleAction, Option<&gtk::glib::Variant>) + 'static {
    let app = app.downgrade();
    move |_, param| {
        if let Some(app) = app.upgrade() {
            f(&app, param);
        }
    }
}
