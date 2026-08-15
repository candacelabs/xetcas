//! Git smart HTTP, bridged to `git http-backend` over CGI.
//!
//! This also owns the LFS batch dispatch. matchit (axum routing) only allows a
//! wildcard as the final path segment, so the batch route cannot be registered
//! as its own pattern ahead of the catch-all; instead the catch-all inspects
//! the suffix and hands matching POSTs to the LFS bridge before falling
//! through to git.

use std::path::Path as FsPath;
use std::process::Stdio;
use std::sync::atomic::Ordering;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_stream::StreamExt;

use crate::error::{AppError, AppResult};
use crate::routes::lfs;
use crate::state::AppState;

/// Suffix git-lfs appends to the repository URL for the batch endpoint.
const BATCH_SUFFIX: &str = "/info/lfs/objects/batch";

/// Reject anything that could escape the git root or confuse http-backend.
fn sanitize(path: &str) -> AppResult<()> {
    if path.is_empty() {
        return Err(AppError::bad_request("empty repository path"));
    }
    // axum percent-decodes the capture first, so a leading, doubled, or trailing
    // slash arrives as an EMPTY component. Left unchecked, split_repo would hand
    // Path::join an ABSOLUTE argument, which discards the base and escapes the
    // data dir. Require every component non-empty, non-dot, and in the charset.
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(AppError::bad_request(
                "empty or traversal path component rejected",
            ));
        }
        let ok = component
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok {
            return Err(AppError::bad_request(
                "illegal character in repository path",
            ));
        }
    }
    Ok(())
}

/// Split a request path into the repository prefix and the rest.
///
/// Everything up to and including the first component ending in `.git` names
/// the repository.
fn split_repo(path: &str) -> AppResult<(String, String)> {
    let mut prefix = Vec::new();
    let mut parts = path.split('/');
    for part in parts.by_ref() {
        prefix.push(part);
        if part.ends_with(".git") {
            let repo = prefix.join("/");
            let rest = parts.collect::<Vec<_>>().join("/");
            return Ok((repo, rest));
        }
    }
    Err(AppError::bad_request(
        "repository path must contain a component ending in .git",
    ))
}

/// Where a repository is initialized before it is renamed into place. A leading
/// dot keeps it out of the way of real repository names, and it lives under the
/// git root so the rename never crosses a filesystem.
const STAGING_DIR: &str = ".xetcasd-staging";

/// Whether `dir` is a repository `git http-backend` can be pointed at.
///
/// Directory existence is NOT readiness: [`autocreate`] builds the repository
/// elsewhere and renames it in, so `HEAD` is present only on a repository whose
/// init and configuration both finished. That also means a directory left
/// behind by some earlier failure is retried instead of permanently shadowing
/// creation.
async fn repo_is_ready(dir: &FsPath) -> bool {
    tokio::fs::metadata(dir.join("HEAD"))
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Create a bare repository on first touch.
///
/// Initialization happens under a staging path and the finished repository is
/// renamed into place, so the directory at `dir` appears atomically: a
/// concurrent request either sees no repository or sees a complete one, never
/// one that `git init` has created but not yet configured. A failure part-way
/// through leaves only the staging directory, which is removed.
///
/// `http.receivepack` must be enabled explicitly: without it an anonymous
/// push fails with "git-http-push failed" even though the fetch side works.
async fn autocreate(state: &AppState, repo: &str, dir: &FsPath) -> AppResult<()> {
    let staging = state
        .config
        .git_root()
        .join(STAGING_DIR)
        .join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&staging).await?;

    let built = async {
        run_git(&["init", "--bare", "-b", "main"], &staging).await?;
        run_git(&["config", "http.receivepack", "true"], &staging).await?;
        if let Some(parent) = dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Replaces an empty leftover directory; anything else is a lost race.
        tokio::fs::rename(&staging, dir)
            .await
            .map_err(AppError::from)
    }
    .await;

    if let Err(e) = built {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        // Another writer may have won the rename while we were building.
        if repo_is_ready(dir).await {
            return Ok(());
        }
        return Err(e);
    }

    let total = state.repos_created.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(repo = %repo, total, "created bare repository");
    Ok(())
}

async fn run_git(args: &[&str], dir: &FsPath) -> AppResult<()> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| AppError::internal(format!("spawn git: {e}")))?;
    if !output.status.success() {
        return Err(AppError::internal(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Catch-all for `/git/...`: LFS batch first, then git smart HTTP.
pub async fn dispatch(
    State(state): State<AppState>,
    method: Method,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> AppResult<Response> {
    sanitize(&path)?;

    if method == Method::POST {
        if let Some(repo) = path.strip_suffix(BATCH_SUFFIX) {
            let repo = repo.to_string();
            let bytes = axum::body::to_bytes(body, 8 * 1024 * 1024)
                .await
                .map_err(|e| AppError::bad_request(format!("reading batch body: {e}")))?;
            return lfs::batch(state, &repo, &headers, bytes).await;
        }
    }

    let (repo, _rest) = split_repo(&path)?;
    let root = state.config.git_root();
    let repo_dir = root.join(&repo);
    // Defense in depth: sanitize already rejects the components that could
    // escape, but never operate on a path that resolves outside the data root.
    if !repo_dir.starts_with(&root) {
        return Err(AppError::bad_request(
            "repository path escapes the data root",
        ));
    }
    if !repo_is_ready(&repo_dir).await {
        // Serialize first touch per repository. Without this, concurrent
        // requests all decide the repository is missing and race each other's
        // `git init`, and a request can reach `git http-backend` while another
        // is still building the repository it is about to be pointed at.
        let _guard = state.repo_locks.lock(&repo).await;
        if !repo_is_ready(&repo_dir).await {
            if !state.config.git_autocreate {
                return Err(AppError::NotFound);
            }
            autocreate(&state, &repo, &repo_dir).await?;
        }
    }

    cgi(&state, &method, &path, &uri, &headers, body).await
}

/// Run `git http-backend` as a CGI program and translate its output.
async fn cgi(
    state: &AppState,
    method: &Method,
    path: &str,
    uri: &Uri,
    headers: &HeaderMap,
    body: Body,
) -> AppResult<Response> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("http-backend")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GIT_PROJECT_ROOT", state.config.git_root())
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", format!("/{path}"))
        .env("REQUEST_METHOD", method.as_str())
        .env("QUERY_STRING", uri.query().unwrap_or_default())
        .env("REMOTE_ADDR", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (name, env) in [
        (axum::http::header::CONTENT_TYPE, "CONTENT_TYPE"),
        (axum::http::header::CONTENT_LENGTH, "CONTENT_LENGTH"),
        // git sends gzip-compressed request bodies; http-backend inflates them
        // itself, so the encoding just has to be passed through.
        (
            axum::http::header::CONTENT_ENCODING,
            "HTTP_CONTENT_ENCODING",
        ),
    ] {
        if let Some(value) = headers.get(&name).and_then(|v| v.to_str().ok()) {
            command.env(env, value);
        }
    }

    let mut child = command
        .spawn()
        .map_err(|e| AppError::internal(format!("spawn git http-backend: {e}")))?;

    // Feed the request body in as it arrives rather than buffering a whole
    // pack: receive-pack bodies are unbounded.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::internal("no stdin on http-backend"))?;
    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = stdin.shutdown().await;
    });

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::internal("no stdout on http-backend"))?;
    let stderr = child.stderr.take();

    tokio::spawn(async move {
        if let Some(mut stderr) = stderr {
            let mut message = String::new();
            let _ = stderr.read_to_string(&mut message).await;
            if !message.trim().is_empty() {
                tracing::warn!(stderr = %message.trim(), "git http-backend");
            }
        }
        let _ = child.wait().await;
    });

    // Read just far enough to have the whole CGI header block, then stream the
    // rest of the body straight through.
    let mut buffered = Vec::with_capacity(1024);
    let mut split = None;
    let mut scratch = [0u8; 4096];
    loop {
        if let Some(at) = find_header_end(&buffered) {
            split = Some(at);
            break;
        }
        let n = stdout
            .read(&mut scratch)
            .await
            .map_err(|e| AppError::internal(format!("read http-backend: {e}")))?;
        if n == 0 {
            break;
        }
        buffered.extend_from_slice(&scratch[..n]);
    }

    let (head, rest) = match split {
        Some((end, body_at)) => (buffered[..end].to_vec(), buffered[body_at..].to_vec()),
        None => (buffered.clone(), Vec::new()),
    };

    let mut builder = Response::builder();
    let mut status = StatusCode::OK;
    for line in String::from_utf8_lossy(&head).lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("Status") {
            let code = value.split_whitespace().next().unwrap_or("200");
            status = code
                .parse()
                .ok()
                .and_then(|c| StatusCode::from_u16(c).ok())
                .unwrap_or(StatusCode::OK);
        } else {
            builder = builder.header(name, value);
        }
    }

    let stream =
        tokio_stream::once(Ok(Bytes::from(rest))).chain(tokio_util::io::ReaderStream::new(stdout));

    builder
        .status(status)
        .body(Body::from_stream(stream))
        .map_err(|e| AppError::internal(format!("build git response: {e}")))
}

/// Locate the end of a CGI header block, returning (header_end, body_start).
///
/// http-backend emits CRLF, but tolerate bare LF too.
fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, i + 4));
    let lf = buf
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (i, i + 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_repo_from_the_rest_of_the_path() {
        let (repo, rest) = split_repo("t/x.git/info/refs").unwrap();
        assert_eq!(repo, "t/x.git");
        assert_eq!(rest, "info/refs");
    }

    #[test]
    fn requires_a_git_component() {
        assert!(split_repo("t/x/info/refs").is_err());
    }

    #[test]
    fn rejects_traversal_and_odd_characters() {
        assert!(sanitize("a/../b.git").is_err());
        assert!(sanitize("a b.git").is_err());
        assert!(sanitize("ok/repo.git").is_ok());
        // Absolute-path escape: axum decodes `%2F...` to a leading slash, which
        // splits into an empty first component and must be rejected before
        // Path::join drops the base and escapes the data dir.
        assert!(sanitize("/tmp/pwn.git/info/refs").is_err());
        assert!(sanitize("//tmp/x.git/info/refs").is_err());
        assert!(sanitize("a/./b.git/info/refs").is_err());
        assert!(sanitize("trailing/slash.git/").is_err());
        // Ordinary repository paths still pass.
        assert!(sanitize("models/demo.git/info/refs").is_ok());
        assert!(sanitize("t/x.git/git-upload-pack").is_ok());
    }

    #[test]
    fn finds_both_header_terminators() {
        assert_eq!(find_header_end(b"A: b\r\n\r\nbody"), Some((4, 8)));
        assert_eq!(find_header_end(b"A: b\n\nbody"), Some((4, 6)));
        assert_eq!(find_header_end(b"A: b"), None);
    }
}
