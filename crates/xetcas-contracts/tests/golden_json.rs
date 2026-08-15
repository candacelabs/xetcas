//! Golden-JSON tests for the reconstruction bodies.
//!
//! The expected documents are the `openapi/cas.openapi.yaml` examples from
//! xet-core @ 77fc84d3d, quoted literally. If a serde attribute in
//! `xtask/src/main.rs` regresses, these fail before anything reaches a client.

mod common;

use serde_json::{json, Value};
use xetcas_contracts::v1::{QueryReconstructionResponse, QueryReconstructionResponseV2};

use common::{
    openapi_v1_response, openapi_v2_response, EXAMPLE_HASH, EXAMPLE_V1_URL, EXAMPLE_V2_URL,
};

/// `openapi/cas.openapi.yaml`, `GET /v1/reconstructions/{file_id}` → 200,
/// `examples.v1.value`.
fn expected_v1() -> Value {
    json!({
        "offset_into_first_range": 0,
        "terms": [
            {
                "hash": EXAMPLE_HASH,
                "unpacked_length": 263873,
                "range": { "start": 0, "end": 4 }
            }
        ],
        "fetch_info": {
            EXAMPLE_HASH: [
                {
                    "range": { "start": 0, "end": 4 },
                    "url": EXAMPLE_V1_URL,
                    "url_range": { "start": 0, "end": 131071 }
                }
            ]
        }
    })
}

/// `openapi/cas.openapi.yaml`, `GET /v2/reconstructions/{file_id}` → 200,
/// `examples.v2.value`.
fn expected_v2() -> Value {
    json!({
        "offset_into_first_range": 0,
        "terms": [
            {
                "hash": EXAMPLE_HASH,
                "unpacked_length": 263873,
                "range": { "start": 0, "end": 4 }
            }
        ],
        "xorbs": {
            EXAMPLE_HASH: [
                {
                    "url": EXAMPLE_V2_URL,
                    "ranges": [
                        {
                            "chunks": { "start": 0, "end": 4 },
                            "bytes": { "start": 0, "end": 131071 }
                        }
                    ]
                }
            ]
        }
    })
}

#[test]
fn v1_reconstruction_serializes_to_the_openapi_example() {
    let serialized = serde_json::to_value(openapi_v1_response()).expect("serialize v1");
    assert_eq!(serialized, expected_v1());
}

#[test]
fn v1_reconstruction_deserializes_from_the_openapi_example() {
    let parsed: QueryReconstructionResponse =
        serde_json::from_value(expected_v1()).expect("deserialize v1");
    assert_eq!(parsed, openapi_v1_response());
}

#[test]
fn v2_reconstruction_serializes_to_the_openapi_example() {
    let serialized = serde_json::to_value(openapi_v2_response()).expect("serialize v2");
    assert_eq!(serialized, expected_v2());
}

#[test]
fn v2_reconstruction_deserializes_from_the_openapi_example() {
    let parsed: QueryReconstructionResponseV2 =
        serde_json::from_value(expected_v2()).expect("deserialize v2");
    assert_eq!(parsed, openapi_v2_response());
}

/// The fetch_info / xorbs map values are bare arrays, never
/// `{"entries": [...]}`. This is what `#[serde(transparent)]` on the list
/// wrappers buys, and it is the single easiest thing to break.
#[test]
fn map_values_are_bare_arrays_not_wrapper_objects() {
    let v1 = serde_json::to_value(openapi_v1_response()).expect("serialize v1");
    assert!(
        v1["fetch_info"][EXAMPLE_HASH].is_array(),
        "fetch_info values must be JSON arrays, got {}",
        v1["fetch_info"][EXAMPLE_HASH]
    );

    let v2 = serde_json::to_value(openapi_v2_response()).expect("serialize v2");
    assert!(
        v2["xorbs"][EXAMPLE_HASH].is_array(),
        "xorbs values must be JSON arrays, got {}",
        v2["xorbs"][EXAMPLE_HASH]
    );
}

/// `offset_into_first_range: 0` is the common case and must still appear:
/// the OpenAPI schema marks it required with `additionalProperties: false`,
/// and the client reads it unconditionally. No skip-if-default anywhere in
/// the reconstruction bodies.
#[test]
fn zero_offset_is_present_and_terms_are_never_omitted() {
    let mut empty = openapi_v1_response();
    empty.terms.clear();
    empty.fetch_info.clear();

    let serialized = serde_json::to_value(&empty).expect("serialize");
    let object = serialized.as_object().expect("object");
    assert_eq!(object.get("offset_into_first_range"), Some(&json!(0)));
    assert_eq!(object.get("terms"), Some(&json!([])));
    assert_eq!(object.get("fetch_info"), Some(&json!({})));
    assert_eq!(object.len(), 3, "unexpected extra keys in {serialized}");
}

/// A ranged query answers with a nonzero offset; nothing about the shape
/// changes.
#[test]
fn nonzero_offset_round_trips() {
    let mut response = openapi_v1_response();
    response.offset_into_first_range = 12_345;

    let serialized = serde_json::to_value(&response).expect("serialize");
    assert_eq!(serialized["offset_into_first_range"], json!(12_345));

    let parsed: QueryReconstructionResponse =
        serde_json::from_value(serialized).expect("deserialize");
    assert_eq!(parsed, response);
}

/// The two reconstruction responses use distinct key names for the fetch
/// map (`fetch_info` vs `xorbs`); a v1 body must not parse as a v2 body's
/// shape by accident.
#[test]
fn v1_and_v2_bodies_do_not_share_a_fetch_key() {
    let v1 = serde_json::to_value(openapi_v1_response()).expect("serialize v1");
    let v2 = serde_json::to_value(openapi_v2_response()).expect("serialize v2");
    assert!(v1.get("xorbs").is_none());
    assert!(v2.get("fetch_info").is_none());
}
