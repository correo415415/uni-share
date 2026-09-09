//! Transfer history persisted in SQLite (`rusqlite`, bundled).
//!
//! SQLite was preferred over a JSON file because it supports concurrent
//! readers (daemon + CLI), atomic writes, indexed queries and grows well.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    LanSend,
    LanReceive,
    GlobalUpload,
    Download,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::LanSend => "lan_send",
            Kind::LanReceive => "lan_receive",
            Kind::GlobalUpload => "global_upload",
            Kind::Download => "download",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lan_send" => Kind::LanSend,
            "lan_receive" => Kind::LanReceive,
            "global_upload" => Kind::GlobalUpload,
            "download" => Kind::Download,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InProgress,
    Completed,
    Failed,
    Rejected,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::InProgress => "in_progress",
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Rejected => "rejected",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "in_progress" => Status::InProgress,
            "completed" => Status::Completed,
            "failed" => Status::Failed,
            "rejected" => Status::Rejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub kind: Kind,
    pub name: String,
    pub size: u64,
    /// Peer device (LAN) or share link (global) or source URL (download).
    pub peer_or_link: String,
    pub status: Status,
    pub file_count: u32,
    pub error: Option<String>,
    /// storage.to owner token / extra metadata (JSON).
    pub meta: Option<String>,
}

#[derive(Clone)]
pub struct History {
    conn: Arc<Mutex<Connection>>,
}

impl History {
    pub fn default_path() -> PathBuf {
        crate::config::data_dir().join("history.sqlite3")
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening history db {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transfers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                size INTEGER NOT NULL DEFAULT 0,
                peer_or_link TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL,
                file_count INTEGER NOT NULL DEFAULT 1,
                error TEXT,
                meta TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_transfers_ts ON transfers(ts DESC);",
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE transfers (
                id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, kind TEXT NOT NULL,
                name TEXT NOT NULL, size INTEGER NOT NULL DEFAULT 0,
                peer_or_link TEXT NOT NULL DEFAULT '', status TEXT NOT NULL,
                file_count INTEGER NOT NULL DEFAULT 1, error TEXT, meta TEXT);",
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Insert a new record (usually `InProgress`) and return its id.
    pub fn start(
        &self,
        kind: Kind,
        name: &str,
        size: u64,
        peer_or_link: &str,
        file_count: u32,
    ) -> Result<i64> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        conn.execute(
            "INSERT INTO transfers (ts, kind, name, size, peer_or_link, status, file_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                Utc::now().to_rfc3339(),
                kind.as_str(),
                name,
                size as i64,
                peer_or_link,
                Status::InProgress.as_str(),
                file_count as i64
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish(&self, id: i64, status: Status, error: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        conn.execute(
            "UPDATE transfers SET status = ?1, error = ?2 WHERE id = ?3",
            params![status.as_str(), error, id],
        )?;
        Ok(())
    }

    pub fn set_link(&self, id: i64, link: &str, meta: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        conn.execute(
            "UPDATE transfers SET peer_or_link = ?1, meta = COALESCE(?2, meta) WHERE id = ?3",
            params![link, meta, id],
        )?;
        Ok(())
    }

    /// Merge `patch` (a JSON object) into the `meta` of the most recent record with this
    /// kind and name — used to attach the safety-scan verdict, which is only known after
    /// `finish()` has run. Best effort: no record, no error.
    pub fn merge_meta_latest(&self, kind: Kind, name: &str, patch: &serde_json::Value) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        let row: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT id, meta FROM transfers WHERE kind = ?1 AND name = ?2 ORDER BY ts DESC, id DESC LIMIT 1",
                params![kind.as_str(), name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, meta)) = row else { return Ok(()) };
        let mut obj = meta.as_deref().and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok()).unwrap_or(serde_json::json!({}));
        if !obj.is_object() {
            obj = serde_json::json!({ "value": obj });
        }
        if let (Some(dst), Some(src)) = (obj.as_object_mut(), patch.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        conn.execute("UPDATE transfers SET meta = ?1 WHERE id = ?2", params![obj.to_string(), id])?;
        Ok(())
    }

    pub fn list(&self, limit: usize) -> Result<Vec<Record>> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, name, size, peer_or_link, status, file_count, error, meta
             FROM transfers ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], row_to_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get(&self, id: i64) -> Result<Option<Record>> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        conn.query_row(
            "SELECT id, ts, kind, name, size, peer_or_link, status, file_count, error, meta
             FROM transfers WHERE id = ?1",
            [id],
            row_to_record,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn clear(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("history mutex poisoned"))?;
        Ok(conn.execute("DELETE FROM transfers", [])?)
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    let ts: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let status: String = row.get(6)?;
    Ok(Record {
        id: row.get(0)?,
        timestamp: DateTime::parse_from_rfc3339(&ts)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        kind: Kind::parse(&kind).unwrap_or(Kind::Download),
        name: row.get(3)?,
        size: row.get::<_, i64>(4)? as u64,
        peer_or_link: row.get(5)?,
        status: Status::parse(&status).unwrap_or(Status::Failed),
        file_count: row.get::<_, i64>(7)? as u32,
        error: row.get(8)?,
        meta: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_update_list() {
        let h = History::open_in_memory().unwrap();
        let id = h.start(Kind::LanSend, "video.mp4", 1234, "PC-Sala", 1).unwrap();
        h.finish(id, Status::Completed, None).unwrap();
        let id2 = h.start(Kind::GlobalUpload, "proj", 999, "", 3).unwrap();
        h.set_link(id2, "https://storage.to/abc", Some("{\"owner\":\"x\"}")).unwrap();
        h.finish(id2, Status::Failed, Some("boom")).unwrap();
        let list = h.list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, id2);
        assert_eq!(list[0].status, Status::Failed);
        assert_eq!(list[0].peer_or_link, "https://storage.to/abc");
        assert_eq!(list[0].error.as_deref(), Some("boom"));
        assert_eq!(list[1].kind, Kind::LanSend);
        assert_eq!(h.get(id).unwrap().unwrap().name, "video.mp4");
        assert!(h.get(9999).unwrap().is_none());
        assert_eq!(h.clear().unwrap(), 2);
    }

    #[test]
    fn file_backed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x/h.sqlite3");
        let h = History::open(&p).unwrap();
        h.start(Kind::Download, "a", 1, "u", 1).unwrap();
        drop(h);
        let h2 = History::open(&p).unwrap();
        assert_eq!(h2.list(5).unwrap().len(), 1);
    }
}
