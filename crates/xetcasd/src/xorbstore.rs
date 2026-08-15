//! On-disk xorb object storage.
//!
//! Layout: `<root>/<h[0..2]>/<h[2..4]>/<64 hex>`. Objects are stored WITH the
//! canonical v1 footer appended, but the chunk frames still begin at offset 0,
//! so a reconstruction `url_range` indexes the stored file directly and the
//! data route can serve it with a plain positional read
//! (docs/research/binary-formats.md section 3.4).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{AppError, AppResult};

/// Hook that makes a directory's entries durable.
///
/// Production always uses [`fsync_dir`]; it is a field so a unit test can
/// observe exactly which directories are synced, which is otherwise invisible
/// short of crashing the host.
type DirSync = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

/// fsync a directory, making the entries created in it survive a crash.
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// The directories `create_dir_all(parent)` will have to create, shallowest
/// first.
///
/// Recorded before the create so we know whose entries still need an fsync:
/// a directory only exists durably once the directory holding its ENTRY has
/// been synced.
fn missing_ancestors(parent: &Path) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut cursor = Some(parent);
    while let Some(dir) = cursor {
        if dir.exists() {
            break;
        }
        missing.push(dir.to_path_buf());
        cursor = dir.parent();
    }
    missing.reverse();
    missing
}

/// Content-addressed xorb store rooted at a directory.
#[derive(Clone)]
pub struct XorbStore {
    root: PathBuf,
    /// Staging directory for atomic renames. Deliberately a SIBLING of `root`,
    /// not a child: the demo ledger measures the object tree with `du -sb`, and
    /// staging files must not show up in that number.
    tmp: PathBuf,
    dir_sync: DirSync,
}

impl XorbStore {
    /// Create a store of objects under `root`, staging writes in `tmp`.
    ///
    /// Both must live on the same filesystem so the rename is atomic.
    pub fn new(root: PathBuf, tmp: PathBuf) -> Self {
        Self {
            root,
            tmp,
            dir_sync: Arc::new(fsync_dir),
        }
    }

    /// Root directory of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the object for `hash_hex`, sharded two levels deep so no single
    /// directory accumulates every object.
    pub fn path_for(&self, hash_hex: &str) -> PathBuf {
        debug_assert_eq!(hash_hex.len(), 64);
        self.root
            .join(&hash_hex[0..2])
            .join(&hash_hex[2..4])
            .join(hash_hex)
    }

    /// Write `bytes` as the object for `hash_hex`, durably and atomically.
    ///
    /// The temp file is `sync_all`'d before the rename and every directory this
    /// write creates is fsynced through its own parent afterwards, so a xorb the
    /// server has already acknowledged with a 200 survives power loss. The
    /// rename only makes the final NAME appear atomically -- it is not itself a
    /// crash data barrier, and syncing the leaf directory alone is not enough:
    /// the layout is two levels deep, so an unsynced `<h0..2>` entry can take
    /// the whole object with it while SQLite still holds the xorb record, after
    /// which every later upload of that hash short-circuits and never rewrites
    /// the missing blob. Durability matters here because the client caches
    /// shards for weeks: a lost xorb would make a later push fail with a
    /// non-retried "unknown xorb".
    ///
    /// A concurrent upload of the same xorb is harmless: both write identical
    /// verified content and the rename is atomic, so a reader always sees a
    /// complete object.
    pub async fn write_atomic(&self, hash_hex: &str, bytes: Vec<u8>) -> AppResult<()> {
        let final_path = self.path_for(hash_hex);
        let tmp_dir = self.tmp.clone();
        let tmp_path = tmp_dir.join(format!("{}.{}", hash_hex, uuid::Uuid::new_v4()));
        let parent = final_path
            .parent()
            .ok_or_else(|| AppError::internal("xorb path has no parent"))?
            .to_path_buf();
        let dir_sync = self.dir_sync.clone();

        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(&tmp_dir)?;
            let fresh = missing_ancestors(&parent);
            std::fs::create_dir_all(&parent)?;
            // Shallowest first: a child entry is only meaningful once its
            // parent's entry is itself durable.
            for dir in &fresh {
                if let Some(dir_parent) = dir.parent() {
                    dir_sync(dir_parent)?;
                }
            }
            {
                let mut file = std::fs::File::create(&tmp_path)?;
                file.write_all(&bytes)?;
                // Flush the data to disk before it takes its final name.
                file.sync_all()?;
            }
            std::fs::rename(&tmp_path, &final_path)?;
            // fsync the directory so the rename itself is durable across a crash.
            dir_sync(&parent)?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::internal(format!("xorb write task: {e}")))??;
        Ok(())
    }
}

/// Read `len` bytes starting at `start` from an open object. Blocking; call
/// from a blocking context.
pub fn read_range_at(path: &Path, start: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

impl XorbStore {
    /// Read a byte range of a stored object.
    pub async fn read_range(&self, hash_hex: &str, start: u64, len: usize) -> AppResult<Vec<u8>> {
        let path = self.path_for(hash_hex);
        tokio::task::spawn_blocking(move || read_range_at(&path, start, len))
            .await
            .map_err(|e| AppError::internal(format!("xorb read task: {e}")))?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::internal(format!("xorb read: {e}")),
            })
    }

    /// Constant-time liveness probe of the object tree.
    ///
    /// The root must exist, be a directory, and be enumerable. This is what
    /// `/health` uses: a full walk would grow with the store, and the container
    /// health-checks that endpoint every five seconds. Opening the directory
    /// (rather than only stat'ing it) is what catches a failed mount or a
    /// permission problem, so indexed objects that cannot be read make the
    /// health request fail instead of reporting `"status":"ok"`.
    pub async fn probe(&self) -> AppResult<()> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let meta = std::fs::metadata(&root)?;
            if !meta.is_dir() {
                return Err(std::io::Error::other(format!(
                    "{} is not a directory",
                    root.display()
                )));
            }
            // One entry is enough to prove the directory is readable, and keeps
            // the probe independent of how many objects are stored.
            std::fs::read_dir(&root)?.next().transpose()?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::internal(format!("xorb probe task: {e}")))?
        .map_err(|e| AppError::internal(format!("xorb store unavailable: {e}")))
    }

    /// Total bytes occupied by stored objects, measured by walking the tree.
    ///
    /// This is the auditing view: it agrees with what `du` reports, which is
    /// what the demo ledger measures. It is deliberately NOT what `/health`
    /// serves -- see [`XorbStore::probe`] and `Index::stats`. A traversal
    /// failure is propagated rather than counted as zero.
    pub async fn stored_bytes(&self) -> AppResult<u64> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || walk_size(&root))
            .await
            .map_err(|e| AppError::internal(format!("xorb stat task: {e}")))?
            .map_err(|e| AppError::internal(format!("walking the object tree: {e}")))
    }
}

fn walk_size(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += walk_size(&entry.path())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    impl XorbStore {
        /// Store whose directory syncs run through `dir_sync`.
        fn with_dir_sync(root: PathBuf, tmp: PathBuf, dir_sync: DirSync) -> Self {
            Self {
                root,
                tmp,
                dir_sync,
            }
        }
    }

    /// A `DirSync` that still fsyncs, but records what it was asked to sync.
    fn recorder(log: Arc<Mutex<Vec<PathBuf>>>) -> DirSync {
        Arc::new(move |dir: &Path| {
            log.lock()
                .expect("sync log poisoned")
                .push(dir.to_path_buf());
            fsync_dir(dir)
        })
    }

    #[test]
    fn missing_ancestors_lists_only_what_has_to_be_created() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("xorbs");
        std::fs::create_dir_all(&root).unwrap();
        let parent = root.join("ab").join("cd");

        assert_eq!(
            missing_ancestors(&parent),
            vec![root.join("ab"), root.join("ab").join("cd")],
            "both sharding levels are new"
        );

        std::fs::create_dir_all(&parent).unwrap();
        assert!(
            missing_ancestors(&parent).is_empty(),
            "an existing prefix must not be re-synced on every write"
        );
    }

    /// The two-level layout means the leaf fsync alone is not enough: a crash
    /// can drop the unsynced `<h0..2>` entry and take the object with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_directory_this_write_creates_is_fsynced() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("xorbs");
        std::fs::create_dir_all(&root).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let store = XorbStore::with_dir_sync(
            root.clone(),
            dir.path().join("staging"),
            recorder(log.clone()),
        );

        let hash = format!("abcd{}", "0".repeat(60));
        store.write_atomic(&hash, vec![7u8; 32]).await.unwrap();

        let level1 = root.join("ab");
        let level2 = level1.join("cd");
        let synced = log.lock().unwrap().clone();
        assert!(
            synced.contains(&root),
            "the object root holds the new <h0..2> entry and was not synced: {synced:?}"
        );
        assert!(
            synced.contains(&level1),
            "the <h0..2> directory holds the new <h2..4> entry and was not synced: {synced:?}"
        );
        assert!(
            synced.contains(&level2),
            "the leaf directory holds the renamed object and was not synced: {synced:?}"
        );

        // A second object under the same prefix creates nothing, so only the
        // leaf (which gains the rename) is synced again.
        log.lock().unwrap().clear();
        let sibling = format!("abcd{}", "1".repeat(60));
        store.write_atomic(&sibling, vec![9u8; 16]).await.unwrap();
        assert_eq!(
            log.lock().unwrap().clone(),
            vec![level2],
            "an existing prefix must cost exactly one directory fsync"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_object_root_is_an_error_not_a_zero() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = XorbStore::new(dir.path().join("gone"), dir.path().join("staging"));
        assert!(
            store.probe().await.is_err(),
            "probe must report the failure"
        );
        assert!(
            store.stored_bytes().await.is_err(),
            "an unreadable tree must not be reported as zero stored bytes"
        );
    }
}
