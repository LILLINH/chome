//! Tùy biến DevTools (Web Inspector của WebKit).
//!
//! Giao diện DevTools là HTML/JS nhúng trong libwebkitgtk dưới dạng GResource
//! (`/org/webkit/inspector/UserInterface/…`). GLib cho phép thay thế tài nguyên bằng file
//! ngoài qua biến môi trường `G_RESOURCE_OVERLAYS`. Ta tạo bản `Main.html` gốc (lấy từ
//! chính phiên bản WebKit đang chạy) có chèn thêm `ChomeDevTools.js`, rồi trỏ overlay vào
//! thư mục đó. Mọi file khác của DevTools vẫn lấy từ bản gốc.
//!
//! Phải gọi `setup()` ở đầu `main()`, trước khi GTK/WebKit tra cứu tài nguyên lần đầu
//! (GLib chỉ đọc biến môi trường này một lần).

use std::path::PathBuf;
use webkit6::{gio, glib};

const MAIN_HTML: &str = "/org/webkit/inspector/UserInterface/Main.html";
const TWEAKS_JS: &str = include_str!("devtools.js");
const ANCHOR: &str = r#"<script src="Main.js"></script>"#;

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn overlay_dir() -> PathBuf {
    let version = format!(
        "{}.{}.{}",
        webkit6::functions::major_version(),
        webkit6::functions::minor_version(),
        webkit6::functions::micro_version()
    );
    // Đổi phiên bản WebKit hoặc nội dung chỉnh sửa → thư mục mới, tự tạo lại.
    glib::user_cache_dir()
        .join("chome")
        .join(format!("devtools-{version}-{:08x}", fnv(TWEAKS_JS) as u32))
}

pub fn setup() {
    if std::env::var_os("G_RESOURCE_OVERLAYS").is_some() || std::env::var_os("CHOME_NO_DEVTOOLS_TWEAKS").is_some() {
        return; // tôn trọng cấu hình sẵn có / cho phép tắt
    }
    // GLib in "Adding GResources overlay…" mỗi lần chạy — ẩn riêng thông báo này.
    glib::log_set_handler(Some("GLib-GIO"), glib::LogLevels::LEVEL_MESSAGE, false, false, |domain, level, msg| {
        if !msg.contains("overlay") {
            glib::log_default_handler(domain, level, Some(msg));
        }
    });
    let dir = overlay_dir();
    let Some(dir_str) = dir.to_str().filter(|d| !d.contains('=') && !d.contains(':')) else { return };
    // Đặt trước khi tra cứu: thư mục chưa có Main.html thì GLib rơi về bản gốc.
    // SAFETY: gọi ở đầu main(), khi chương trình còn một luồng.
    unsafe { std::env::set_var("G_RESOURCE_OVERLAYS", format!("/org/webkit/inspector/UserInterface={dir_str}")) };

    if dir.join("Main.html").is_file() {
        return;
    }
    let ok = (|| -> Option<()> {
        let original = gio::resources_lookup_data(MAIN_HTML, gio::ResourceLookupFlags::NONE).ok()?;
        let html = std::str::from_utf8(&original).ok()?;
        if !html.contains(ANCHOR) {
            return None; // cấu trúc lạ (phiên bản WebKit khác) → giữ nguyên DevTools gốc
        }
        let patched = html.replacen(ANCHOR, &format!("{ANCHOR}\n    <script src=\"ChomeDevTools.js\"></script>"), 1);
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(dir.join("ChomeDevTools.js"), TWEAKS_JS).ok()?;
        std::fs::write(dir.join("Main.html"), patched).ok()
    })();
    if ok.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: như trên.
        unsafe { std::env::remove_var("G_RESOURCE_OVERLAYS") };
    }
}

/// Dùng khi kiểm tra: DevTools có đang dùng bản đã chỉnh sửa không.
pub fn is_active() -> bool {
    gio::resources_lookup_data(MAIN_HTML, gio::ResourceLookupFlags::NONE)
        .ok()
        .is_some_and(|d| std::str::from_utf8(&d).is_ok_and(|h| h.contains("ChomeDevTools.js")))
}
