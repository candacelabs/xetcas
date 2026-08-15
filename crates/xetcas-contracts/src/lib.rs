//! Wire-exact contracts for the Xet CAS storage/transfer protocol.
//!
//! The protobuf schemas under `proto/xetcas/v1/` are the source of truth for
//! this crate; the Rust types in [`v1`] are generated from them by
//! `cargo xtask gen-proto` and committed. This crate adds two things the
//! schema cannot express on its own:
//!
//! * **serde attributes** that make JSON serialization byte-compatible with
//!   the real `xet-core` client (see `xtask/src/main.rs`, where every
//!   attribute is configured and justified).
//! * **boundary validators** ([`v1::validate`]) mirroring the Liquid Proto
//!   `expr` refinements plus the cross-field invariants the proto grammar
//!   cannot state.
//!
//! Nothing here performs I/O. A server uses [`v1`] for its request/response
//! bodies, calls the matching `validate_*` function at every trust boundary,
//! and reads fixed protocol strings from [`constants`].

#![deny(missing_docs)]

pub mod constants;
pub mod v1;

pub use v1::validate::ValidationError;
