//! Shared harness: an in-process server plus real xet-core client sessions.
//!
//! Each test binary compiles this module separately, so helpers only some of
//! them use would otherwise warn.

#![allow(dead_code)]

use std::path::Path;
use std::sync::{Arc, Once};

use tempfile::TempDir;
use xet_data::processing::configurations::{SessionContext, TranslatorConfig};
use xet_runtime::core::XetContext;
use xetcasd::config::Config;
use xetcasd::state::AppState;

static INIT: Once = Once::new();

/// Point the client cache root at a throwaway directory and silence telemetry.
///
/// These are process-global, so they are set exactly once and before any
/// `XetContext` exists. Everything else that varies per test is passed through
/// `TranslatorConfig` explicitly rather than through the environment.
pub fn init_env() {
    INIT.call_once(|| {
        let root = std::env::temp_dir().join(format!("xetcasd-it-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create client cache root");
        std::env::set_var("HF_XET_CACHE", &root);
        std::env::set_var("HF_XET_TELEMETRY_ENABLED", "false");
    });
}

/// A xetcasd instance bound to an OS-assigned port, with its own data dir.
pub struct TestServer {
    /// Base URL clients should use, with no trailing slash.
    pub base_url: String,
    /// Server-side state, so tests can inspect the index directly.
    pub state: AppState,
    _data: TempDir,
}

/// Non-default server settings a test wants.
#[derive(Default)]
pub struct Knobs {
    /// Static bearer token required on CAS write routes.
    pub token: Option<String>,
    /// Cap on concurrent reconstruction decoders.
    pub download_permits: Option<usize>,
    /// Cap on concurrently buffered xorb upload bodies.
    pub upload_permits: Option<usize>,
}

impl TestServer {
    /// Start a server on 127.0.0.1 with an ephemeral port.
    pub async fn start() -> Self {
        Self::build(Knobs::default()).await
    }

    /// Start a server that requires `token` on CAS write routes.
    pub async fn start_with_token(token: Option<String>) -> Self {
        Self::build(Knobs {
            token,
            ..Knobs::default()
        })
        .await
    }

    /// Start a server whose download decoder concurrency is capped at `permits`.
    pub async fn start_with_download_permits(permits: usize) -> Self {
        Self::build(Knobs {
            download_permits: Some(permits),
            ..Knobs::default()
        })
        .await
    }

    /// Start a server whose xorb upload buffering is capped at `permits`.
    pub async fn start_with_upload_permits(permits: usize) -> Self {
        Self::build(Knobs {
            upload_permits: Some(permits),
            ..Knobs::default()
        })
        .await
    }

    async fn build(knobs: Knobs) -> Self {
        init_env();
        let data = TempDir::new().expect("temp data dir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");

        let config = Config {
            data_dir: data.path().to_path_buf(),
            listen: addr.to_string(),
            public_url: base_url.clone(),
            git_root: None,
            git_autocreate: true,
            token: knobs.token,
        };
        let mut state = AppState::new(config).await.expect("open data dir");
        if let Some(n) = knobs.download_permits {
            state.download_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(n));
        }
        if let Some(n) = knobs.upload_permits {
            state.upload_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(n));
        }
        let app = xetcasd::routes::router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            base_url,
            state,
            _data: data,
        }
    }

    /// Number of stored xorbs and files.
    pub async fn counts(&self) -> (u64, u64) {
        self.state.index.counts().await.expect("counts")
    }

    /// Total bytes of stored xorb objects.
    pub async fn stored_bytes(&self) -> u64 {
        self.state.xorbs.stored_bytes().await.expect("stored bytes")
    }
}

/// Build a client config for `endpoint` whose shard cache and session dirs
/// live under `cache`.
///
/// Overriding the two directories is what gives each session real isolation: a
/// fresh cache means the client cannot dedup locally and has to go through the
/// server global-dedup route, which is exactly what some tests are proving.
pub fn client_config(endpoint: &str, cache: &Path) -> Arc<TranslatorConfig> {
    init_env();
    let ctx = XetContext::default().expect("xet context");
    let session = SessionContext {
        endpoint: endpoint.to_string(),
        auth: None,
        custom_headers: None,
        repo_paths: vec!["".into()],
        session_id: None,
    };
    let mut config = TranslatorConfig::new(&ctx, session).expect("translator config");
    config.shard_cache_directory = cache.join("shard-cache");
    config.shard_session_directory = cache.join("shard-session");
    std::fs::create_dir_all(&config.shard_cache_directory).expect("shard cache dir");
    std::fs::create_dir_all(&config.shard_session_directory).expect("shard session dir");
    Arc::new(config)
}

/// Deterministic, chunk-diverse pseudo-random bytes.
///
/// SplitMix64 so the content is reproducible across runs and varied enough that
/// the content-defined chunker produces many distinct chunks.
pub fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Upload a set of named byte blobs in one session, returning their file info.
pub async fn upload_all(
    endpoint: &str,
    cache: &Path,
    files: &[(&str, Vec<u8>)],
) -> Vec<xet_data::processing::XetFileInfo> {
    use xet_data::processing::{FileUploadSession, Sha256Policy};

    let config = client_config(endpoint, cache);
    let session = FileUploadSession::new(config)
        .await
        .expect("upload session");

    let mut infos = Vec::with_capacity(files.len());
    for (name, bytes) in files {
        let (_id, mut cleaner) = session
            .start_clean(
                Some(Arc::from(*name)),
                Some(bytes.len() as u64),
                // Compute the sha256 so the shard carries the LFS oid linkage.
                Sha256Policy::Compute,
            )
            .expect("start clean");
        cleaner.add_data(bytes).await.expect("add data");
        let (info, _metrics) = cleaner.finish().await.expect("finish file");
        infos.push(info);
    }

    session.finalize().await.expect("finalize session");
    infos
}

/// Download one file to a temp path and return its bytes.
pub async fn download_bytes(
    endpoint: &str,
    cache: &Path,
    info: &xet_data::processing::XetFileInfo,
) -> Vec<u8> {
    use xet_data::processing::FileDownloadSession;

    let config = client_config(endpoint, cache);
    let session = FileDownloadSession::new(config, None)
        .await
        .expect("download session");
    let out = cache.join(format!("download-{}", info.hash()));
    session
        .download_file(info, &out)
        .await
        .expect("download file");
    std::fs::read(&out).expect("read downloaded file")
}

/// Download a byte range of a file, returning the bytes or the client error.
pub async fn download_range(
    endpoint: &str,
    cache: &Path,
    info: &xet_data::processing::XetFileInfo,
    range: impl std::ops::RangeBounds<u64>,
) -> Result<Vec<u8>, String> {
    use xet_data::processing::FileDownloadSession;

    let config = client_config(endpoint, cache);
    let session = FileDownloadSession::new(config, None)
        .await
        .map_err(|e| e.to_string())?;

    let out = cache.join(format!("range-{}", uuid_like()));
    let writer = std::fs::File::create(&out).map_err(|e| e.to_string())?;
    session
        .download_to_writer(info, range, writer)
        .await
        .map_err(|e| e.to_string())?;
    std::fs::read(&out).map_err(|e| e.to_string())
}

/// Cheap unique suffix for scratch file names.
fn uuid_like() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}
