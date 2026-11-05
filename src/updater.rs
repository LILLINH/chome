//! Tự cập nhật qua GitHub Releases.
//!
//! Mỗi ngày (hoặc khi bấm "Kiểm tra cập nhật") hỏi GitHub bản phát hành mới nhất của repo.
//! Nếu mới hơn bản đang chạy: hiện thanh báo; bấm "Cập nhật" thì tải file .deb của bản đó và
//! cài bằng `pkexec apt-get install` (hệ thống hỏi mật khẩu như khi cài phần mềm), xong thì
//! đề nghị khởi động lại. Nếu Chỏme không được cài từ gói .deb (chạy từ mã nguồn) thì chỉ mở
//! trang phát hành.

use std::path::{Path, PathBuf};
use webkit6::{gio, glib, glib::translate::IntoGlib, prelude::*, soup};

/// Repo phát hành chính thức.
const REPO: &str = "LILLINH/chome";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub deb_url: Option<String>,
    pub page_url: String,
}

fn repo() -> String {
    std::env::var("CHOME_UPDATE_REPO").unwrap_or_else(|_| REPO.into())
}

/// Phiên bản đang chạy (`CHOME_FAKE_VERSION` chỉ dùng khi kiểm thử).
pub fn current_version() -> String {
    std::env::var("CHOME_FAKE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into())
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0))
        .collect()
}

/// `a` mới hơn `b`?
pub fn is_newer(a: &str, b: &str) -> bool {
    let (mut x, mut y) = (parse_version(a), parse_version(b));
    x.resize(3, 0);
    y.resize(3, 0);
    x > y
}

fn deb_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" }
}

async fn get(url: &str, accept: &str) -> Result<(soup::Status, glib::Bytes), String> {
    let session = soup::Session::new();
    session.set_timeout(30);
    session.set_user_agent(&format!("Chome/{} (+https://github.com/{})", current_version(), repo()));
    let msg = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
    if let Some(h) = msg.request_headers() {
        h.append("Accept", accept);
    }
    let bytes = session
        .send_and_read_future(&msg, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("không kết nối được GitHub ({e})"))?;
    Ok((msg.status(), bytes))
}

/// Bản phát hành mới hơn bản đang chạy, hoặc `None` nếu đang dùng bản mới nhất.
pub async fn check() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let (status, bytes) = get(&url, "application/vnd.github+json").await?;
    match status {
        soup::Status::Ok => {}
        soup::Status::NotFound => return Ok(None), // repo chưa có bản phát hành nào
        other => return Err(format!("GitHub trả về mã {}", other.into_glib())),
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let version = v["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_string();
    if version.is_empty() || !is_newer(&version, &current_version()) {
        return Ok(None);
    }
    let suffix = format!("_{}.deb", deb_arch());
    let deb_url = v["assets"].as_array().and_then(|assets| {
        assets
            .iter()
            .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(&suffix)))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(String::from)
    });
    let page_url = v["html_url"].as_str().map(String::from).unwrap_or_else(|| format!("https://github.com/{}/releases", repo()));
    Ok(Some(Release { version, deb_url, page_url }))
}

/// Chỏme được cài từ gói .deb (khi đó mới tự cài bản mới được).
pub fn installed_from_package() -> bool {
    std::env::current_exe().is_ok_and(|p| p == Path::new("/usr/bin/chome"))
}

/// Tải file .deb về thư mục cache, trả về đường dẫn.
pub async fn download(release: &Release) -> Result<PathBuf, String> {
    let url = release.deb_url.as_deref().ok_or("bản phát hành không có file .deb")?;
    let (status, bytes) = get(url, "application/octet-stream").await?;
    if status != soup::Status::Ok {
        return Err(format!("tải file thất bại (mã {})", status.into_glib()));
    }
    // File .deb luôn bắt đầu bằng chữ ký của định dạng ar.
    if !bytes.starts_with(b"!<arch>\n") {
        return Err("file tải về không phải gói .deb hợp lệ".into());
    }
    let dir = glib::user_cache_dir().join("chome").join("update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("chome_{}_{}.deb", release.version, deb_arch()));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Cài gói bằng apt (qua pkexec — hệ thống hỏi mật khẩu quản trị).
pub async fn install(deb: &Path) -> Result<(), String> {
    let deb = deb.to_str().ok_or("đường dẫn không hợp lệ")?;
    let argv = ["pkexec", "apt-get", "install", "-y", "--allow-downgrades", deb];
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let proc = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE)
        .map_err(|e| format!("không chạy được trình cài đặt ({e})"))?;
    proc.wait_future().await.map_err(|e| e.to_string())?;
    match proc.exit_status() {
        0 => Ok(()),
        126 | 127 => Err("bạn đã hủy nhập mật khẩu".into()),
        code => Err(format!("cài đặt thất bại (mã {code})")),
    }
}

/// Mở lại Chỏme (bản vừa cài) sau khi thoát.
pub fn relaunch_after_exit() {
    let argv: Vec<&std::ffi::OsStr> = ["/bin/sh", "-c", "sleep 1; exec /usr/bin/chome"].iter().map(std::ffi::OsStr::new).collect();
    let _ = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chạy tay (cần mạng): `CHOME_UPDATE_REPO=sharkdp/bat CHOME_FAKE_VERSION=0.1.0 cargo test -- --ignored live_update`
    #[test]
    #[ignore]
    fn live_update_download() {
        let ctx = glib::MainContext::default();
        let release = ctx.block_on(check()).expect("kiểm tra lỗi").expect("không thấy bản mới");
        println!("bản mới: {} | deb: {:?}", release.version, release.deb_url);
        let path = ctx.block_on(download(&release)).expect("tải lỗi");
        println!("đã tải: {} ({} byte)", path.display(), std::fs::metadata(&path).unwrap().len());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.1.1", "0.1"));
    }
}
