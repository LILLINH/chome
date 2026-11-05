//! Quản lý tải xuống (giống "bong bóng tải xuống" + trang chrome://downloads của Chrome).
//!
//! Theo dõi từng lượt tải của WebKit (tiến trình, tốc độ, trạng thái), lưu lịch sử vào
//! SQLite (trừ lượt tải trong cửa sổ ẩn danh) và báo cho giao diện mỗi khi có thay đổi
//! (gộp cập nhật tiến trình tối đa ~4 lần/giây để không tốn CPU).

use crate::storage::Storage;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use webkit6::{gio, glib, prelude::*};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }
}

pub struct Item {
    download: webkit6::Download,
    pub url: String,
    pub filename: RefCell<String>,
    pub path: RefCell<Option<PathBuf>>,
    pub state: Cell<State>,
    pub received: Cell<u64>,
    pub total: Cell<u64>,
    pub started: Instant,
    pub incognito: bool,
}

impl Item {
    pub fn cancel(&self) {
        self.download.cancel();
    }

    /// Tốc độ trung bình (byte/giây) từ lúc bắt đầu.
    pub fn speed(&self) -> f64 {
        let secs = self.started.elapsed().as_secs_f64().max(0.5);
        self.received.get() as f64 / secs
    }

    /// Dòng trạng thái kiểu Chrome: "12,3 MB / 40 MB · còn 5 giây".
    pub fn status_text(&self) -> String {
        match self.state.get() {
            State::Running => {
                let (got, total) = (self.received.get(), self.total.get());
                if total > 0 {
                    let speed = self.speed();
                    let left = if speed > 1.0 { ((total.saturating_sub(got)) as f64 / speed) as u64 } else { 0 };
                    format!("{} / {} · còn {}", human_size(got), human_size(total), human_duration(left))
                } else {
                    format!("{} · {}/giây", human_size(got), human_size(self.speed() as u64))
                }
            }
            State::Done => format!("Hoàn tất · {}", human_size(self.received.get().max(self.total.get()))),
            State::Failed => "Lỗi tải xuống".into(),
            State::Cancelled => "Đã hủy".into(),
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        let total = self.total.get();
        (self.state.get() == State::Running && total > 0).then(|| (self.received.get() as f64 / total as f64).min(1.0))
    }
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
    .replace('.', ",")
}

fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} giây"),
        60..=3599 => format!("{} phút", secs.div_ceil(60)),
        _ => format!("{} giờ {} phút", secs / 3600, (secs % 3600) / 60),
    }
}

/// ~/Downloads/<tên>, thêm " (1)", " (2)"… nếu đã tồn tại.
pub fn unique_path(dir: &Path, suggested: &str) -> PathBuf {
    let name = Path::new(suggested)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".into());
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut path = dir.join(&name);
    let mut i = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({i}){ext}"));
        i += 1;
    }
    path
}

pub fn downloads_dir() -> PathBuf {
    let dir = glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(|| glib::home_dir().join("Downloads"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Mở file bằng ứng dụng mặc định.
pub fn open_file(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    let _ = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
}

/// Mở thư mục chứa file và chọn sẵn file (như "Hiện trong thư mục" của Chrome).
pub fn show_in_folder(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    let shown = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).is_ok_and(|bus| {
        bus.call_sync(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&(vec![uri.to_string()], "").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            None::<&gio::Cancellable>,
        )
        .is_ok()
    });
    if !shown {
        if let Some(dir) = path.parent() {
            let _ = gio::AppInfo::launch_default_for_uri(&gio::File::for_path(dir).uri(), None::<&gio::AppLaunchContext>);
        }
    }
}

pub struct Manager {
    me: Weak<Manager>,
    db: Rc<Storage>,
    items: RefCell<Vec<Rc<Item>>>,
    listeners: RefCell<Vec<Box<dyn Fn(Option<&Rc<Item>>)>>>,
    last_progress_notify: Cell<Option<Instant>>,
}

impl Manager {
    pub fn new(db: Rc<Storage>) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            db,
            items: RefCell::new(vec![]),
            listeners: RefCell::new(vec![]),
            last_progress_notify: Cell::new(None),
        })
    }

    /// Đăng ký nhận thông báo thay đổi. `Some(item)` = vừa có lượt tải MỚI (để mở bong bóng).
    pub fn add_listener(&self, f: impl Fn(Option<&Rc<Item>>) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    fn notify(&self, new_item: Option<&Rc<Item>>) {
        for f in self.listeners.borrow().iter() {
            f(new_item);
        }
    }

    pub fn items(&self) -> Vec<Rc<Item>> {
        self.items.borrow().clone()
    }

    pub fn clear_finished(&self) {
        self.items.borrow_mut().retain(|i| i.state.get() == State::Running);
        self.notify(None);
    }

    /// Theo dõi mọi lượt tải của một phiên mạng (phiên thường hoặc phiên ẩn danh).
    pub fn attach(&self, session: &webkit6::NetworkSession, incognito: bool) {
        let me = self.me.clone();
        session.connect_download_started(move |_, download| {
            if let Some(m) = me.upgrade() {
                m.track(download, incognito);
            }
        });
    }

    fn track(&self, download: &webkit6::Download, incognito: bool) {
        let url = download.request().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
        let item = Rc::new(Item {
            download: download.clone(),
            url,
            filename: RefCell::new(String::new()),
            path: RefCell::new(None),
            state: Cell::new(State::Running),
            received: Cell::new(0),
            total: Cell::new(0),
            started: Instant::now(),
            incognito,
        });
        self.items.borrow_mut().insert(0, item.clone());

        let weak_item = Rc::downgrade(&item);
        download.connect_decide_destination(move |dl, suggested| {
            let path = unique_path(&downloads_dir(), suggested);
            if let Some(it) = weak_item.upgrade() {
                *it.filename.borrow_mut() = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                *it.path.borrow_mut() = Some(path.clone());
            }
            dl.set_destination(&path.to_string_lossy());
            true
        });

        let (me, weak_item) = (self.me.clone(), Rc::downgrade(&item));
        download.connect_received_data(move |dl, _| {
            let (Some(m), Some(it)) = (me.upgrade(), weak_item.upgrade()) else { return };
            it.received.set(dl.received_data_length());
            if let Some(len) = dl.response().map(|r| r.content_length()).filter(|l| *l > 0) {
                it.total.set(len);
            }
            // Gộp cập nhật: tối đa ~4 lần/giây.
            let due = m.last_progress_notify.get().is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
            if due {
                m.last_progress_notify.set(Some(Instant::now()));
                m.notify(None);
            }
        });

        let (me, weak_item) = (self.me.clone(), Rc::downgrade(&item));
        download.connect_failed(move |_, err| {
            let (Some(m), Some(it)) = (me.upgrade(), weak_item.upgrade()) else { return };
            let cancelled = err.matches(webkit6::DownloadError::CancelledByUser);
            it.state.set(if cancelled { State::Cancelled } else { State::Failed });
            m.record(&it);
            m.notify(None);
        });

        let (me, weak_item) = (self.me.clone(), Rc::downgrade(&item));
        download.connect_finished(move |dl| {
            let (Some(m), Some(it)) = (me.upgrade(), weak_item.upgrade()) else { return };
            if it.state.get() != State::Running {
                return; // "finished" cũng được phát sau "failed"
            }
            it.received.set(dl.received_data_length());
            it.state.set(State::Done);
            m.record(&it);
            m.notify(None);
        });

        self.notify(Some(&item));
    }

    fn record(&self, it: &Item) {
        if it.incognito {
            return; // tải xuống ẩn danh không ghi lịch sử
        }
        let path = it.path.borrow().as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        self.db.add_download(&it.url, &path, &it.filename.borrow(), it.received.get() as i64, it.state.get().as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "2 KB");
        assert_eq!(human_size(12 * 1024 * 1024 + 300 * 1024), "12,3 MB");
        assert_eq!(human_duration(5), "5 giây");
        assert_eq!(human_duration(61), "2 phút");
        assert_eq!(human_duration(3700), "1 giờ 1 phút");
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = std::env::temp_dir().join(format!("chome-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.zip"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.zip").file_name().unwrap(), "a (1).zip");
        assert_eq!(unique_path(&dir, "b.zip").file_name().unwrap(), "b.zip");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
