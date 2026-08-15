//! `candace.xetcas.v1` message types and their boundary validators.
//!
//! The types come straight out of `proto/xetcas/v1/{transfer,storage,bridge}.proto`
//! via `cargo xtask gen-proto`. Do not edit
//! `src/generated/candace.xetcas.v1.rs`: every attribute on it, including the
//! serde rules that make the JSON wire-exact, is configured in
//! `xtask/src/main.rs`.

// Generated code is exempt from this crate's lint policy: it is never edited
// by hand, so lints on it can only be fixed by changing the generator.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod generated {
    include!("../generated/candace.xetcas.v1.rs");
}

pub use generated::*;

pub mod validate;

/// `skip_serializing_if` predicate for `LfsAction.expires_in`.
///
/// Git LFS treats `expires_in` as advisory; this server omits it entirely
/// rather than advertising an expiry of zero seconds.
pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}
