//! Shared application state.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::index::Index;
use crate::xorbstore::XorbStore;

/// Maximum number of concurrent file reconstruction/download decoders.
///
/// Each in-flight download parks one blocking-pool thread on `blocking_send`
/// until the client reads or hangs up, and that pool is shared with every
/// sqlite `Index::with` op. An unbounded number of stalled downloads would
/// starve the whole server, `/health` included; capping the decoders keeps at
/// most this many blocking threads parked and leaves the rest of the pool for
/// sqlite (docs/research/dataplane.md section 8.10).
pub const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 64;

/// Maximum number of xorb upload bodies buffered at once.
///
/// A xorb body is capped at `routes::xorbs::MAX_UPLOAD_BYTES` (~68 MiB) and the
/// verifier holds a second, footered copy plus the parsed chunk vectors while
/// it stores one, so a single in-flight upload can cost well over 150 MiB. The
/// client documents 64 concurrent uploads (docs/research/dataplane.md section
/// 4), which unbounded is multiple GiB resident on an ordinary self-hosted box.
/// 16 permits bound that at roughly 1 GiB of bodies while still keeping the
/// disk busy; the excess requests queue rather than fail, and V1 xorb upload has
/// no client-side read timeout to trip.
pub const DEFAULT_XORB_UPLOAD_CONCURRENCY: usize = 16;

/// State handed to every handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// Immutable runtime configuration.
    pub config: Arc<Config>,
    /// SQLite metadata index.
    pub index: Index,
    /// Content-addressed xorb store.
    pub xorbs: XorbStore,
    /// Per-xorb-hash locks serializing the check-write-insert critical section
    /// of xorb upload (first-writer-wins across compression policies).
    pub xorb_locks: KeyedLocks,
    /// Per-repository locks serializing first-touch bare-repository creation,
    /// so `git http-backend` is never run against a repository another request
    /// is still initializing.
    pub repo_locks: KeyedLocks,
    /// Bare repositories this process has created, reported in the creation log
    /// line. Concurrent first touches of one path must only ever increment it
    /// once.
    pub repos_created: Arc<AtomicU64>,
    /// Caps concurrent download decoders so a stalled client cannot starve the
    /// blocking pool that sqlite shares (see DEFAULT_DOWNLOAD_CONCURRENCY).
    pub download_permits: Arc<tokio::sync::Semaphore>,
    /// Caps how many xorb upload bodies are buffered at once (see
    /// DEFAULT_XORB_UPLOAD_CONCURRENCY).
    pub upload_permits: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// Open the data directory and build the state.
    pub async fn new(config: Config) -> AppResult<Self> {
        tokio::fs::create_dir_all(&config.data_dir).await?;
        tokio::fs::create_dir_all(config.xorb_dir()).await?;
        tokio::fs::create_dir_all(config.git_root()).await?;

        let index = Index::open(&config.index_path()).await?;
        let xorbs = XorbStore::new(config.xorb_dir(), config.staging_dir());

        Ok(Self {
            config: Arc::new(config),
            index,
            xorbs,
            xorb_locks: KeyedLocks::default(),
            repo_locks: KeyedLocks::default(),
            repos_created: Arc::new(AtomicU64::new(0)),
            download_permits: Arc::new(tokio::sync::Semaphore::new(DEFAULT_DOWNLOAD_CONCURRENCY)),
            upload_permits: Arc::new(tokio::sync::Semaphore::new(DEFAULT_XORB_UPLOAD_CONCURRENCY)),
        })
    }

    /// Enforce the optional static bearer token on a write route.
    ///
    /// With no token configured the server is fully permissive: authentication
    /// is deliberately out of scope (docs/research/api-surface.md section 3
    /// notes the reference server has none either).
    pub fn authorize(&self, headers: &HeaderMap) -> AppResult<()> {
        let Some(expected) = self.config.required_token() else {
            return Ok(());
        };
        let presented = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        if presented == Some(expected) {
            Ok(())
        } else {
            Err(AppError::Unauthorized)
        }
    }
}

/// A map of per-key async locks. Xorb upload must be first-writer-wins per
/// hash: the hash is compression-independent but the stored frame layout is
/// not, so a re-upload under a different compression policy must not rewrite the
/// blob while the index keeps the original offsets (docs/research 8.1). Holding
/// the lock across the whole check-write-insert closes that race on one node.
#[derive(Clone, Default)]
pub struct KeyedLocks {
    inner: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl KeyedLocks {
    /// Acquire the lock for `key`, held until the returned guard drops.
    pub async fn lock(&self, key: &str) -> KeyedGuard {
        let mutex = {
            let mut map = self.inner.lock().expect("keyed locks poisoned");
            map.entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = mutex.lock_owned().await;
        KeyedGuard {
            _guard: guard,
            locks: self.inner.clone(),
            key: key.to_string(),
        }
    }
}

/// Guard from [`KeyedLocks::lock`]; releasing it prunes the map entry when no
/// other task is waiting, so the table cannot grow without bound.
pub struct KeyedGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    key: String,
}

impl Drop for KeyedGuard {
    fn drop(&mut self) {
        // Drop::drop runs before the fields, so `_guard` still holds one Arc and
        // the map holds another. Exactly two means no other task references this
        // key (a waiter would hold a third clone), so the entry is safe to
        // remove. The map mutex serializes this against a concurrent `lock`.
        let mut map = self.locks.lock().expect("keyed locks poisoned");
        if let Some(mutex) = map.get(&self.key) {
            if Arc::strong_count(mutex) <= 2 {
                map.remove(&self.key);
            }
        }
    }
}
