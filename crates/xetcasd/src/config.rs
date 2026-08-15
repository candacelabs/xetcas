//! Server configuration.
//!
//! Every field has both a CLI flag and an environment variable; the demo and
//! the container image drive the server purely through the environment.

use std::path::PathBuf;

use clap::Parser;

/// Runtime configuration for `xetcasd`.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "xetcasd",
    about = "Self-hosted Xet CAS server with a Git/LFS bridge."
)]
pub struct Config {
    /// Root directory for xorb objects, the SQLite index, and bare git repos.
    #[arg(long, env = "XETCAS_DATA_DIR", default_value = "./xetcas-data")]
    pub data_dir: PathBuf,

    /// Address to bind, e.g. `0.0.0.0:8080`.
    #[arg(long, env = "XETCAS_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: String,

    /// Base URL clients reach this server on. Every URL the server hands out
    /// (reconstruction fetch URLs, LFS hrefs, the CAS URL in batch headers) is
    /// minted from this, so it must be reachable by the client, not by the
    /// server itself.
    #[arg(
        long,
        env = "XETCAS_PUBLIC_URL",
        default_value = "http://localhost:8080"
    )]
    pub public_url: String,

    /// Directory holding bare git repositories. Defaults to `<data_dir>/git`.
    #[arg(long, env = "XETCAS_GIT_ROOT")]
    pub git_root: Option<PathBuf>,

    /// Create a bare repo on first push/fetch of an unknown path.
    #[arg(long, env = "XETCAS_GIT_AUTOCREATE", default_value_t = true, action = clap::ArgAction::Set)]
    pub git_autocreate: bool,

    /// Optional static bearer token. Unset means fully permissive: authentication
    /// is deliberately out of scope for this server. When set it is required on
    /// CAS write routes and is minted into LFS batch headers.
    #[arg(long, env = "XETCAS_TOKEN")]
    pub token: Option<String>,
}

impl Config {
    /// Resolved git root.
    pub fn git_root(&self) -> PathBuf {
        self.git_root
            .clone()
            .unwrap_or_else(|| self.data_dir.join("git"))
    }

    /// Public base URL with any trailing slashes removed.
    ///
    /// xet-core builds request URLs by string concatenation, so a trailing
    /// slash would produce `//v1/...` (docs/research/api-surface.md, "Base URL").
    pub fn public_base(&self) -> &str {
        self.public_url.trim_end_matches('/')
    }

    /// Directory holding xorb objects.
    pub fn xorb_dir(&self) -> PathBuf {
        self.data_dir.join("xorbs")
    }

    /// Staging directory for atomic object writes. A sibling of the object
    /// tree so temporary files never count toward stored bytes.
    pub fn staging_dir(&self) -> PathBuf {
        self.data_dir.join("staging")
    }

    /// Path to the SQLite index.
    pub fn index_path(&self) -> PathBuf {
        self.data_dir.join("index.sqlite")
    }

    /// The token a client must present, if any.
    pub fn required_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Token handed to git-xet through the LFS batch response. Falls back to a
    /// placeholder when no token is configured: git-xet requires the header to
    /// be present and non-empty even when the server ignores its value
    /// (docs/research/git-xet.md, section 4).
    pub fn advertised_token(&self) -> &str {
        self.token.as_deref().unwrap_or("anonymous")
    }
}
