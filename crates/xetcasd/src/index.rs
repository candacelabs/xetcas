//! The SQLite metadata index.
//!
//! Access pattern: a small free-list of connections, each checked out inside a
//! `spawn_blocking` call. rusqlite is synchronous and `Connection` is not
//! `Sync`, so some form of hand-off is mandatory; a free-list beats a single
//! shared writer because reconstruction and LFS download are read-heavy and
//! WAL mode lets those readers proceed concurrently with the one writer.
//! Write serialization is left to SQLite itself, with `busy_timeout` absorbing
//! the brief contention window rather than surfacing SQLITE_BUSY to a client.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use prost::Message;
use rusqlite::{Connection, OptionalExtension};
use xetcas_contracts::v1::{FileRecord, XorbRecord};

use crate::error::{AppError, AppResult};

/// Current on-disk schema version.
const SCHEMA_VERSION: i64 = 1;

/// Handle to the metadata index. Cheap to clone.
#[derive(Clone)]
pub struct Index {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Inner {
    fn open_connection(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // FULL, not NORMAL: an acknowledged xorb/shard upload must survive power
        // loss, or the client's weeks-long shard cache will later reference a
        // xorb the server lost and every push 400s with a non-retried error.
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(conn)
    }

    fn checkout(&self) -> rusqlite::Result<Connection> {
        if let Some(conn) = self.idle.lock().expect("index pool poisoned").pop() {
            return Ok(conn);
        }
        self.open_connection()
    }

    fn checkin(&self, conn: Connection) {
        let mut idle = self.idle.lock().expect("index pool poisoned");
        // Cap the free list; bursty concurrency should not pin file handles.
        if idle.len() < 16 {
            idle.push(conn);
        }
    }
}

impl Index {
    /// Open (creating if needed) the index at `path` and apply the schema.
    pub async fn open(path: &Path) -> AppResult<Self> {
        let inner = Arc::new(Inner {
            path: path.to_path_buf(),
            idle: Mutex::new(Vec::new()),
        });
        let this = Self { inner };
        this.with(|conn| {
            conn.execute_batch(SCHEMA)?;
            let version: Option<i64> = conn
                .query_row("SELECT version FROM schema_meta", [], |r| r.get(0))
                .optional()?;
            if version.is_none() {
                conn.execute(
                    "INSERT INTO schema_meta (version) VALUES (?1)",
                    [SCHEMA_VERSION],
                )?;
            }
            Ok(())
        })
        .await?;
        Ok(this)
    }

    async fn with<T, F>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.checkout()?;
            let out = f(&mut conn);
            // Return the connection to the pool whether or not the closure
            // succeeded; a failed statement does not poison the handle.
            inner.checkin(conn);
            out
        })
        .await
        .map_err(|e| AppError::internal(format!("index task: {e}")))?
        .map_err(AppError::from)
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schema_meta (version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS xorbs (
    hash   TEXT PRIMARY KEY,
    record BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS files (
    file_hash TEXT PRIMARY KEY,
    sha256    TEXT,
    record    BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS files_sha256 ON files (sha256);
CREATE TABLE IF NOT EXISTS chunks (
    chunk_hash  TEXT PRIMARY KEY,
    xorb_hash   TEXT NOT NULL,
    chunk_index INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS store_stats (
    id         INTEGER PRIMARY KEY CHECK (id = 0),
    disk_bytes INTEGER NOT NULL
);
INSERT OR IGNORE INTO store_stats (id, disk_bytes) VALUES (0, 0);
";

fn decode_xorb(blob: Vec<u8>) -> rusqlite::Result<XorbRecord> {
    XorbRecord::decode(blob.as_slice()).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
    })
}

fn decode_file(blob: Vec<u8>) -> rusqlite::Result<FileRecord> {
    FileRecord::decode(blob.as_slice()).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
    })
}

impl Index {
    /// Register a verified xorb plus the chunks of it that are eligible to
    /// answer a global-dedup probe. Returns false when the xorb already
    /// existed: xorb upload is idempotent (docs/research/dataplane.md 8.1).
    ///
    /// `disk_bytes` is the size of the object as written to the store. It is
    /// accumulated in the same transaction as the record, so `/health` can
    /// report stored bytes without walking the object tree.
    pub async fn put_xorb(
        &self,
        record: XorbRecord,
        dedup_chunks: Vec<(String, u32)>,
        disk_bytes: u64,
    ) -> AppResult<bool> {
        let hash = record.xorb_hash.clone();
        let blob = record.encode_to_vec();
        self.with(move |conn| {
            let tx = conn.transaction()?;
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO xorbs (hash, record) VALUES (?1, ?2)",
                rusqlite::params![&hash, &blob],
            )? > 0;
            if inserted {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO chunks (chunk_hash, xorb_hash, chunk_index) VALUES (?1, ?2, ?3)",
                )?;
                for (chunk_hash, index) in &dedup_chunks {
                    stmt.execute(rusqlite::params![chunk_hash, &hash, index])?;
                }
                drop(stmt);
                tx.execute(
                    "UPDATE store_stats SET disk_bytes = disk_bytes + ?1 WHERE id = 0",
                    [disk_bytes as i64],
                )?;
            }
            tx.commit()?;
            Ok(inserted)
        })
        .await
    }

    /// Fetch one xorb record.
    pub async fn get_xorb(&self, hash: &str) -> AppResult<Option<XorbRecord>> {
        let hash = hash.to_string();
        self.with(move |conn| {
            let blob: Option<Vec<u8>> = conn
                .query_row("SELECT record FROM xorbs WHERE hash = ?1", [&hash], |r| {
                    r.get(0)
                })
                .optional()?;
            blob.map(decode_xorb).transpose()
        })
        .await
    }

    /// Fetch several xorb records at once, skipping any that are missing.
    pub async fn get_xorbs(&self, hashes: Vec<String>) -> AppResult<HashMap<String, XorbRecord>> {
        self.with(move |conn| {
            let mut stmt = conn.prepare("SELECT record FROM xorbs WHERE hash = ?1")?;
            let mut out = HashMap::with_capacity(hashes.len());
            for hash in hashes {
                let blob: Option<Vec<u8>> = stmt.query_row([&hash], |r| r.get(0)).optional()?;
                if let Some(blob) = blob {
                    out.insert(hash, decode_xorb(blob)?);
                }
            }
            Ok(out)
        })
        .await
    }
}

impl Index {
    /// Look up a file by its xet file hash.
    pub async fn get_file(&self, file_hash: &str) -> AppResult<Option<FileRecord>> {
        let file_hash = file_hash.to_string();
        self.with(move |conn| {
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT record FROM files WHERE file_hash = ?1",
                    [&file_hash],
                    |r| r.get(0),
                )
                .optional()?;
            blob.map(decode_file).transpose()
        })
        .await
    }

    /// Look up a file by the SHA-256 recorded in its shard metadata. This is
    /// the linkage that lets the LFS bridge serve a download by git-lfs oid
    /// (docs/research/git-xet.md section 6.4).
    pub async fn file_by_sha256(&self, sha256: &str) -> AppResult<Option<FileRecord>> {
        let sha256 = sha256.to_string();
        self.with(move |conn| {
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT record FROM files WHERE sha256 = ?1 ORDER BY file_hash LIMIT 1",
                    [&sha256],
                    |r| r.get(0),
                )
                .optional()?;
            blob.map(decode_file).transpose()
        })
        .await
    }

    /// Upsert file records and index any additional dedup-eligible chunks the
    /// shard told us about. Returns the number of files that were new.
    pub async fn put_files(
        &self,
        files: Vec<FileRecord>,
        chunks: Vec<(String, String, u32)>,
    ) -> AppResult<usize> {
        self.with(move |conn| {
            let tx = conn.transaction()?;
            let mut new_files = 0usize;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO files (file_hash, sha256, record) VALUES (?1, ?2, ?3)",
                )?;
                for file in &files {
                    let sha = if file.sha256.is_empty() { None } else { Some(&file.sha256) };
                    let blob = file.encode_to_vec();
                    new_files += stmt.execute(rusqlite::params![&file.file_hash, sha, &blob])?;
                }
                let mut chunk_stmt = tx.prepare(
                    "INSERT OR IGNORE INTO chunks (chunk_hash, xorb_hash, chunk_index) VALUES (?1, ?2, ?3)",
                )?;
                for (chunk_hash, xorb_hash, index) in &chunks {
                    chunk_stmt.execute(rusqlite::params![chunk_hash, xorb_hash, index])?;
                }
            }
            tx.commit()?;
            Ok(new_files)
        })
        .await
    }

    /// Resolve a chunk hash to the xorb that holds it and its index there.
    pub async fn lookup_chunk(&self, chunk_hash: &str) -> AppResult<Option<(String, u32)>> {
        let chunk_hash = chunk_hash.to_string();
        self.with(move |conn| {
            conn.query_row(
                "SELECT xorb_hash, chunk_index FROM chunks WHERE chunk_hash = ?1",
                [&chunk_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
        .await
    }

    /// Everything `/health` reports, in one round trip.
    ///
    /// All three numbers come out of the index, so the endpoint stays constant
    /// time as the store grows: the container health-checks it every five
    /// seconds and a recursive stat of every stored object would eventually
    /// time out on a large store even though request serving is healthy.
    pub async fn stats(&self) -> AppResult<IndexStats> {
        self.with(|conn| {
            let xorbs: i64 = conn.query_row("SELECT COUNT(*) FROM xorbs", [], |r| r.get(0))?;
            let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            let disk_bytes: i64 =
                conn.query_row("SELECT disk_bytes FROM store_stats WHERE id = 0", [], |r| {
                    r.get(0)
                })?;
            Ok(IndexStats {
                xorbs: xorbs as u64,
                files: files as u64,
                stored_bytes: disk_bytes as u64,
            })
        })
        .await
    }

    /// Row counts only.
    pub async fn counts(&self) -> AppResult<(u64, u64)> {
        let stats = self.stats().await?;
        Ok((stats.xorbs, stats.files))
    }
}

/// The index's own view of what the server holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexStats {
    /// Number of stored xorbs.
    pub xorbs: u64,
    /// Number of registered files.
    pub files: u64,
    /// Bytes of xorb objects written to the store, accumulated as each xorb
    /// was indexed. Equal to the sum of the object file sizes under the object
    /// root for any index this schema created; it deliberately excludes orphan
    /// blobs left by a crash between the write and the index insert, which no
    /// reconstruction can reach.
    pub stored_bytes: u64,
}
