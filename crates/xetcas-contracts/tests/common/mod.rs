//! Fixtures shared by the contract test binaries.
//!
//! The reconstruction fixtures are the `openapi/cas.openapi.yaml` examples
//! from xet-core @ 77fc84d3d, transcribed field for field; the LFS fixtures
//! come from `docs/research/git-xet.md` §6.

#![allow(dead_code)]

use std::collections::HashMap;

use xetcas_contracts::v1::{
    ByteRange, CasReconstructionFetchInfo, CasReconstructionTerm, FetchInfoList, IndexRange,
    QueryReconstructionResponse, QueryReconstructionResponseV2, XorbFetchList, XorbMultiRangeFetch,
    XorbRangeDescriptor,
};

/// The xorb/file hash used by both OpenAPI reconstruction examples.
pub const EXAMPLE_HASH: &str = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456";

/// `fetch_info[].url` from the v1 example.
pub const EXAMPLE_V1_URL: &str =
    "https://transfer.xethub.hf.co/xorb/default/a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456";

/// `xorbs[].url` from the v2 example (the spec's own elided placeholder).
pub const EXAMPLE_V2_URL: &str =
    "https://transfer.xethub.hf.co/xorbs/default/a1b2c3...?<signed-params>";

/// A second, distinct valid hash for multi-xorb fixtures.
pub const OTHER_HASH: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// The v1 reconstruction body from `openapi/cas.openapi.yaml` (`examples.v1`).
#[must_use]
pub fn openapi_v1_response() -> QueryReconstructionResponse {
    QueryReconstructionResponse {
        offset_into_first_range: 0,
        terms: vec![CasReconstructionTerm {
            hash: EXAMPLE_HASH.to_owned(),
            range: Some(IndexRange { start: 0, end: 4 }),
            unpacked_length: 263_873,
        }],
        fetch_info: HashMap::from([(
            EXAMPLE_HASH.to_owned(),
            FetchInfoList {
                entries: vec![CasReconstructionFetchInfo {
                    range: Some(IndexRange { start: 0, end: 4 }),
                    url: EXAMPLE_V1_URL.to_owned(),
                    url_range: Some(ByteRange {
                        start: 0,
                        end: 131_071,
                    }),
                }],
            },
        )]),
    }
}

/// The v2 reconstruction body from `openapi/cas.openapi.yaml` (`examples.v2`).
#[must_use]
pub fn openapi_v2_response() -> QueryReconstructionResponseV2 {
    QueryReconstructionResponseV2 {
        offset_into_first_range: 0,
        terms: vec![CasReconstructionTerm {
            hash: EXAMPLE_HASH.to_owned(),
            range: Some(IndexRange { start: 0, end: 4 }),
            unpacked_length: 263_873,
        }],
        xorbs: HashMap::from([(
            EXAMPLE_HASH.to_owned(),
            XorbFetchList {
                entries: vec![XorbMultiRangeFetch {
                    url: EXAMPLE_V2_URL.to_owned(),
                    ranges: vec![XorbRangeDescriptor {
                        chunks: Some(IndexRange { start: 0, end: 4 }),
                        bytes: Some(ByteRange {
                            start: 0,
                            end: 131_071,
                        }),
                    }],
                }],
            },
        )]),
    }
}
