//! Pointeur laser de la télécommande de présentation : un point rouge sur l'écran,
//! dans une fenêtre plein écran transparente qui laisse passer tous les clics.
//! Comme dans KDE Connect, la fenêtre disparaît d'elle-même quand le téléphone se tait.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Sans nouveau mouvement pendant ce délai, le point disparaît.
const HIDE_AFTER: Duration = Duration::from_millis(500);

pub struct Presenter {
    window: RefCell<Option<(gtk::Window, gtk::DrawingArea)>>,
    /// Position normalisée (0 à 1) du point sur l'écran.
    pos: Cell<(f64, f64)>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Presenter {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            window: RefCell::new(None),
            pos: Cell::new((0.5, 0.5)),
            timer: RefCell::new(None),
        })
    }

    /// Déplacement relatif demandé par le téléphone (même échelle que KDE Connect).
    pub fn moved(self: &Rc<Self>, dx: f64, dy: f64) {
        if self.window.borrow().is_none() {
            self.pos.set((0.5, 0.5));
            self.show();
        }
        let Some((window, area)) = self.window.borrow().clone() else {
            return;
        };
        let ratio = screen_ratio(&window);
        let (x, y) = self.pos.get();
        self.pos
            .set(((x + dx).clamp(0.0, 1.0), (y + dy * ratio).clamp(0.0, 1.0)));
        area.queue_draw();

        if let Some(t) = self.timer.borrow_mut().take() {
            t.remove();
        }
        let me = self.clone();
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            me.timer.borrow_mut().take();
            me.stop();
        });
        *self.timer.borrow_mut() = Some(id);
    }

    pub fn stop(&self) {
        if let Some(t) = self.timer.borrow_mut().take() {
            t.remove();
        }
        if let Some((window, _)) = self.window.borrow_mut().take() {
            window.destroy();
        }
    }

    fn show(self: &Rc<Self>) {
        let area = gtk::DrawingArea::new();
        let me = self.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let (x, y) = me.pos.get();
            let radius = (f64::from(w) / 300.0).max(7.0);
            cr.arc(f64::from(w) * x, f64::from(h) * y, radius, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(0.9, 0.1, 0.1, 0.85);
            let _ = cr.fill_preserve();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
            cr.set_line_width(1.5);
            let _ = cr.stroke();
        });
        let window = gtk::Window::builder().decorated(false).child(&area).build();
        window.set_css_classes(&["fl-overlay"]);
        // Aucune zone d'entrée : souris et clavier continuent d'aller à la présentation.
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        window.fullscreen();
        window.present();
        *self.window.borrow_mut() = Some((window, area));
    }
}

/// Largeur / hauteur de l'écran où s'affiche le point.
fn screen_ratio(window: &gtk::Window) -> f64 {
    let display = gtk::prelude::WidgetExt::display(window);
    let monitor = window
        .surface()
        .and_then(|s| display.monitor_at_surface(&s))
        .or_else(|| {
            display
                .monitors()
                .item(0)
                .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
        });
    monitor
        .map(|m| m.geometry())
        .filter(|g| g.height() > 0)
        .map_or(16.0 / 9.0, |g| f64::from(g.width()) / f64::from(g.height()))
}
