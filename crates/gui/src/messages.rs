//! Page « Messages » : conversations à gauche, fil de discussion à droite, envoi de SMS.
//! Les données viennent du daemon (D-Bus) ; cette page ne garde que l'état d'affichage.

use crate::client::Client;
use crate::widgets::{a11y, round_button};
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

fn t(s: &str) -> String {
    s.to_owned()
}

/// Destinataire du fil affiché : une conversation existante ou un brouillon (nouveau numéro).
#[derive(Clone, Debug)]
struct Target {
    thread: Option<i64>,
    addresses: Vec<String>,
}

pub struct Messages {
    pub root: gtk::Box,
    toast: Box<dyn Fn(&str)>,
    conv_list: gtk::ListBox,
    thread_ids: RefCell<Vec<i64>>,
    right: gtk::Stack,
    title: gtk::Label,
    thread_box: gtk::Box,
    scroller: gtk::ScrolledWindow,
    entry: gtk::Entry,
    left_empty: gtk::Label,
    client: RefCell<Option<Client>>,
    device: RefCell<Option<String>>,
    target: RefCell<Option<Target>>,
    conversations: RefCell<Vec<Value>>,
}

fn same_line(a: &str, b: &str) -> bool {
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    let (da, db) = (digits(a), digits(b));
    let n = da.len().min(db.len()).min(9);
    n >= 5 && da[da.len() - n..] == db[db.len() - n..] || (!da.is_empty() && da == db)
}

fn local_time(ms: i64, format: &str) -> String {
    glib::DateTime::from_unix_local(ms / 1000)
        .ok()
        .and_then(|d| d.format(format).ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// « 14:32 » aujourd'hui, sinon « 21/10 ».
fn short_time(ms: i64) -> String {
    let today = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y%m%d").ok())
        .map(|s| s.to_string());
    let day = local_time(ms, "%Y%m%d");
    if Some(day) == today {
        local_time(ms, "%H:%M")
    } else {
        local_time(ms, "%d/%m")
    }
}

fn avatar(title: &str, path: Option<&str>, size: i32) -> adw::Avatar {
    let a = adw::Avatar::new(size, Some(title), true);
    if let Some(texture) = path.and_then(|p| gtk::gdk::Texture::from_filename(p).ok()) {
        a.set_custom_image(Some(&texture));
    }
    a
}

impl Messages {
    pub fn new(toast: impl Fn(&str) + 'static) -> Rc<Self> {
        // --- colonne de gauche -------------------------------------------------
        let new_btn = round_button("list-add-symbolic", &t("Nouvelle conversation"));
        let heading = gtk::Label::builder()
            .label(t("Messages"))
            .xalign(0.0)
            .hexpand(true)
            .build();
        heading.add_css_class("fl-section-title");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.set_margin_start(16);
        head.set_margin_end(8);
        head.set_margin_top(8);
        head.append(&heading);
        head.append(&new_btn);

        let conv_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        conv_list.add_css_class("fl-nav");
        let left_empty = gtk::Label::new(Some(&t("Aucune conversation")));
        left_empty.add_css_class("fl-dim");
        left_empty.set_margin_top(24);
        let list_scroller = gtk::ScrolledWindow::builder()
            .child(&conv_list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let left = gtk::Box::new(gtk::Orientation::Vertical, 4);
        left.set_width_request(320);
        left.append(&head);
        left.append(&left_empty);
        left.append(&list_scroller);

        // --- fil de discussion --------------------------------------------------
        let title = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        title.add_css_class("fl-section-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_margin_start(16);
        title.set_margin_top(14);
        title.set_margin_bottom(10);
        let thread_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        thread_box.set_margin_start(16);
        thread_box.set_margin_end(16);
        thread_box.set_margin_top(8);
        thread_box.set_margin_bottom(8);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&thread_box)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let entry = gtk::Entry::builder()
            .placeholder_text(t("Écrire un message"))
            .hexpand(true)
            .build();
        entry.add_css_class("fl-msg-entry");
        let send = round_button("document-send-symbolic", &t("Envoyer"));
        send.add_css_class("suggested-action");
        let input = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        input.set_margin_start(16);
        input.set_margin_end(16);
        input.set_margin_bottom(12);
        input.append(&entry);
        input.append(&send);
        a11y(&entry, &t("Message à envoyer"));

        let thread_view = gtk::Box::new(gtk::Orientation::Vertical, 0);
        thread_view.append(&title);
        thread_view.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        thread_view.append(&scroller);
        thread_view.append(&input);
        let placeholder = adw::StatusPage::builder()
            .icon_name("chat-bubble-text-symbolic")
            .title(t("Choisissez une conversation"))
            .description(t("Ou démarrez-en une avec le bouton +"))
            .build();
        let right = gtk::Stack::new();
        right.add_named(&placeholder, Some("empty"));
        right.add_named(&thread_view, Some("thread"));
        right.set_hexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("fl-page");
        root.append(&left);
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        root.append(&right);

        let m = Rc::new(Self {
            root,
            toast: Box::new(toast),
            conv_list,
            thread_ids: RefCell::new(Vec::new()),
            right,
            title,
            thread_box,
            scroller,
            entry,
            left_empty,
            client: RefCell::new(None),
            device: RefCell::new(None),
            target: RefCell::new(None),
            conversations: RefCell::new(Vec::new()),
        });

        let me = m.clone();
        m.conv_list.connect_row_activated(move |_, row| {
            let Some(conv) = me.conversations.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            me.select_conversation(&conv);
        });
        let me = m.clone();
        new_btn.connect_clicked(move |_| me.new_conversation());
        let me = m.clone();
        send.connect_clicked(move |_| me.send());
        let me = m.clone();
        m.entry.connect_activate(move |_| me.send());
        m
    }

    /// Téléphone actif (ou aucun) : recharge les conversations et les contacts.
    pub async fn set_device(self: &Rc<Self>, client: Option<Client>, device: Option<String>) {
        let changed = *self.device.borrow() != device;
        *self.client.borrow_mut() = client;
        *self.device.borrow_mut() = device;
        if changed {
            *self.target.borrow_mut() = None;
            self.right.set_visible_child_name("empty");
        }
        self.refresh_conversations().await;
    }

    pub async fn on_sms_changed(self: &Rc<Self>, thread: i64) {
        self.refresh_conversations().await;
        let current = self.target.borrow().as_ref().and_then(|t| t.thread);
        if current == Some(thread) {
            self.load_thread().await;
        }
    }

    pub fn open_thread(self: &Rc<Self>, thread: i64) {
        let me = self.clone();
        glib::MainContext::default().spawn_local(async move {
            me.refresh_conversations().await;
            let conv = me
                .conversations
                .borrow()
                .iter()
                .find(|c| c["thread_id"].as_i64() == Some(thread))
                .cloned();
            if let Some(conv) = conv {
                me.select_conversation(&conv);
            }
        });
    }

    fn ctx(&self) -> Option<(Client, String)> {
        Some((self.client.borrow().clone()?, self.device.borrow().clone()?))
    }

    async fn refresh_conversations(self: &Rc<Self>) {
        let Some((client, device)) = self.ctx() else {
            self.show_conversations(Vec::new());
            return;
        };
        let list = client.conversations(&device).await.unwrap_or_default();
        self.show_conversations(list);
        // Un brouillon devient une vraie conversation quand le téléphone la renvoie.
        let draft = self.target.borrow().clone().filter(|t| t.thread.is_none());
        if let Some(draft) = draft {
            let found = self
                .conversations
                .borrow()
                .iter()
                .find(|c| {
                    let addrs: Vec<&str> = c["addresses"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    addrs.len() == draft.addresses.len()
                        && draft
                            .addresses
                            .iter()
                            .all(|d| addrs.iter().any(|a| same_line(a, d)))
                })
                .cloned();
            if let Some(conv) = found {
                self.select_conversation(&conv);
            }
        }
    }

    fn show_conversations(&self, list: Vec<Value>) {
        while let Some(row) = self.conv_list.first_child() {
            self.conv_list.remove(&row);
        }
        self.left_empty.set_visible(list.is_empty());
        let mut ids = Vec::new();
        for c in &list {
            ids.push(c["thread_id"].as_i64().unwrap_or(0));
            self.conv_list.append(&self.conversation_row(c));
        }
        *self.thread_ids.borrow_mut() = ids;
        *self.conversations.borrow_mut() = list;
        let current = self.target.borrow().as_ref().and_then(|t| t.thread);
        if let Some(i) = self
            .thread_ids
            .borrow()
            .iter()
            .position(|id| Some(*id) == current)
        {
            self.conv_list
                .select_row(self.conv_list.row_at_index(i as i32).as_ref());
        }
    }

    fn conversation_row(&self, c: &Value) -> gtk::ListBoxRow {
        let title = c["title"].as_str().unwrap_or("");
        let unread = c["unread"].as_bool().unwrap_or(false);
        let snippet = match (
            c["body"].as_str().unwrap_or(""),
            c["has_attachment"].as_bool(),
        ) {
            ("", Some(true)) => t("Pièce jointe"),
            (body, _) => body.replace('\n', " "),
        };
        let prefix = if c["incoming"].as_bool().unwrap_or(true) {
            ""
        } else {
            "Vous : "
        };

        let name = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        name.add_css_class(if unread { "heading" } else { "body" });
        let time = gtk::Label::new(Some(&short_time(c["date"].as_i64().unwrap_or(0))));
        time.add_css_class("fl-dim");
        time.add_css_class("caption");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&name);
        top.append(&time);
        let last = gtk::Label::builder()
            .label(format!("{prefix}{snippet}"))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        last.add_css_class("fl-dim");
        last.add_css_class("caption");
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        texts.append(&top);
        texts.append(&last);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.append(&avatar(title, c["avatar"].as_str(), 40));
        row.append(&texts);
        let list_row = gtk::ListBoxRow::new();
        list_row.set_child(Some(&row));
        list_row
    }

    fn select_conversation(self: &Rc<Self>, conv: &Value) {
        let addresses: Vec<String> = conv["addresses"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let thread = conv["thread_id"].as_i64();
        *self.target.borrow_mut() = Some(Target { thread, addresses });
        self.title.set_label(conv["title"].as_str().unwrap_or(""));
        self.right.set_visible_child_name("thread");
        if let Some(i) = self
            .thread_ids
            .borrow()
            .iter()
            .position(|id| Some(*id) == thread)
        {
            self.conv_list
                .select_row(self.conv_list.row_at_index(i as i32).as_ref());
        }
        let me = self.clone();
        glib::MainContext::default().spawn_local(async move {
            // Messages déjà connus tout de suite, puis l'historique complet demandé au téléphone.
            me.load_thread().await;
            if let (Some((client, device)), Some(thread)) = (me.ctx(), thread) {
                let _ = client.request_conversation(&device, thread).await;
            }
        });
    }

    async fn load_thread(self: &Rc<Self>) {
        let (Some((client, device)), Some(target)) = (self.ctx(), self.target.borrow().clone())
        else {
            return;
        };
        let Some(thread) = target.thread else {
            self.show_messages(&[], &target);
            return;
        };
        let messages = client.messages(&device, thread).await.unwrap_or_default();
        // Une réponse tardive pour une autre conversation ne doit pas écraser celle-ci.
        if self.target.borrow().as_ref().and_then(|t| t.thread) == Some(thread) {
            self.show_messages(&messages, &target);
        }
    }

    fn show_messages(&self, messages: &[Value], target: &Target) {
        while let Some(child) = self.thread_box.first_child() {
            self.thread_box.remove(&child);
        }
        let group = target.addresses.len() > 1;
        let mut last_day = String::new();
        for m in messages {
            let date = m["date"].as_i64().unwrap_or(0);
            let day = local_time(date, "%Y%m%d");
            if day != last_day {
                let sep = gtk::Label::new(Some(&local_time(date, "%A %e %B")));
                sep.add_css_class("fl-dim");
                sep.add_css_class("caption");
                sep.set_margin_top(10);
                self.thread_box.append(&sep);
                last_day = day;
            }
            self.thread_box.append(&self.bubble(m, date, group));
        }
        let scroller = self.scroller.clone();
        // Après la mise en page : défile tout en bas.
        glib::idle_add_local_once(move || {
            let adj = scroller.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    fn bubble(&self, m: &Value, date: i64, group: bool) -> gtk::Box {
        let incoming = m["incoming"].as_bool().unwrap_or(true);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
        column.set_halign(if incoming {
            gtk::Align::Start
        } else {
            gtk::Align::End
        });

        if incoming && group {
            let who = gtk::Label::new(m["sender"].as_str());
            who.add_css_class("fl-dim");
            who.add_css_class("caption");
            who.set_xalign(0.0);
            column.append(&who);
        }
        for a in m["attachments"].as_array().into_iter().flatten() {
            if !a["mime_type"].as_str().unwrap_or("").starts_with("image/") {
                continue;
            }
            let path = a["path"].as_str().or(a["thumbnail"].as_str());
            if let Some(path) = path {
                let pic = gtk::Picture::for_filename(path);
                pic.set_content_fit(gtk::ContentFit::ScaleDown);
                pic.set_can_shrink(true);
                pic.set_size_request(-1, 200);
                pic.set_overflow(gtk::Overflow::Hidden);
                pic.add_css_class("fl-msg-image");
                column.append(&pic);
            }
        }
        let body = m["body"].as_str().unwrap_or("");
        if !body.is_empty() {
            let text = gtk::Label::builder()
                .label(body)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(46)
                .xalign(0.0)
                .selectable(true)
                .build();
            let bubble = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bubble.add_css_class("fl-msg");
            bubble.add_css_class(if incoming { "in" } else { "out" });
            bubble.append(&text);
            column.append(&bubble);
        }
        let time = gtk::Label::new(Some(&local_time(date, "%H:%M")));
        time.add_css_class("fl-dim");
        time.add_css_class("caption");
        time.set_halign(if incoming {
            gtk::Align::Start
        } else {
            gtk::Align::End
        });
        column.append(&time);
        column
    }

    fn send(self: &Rc<Self>) {
        let text = self.entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let (Some((client, device)), Some(target)) = (self.ctx(), self.target.borrow().clone())
        else {
            return;
        };
        let me = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match client.send_sms(&device, &target.addresses, &text).await {
                Ok(()) => {
                    me.entry.set_text("");
                    me.append_pending(&text);
                }
                Err(e) => (me.toast)(e.message()),
            }
        });
    }

    /// Bulle grisée en attendant que le téléphone confirme l'envoi.
    fn append_pending(&self, text: &str) {
        let label = gtk::Label::builder()
            .label(text)
            .wrap(true)
            .max_width_chars(46)
            .xalign(0.0)
            .build();
        let bubble = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bubble.add_css_class("fl-msg");
        bubble.add_css_class("out");
        bubble.add_css_class("pending");
        bubble.set_halign(gtk::Align::End);
        bubble.append(&label);
        self.thread_box.append(&bubble);
    }

    fn new_conversation(self: &Rc<Self>) {
        let Some((client, device)) = self.ctx() else {
            return (self.toast)(&t("Aucun téléphone appairé et connecté."));
        };
        let me = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let contacts = client.contacts(&device).await.unwrap_or_default();
            me.pick_contact(contacts);
        });
    }

    fn pick_contact(self: &Rc<Self>, contacts: Vec<Value>) {
        let search = gtk::SearchEntry::builder()
            .placeholder_text(t("Nom ou numéro"))
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("fl-nav");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(320)
            .vexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.set_margin_bottom(16);
        content.append(&search);
        content.append(&scroller);
        let header = adw::HeaderBar::new();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(t("Nouvelle conversation"))
            .content_width(380)
            .child(&view)
            .build();

        // (adresse, nom) pour chaque ligne affichée.
        let rows: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let contacts = Rc::new(contacts);
        let fill = {
            let (list, rows, contacts) = (list.clone(), rows.clone(), contacts.clone());
            move |query: &str| {
                while let Some(r) = list.first_child() {
                    list.remove(&r);
                }
                let q = query.trim().to_lowercase();
                let mut shown = Vec::new();
                let looks_like_number = q.chars().filter(char::is_ascii_digit).count() >= 3
                    && q.chars()
                        .all(|c| c.is_ascii_digit() || "+ -.()".contains(c));
                if looks_like_number {
                    shown.push((
                        query.trim().to_owned(),
                        format!("{} {}", t("Écrire à"), query.trim()),
                    ));
                }
                for c in contacts.iter() {
                    let name = c["name"].as_str().unwrap_or("");
                    for n in c["numbers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if q.is_empty() || name.to_lowercase().contains(&q) || n.contains(&q) {
                            shown.push((n.to_owned(), format!("{name} · {n}")));
                        }
                    }
                }
                for (_, text) in shown.iter().take(100) {
                    let l = gtk::Label::builder().label(text).xalign(0.0).build();
                    l.set_margin_top(8);
                    l.set_margin_bottom(8);
                    list.append(&l);
                }
                shown.truncate(100);
                *rows.borrow_mut() = shown;
            }
        };
        fill("");
        let fill = Rc::new(fill);
        let f = fill.clone();
        search.connect_search_changed(move |s| f(&s.text()));

        let (me, d, rows2) = (self.clone(), dialog.clone(), rows.clone());
        list.connect_row_activated(move |_, row| {
            let Some((address, text)) = rows2.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            let title = text.split(" · ").next().unwrap_or(&text).to_owned();
            me.start_draft(address, title);
            d.close();
        });
        dialog.present(Some(&self.root));
        search.grab_focus();
    }

    /// Ouvre la conversation existante avec ce numéro, sinon un brouillon.
    fn start_draft(self: &Rc<Self>, address: String, title: String) {
        let existing = self
            .conversations
            .borrow()
            .iter()
            .find(|c| {
                let a = c["addresses"].as_array().cloned().unwrap_or_default();
                a.len() == 1 && a[0].as_str().is_some_and(|x| same_line(x, &address))
            })
            .cloned();
        if let Some(conv) = existing {
            return self.select_conversation(&conv);
        }
        self.title.set_label(&title);
        self.right.set_visible_child_name("thread");
        let target = Target {
            thread: None,
            addresses: vec![address],
        };
        self.show_messages(&[], &target);
        *self.target.borrow_mut() = Some(target);
        self.entry.grab_focus();
    }
}
