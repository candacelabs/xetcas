//! `xetcasd` -- a self-hosted Xet CAS server.
//!
//! The HTTP surface is the one the real `xet-core` client speaks (xorb upload,
//! shard upload, reconstruction, global chunk dedup), plus a Git smart-HTTP and
//! Git LFS bridge that lets stock `git push` with git-lfs and git-xet store its
//! objects here. Every wire body uses the generated contract types from
//! `xetcas-contracts`; every binary format is parsed and produced by upstream's
//! `xet-core-structures`.
//!
//! The protocol details this implements are documented in `docs/research/`.

#![deny(missing_docs)]

pub mod config;
pub mod dedup_shard;
pub mod error;
pub mod filestream;
pub mod http_range;
pub mod index;
pub mod reconstruction;
pub mod routes;
pub mod state;
pub mod xorbstore;

/// Bind the configured address and serve until the process is asked to stop.
pub async fn serve(config: config::Config) -> Result<(), error::AppError> {
    let listen = config.listen.clone();
    let state = state::AppState::new(config).await?;
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| error::AppError::internal(format!("bind {listen}: {e}")))?;

    let local = listener
        .local_addr()
        .map_err(|e| error::AppError::internal(format!("local_addr: {e}")))?;
    tracing::info!(
        address = %local,
        public_url = %state.config.public_base(),
        data_dir = %state.config.data_dir.display(),
        "xetcasd listening"
    );

    axum::serve(listener, routes::router(state))
        .await
        .map_err(|e| error::AppError::internal(format!("serve: {e}")))
}

/// Current unix time in seconds.
///
/// Shared because xorb records, file records and minted tokens must all stamp
/// the same clock.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
