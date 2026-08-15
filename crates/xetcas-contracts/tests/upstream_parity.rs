//! Round-trip parity against the published `xet-client` 1.6.0 types.
//!
//! The golden-JSON tests prove we match the OpenAPI *examples*. These prove
//! we match the *client that will actually be talking to the server*: every
//! body we emit deserializes into the real `xet_client` type, and every body
//! the real type emits deserializes back into ours with the same meaning.
//!
//! Upstream module paths (checked against xet-core @ 77fc84d3d):
//! * `xet_client::cas_types::{QueryReconstructionResponse,
//!   QueryReconstructionResponseV2, UploadXorbResponse, UploadShardResponse,
//!   UploadShardResponseType}`
//! * `xet_client::hub_client::CasJWTInfo` (defined in the crate's private
//!   `hub_client::types`, re-exported from `hub_client`)

mod common;

use serde_json::json;
use xet_client::cas_types::{
    QueryReconstructionResponse as UpstreamV1, QueryReconstructionResponseV2 as UpstreamV2,
    UploadShardResponse as UpstreamShard, UploadShardResponseType,
    UploadXorbResponse as UpstreamXorb,
};
use xet_client::hub_client::CasJWTInfo;
use xetcas_contracts::v1::{
    CasTokenInfo, QueryReconstructionResponse, QueryReconstructionResponseV2, UploadShardResponse,
    UploadXorbResponse,
};

use common::{openapi_v1_response, openapi_v2_response, EXAMPLE_HASH, EXAMPLE_V1_URL};

/// Flattens a v1 body into comparable primitives so the two type families can
/// be compared without sharing a single type.
#[derive(Debug, PartialEq, Eq)]
struct V1Shape {
    offset: u64,
    terms: Vec<(String, u32, u32, u32)>,
    fetch: Vec<(String, u32, u32, String, u64, u64)>,
}

fn ours_v1_shape(value: &QueryReconstructionResponse) -> V1Shape {
    let mut terms: Vec<_> = value
        .terms
        .iter()
        .map(|term| {
            let range = term.range.expect("term range is required");
            (
                term.hash.clone(),
                range.start,
                range.end,
                term.unpacked_length,
            )
        })
        .collect();
    terms.sort();

    let mut fetch: Vec<_> = value
        .fetch_info
        .iter()
        .flat_map(|(hash, list)| {
            list.entries.iter().map(move |entry| {
                let range = entry.range.expect("fetch range is required");
                let url_range = entry.url_range.expect("fetch url_range is required");
                (
                    hash.clone(),
                    range.start,
                    range.end,
                    entry.url.clone(),
                    url_range.start,
                    url_range.end,
                )
            })
        })
        .collect();
    fetch.sort();

    V1Shape {
        offset: value.offset_into_first_range,
        terms,
        fetch,
    }
}

fn upstream_v1_shape(value: &UpstreamV1) -> V1Shape {
    let mut terms: Vec<_> = value
        .terms
        .iter()
        .map(|term| {
            (
                term.hash.to_string(),
                term.range.start,
                term.range.end,
                term.unpacked_length,
            )
        })
        .collect();
    terms.sort();

    let mut fetch: Vec<_> = value
        .fetch_info
        .iter()
        .flat_map(|(hash, entries)| {
            entries.iter().map(move |entry| {
                (
                    hash.to_string(),
                    entry.range.start,
                    entry.range.end,
                    entry.url.clone(),
                    entry.url_range.start,
                    entry.url_range.end,
                )
            })
        })
        .collect();
    fetch.sort();

    V1Shape {
        offset: value.offset_into_first_range,
        terms,
        fetch,
    }
}

/// Flattened v2 body: terms plus one row per (xorb, url, chunk range, byte range).
#[derive(Debug, PartialEq, Eq)]
struct V2Shape {
    offset: u64,
    terms: Vec<(String, u32, u32, u32)>,
    xorbs: Vec<(String, String, u32, u32, u64, u64)>,
}

fn ours_v2_shape(value: &QueryReconstructionResponseV2) -> V2Shape {
    let mut terms: Vec<_> = value
        .terms
        .iter()
        .map(|term| {
            let range = term.range.expect("term range is required");
            (
                term.hash.clone(),
                range.start,
                range.end,
                term.unpacked_length,
            )
        })
        .collect();
    terms.sort();

    let mut xorbs: Vec<_> = value
        .xorbs
        .iter()
        .flat_map(|(hash, list)| {
            list.entries.iter().flat_map(move |fetch| {
                fetch.ranges.iter().map(move |descriptor| {
                    let chunks = descriptor.chunks.expect("chunks are required");
                    let bytes = descriptor.bytes.expect("bytes are required");
                    (
                        hash.clone(),
                        fetch.url.clone(),
                        chunks.start,
                        chunks.end,
                        bytes.start,
                        bytes.end,
                    )
                })
            })
        })
        .collect();
    xorbs.sort();

    V2Shape {
        offset: value.offset_into_first_range,
        terms,
        xorbs,
    }
}

fn upstream_v2_shape(value: &UpstreamV2) -> V2Shape {
    let mut terms: Vec<_> = value
        .terms
        .iter()
        .map(|term| {
            (
                term.hash.to_string(),
                term.range.start,
                term.range.end,
                term.unpacked_length,
            )
        })
        .collect();
    terms.sort();

    let mut xorbs: Vec<_> = value
        .xorbs
        .iter()
        .flat_map(|(hash, entries)| {
            entries.iter().flat_map(move |fetch| {
                fetch.ranges.iter().map(move |descriptor| {
                    (
                        hash.to_string(),
                        fetch.url.clone(),
                        descriptor.chunks.start,
                        descriptor.chunks.end,
                        descriptor.bytes.start,
                        descriptor.bytes.end,
                    )
                })
            })
        })
        .collect();
    xorbs.sort();

    V2Shape {
        offset: value.offset_into_first_range,
        terms,
        xorbs,
    }
}

#[test]
fn v1_reconstruction_round_trips_through_the_real_client_type() {
    let ours = openapi_v1_response();
    let encoded = serde_json::to_string(&ours).expect("serialize ours");

    // Forward: what we put on the wire is what the client parses.
    let upstream: UpstreamV1 = serde_json::from_str(&encoded).expect("client parses our body");
    assert_eq!(ours_v1_shape(&ours), upstream_v1_shape(&upstream));

    // Reverse: what the client would emit parses back into our type unchanged.
    let re_encoded = serde_json::to_string(&upstream).expect("serialize upstream");
    let back: QueryReconstructionResponse =
        serde_json::from_str(&re_encoded).expect("we parse the client's body");
    assert_eq!(back, ours);
}

#[test]
fn v2_reconstruction_round_trips_through_the_real_client_type() {
    let ours = openapi_v2_response();
    let encoded = serde_json::to_string(&ours).expect("serialize ours");

    let upstream: UpstreamV2 = serde_json::from_str(&encoded).expect("client parses our body");
    assert_eq!(ours_v2_shape(&ours), upstream_v2_shape(&upstream));

    let re_encoded = serde_json::to_string(&upstream).expect("serialize upstream");
    let back: QueryReconstructionResponseV2 =
        serde_json::from_str(&re_encoded).expect("we parse the client's body");
    assert_eq!(back, ours);
}

/// A multi-term, multi-xorb body: term ordering is significant to the client,
/// so it must survive the round trip in order.
#[test]
fn multi_xorb_v1_reconstruction_preserves_term_order() {
    use std::collections::HashMap;
    use xetcas_contracts::v1::{
        ByteRange, CasReconstructionFetchInfo, CasReconstructionTerm, FetchInfoList, IndexRange,
    };

    let ours = QueryReconstructionResponse {
        offset_into_first_range: 7,
        terms: vec![
            CasReconstructionTerm {
                hash: common::OTHER_HASH.to_owned(),
                range: Some(IndexRange { start: 2, end: 5 }),
                unpacked_length: 4096,
            },
            CasReconstructionTerm {
                hash: EXAMPLE_HASH.to_owned(),
                range: Some(IndexRange { start: 0, end: 4 }),
                unpacked_length: 263_873,
            },
        ],
        fetch_info: HashMap::from([
            (
                common::OTHER_HASH.to_owned(),
                FetchInfoList {
                    entries: vec![CasReconstructionFetchInfo {
                        range: Some(IndexRange { start: 2, end: 5 }),
                        url: "https://transfer.example/a".to_owned(),
                        url_range: Some(ByteRange {
                            start: 100,
                            end: 4195,
                        }),
                    }],
                },
            ),
            (
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
            ),
        ]),
    };

    let encoded = serde_json::to_string(&ours).expect("serialize ours");
    let upstream: UpstreamV1 = serde_json::from_str(&encoded).expect("client parses our body");

    assert_eq!(upstream.offset_into_first_range, 7);
    let order: Vec<String> = upstream
        .terms
        .iter()
        .map(|term| term.hash.to_string())
        .collect();
    assert_eq!(order, vec![common::OTHER_HASH, EXAMPLE_HASH]);
    assert_eq!(ours_v1_shape(&ours), upstream_v1_shape(&upstream));
}

#[test]
fn upload_xorb_response_matches_upstream() {
    for was_inserted in [true, false] {
        let ours = UploadXorbResponse { was_inserted };
        let encoded = serde_json::to_string(&ours).expect("serialize ours");
        assert_eq!(encoded, format!("{{\"was_inserted\":{was_inserted}}}"));

        let upstream: UpstreamXorb = serde_json::from_str(&encoded).expect("client parses");
        assert_eq!(upstream.was_inserted, was_inserted);

        let back: UploadXorbResponse =
            serde_json::from_str(&serde_json::to_string(&upstream).expect("serialize upstream"))
                .expect("we parse the client's body");
        assert_eq!(back, ours);
    }
}

#[test]
fn upload_shard_response_matches_upstreams_numeric_repr() {
    // Upstream serializes the enum with serde_repr, so the wire is a bare
    // integer: 0 = Exists, 1 = SyncPerformed.
    for (result, expected) in [
        (0_u32, UploadShardResponseType::Exists),
        (1_u32, UploadShardResponseType::SyncPerformed),
    ] {
        let ours = UploadShardResponse { result };
        let encoded = serde_json::to_string(&ours).expect("serialize ours");
        assert_eq!(encoded, format!("{{\"result\":{result}}}"));

        let upstream: UpstreamShard = serde_json::from_str(&encoded).expect("client parses");
        assert_eq!(upstream.result, expected);

        let back: UploadShardResponse =
            serde_json::from_str(&serde_json::to_string(&upstream).expect("serialize upstream"))
                .expect("we parse the client's body");
        assert_eq!(back, ours);
    }
}

/// The `result <= 1` refinement is not arbitrary: upstream's `serde_repr`
/// enum has exactly two variants and rejects anything else outright.
#[test]
fn upstream_rejects_a_shard_result_our_refinement_also_rejects() {
    let out_of_range = serde_json::to_string(&UploadShardResponse { result: 2 })
        .expect("serialize an out-of-contract value");
    assert!(serde_json::from_str::<UpstreamShard>(&out_of_range).is_err());
    assert!(
        xetcas_contracts::v1::validate::validate_upload_shard_response(&UploadShardResponse {
            result: 2
        })
        .is_err()
    );
}

#[test]
fn cas_token_info_matches_upstream_cas_jwt_info() {
    let ours = CasTokenInfo {
        cas_url: "https://cas-server.xethub.hf.co".to_owned(),
        exp: 1_756_489_133,
        access_token: "ey...jQ".to_owned(),
    };

    let encoded = serde_json::to_value(&ours).expect("serialize ours");
    // camelCase, exactly three keys: this is git-xet's token refresh body.
    assert_eq!(
        encoded,
        json!({
            "casUrl": "https://cas-server.xethub.hf.co",
            "exp": 1_756_489_133,
            "accessToken": "ey...jQ"
        })
    );

    let upstream: CasJWTInfo =
        serde_json::from_value(encoded).expect("client parses our token body");
    assert_eq!(upstream.cas_url, ours.cas_url);
    assert_eq!(upstream.exp, ours.exp);
    assert_eq!(upstream.access_token, ours.access_token);

    // Upstream's CasJWTInfo derives Deserialize only, so the reverse
    // direction is pinned against the literal from docs/research/git-xet.md
    // §5.2 (itself the exact sample asserted by
    // xet_client/src/hub_client/types.rs:84-95).
    let pinned =
        r#"{"casUrl":"https://cas-server.xethub.hf.co","exp":1756489133,"accessToken":"ey...jQ"}"#;
    let back: CasTokenInfo = serde_json::from_str(pinned).expect("we parse the pinned literal");
    assert_eq!(back, ours);
    let upstream_from_pinned: CasJWTInfo =
        serde_json::from_str(pinned).expect("client parses the pinned literal");
    assert_eq!(upstream_from_pinned.access_token, ours.access_token);
}
