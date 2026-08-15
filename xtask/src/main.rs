//! Repository automation for the xetcas workspace.
//!
//! `cargo xtask gen-proto` compiles `proto/xetcas/v1/*.proto` with
//! [`protox`] (a pure-Rust protobuf compiler, so no `protoc` binary is needed)
//! and renders Rust types with [`prost_build`]. The single generated file is
//! committed at
//! `crates/xetcas-contracts/src/generated/candace.xetcas.v1.rs`; every serde
//! attribute that makes the types wire-exact is configured here, never
//! hand-edited into the output.
//!
//! `cargo xtask gen-proto --check` regenerates into a temporary directory and
//! exits non-zero when the committed file has drifted.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Proto files compiled into the contracts crate, relative to `proto/`.
const PROTO_FILES: &[&str] = &[
    "xetcas/v1/transfer.proto",
    "xetcas/v1/storage.proto",
    "xetcas/v1/bridge.proto",
];

/// prost writes one file per protobuf package; this is the only package we
/// keep (the imported `candace.liquid.v1` and `google.protobuf` descriptors
/// are compiled for option resolution only).
const GENERATED_BASENAME: &str = "candace.xetcas.v1.rs";

/// Committed location of the generated file, relative to the workspace root.
const GENERATED_PATH: &str = "crates/xetcas-contracts/src/generated/candace.xetcas.v1.rs";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match refs.as_slice() {
        ["gen-proto"] => gen_proto(Mode::Write),
        ["gen-proto", "--check"] => gen_proto(Mode::Check),
        _ => {
            eprintln!("usage: cargo xtask gen-proto [--check]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Check,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest always lives one level below the workspace root")
        .to_path_buf()
}

fn gen_proto(mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let proto_dir = root.join("proto");
    let vendor_dir = proto_dir.join("vendor");

    let files: Vec<PathBuf> = PROTO_FILES
        .iter()
        .map(|file| proto_dir.join(file))
        .collect();
    // `proto/` resolves the xetcas contracts themselves; `proto/vendor/`
    // resolves `liquidproto/v1/refinement.proto`, the vendored copy of the
    // candacelib custom option. protox reads the extension as a real
    // descriptor extension, so the annotations stay in the source of truth.
    let includes = [proto_dir.clone(), vendor_dir];
    let descriptors = protox::compile(&files, includes)?;

    let staging = TempDir::new(&root.join("target"), "xetcas-gen-proto")?;
    let mut config = prost_build::Config::new();
    config.out_dir(staging.path());
    configure(&mut config);
    config.compile_fds(descriptors)?;

    let generated = staging.path().join(GENERATED_BASENAME);
    let rendered = fs::read_to_string(&generated)
        .map_err(|error| format!("prost did not emit {}: {error}", generated.display()))?;

    let committed_path = root.join(GENERATED_PATH);
    match mode {
        Mode::Write => {
            if let Some(parent) = committed_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let unchanged = fs::read_to_string(&committed_path)
                .map(|existing| existing == rendered)
                .unwrap_or(false);
            if unchanged {
                println!("gen-proto: {GENERATED_PATH} already up to date");
            } else {
                fs::write(&committed_path, &rendered)?;
                println!("gen-proto: wrote {GENERATED_PATH}");
            }
            Ok(())
        }
        Mode::Check => {
            let committed = fs::read_to_string(&committed_path)
                .map_err(|error| format!("cannot read {}: {error}", committed_path.display()))?;
            if committed == rendered {
                println!("gen-proto --check: {GENERATED_PATH} is up to date");
                Ok(())
            } else {
                Err(drift_report(&committed, &rendered).into())
            }
        }
    }
}

/// Every wire-shape decision lives here. The protobuf schema is the source of
/// truth for field names and numbers; these attributes carry the JSON
/// omission and casing rules the real xet-core clients require.
fn configure(config: &mut prost_build::Config) {
    config.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");

    // --- transfer.proto -----------------------------------------------
    // fetch_info / xorbs map values are JSON arrays, not objects wrapping an
    // "entries" key: `{"<xorb hash>": [ {...}, {...} ]}`.
    // (docs/research/api-surface.md §1.1, §1.2.)
    config.type_attribute(".candace.xetcas.v1.FetchInfoList", "#[serde(transparent)]");
    config.type_attribute(".candace.xetcas.v1.XorbFetchList", "#[serde(transparent)]");

    // Reconstruction bodies deliberately have NO skip-if-default rules:
    // `offset_into_first_range: 0` must appear, and `terms`/`fetch_info` are
    // required by the OpenAPI schema (additionalProperties: false).

    // --- bridge.proto -------------------------------------------------
    // CasTokenInfo is the one camelCase body in the package; it must match
    // xet-core's CasJWTInfo exactly: {"casUrl","exp","accessToken"}.
    // (docs/research/git-xet.md §5.2.)
    config.type_attribute(
        ".candace.xetcas.v1.CasTokenInfo",
        "#[serde(rename_all = \"camelCase\")]",
    );

    // Git LFS batch requests: git-lfs omits these keys freely.
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchRequest.ref",
        "#[serde(default, rename = \"ref\", skip_serializing_if = \"Option::is_none\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchRequest.transfers",
        "#[serde(default)]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchRequest.hash_algo",
        "#[serde(default)]",
    );

    // Git LFS batch responses: empty optional members are omitted, never
    // emitted as null/[]/{}. An upload entry with no `actions` is the
    // "server already has this object, skip it" signal, so the key must be
    // absent rather than an empty object.
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchObject.authenticated",
        "#[serde(default, skip_serializing_if = \"std::ops::Not::not\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchObject.actions",
        "#[serde(default, skip_serializing_if = \"std::collections::HashMap::is_empty\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchObject.error",
        "#[serde(default, skip_serializing_if = \"Option::is_none\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsBatchResponse.hash_algo",
        "#[serde(default, skip_serializing_if = \"String::is_empty\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsAction.header",
        "#[serde(default, skip_serializing_if = \"std::collections::HashMap::is_empty\")]",
    );
    config.field_attribute(
        ".candace.xetcas.v1.LfsAction.expires_in",
        "#[serde(default, skip_serializing_if = \"crate::v1::is_zero_u64\")]",
    );
}

fn drift_report(committed: &str, rendered: &str) -> String {
    let mut report =
        format!("{GENERATED_PATH} is stale; run `cargo xtask gen-proto`.\nfirst differences:\n");
    let committed_lines: Vec<&str> = committed.lines().collect();
    let rendered_lines: Vec<&str> = rendered.lines().collect();
    let mut shown = 0;
    for index in 0..committed_lines.len().max(rendered_lines.len()) {
        let old = committed_lines.get(index).copied();
        let new = rendered_lines.get(index).copied();
        if old == new {
            continue;
        }
        report.push_str(&format!("  line {}:\n", index + 1));
        report.push_str(&format!("    committed: {}\n", old.unwrap_or("<eof>")));
        report.push_str(&format!("    generated: {}\n", new.unwrap_or("<eof>")));
        shown += 1;
        if shown == 10 {
            report.push_str("  ... (truncated)\n");
            break;
        }
    }
    report
}

/// Minimal scoped temporary directory: the workspace has no `tempfile`
/// dependency and one directory under `target/` is all codegen needs.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(parent: &Path, prefix: &str) -> io::Result<Self> {
        fs::create_dir_all(parent)?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = parent.join(format!("{prefix}-{}-{unique}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
