//! Fixed protocol strings and limits.
//!
//! Everything here is a literal the wire format pins down. Citations point at
//! the research dossiers in `docs/research/`, which in turn cite
//! `huggingface/xet-core` @ 77fc84d3d.

/// Default `{prefix}` path segment the real client sends for both
/// `POST /v1/xorbs/{prefix}/{hash}` and `GET /v1/chunks/{prefix}/{hash}`.
///
/// The OpenAPI spec documents `default-merkledb` for the dedup route, but the
/// client sends its configured `default_prefix` (this value) on both routes;
/// a server should accept any non-empty prefix.
/// (docs/research/api-surface.md §0 "Prefix/key convention", §5 gotcha 9.)
pub const PREFIX_DEFAULT: &str = "default";

/// Header carrying the CAS endpoint base URL (no trailing slash) in a Git LFS
/// batch upload action. git-xet looks this key up with exact-case matching.
/// (docs/research/git-xet.md §4, §6.2.)
pub const HEADER_XET_CAS_URL: &str = "X-Xet-Cas-Url";

/// Header carrying the opaque bearer token git-xet replays toward CAS.
/// (docs/research/git-xet.md §4, §6.2.)
pub const HEADER_XET_ACCESS_TOKEN: &str = "X-Xet-Access-Token";

/// Header carrying the token expiry as decimal unix seconds **in a JSON
/// string**; git-xet parses it with `u64::from_str` and fails the transfer if
/// it does not parse. (docs/research/git-xet.md §4.)
pub const HEADER_XET_TOKEN_EXPIRATION: &str = "X-Xet-Token-Expiration";

/// Optional session-correlation id. When the batch action supplies it,
/// git-xet sets it as the session id and the CAS client attaches it to every
/// outbound request as this same header.
/// (docs/research/git-xet.md §4; docs/research/api-surface.md §0.)
pub const HEADER_XET_SESSION_ID: &str = "X-Xet-Session-Id";

/// Optional CAS response header, read by the client only for logging.
/// (docs/research/api-surface.md §0 "Common response headers".)
pub const HEADER_REQUEST_ID: &str = "X-Request-Id";

/// Content type of every Git LFS batch request and response body.
/// (docs/research/git-xet.md §0; Git LFS batch API.)
pub const LFS_CONTENT_TYPE: &str = "application/vnd.git-lfs+json";

/// Transfer adapter name git-xet registers with git-lfs. A server answers
/// with this only for `"operation": "upload"` batches.
/// (docs/research/git-xet.md §0, §6.2.)
pub const TRANSFER_XET: &str = "xet";

/// Stock git-lfs transfer adapter. Every download batch must be answered with
/// this: git-xet's `init_download` hard-fails when `"xet"` is selected.
/// (docs/research/git-xet.md §6.2, §7.)
pub const TRANSFER_BASIC: &str = "basic";

/// The only hash algorithm git-lfs sends, and the only one this contract
/// accepts. (bridge.proto `LfsBatchRequest.hash_algo`.)
pub const HASH_ALGO_SHA256: &str = "sha256";

/// Maximum number of chunk frames in one xorb.
/// (docs/research/api-surface.md §1.5, citing
/// `xet_core_structures/src/xorb_object/constants.rs`.)
pub const MAX_CHUNKS_PER_XORB: u32 = 8192;

/// Maximum total uncompressed bytes in one xorb: 64 MiB.
/// (docs/research/api-surface.md §1.5.)
pub const MAX_XORB_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum uncompressed bytes in one chunk: 128 KiB.
/// (docs/research/api-surface.md §1.5.)
pub const MAX_CHUNK_UNPACKED_BYTES: u32 = 131_072;

/// Length in bytes of a raw xet-core hash (blake3 digest) as stored in
/// [`crate::v1::XorbRecord::chunk_hashes`] and
/// [`crate::v1::FileRecord::verification_range_hashes`].
pub const HASH_BYTES: usize = 32;

/// Length of a hash rendered as lowercase hex on the wire.
/// (docs/research/api-surface.md §0 "Hashes on the wire".)
pub const HASH_HEX_LEN: usize = 2 * HASH_BYTES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorb_limits_match_the_proto_refinements() {
        // storage.proto pins these same numbers in Liquid Proto exprs:
        // `num_chunks`: "this >= 1 && this <= 8192"
        // `unpacked_length`: "this <= 67108864"
        assert_eq!(MAX_CHUNKS_PER_XORB, 8192);
        assert_eq!(MAX_XORB_UNPACKED_BYTES, 67_108_864);
    }

    #[test]
    fn hash_hex_length_is_the_documented_64() {
        assert_eq!(HASH_HEX_LEN, 64);
    }
}
