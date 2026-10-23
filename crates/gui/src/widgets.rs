//! Petits widgets partagés : pastilles d'icônes, anneau de batterie, illustration du téléphone.

use gtk::cairo;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

/// Nom accessible d'un widget (lecteurs d'écran).
pub fn a11y(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

/// Pastille ronde fond accent doux (44 px, ou 32 px grise) avec une icône.
pub fn icon_bubble(icon: &str, small: bool) -> gtk::CenterBox {
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(if small { 16 } else { 20 });
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    // CenterBox : centre l'icône sans propager d'expansion verticale aux parents.
    let bubble = gtk::CenterBox::new();
    bubble.add_css_class("fl-bubble");
    if small {
        bubble.add_css_class("small");
        bubble.add_css_class("grey");
    }
    bubble.set_halign(gtk::Align::Center);
    bubble.set_valign(gtk::Align::Center);
    bubble.set_center_widget(Some(&image));
    bubble
}

/// Bouton rond 44 px à icône seule : infobulle + nom accessible.
pub fn round_button(icon: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("fl-round");
    b.set_tooltip_text(Some(label));
    b.set_valign(gtk::Align::Center);
    a11y(&b, label);
    b
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

fn set_color(cr: &cairo::Context, c: &gtk::gdk::RGBA, alpha: f64) {
    cr.set_source_rgba(
        c.red() as f64,
        c.green() as f64,
        c.blue() as f64,
        c.alpha() as f64 * alpha,
    );
}

/// Illustration 92×160 : contour de téléphone, encoche, trois barres de notification.
pub fn phone_art() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(92)
        .content_height(160)
        .valign(gtk::Align::Center)
        .build();
    area.add_css_class("fl-hero-art");
    area.set_draw_func(|area, cr, _, _| {
        let c = area.color();
        set_color(cr, &c, 1.0);
        cr.set_line_width(3.0);
        rounded_rect(cr, 1.5, 1.5, 89.0, 157.0, 16.0);
        let _ = cr.stroke();
        rounded_rect(cr, 33.0, 11.0, 26.0, 5.0, 2.5);
        let _ = cr.fill();
        set_color(cr, &c, 0.25);
        for (y, w) in [(40.0, 64.0), (66.0, 64.0), (92.0, 44.0)] {
            rounded_rect(cr, 14.0, y, w, 16.0, 6.0);
            let _ = cr.fill();
        }
    });
    area
}

/// Anneau de batterie 96 px : piste à 18 %, arc arrondi proportionnel, texte au centre.
#[derive(Clone)]
pub struct BatteryRing {
    pub root: gtk::Overlay,
    area: gtk::DrawingArea,
    level: Rc<Cell<i32>>,
    pct: gtk::Label,
    sub: gtk::Label,
}

impl BatteryRing {
    pub fn new() -> Self {
        let level = Rc::new(Cell::new(0));
        let area = gtk::DrawingArea::builder()
            .content_width(96)
            .content_height(96)
            .build();
        area.add_css_class("fl-hero-ring");
        let l = level.clone();
        area.set_draw_func(move |area, cr, w, h| {
            let c = area.color();
            let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
            cr.set_line_width(8.0);
            cr.set_line_cap(cairo::LineCap::Round);
            set_color(cr, &c, 0.18);
            cr.arc(cx, cy, 38.0, 0.0, 2.0 * PI);
            let _ = cr.stroke();
            let frac = l.get().clamp(0, 100) as f64 / 100.0;
            if frac > 0.0 {
                set_color(cr, &c, 1.0);
                cr.arc(cx, cy, 38.0, -PI / 2.0, -PI / 2.0 + frac * 2.0 * PI);
                let _ = cr.stroke();
            }
        });

        let pct = gtk::Label::new(None);
        pct.add_css_class("fl-ring-pct");
        let sub = gtk::Label::new(None);
        sub.add_css_class("fl-ring-sub");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_halign(gtk::Align::Center);
        text.set_valign(gtk::Align::Center);
        text.append(&pct);
        text.append(&sub);

        let root = gtk::Overlay::new();
        root.set_child(Some(&area));
        root.add_overlay(&text);
        root.set_valign(gtk::Align::Center);
        Self {
            root,
            area,
            level,
            pct,
            sub,
        }
    }

    pub fn set(&self, level: i32, charging: bool) {
        self.level.set(level);
        self.pct.set_label(&format!("{level} %"));
        self.sub
            .set_label(if charging { "en charge" } else { "batterie" });
        a11y(&self.root, &format!("Batterie du téléphone : {level} %"));
        self.area.queue_draw();
    }
}
