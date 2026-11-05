//! Lưu trữ cục bộ bằng SQLite: lịch sử, bookmark và phiên tab đang mở.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub url: String,
    pub title: String,
}

pub struct Storage {
    conn: Connection,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Storage {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS history(
                 id INTEGER PRIMARY KEY,
                 url TEXT NOT NULL UNIQUE,
                 title TEXT NOT NULL DEFAULT '',
                 visits INTEGER NOT NULL DEFAULT 1,
                 last_visit INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS history_last ON history(last_visit DESC);
             CREATE TABLE IF NOT EXISTS bookmarks(
                 id INTEGER PRIMARY KEY,
                 url TEXT NOT NULL UNIQUE,
                 title TEXT NOT NULL DEFAULT '',
                 created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS downloads(
                 id INTEGER PRIMARY KEY,
                 url TEXT NOT NULL,
                 path TEXT NOT NULL,
                 filename TEXT NOT NULL,
                 size INTEGER NOT NULL DEFAULT 0,
                 state TEXT NOT NULL,
                 time INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS settings(
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS session(
                 pos INTEGER PRIMARY KEY,
                 url TEXT NOT NULL,
                 title TEXT NOT NULL DEFAULT '',
                 active INTEGER NOT NULL DEFAULT 0);",
        )?;
        // Cột "pinned" thêm sau (ghim tab) — bỏ qua lỗi nếu đã có.
        let _ = conn.execute("ALTER TABLE session ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0", []);
        Ok(Self { conn })
    }

    pub fn in_memory() -> Self {
        Self::open(Path::new(":memory:")).expect("không mở được SQLite trong bộ nhớ")
    }

    fn rows(&self, sql: &str, p: impl rusqlite::Params) -> Vec<Entry> {
        let Ok(mut stmt) = self.conn.prepare_cached(sql) else { return vec![] };
        stmt.query_map(p, |r| {
            Ok(Entry { id: r.get(0)?, url: r.get(1)?, title: r.get(2)? })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // ---------- Lịch sử ----------

    pub fn add_visit(&self, url: &str, title: &str) {
        let _ = self.conn.execute(
            "INSERT INTO history(url, title, visits, last_visit) VALUES(?1, ?2, 1, ?3)
             ON CONFLICT(url) DO UPDATE SET visits = visits + 1, last_visit = ?3,
                 title = CASE WHEN ?2 = '' THEN title ELSE ?2 END",
            params![url, title, now()],
        );
    }

    pub fn set_title(&self, url: &str, title: &str) {
        if title.is_empty() {
            return;
        }
        let _ = self.conn.execute(
            "UPDATE history SET title = ?2 WHERE url = ?1",
            params![url, title],
        );
        let _ = self.conn.execute(
            "UPDATE bookmarks SET title = ?2 WHERE url = ?1 AND title = ''",
            params![url, title],
        );
    }

    pub fn recent_history(&self, limit: i64) -> Vec<(Entry, i64)> {
        let Ok(mut stmt) = self.conn.prepare_cached(
            "SELECT id, url, title, last_visit FROM history ORDER BY last_visit DESC LIMIT ?1",
        ) else {
            return vec![];
        };
        stmt.query_map([limit], |r| {
            Ok((Entry { id: r.get(0)?, url: r.get(1)?, title: r.get(2)? }, r.get(3)?))
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn top_sites(&self, limit: i64) -> Vec<Entry> {
        self.rows(
            "SELECT id, url, title FROM history ORDER BY visits DESC, last_visit DESC LIMIT ?1",
            [limit],
        )
    }

    /// Gợi ý cho thanh địa chỉ: bookmark trước, sau đó lịch sử hay truy cập.
    pub fn suggest(&self, query: &str, limit: i64) -> Vec<Entry> {
        let q = query.trim();
        if q.is_empty() {
            return vec![];
        }
        let pattern = format!(
            "%{}%",
            q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        self.rows(
            "SELECT id, url, title FROM (
                 SELECT id, url, title, 1000000 AS score FROM bookmarks
                   WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
                 UNION ALL
                 SELECT id, url, title, visits AS score FROM history
                   WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\')
             GROUP BY url ORDER BY MAX(score) DESC LIMIT ?2",
            params![pattern, limit],
        )
    }

    pub fn delete_history(&self, id: i64) {
        let _ = self.conn.execute("DELETE FROM history WHERE id = ?1", [id]);
    }

    pub fn clear_history(&self) {
        let _ = self.conn.execute("DELETE FROM history", []);
        let _ = self.conn.execute_batch("VACUUM");
    }

    // ---------- Bookmark ----------

    pub fn bookmarks(&self) -> Vec<Entry> {
        self.rows("SELECT id, url, title FROM bookmarks ORDER BY created DESC", [])
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM bookmarks WHERE url = ?1", [url], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// Bật/tắt bookmark, trả về trạng thái mới.
    pub fn toggle_bookmark(&self, url: &str, title: &str) -> bool {
        if self.is_bookmarked(url) {
            let _ = self.conn.execute("DELETE FROM bookmarks WHERE url = ?1", [url]);
            false
        } else {
            let _ = self.conn.execute(
                "INSERT INTO bookmarks(url, title, created) VALUES(?1, ?2, ?3)",
                params![url, title, now()],
            );
            true
        }
    }

    pub fn delete_bookmark(&self, id: i64) {
        let _ = self.conn.execute("DELETE FROM bookmarks WHERE id = ?1", [id]);
    }

    // ---------- Tải xuống ----------

    pub fn add_download(&self, url: &str, path: &str, filename: &str, size: i64, state: &str) {
        let _ = self.conn.execute(
            "INSERT INTO downloads(url, path, filename, size, state, time) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![url, path, filename, size, state, now()],
        );
    }

    /// (id, url, đường dẫn, tên file, kích thước, trạng thái, thời điểm)
    pub fn downloads(&self, limit: i64) -> Vec<(i64, String, String, String, i64, String, i64)> {
        let Ok(mut stmt) = self.conn.prepare_cached(
            "SELECT id, url, path, filename, size, state, time FROM downloads ORDER BY time DESC, id DESC LIMIT ?1",
        ) else {
            return vec![];
        };
        stmt.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn download_path(&self, id: i64) -> Option<String> {
        self.conn
            .query_row("SELECT path FROM downloads WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn delete_download(&self, id: i64) {
        let _ = self.conn.execute("DELETE FROM downloads WHERE id = ?1", [id]);
    }

    pub fn clear_downloads(&self) {
        let _ = self.conn.execute("DELETE FROM downloads", []);
    }

    // ---------- Cài đặt ----------

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        );
    }

    // ---------- Phiên làm việc ----------

    pub fn save_session(&self, tabs: &[(String, String, bool)], active: usize) {
        let Ok(tx) = self.conn.unchecked_transaction() else { return };
        let _ = tx.execute("DELETE FROM session", []);
        for (i, (url, title, pinned)) in tabs.iter().enumerate() {
            let _ = tx.execute(
                "INSERT INTO session(pos, url, title, active, pinned) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![i as i64, url, title, (i == active) as i64, *pinned as i64],
            );
        }
        let _ = tx.commit();
    }

    /// Trả về danh sách (url, title, đã ghim) và vị trí tab đang chọn.
    pub fn load_session(&self) -> (Vec<(String, String, bool)>, usize) {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT url, title, active, pinned FROM session ORDER BY pos")
        else {
            return (vec![], 0);
        };
        let mut active = 0;
        let tabs = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
            })
            .map(|it| {
                it.filter_map(Result::ok)
                    .enumerate()
                    .map(|(i, (u, t, a, p))| {
                        if a != 0 {
                            active = i;
                        }
                        (u, t, p != 0)
                    })
                    .collect()
            })
            .unwrap_or_default();
        (tabs, active)
    }
}
