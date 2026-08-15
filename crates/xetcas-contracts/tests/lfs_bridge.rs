//! Git LFS batch-API fixtures.
//!
//! Fixtures follow `docs/research/git-xet.md` §6 ("Minimum surface for a
//! NON-HuggingFace server"). Two properties matter beyond field names:
//!
//! * git-xet does **exact-case** map lookups on the action `header` keys, so
//!   the `X-Xet-*` spellings are load-bearing.
//! * empty optional members must be **absent**, not `null`/`[]`/`{}`. An
//!   upload entry with no `actions` is the "server already has this object,
//!   skip it" signal, so an empty `actions: {}` would change the meaning.

mod common;

use std::collections::HashMap;

use serde_json::{json, Value};
use xetcas_contracts::constants::{
    HASH_ALGO_SHA256, HEADER_XET_ACCESS_TOKEN, HEADER_XET_CAS_URL, HEADER_XET_SESSION_ID,
    HEADER_XET_TOKEN_EXPIRATION, LFS_CONTENT_TYPE, TRANSFER_BASIC, TRANSFER_XET,
};
use xetcas_contracts::v1::validate::{validate_lfs_batch_request, validate_lfs_batch_response};
use xetcas_contracts::v1::{
    LfsAction, LfsBatchObject, LfsBatchRequest, LfsBatchResponse, LfsObjectError, LfsObjectSpec,
    LfsRef,
};

use common::{EXAMPLE_HASH, OTHER_HASH};

const TOKEN_REFRESH_HREF: &str = "https://git.example/candace/models.git/info/lfs/xet-token";
const CAS_URL: &str = "https://cas.example";
const ACCESS_TOKEN: &str = "ey...jQ";
const TOKEN_EXPIRATION: &str = "1756489133";
const SESSION_ID: &str = "01930000-0000-7000-8000-000000000000";

/// A realistic upload batch as git-lfs sends it when git-xet is registered.
const UPLOAD_BATCH_REQUEST: &str = r#"{
  "operation": "upload",
  "transfers": ["xet", "basic"],
  "ref": { "name": "refs/heads/main" },
  "objects": [
    { "oid": "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456", "size": 1048576 },
    { "oid": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff", "size": 42 }
  ],
  "hash_algo": "sha256"
}"#;

#[test]
fn upload_batch_request_deserializes() {
    let request: LfsBatchRequest =
        serde_json::from_str(UPLOAD_BATCH_REQUEST).expect("parse batch request");

    assert_eq!(request.operation, "upload");
    assert_eq!(request.transfers, vec![TRANSFER_XET, TRANSFER_BASIC]);
    assert_eq!(
        request.r#ref,
        Some(LfsRef {
            name: "refs/heads/main".to_owned()
        })
    );
    assert_eq!(request.hash_algo, HASH_ALGO_SHA256);
    assert_eq!(
        request.objects,
        vec![
            LfsObjectSpec {
                oid: EXAMPLE_HASH.to_owned(),
                size: 1_048_576,
            },
            LfsObjectSpec {
                oid: OTHER_HASH.to_owned(),
                size: 42,
            },
        ]
    );
    validate_lfs_batch_request(&request).expect("the fixture is a valid batch request");
}

/// git-lfs omits `transfers`, `ref`, and `hash_algo` freely; absent
/// `transfers` means `["basic"]` per the LFS spec, which is the server's
/// decision, not a parse error.
#[test]
fn minimal_batch_request_tolerates_absent_optional_keys() {
    let minimal = json!({
        "operation": "download",
        "objects": [{ "oid": EXAMPLE_HASH, "size": 7 }]
    });

    let request: LfsBatchRequest =
        serde_json::from_value(minimal).expect("parse minimal batch request");
    assert_eq!(request.operation, "download");
    assert!(request.transfers.is_empty());
    assert_eq!(request.r#ref, None);
    assert_eq!(request.hash_algo, "");
    validate_lfs_batch_request(&request).expect("absent hash_algo is legal");
}

/// The `ref` key is a Rust keyword; it must still be spelled `ref` on the wire.
#[test]
fn ref_field_uses_the_wire_spelling() {
    let request = LfsBatchRequest {
        operation: "upload".to_owned(),
        transfers: vec![TRANSFER_XET.to_owned()],
        r#ref: Some(LfsRef {
            name: "refs/heads/main".to_owned(),
        }),
        objects: vec![LfsObjectSpec {
            oid: EXAMPLE_HASH.to_owned(),
            size: 1,
        }],
        hash_algo: HASH_ALGO_SHA256.to_owned(),
    };

    let serialized = serde_json::to_value(&request).expect("serialize");
    assert_eq!(serialized["ref"], json!({ "name": "refs/heads/main" }));
    assert!(serialized.get("r#ref").is_none());
}

fn xet_upload_response() -> LfsBatchResponse {
    LfsBatchResponse {
        transfer: TRANSFER_XET.to_owned(),
        objects: vec![LfsBatchObject {
            oid: EXAMPLE_HASH.to_owned(),
            size: 1_048_576,
            authenticated: true,
            actions: HashMap::from([(
                "upload".to_owned(),
                LfsAction {
                    href: TOKEN_REFRESH_HREF.to_owned(),
                    header: HashMap::from([
                        (HEADER_XET_CAS_URL.to_owned(), CAS_URL.to_owned()),
                        (HEADER_XET_ACCESS_TOKEN.to_owned(), ACCESS_TOKEN.to_owned()),
                        (
                            HEADER_XET_TOKEN_EXPIRATION.to_owned(),
                            TOKEN_EXPIRATION.to_owned(),
                        ),
                        (HEADER_XET_SESSION_ID.to_owned(), SESSION_ID.to_owned()),
                    ]),
                    expires_in: 3600,
                },
            )]),
            error: None,
        }],
        hash_algo: HASH_ALGO_SHA256.to_owned(),
    }
}

#[test]
fn xet_upload_batch_response_serializes_exactly() {
    let expected: Value = json!({
        "transfer": "xet",
        "objects": [
            {
                "oid": EXAMPLE_HASH,
                "size": 1_048_576,
                "authenticated": true,
                "actions": {
                    "upload": {
                        "href": TOKEN_REFRESH_HREF,
                        "header": {
                            "X-Xet-Cas-Url": CAS_URL,
                            "X-Xet-Access-Token": ACCESS_TOKEN,
                            "X-Xet-Token-Expiration": TOKEN_EXPIRATION,
                            "X-Xet-Session-Id": SESSION_ID
                        },
                        "expires_in": 3600
                    }
                }
            }
        ],
        "hash_algo": "sha256"
    });

    let response = xet_upload_response();
    assert_eq!(
        serde_json::to_value(&response).expect("serialize"),
        expected
    );
    validate_lfs_batch_response(&response).expect("the fixture is a valid batch response");

    let parsed: LfsBatchResponse = serde_json::from_value(expected).expect("deserialize");
    assert_eq!(parsed, response);
}

/// The exact header spellings git-xet looks up (`git_xet/src/constants.rs`).
/// A casing change here breaks `git push` with "Hugging Face Hub didn't
/// provide a CAS URL".
#[test]
fn action_headers_use_the_exact_casing_git_xet_looks_up() {
    let serialized = serde_json::to_value(xet_upload_response()).expect("serialize");
    let header = &serialized["objects"][0]["actions"]["upload"]["header"];
    for key in [
        "X-Xet-Cas-Url",
        "X-Xet-Access-Token",
        "X-Xet-Token-Expiration",
        "X-Xet-Session-Id",
    ] {
        assert!(header.get(key).is_some(), "missing header key {key}");
    }
    // The expiration is a JSON string; git-xet parses it with u64::from_str.
    assert!(header["X-Xet-Token-Expiration"].is_string());
}

/// "server already has this object, skip it" is an entry with *no* actions
/// and *no* error. Emitting `"actions": {}` would be a different message.
#[test]
fn empty_actions_error_and_flags_are_absent_from_the_wire() {
    let response = LfsBatchResponse {
        transfer: TRANSFER_XET.to_owned(),
        objects: vec![LfsBatchObject {
            oid: EXAMPLE_HASH.to_owned(),
            size: 1_048_576,
            authenticated: false,
            actions: HashMap::new(),
            error: None,
        }],
        hash_algo: String::new(),
    };

    let serialized = serde_json::to_value(&response).expect("serialize");
    assert_eq!(
        serialized,
        json!({
            "transfer": "xet",
            "objects": [{ "oid": EXAMPLE_HASH, "size": 1_048_576 }]
        })
    );

    let object = serialized["objects"][0].as_object().expect("object");
    assert!(
        object.get("actions").is_none(),
        "empty actions must be absent"
    );
    assert!(object.get("error").is_none(), "absent error must be absent");
    assert!(
        object.get("authenticated").is_none(),
        "authenticated=false must be absent"
    );
    let top = serialized.as_object().expect("object");
    assert!(
        top.get("hash_algo").is_none(),
        "empty hash_algo must be absent"
    );
}

/// An action with no headers and no advisory expiry is just an href.
#[test]
fn empty_action_header_and_zero_expiry_are_absent() {
    let action = LfsAction {
        href: "https://downloads.example/oid".to_owned(),
        header: HashMap::new(),
        expires_in: 0,
    };
    assert_eq!(
        serde_json::to_value(&action).expect("serialize"),
        json!({ "href": "https://downloads.example/oid" })
    );
}

/// Downloads must never negotiate "xet": git-xet's `init_download` hard-fails.
#[test]
fn download_batch_answers_basic_transfer() {
    let response = LfsBatchResponse {
        transfer: TRANSFER_BASIC.to_owned(),
        objects: vec![LfsBatchObject {
            oid: EXAMPLE_HASH.to_owned(),
            size: 1_048_576,
            authenticated: true,
            actions: HashMap::from([(
                "download".to_owned(),
                LfsAction {
                    href: "https://git.example/candace/models.git/info/lfs/objects/a1b2".to_owned(),
                    header: HashMap::new(),
                    expires_in: 0,
                },
            )]),
            error: None,
        }],
        hash_algo: HASH_ALGO_SHA256.to_owned(),
    };

    let serialized = serde_json::to_value(&response).expect("serialize");
    assert_eq!(serialized["transfer"], json!("basic"));
    assert_eq!(
        serialized["objects"][0]["actions"]["download"],
        json!({ "href": "https://git.example/candace/models.git/info/lfs/objects/a1b2" })
    );
    validate_lfs_batch_response(&response).expect("valid");
}

/// A per-object failure inside a 200 response carries `error` and no actions.
#[test]
fn per_object_error_serializes_without_actions() {
    let response = LfsBatchResponse {
        transfer: TRANSFER_BASIC.to_owned(),
        objects: vec![LfsBatchObject {
            oid: OTHER_HASH.to_owned(),
            size: 42,
            authenticated: false,
            actions: HashMap::new(),
            error: Some(LfsObjectError {
                code: 404,
                message: "object does not exist".to_owned(),
            }),
        }],
        hash_algo: String::new(),
    };

    assert_eq!(
        serde_json::to_value(&response).expect("serialize"),
        json!({
            "transfer": "basic",
            "objects": [{
                "oid": OTHER_HASH,
                "size": 42,
                "error": { "code": 404, "message": "object does not exist" }
            }]
        })
    );
    validate_lfs_batch_response(&response).expect("valid");
}

/// Not a serialization rule, but the content type the whole exchange rides
/// on; keeping it beside the fixtures is how it stays correct.
#[test]
fn lfs_content_type_is_the_git_lfs_media_type() {
    assert_eq!(LFS_CONTENT_TYPE, "application/vnd.git-lfs+json");
}
