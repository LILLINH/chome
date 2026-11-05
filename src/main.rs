//! Chỏme — trình duyệt nhẹ kiểu Chrome, viết bằng Rust + GTK4 + WebKitGTK.

mod adblock;
mod browser;
mod devtools;
mod downloads;
mod engine;
mod mcp;
mod pages;
mod passwords;
mod storage;
mod updater;

use browser::{Browser, Startup};
use engine::Engine;
use std::cell::RefCell;
use std::rc::Rc;
use webkit6::{gdk, gio, glib, gtk, prelude::*};

const APP_ID: &str = "vn.chome.Browser";

const ACCELS: &[(&str, &[&str])] = &[
    ("win.new-tab", &["<Ctrl>t"]),
    ("win.new-window", &["<Ctrl>n"]),
    ("win.new-incognito", &["<Ctrl><Shift>n"]),
    ("win.close-tab", &["<Ctrl>w", "<Ctrl>F4"]),
    ("win.reopen-tab", &["<Ctrl><Shift>t"]),
    ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
    ("win.prev-tab", &["<Ctrl><Shift>Tab", "<Ctrl><Shift>ISO_Left_Tab", "<Ctrl>Page_Up"]),
    ("win.back", &["<Alt>Left", "Back"]),
    ("win.forward", &["<Alt>Right", "Forward"]),
    ("win.reload", &["F5", "<Ctrl>r"]),
    ("win.hard-reload", &["<Ctrl>F5", "<Ctrl><Shift>r"]),
    ("win.focus-url", &["<Ctrl>l", "F6", "<Alt>d"]),
    ("win.find", &["<Ctrl>f"]),
    ("win.bookmark", &["<Ctrl>d"]),
    ("win.history", &["<Ctrl>h"]),
    ("win.bookmarks", &["<Ctrl><Shift>o"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
    ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
    ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
    ("win.devtools", &["F12", "<Ctrl><Shift>i"]),
    ("win.devtools-open", &["<Ctrl><Shift>j", "<Ctrl><Shift>c"]),
    ("win.fullscreen", &["F11"]),
    ("win.print", &["<Ctrl>p"]),
    ("win.settings", &["<Ctrl>comma"]),
    ("win.downloads", &["<Ctrl>j"]),
    ("win.bookmarks-bar", &["<Ctrl><Shift>b"]),
    ("app.quit", &["<Ctrl>q"]),
];

fn main() -> glib::ExitCode {
    // Phải chạy trước mọi thứ khác: GLib chỉ đọc G_RESOURCE_OVERLAYS một lần.
    devtools::setup();
    if std::env::var_os("LITE_DEBUG_DEVTOOLS").is_some() {
        eprintln!("[devtools] bản chỉnh sửa đang dùng: {}", devtools::is_active());
    }
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(if std::env::var_os("LITE_NON_UNIQUE").is_some() {
            // Chạy thử độc lập, không chuyển yêu cầu sang cửa sổ đang mở của người dùng.
            gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::HANDLES_OPEN
        })
        .build();
    let engine: Rc<RefCell<Option<Rc<Engine>>>> = Rc::default();

    let e = engine.clone();
    app.connect_startup(move |app| {
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("style.css"));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        for (action, keys) in ACCELS {
            app.set_accels_for_action(action, keys);
        }
        for n in 1..=9 {
            app.set_accels_for_action(&format!("win.select-tab({n})"), &[&format!("<Ctrl>{n}"), &format!("<Alt>{n}")]);
        }
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = a.upgrade() {
                for w in app.windows() {
                    w.close();
                }
            }
        });
        app.add_action(&quit);
        // Icon app: cài trong ~/.local/share/icons, dự phòng thư mục assets/ cạnh file chạy.
        if let Some(display) = gdk::Display::default() {
            if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent()?.parent().map(|d| d.join("assets"))) {
                gtk::IconTheme::for_display(&display).add_search_path(dir);
            }
        }
        gtk::Window::set_default_icon_name(APP_ID);
        let engine = Engine::new();
        mcp::init(&engine);
        browser::schedule_update_check(engine.clone());
        *e.borrow_mut() = Some(engine);
    });

    let e = engine.clone();
    app.connect_activate(move |app| {
        let Some(engine) = e.borrow().clone() else { return };
        if app.windows().is_empty() {
            Browser::new(app, engine, Startup::Restore(vec![]));
        } else {
            Browser::new(app, engine, Startup::Urls(vec![]));
        }
    });

    let e = engine;
    app.connect_open(move |app, files, _| {
        let Some(engine) = e.borrow().clone() else { return };
        let urls: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        match Browser::first() {
            Some(b) => b.open_urls(&urls),
            None => {
                Browser::new(app, engine, Startup::Restore(urls));
            }
        }
    });

    app.connect_shutdown(|_| mcp::stop()); // xóa socket khi thoát
    app.run()
}
