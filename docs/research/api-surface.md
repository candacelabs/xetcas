All research complete. Below is the dossier — my final deliverable.

---

# Xet CAS HTTP Wire Contract — Implementation Dossier

**Source tree root** (all paths below are relative to it): `/tmp/claude-1000/-home-bertha-candace-server--claude-worktrees-xet-cas-storage-transfer-07271f/046ce30e-cd86-469f-9c94-1dbeda51d318/scratchpad/xet-core` (huggingface/xet-core @ 77fc84d3d, 2026-08-11).

Primary sources: `openapi/cas.openapi.yaml` (499 lines, OpenAPI 3.1.0, title "Xet CAS API", version 1.0.0), `openapi/Makefile` (client codegen only — `openapi-generator-cli` for rust/typescript/python/java/go; no server stubs, no wire facts beyond the spec itself), and the real client in `xet_client/`.

---

## 0. Global conventions (apply to every endpoint)

- **Base URL / endpoint**: an opaque base URL string ("endpoint"). The client builds request URLs by naive string concat, e.g. `format!("{}/v1/shards", self.endpoint)` — so the endpoint must have **no trailing slash** (`xet_client/src/cas_client/remote_client.rs:348`). Default dev endpoint constant: `pub const CAS_ENDPOINT: &str = "http://localhost:8080"` (`remote_client.rs:40`). In production the endpoint comes from the Hub token API's `casUrl` field (see §10).
- **HTTP version**: the client forces **HTTP/1.1** (`.http1_only()`, `xet_client/src/common/http_client.rs:90`). Your server does not need HTTP/2.
- **Hashes on the wire**: always 64-char lowercase hex (`MerkleHash::hex()`; OpenAPI schema `HexString64Lowercase`, pattern `^[0-9a-f]{64}$`, `openapi/cas.openapi.yaml:318-323`).
- **Prefix/key convention**: `Key { prefix, hash }` displays as `"{prefix}/{hash_hex}"` (`xet_client/src/cas_types/key.rs:17-21`); it is spliced directly into paths, producing `/v1/xorbs/{prefix}/{hash}` and `/v1/chunks/{prefix}/{hash}`. The prefix value is `ctx.config.data.default_prefix`, default `"default"` (`xet_runtime/src/config/groups/data.rs:109`), used for **both** xorb upload (`xet_data/src/processing/file_upload_session.rs:404`) and the global-dedup chunk query (`xet_data/src/processing/shard_interface/native.rs:150`). ⚠️ Discrepancy: the OpenAPI says the only acceptable dedup prefix is `default-merkledb` (`cas.openapi.yaml:292-299`) and xorb prefix `default` (`:300-307`), but the current client sends `default` for both. **A permissive server should accept any prefix on both routes** (the local test server does — it just forwards `key.prefix`, `xet_client/src/cas_client/simulation/local_server/handlers.rs:513-523`).
- **Range typing** (critical): three distinct range types (`xet_client/src/cas_types/mod.rs:35-40`):
  - `ChunkRange` (u32) — chunk indexes, **end-exclusive** `[start, end)` (OpenAPI `IndexRange`, `cas.openapi.yaml:324-335`).
  - `FileRange` (u64) — file/xorb byte offsets, **end-exclusive**.
  - `HttpRange` (u64) — HTTP range bytes, **end-inclusive** `[start, end]` (OpenAPI `ByteRange`, `:336-347`). Conversions: `HttpRange::from(FileRange)` does `end-1`; reverse does `end+1` (`cas_types/mod.rs:74-96`). Serialized JSON shape for all of them: `{"start": <int>, "end": <int>}` (the `_marker` field is `#[serde(skip)]`, `cas_types/mod.rs:99-105`).
  - `Range` header format emitted by client: `bytes={start}-{end}` with inclusive end (`HttpRange::range_header()`, `cas_types/mod.rs:81-84`).
- **Common request headers**:
  - `Authorization: Bearer <token>` — added by `AuthMiddleware` **only if an `AuthConfig` was supplied**; with `auth = None` there is **no Authorization header at all** (`common/http_client.rs:166` logs "CAS auth disabled"; header insertion at `:329-344`).
  - `X-Xet-Session-Id: <session id string>` — added by `SessionMiddleware` on **every** request (including presigned-URL downloads) when the session id is non-empty (`cas_types/mod.rs:17`, `common/http_client.rs:346-369`).
  - `User-Agent` — passed as a custom default header; hf_xet builds `hf_xet/<version>` (or `"<existing>; hf_xet/<version>"`) (`hf_xet/src/headers.rs:7-23`). Telemetry falls back to `xet-client/<version>` (`xet_client/src/cas_client/telemetry/mod.rs:35-37`).
  - No `Content-Type` is sent on xorb/shard uploads (spec says `application/octet-stream`, but the client sets only `Content-Length`); telemetry sends `Content-Type: application/json` (`telemetry/sink.rs:204`).
  - No `Content-Encoding` / `Accept-Encoding` semantics anywhere; compression is inside the xorb chunk format, not the HTTP layer.
- **Common response headers**: `X-Request-Id` — optional, read by the client purely for logging (`cas_types/mod.rs:19`, `common/http_client.rs:397-402`). Safe to stub (absent → logged as empty).
- **Error bodies**: the client **never parses error bodies**; it acts only on status codes (`retry_wrapper.rs:196-240`). Plain-text error bodies are fine (local server does `(status, message_string)` — `local_server/handlers.rs:140-148`).
- **Timeouts** (client defaults, `xet_runtime/src/config/groups/client.rs`): connect 60s (`:53`), read (between packets) 300s (`:64`), pool idle 60s (`:38`), max idle conns/host 16 (`:45`). The **shard-upload V1 client has no read timeout at all** (`common/http_client.rs:104-142`; `remote_client.rs:51-54`) because server-side shard validation may be silent for a long time. `/v2/shards` uses the normal 300s-read-timeout client because the server is expected to emit an NDJSON heartbeat frame roughly every ~20s during quiet validation (`remote_client.rs:403-407`).
- **Retry/backoff** (`xet_client/src/cas_client/retry_wrapper.rs`): max attempts 5 (`client.rs:12`), base delay 3s (`client.rs:20`), per-sleep cap 6min (`client.rs:31`). Delay sequence before jitter: 3s, 9s, 27s, 81s, 243s (verified by test `retry_wrapper.rs:628-639`); jitter applied (`:272`). Classification (`default_on_request_success`, `:513-522`): 5xx / 408 / 429 → retry; other 4xx → fatal; **501 → fatal even though 5xx** (`:219-222`); 429 not retried for the dedup query (`with_429_no_retry`, `remote_client.rs:186`); 403 retried only for presigned-URL downloads (`with_retry_on_403`, `remote_client.rs:600`). Connection errors, timeouts, hyper IncompleteMessage/Canceled/IO → retry (`:530-572`). JSON/bytes body decode failures on a 200 are also retried (`:396-421`, `:432-462`).
- **Adaptive concurrency** (server-visible only as parallelism): uploads start at 2 concurrent streams, max 64; downloads start at 4, max 64 (`client.rs:206-247`).

---

## 1. Endpoint catalog

### 1.1 `GET /v1/reconstructions/{file_id}` — file reconstruction (V1)

- **Client call sites**: `RemoteClient::get_reconstruction_impl` builds `{endpoint}/{v1|v2}/reconstructions/{file_id_hex}` (`remote_client.rs:226`); wrapper `get_reconstruction_v1` (`:279-285`); also `get_file_reconstruction_info` always uses V1 with no Range (`:743`).
- **Path param**: `file_id` = 64-hex file hash.
- **Request headers**: optional `Range: bytes={start}-{end}` (inclusive end; produced from the query `FileRange` via `HttpRange::from`, `remote_client.rs:252-254`). OpenAPI pattern `^bytes=\d+-\d+$` (`cas.openapi.yaml:308-316`). The local test server additionally accepts `bytes=N-` and `bytes=-N` (`handlers.rs:76-137`), but the real client only ever sends the two-sided form.
- **Response 200** `application/json` — `QueryReconstructionResponse` (`cas_types/mod.rs:204-217`; OpenAPI `:374-396`):
  ```json
  {
    "offset_into_first_range": 0,
    "terms": [
      {"hash": "<64hex xorb hash>", "unpacked_length": 263873, "range": {"start": 0, "end": 4}}
    ],
    "fetch_info": {
      "<64hex xorb hash>": [
        {"range": {"start": 0, "end": 4},
         "url": "https://…",
         "url_range": {"start": 0, "end": 131071}}
      ]
    }
  }
  ```
  Field types: `offset_into_first_range: u64`; `terms[].unpacked_length: u32`; `terms[].range`: ChunkRange (end-exclusive); `fetch_info[].url_range`: HttpRange (end-**inclusive**, used verbatim as the Range header on the fetch). All three top-level fields required; `additionalProperties: false` in spec.
- **Status codes**: 400 malformed file_id; 401 missing/expired token; 404 file unknown; **416** when the requested Range start ≥ file length — the client maps 416 to `Ok(None)` = "past EOF", which is how it detects end-of-file during segmented downloads (`remote_client.rs:271-273`; `retry_wrapper.rs:204-206` `with_expected_416`).
- **Semantics** (canonical algorithm in `xet_client/src/cas_client/simulation/xorb_utils.rs:55-197`, used by both reference in-repo servers):
  - Walk the file's segment list; skip whole segments before the range start; within the first overlapping segment, trim leading whole chunks whose bytes fall entirely before `range.start`; within the last, trim trailing whole chunks entirely past `range.end`. Terms are therefore **chunk-aligned supersets** of the byte range.
  - `offset_into_first_range = range.start − byte_offset_of_first_returned_chunk` (`xorb_utils.rs:196`): number of leading bytes of term 0's unpacked data the client must discard. Only term 0 carries it (`xet_data/.../file_term.rs:239-247`); the client also trims the tail of the last term itself (`file_term.rs:303-314`).
  - `fetch_info` groups terms by xorb, sorted by chunk start, with adjacent/overlapping chunk ranges **merged** into one entry (`xorb_utils.rs:165-194`). `url_range` = physical (compressed, on-disk) byte range of that chunk range within the serialized xorb, converted to inclusive-end.
  - `unpacked_length` is the uncompressed byte length the term must decode to; the client verifies it (`remote_client.rs:688-695,715-722`).

### 1.2 `GET /v2/reconstructions/{file_id}` — multi-range optimized

- **Client**: `get_reconstruction_v2` (`remote_client.rs:288-294`); same URL scheme with `v2`, same optional `Range` header.
- **Response 200** `application/json` — `QueryReconstructionResponseV2` (`cas_types/mod.rs:222-249`; OpenAPI `:424-446`):
  ```json
  {
    "offset_into_first_range": 0,
    "terms": [ …same as V1… ],
    "xorbs": {
      "<64hex xorb hash>": [
        {"url": "https://…?<signed>",
         "ranges": [
           {"chunks": {"start": 0, "end": 4}, "bytes": {"start": 0, "end": 131071}}
         ]}
      ]
    }
  }
  ```
  `ranges` sorted by chunk start; typically one `XorbMultiRangeFetch` per xorb; multiple entries only when a URL-length limit (~8 KiB, ≈500 ranges) forces a split (`cas_types/mod.rs:226-229`). Spec: "Client must send exactly the signed range value as the Range header" (`cas_types/mod.rs:235-237`, `cas.openapi.yaml:412-416`).
- **Status codes**: as V1, plus **404 or 501 mean "V2 unsupported → client falls back to V1"** (`cas.openapi.yaml:111-116`).
- **V1→V2 conversion inside the client** (`cas_types/mod.rs:251-277`): each V1 fetch_info entry becomes one `XorbMultiRangeFetch` with a single range. The rest of the pipeline always consumes V2 shape.
- **Version negotiation** (`remote_client.rs:296-335`): try V2 first; on 404/501 fall back to V1 and cache "1" in `detected_reconstruction_api_version: AtomicU32` (0=unprobed, `remote_client.rs:57-58`); success caches "2". Config override `HF_XET_CLIENT_RECONSTRUCTION_API_VERSION` = 1 or 2 forces a version with no fallback (`xet_runtime/src/config/groups/client.rs:265`). **A V1-only server just needs to return 404 (or 501) from `/v2/reconstructions/...` and `/v2/shards`.**

### 1.3 `GET /v1/reconstructions?file_id=<hex>&file_id=<hex>…` — batch (V1 only)

- **Client**: `batch_get_reconstruction` (`remote_client.rs:535-568`). URL literally built as `{endpoint}/v1/reconstructions?` then `file_id=<hex>` params joined with `&` (note: same path as 1.1 but no path segment; the local server routes it as `/v1/reconstructions` exact-match, `local_server/server.rs:201`). No Range header.
- **Response 200** — `BatchQueryReconstructionResponse` (`cas_types/mod.rs:285-295`):
  ```json
  {"files": {"<file hash hex>": [ <terms as in V1> ]},
   "fetch_info": { …same map as V1… }}
  ```
- **Callers**: nothing in the upload/download pipelines calls it (grep found only the client lib + its tests) — implement or stub as you like; not needed for hf_xet compatibility.

### 1.4 `GET /v1/chunks/{prefix}/{hash}` — global chunk dedup query

- **Client**: `query_dedup_api` builds `{endpoint}/v1/chunks/{prefix}/{hash_hex}` (`remote_client.rs:164-212`, URL at `:172`); public wrapper `query_for_global_dedup_shard` (`:775-781`) returns the raw body bytes.
- **When called** (`xet_data/src/deduplication/file_deduplication.rs:155-174`): during upload chunking, on the **first pass** over each batch of chunks, for a chunk that failed all local dedup, **if** it is the file's very first chunk (`global_chunk_index == 0`) **or** `chunk_hash % 1024 == 0` (`hash_is_global_dedup_eligible`, `xet_core_structures/src/metadata_shard/constants.rs:8,20-22`; modulus constant `MDB_SHARD_GLOBAL_DEDUP_CHUNK_MODULUS = 1024`), rate-limited by a chunk-spacing counter. Queries run in the background; found shards are imported into the local shard cache and dedup re-runs (`xet_data/src/processing/shard_interface/native.rs:146-163`).
- **Request**: plain GET, no body, no special headers (Authorization/session as usual).
- **Response 200** `application/octet-stream`: **a serialized xet shard** (MDB shard format) containing at minimum the xorb metadata (chunk hash lists) covering that chunk, so the client can dedup against it. The client feeds the bytes straight into `import_shard_from_bytes` (`native.rs:159`); the reference servers store/serve the uploaded shard bytes for every dedup-eligible chunk in it (`memory_client.rs:816-839`, expiry-stamped re-serialization via `MDBMinimalShard::serialize_xorb_subset_with_expiry` at `:833-838`). Note: `cas_types/mod.rs:379-382` defines a `QueryChunkResponse {shard}` JSON struct but it is **unused** — the endpoint returns raw shard bytes, not JSON.
- **Status codes**: 404 = "chunk not tracked" → client returns `Ok(None)` and moves on (`remote_client.rs:187,191-201`; `with_expected_404` documented `retry_wrapper.rs:90-97`); 429 is treated as **fatal, no retry** for this endpoint (`with_429_no_retry`, `remote_client.rs:186`); 400 malformed hash; 401 auth. **A minimal server can simply always return 404** — upload still works, only cross-repo dedup is lost.

### 1.5 `POST /v1/xorbs/{prefix}/{hash}` — xorb upload

- **Client**: `upload_xorb` (`remote_client.rs:824-934`); URL `{endpoint}/v1/xorbs/{prefix}/{hash_hex}` via `Key` Display (`:831-837`).
- **Request**: body = `SerializedXorbObject.serialized_data` — the serialized xorb **without footer** (`serialize_footer=false` at the only production call site, `xet_data/src/processing/file_upload_session.rs:387-399`, comment: "XORBs are sent without footer - the server/client reconstructs it from chunk data"). Body is a concatenation of chunk records, each `XorbChunkHeader` (8 bytes, `#[repr(C,packed)]`: `version:u8` (currently 0), `compressed_length: 3-byte LE`, `compression_scheme: u8`, `uncompressed_length: 3-byte LE` — `xet_core_structures/src/xorb_object/xorb_chunk_format.rs:11-21`) followed by that chunk's (possibly compressed) payload. Limits: xorb ≤ 64 MiB, ≤ 8192 chunks, chunk ≤ 128 KiB uncompressed (`xet_core_structures/src/xorb_object/constants.rs:5-26`).
  - Sent as a **streamed chunked body** (`Body::wrap_stream`) in 512 KiB reporting blocks with an explicit `Content-Length: <n>` header ("must be set because of streaming", `remote_client.rs:873-885`; block size `upload_reporting_block_size` default 524288, `client.rs:71`). On wasm it's a plain body. **Your server must accept a chunked transfer that also carries Content-Length**, buffering the full body.
- **Expected server-side verification**: recompute the xorb hash from the chunk data and compare against `{hash}` in the path — spec 400 reason "Malformed hash, mismatched body hash, or bad serialization" (`cas.openapi.yaml:175-176`); reference implementation `reconstruct_xorb_with_footer(...)` + `computed_hash != hash → error` (`xet_client/src/cas_client/simulation/memory_client.rs:918-937`).
- **Response 200** `application/json`: `{"was_inserted": <bool>}` (`cas_types/mod.rs:21-24`; OpenAPI `UploadXorbResponse`, `:447-454`). `false` = already existed; the client only logs the distinction (`remote_client.rs:906-931`). Any 200 is success.
- **Status codes**: 400 (see above), 401, 403 insufficient scope. Standard retry rules apply (5xx/408/429 retried).
- Companion `HEAD /v1/xorbs/{prefix}/{hash}` exists only in the local test server (`handlers.rs:525-543`); RemoteClient never calls it.

### 1.6 `POST /v1/shards` — shard upload (V1)

- **Client**: `upload_shard_v1` (`remote_client.rs:337-381`); URL `{endpoint}/v1/shards` (`:348`). Uses the **no-read-timeout** HTTP client (`:350-354`).
- **Request**: body = raw serialized shard bytes (MDB shard format: file reconstructions + xorb listings), non-streamed `Bytes` body (reqwest sets Content-Length automatically). No Content-Type.
- **Response 200** `application/json`: `{"result": 0|1}` — integer via `serde_repr`: `0` = Exists, `1` = SyncPerformed (`cas_types/mod.rs:297-307`; OpenAPI `:455-464` "Any 200 OK means success").
- **Status codes**: 400 invalid serialization/verification failure; 401; 403.
- **Server-side expectation** (per reference impl `memory_client.rs:846-897`): parse the shard (`MDBMinimalShard::from_reader`), register the file-reconstruction entries and xorb metadata, and index dedup-eligible chunk hashes → shard bytes for §1.4.

### 1.7 `POST /v2/shards` — shard upload with NDJSON progress stream

- **Client**: `upload_shard_v2` (`remote_client.rs:384-472`); URL `{endpoint}/v2/shards` (`:393`). Streamed body with explicit `Content-Length` (`:442`), normal (300s-read-timeout) client. Version negotiation identical to reconstructions: prefer V2, fall back to V1 on 404/501, cache in `detected_shard_api_version` (`remote_client.rs:475-519`); override env `HF_XET_CLIENT_SHARD_API_VERSION` (`client.rs:274`). Wasm always uses V1 (`remote_client.rs:808-811`).
- **Response**: HTTP **200** with `Content-Type: application/x-ndjson` (local server sets it, `handlers.rs:696`; client does not check it), body = newline-delimited JSON frames of `ShardUploadEvent` (`cas_types/mod.rs:320-343`; tagged `{"type": ...}` snake_case). Exact frame JSON (round-trip tests `cas_types/mod.rs:459-511`):
  - `{"type":"validating","verified":3,"total":7}` — progress; `total` may still grow.
  - `{"type":"committing","stage":"uploading"}` / `{"type":"committing","stage":"syncing"}`
  - `{"type":"result"}` — **terminal success**.
  - `{"type":"error","message":"…","retryable":false}` — **terminal failure. The HTTP status is already 200; this frame is the error signal** (`cas_types/mod.rs:332-339`). `retryable` defaults to false when omitted; `retryable:true` makes the client retry the whole upload through RetryWrapper.
  - Unknown `"type"` values are ignored (forward-compat `#[serde(other)] Unknown`); the production server may emit e.g. a heartbeat by re-sending the last frame every ~20s (`remote_client.rs:403-407`).
- **Client-side parsing** (`xet_client/src/cas_client/shard_upload_v2.rs:18-70`): frames split on `\n`, blank lines skipped, one trailing frame without newline accepted, per-frame cap **1,048,576 bytes** (`MAX_NDJSON_LINE_BYTES`, `:11`). A stream that ends without a terminal frame is a fatal `InvalidResponse` (`:67-69`). Unparseable frame → fatal.
- **Simplest conforming success stream** (what the local server sends, `handlers.rs:656-669`): `validating(0/1)`, `validating(1/1)`, `committing uploading`, `committing syncing`, `result` — or even just `{"type":"result"}\n`.

### 1.8 `GET /v2/file-chunk-hashes/{file_id}` — incremental-upload chunk windows

- **Client**: `get_file_chunk_hashes` (`remote_client.rs:936-976`); URL `{endpoint}/v2/file-chunk-hashes/{file_id_hex}` (`:946`). Called from the range-upload path (`xet_data/src/processing/range_upload.rs:171`). Not in the OpenAPI file.
- **Request header**: `X-Range-Dirty: bytes=A-B,C-D` — same syntax as Range (inclusive ends, multiple comma-joined), tags regions the client will re-chunk; distinct from `Range` (`cas_types/mod.rs:384-389` const `X_RANGE_DIRTY_HEADER = "X-Range-Dirty"`; value built `remote_client.rs:949-960`).
- **Response 200** JSON, **camelCase** (`#[serde(rename_all = "camelCase")]`, `cas_types/mod.rs:395-418`):
  ```json
  {"totalChunks": 0, "fileSize": 0,
   "windows": [{"dirtyByteRange": [start, end_exclusive]}],
   "hashRanges": [ <MerkleHashSubtree|null> … ],   // windows.len()+1 entries
   "gapVerification": ["<64hex>", …]}
  ```
  Server-side window state machine is mirrored in `xet_client/src/cas_client/chunk_window_builder.rs` ("mirrored from xetcas PR #987"). **Optional for a basic server** — only the experimental range-upload flow uses it.

### 1.9 `POST /v1/telemetry` — client transfer telemetry

- **Client**: `TelemetrySink::send` — URL built as `base.join("/v1/telemetry")` (absolute path, so it lands at the endpoint root even if the base has a path — `telemetry/mod.rs:117-123`); `Content-Type: application/json` (`sink.rs:204`); **fire-and-forget, never retried, response ignored** (`sink.rs:83-93,208-219`).
- **Body** (client emits exactly these five snake_case keys — `telemetry/envelope.rs:17-27`, key-set test `:53-58`):
  ```json
  {"time":"2026-07-28T12:00:00.000Z","event":"xet_upload_summary",
   "session_id":"…","user_agent":"hf_xet/1.5.4","metrics":{…flat scalars…}}
  ```
  Events: `xet_upload_summary`, `xet_download_summary` (terminal), `xet_transfer_heartbeat` (`telemetry/mod.rs:64-75`). OpenAPI documents `userAgent` (camelCase) as required with `user_agent` accepted as an alias, both present = 400 duplicate (`cas.openapi.yaml:465-497`); the current client sends `user_agent`. Statuses: 200 (even when disabled), 400, 401, 413 (body > 1 MiB), 429, 500 (`cas.openapi.yaml:255-267`). **Stub: unconditionally return 200.**

### 1.10 Presigned/fetch URL `GET <url from fetch_info>` — the actual data download

Not a fixed route: the server chooses the URL. The client fetches it with the **unauthenticated** HTTP client (`self.http_client`, built with `auth=None` — `remote_client.rs:109-111`, used at `:587`), so **no Authorization header** is sent; `X-Xet-Session-Id` and default headers (User-Agent) still are. The URL must therefore be self-authorizing (query token) or unprotected.

- **Request** (`get_file_term_data`, `remote_client.rs:579-734`, api tag `"s3::get_range"`): `GET <url>` with `Range` always present:
  - single range: `Range: bytes={start}-{end}` (inclusive; exactly `url_range`/`bytes` from the reconstruction) — `remote_client.rs:616-617`;
  - multi-range (only when `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING=true`; **default false**, `client.rs:276-285`): `Range: bytes=s1-e1,s2-e2,…` (`remote_client.rs:618-625`). With the default off, the client splits each V2 `XorbRangeDescriptor` into its own single-range request **against the same URL** (`xet_data/.../file_term.rs:206-232`).
- **Response**:
  - Single range: 200 or 206, body = the raw serialized-xorb bytes of that range (a whole number of chunk records in the §1.5 format). Streamed and decoded incrementally (`remote_client.rs:697-727`); after decode the client checks the uncompressed length against the terms' `unpacked_length` sum when known (`:715-722`). Content-Length is used only for progress accounting.
  - Multi-range: if `Content-Type` contains `multipart/byteranges`, the client parses RFC 7233 §4.1 multipart (`remote_client.rs:645-696`; parser `xet_client/src/cas_client/multipart.rs:16-117`): boundary from `Content-Type: multipart/byteranges; boundary=…` (quoted or bare), parts delimited `--{boundary}\r\n`, headers/body split on `\r\n\r\n`, each part must carry `Content-Range: bytes S-E/TOTAL` (inclusive end); parts are sorted by start before concatenation. A server may also legally answer a multi-range request with a single-range body (the S3 behavior the local server simulates, `handlers.rs:429-448`).
  - **403 → URL refresh**: on 403 the client calls `URLProvider::refresh_url()` (single-flight re-issue of the reconstruction query for the same file/byte-range, swap in the new URLs — `remote_client.rs:634-639`; `xet_data/.../retrieval_urls.rs:56-135`) and retries (403 marked retryable here, `remote_client.rs:600`).
- **Can the URLs point back at the same server? Yes.** The in-repo reference server returns `http://{Host}/v1/fetch_term?term=<base64url(payload)>` for both V1 and V2 (`handlers.rs:215-226,342-350`), where the V1 payload is `base64url(xorb_hash_hex)` and V2 is `base64url("{hash_hex}:{s1}-{e1},{s2}-{e2}…")` with **exclusive** ends (`handlers.rs:150-199`). Nothing in the client cares about URL shape, host, scheme, or query — any absolute URL parseable by `url::Url` works. Expiry is your policy (client handles it via the 403-refresh loop; simulated with `url_expiration` config, `handlers.rs:940-946`).
- **Download segmentation context**: the client asks for reconstructions in prefetch blocks of at least `min_reconstruction_fetch_size` = 256 MB up to 8 GB (`xet_runtime/src/config/groups/reconstruction.rs:13,21`), detects EOF via a short/absent (416 → None) reconstruction (`manager.rs:296-320`).

### 1.11 Routes that exist only in the in-repo test server (not called by RemoteClient)

`GET /health` (200 + no-cache headers, `handlers.rs:753-767`); `HEAD /v1/xorbs/{prefix}/{hash}`; `HEAD /v1/files/{file_id}` (Content-Length = file size, `handlers.rs:700-720`); `GET /v1/get_xorb/{prefix}/{hash}/`; `GET /v1/fetch_term`; the `/simulation/*` control plane (`local_server/server.rs:195-232`, simulation routes `simulation_handlers.rs:26-49`: xorb/shard/file-entry listing & deletion, `set_config`, `ping`, `dummy_upload`).

---

## 2. Which endpoints the real client actually uses, summarized

| Endpoint | Used in production flows? | Call site |
|---|---|---|
| GET /v2/reconstructions/{id} → fallback GET /v1/… | yes (every download) | `remote_client.rs:216-335` |
| GET presigned URL (+Range) | yes (every download) | `remote_client.rs:579-734` |
| POST /v1/xorbs/{prefix}/{hash} | yes (every upload) | `remote_client.rs:824-934` |
| POST /v2/shards → fallback POST /v1/shards | yes (end of every upload) | `remote_client.rs:337-519,797-819` |
| GET /v1/chunks/{prefix}/{hash} | yes (uploads, opportunistic) | `remote_client.rs:164-212,775-781` |
| POST /v1/telemetry | yes (best-effort) | `telemetry/sink.rs:189-220` |
| GET /v1/reconstructions?file_id=… (batch) | no production caller | `remote_client.rs:535-568` |
| GET /v2/file-chunk-hashes/{id} | only range-upload feature | `remote_client.rs:936-976` |
| GET /v1/reconstructions/{id} (as MDBFileInfo source) | utility path | `remote_client.rs:738-773` |

**Minimum viable server** for the real client: `/v1/reconstructions/{id}` (with Range + 416), `/v1/xorbs/{prefix}/{hash}`, `/v1/shards`, `/v1/chunks/{prefix}/{hash}` (may always 404), `/v1/telemetry` (may always 200), presigned-URL GET with single Range, and 404 on `/v2/reconstructions/...` + `/v2/shards` to trigger V1 fallback.

---

## 3. Auth: what to accept / stub

- Client attaches `Authorization: Bearer <opaque JWT>` from `AuthConfig {token, token_expiration, token_refresher}` (`cas_client/auth.rs:142-186`), refreshed 30s before expiry (`REFRESH_BUFFER_SEC = 30`, `:30`) via the Hub, **not** via the CAS server. With no AuthConfig no header is sent at all. `NoOpTokenRefresher` (tests) yields the literal token `"token"` (`auth.rs:49-53`).
- Token acquisition (context only): `GET {hub}/api/{model|dataset|space}s/{repo_id}/xet-{read|write}-token/{rev}[?create_pr=1]` with `Authorization: Bearer <hf token>` → JSON `{"casUrl": "...", "exp": 1756489133, "accessToken": "..."}` (camelCase; `hub_client/client.rs:68-111`, `hub_client/types.rs:9-15,84-95`). `casUrl` becomes the CAS endpoint. Also `DirectRefreshRouteTokenRefresher` GETs an arbitrary refresh URL returning the same JSON (`cas_client/auth.rs:66-140`).
- **A permissive server can ignore the Authorization header entirely** (the local test server has zero auth). Scope model in the spec: `read` for reconstructions/chunks/telemetry, `write` for xorbs/shards; 401 missing/expired, 403 wrong scope (`cas.openapi.yaml:22-23,268-275`). Note 401/403 are fatal (no retry) on all endpoints except the presigned-URL 403-refresh path — so a broken auth stub fails fast rather than hammering.

---

## 4. Mock/test servers inside xet_client (behavioral reference)

1. **`LocalServer` / `LocalTestServer`** (`src/cas_client/simulation/local_server/{server,handlers}.rs`) — full Axum server implementing every route in §1 (V1+V2+fetch_term+telemetry+simulation), backed by `LocalClient` (disk, redb index) or `MemoryClient`. This is the most authoritative executable spec of server behavior: 416 for bad ranges, 404 xorb/file-not-found, NDJSON success/error streams, `multipart/byteranges` generation with boundary `xet_multipart_boundary` (`handlers.rs:457`), V2-disable knobs returning arbitrary status codes for fallback testing (`handlers.rs:296-303,625-630`), URL-expiry simulation. `LocalTestServer` wires a real `RemoteClient` at `http://127.0.0.1:<random port>` with session id `test-session` and `User-Agent: test-agent` (`server.rs:352-420`).
2. **`MemoryClient`** (`simulation/memory_client.rs`) — in-memory backend; documents server obligations: xorb hash verification on upload (`:918-937`), shard parse/merge + global-dedup indexing (`:846-897`), overwrite-on-reupload semantics.
3. **`LocalClient`** (`simulation/local_client.rs`) — disk backend; canonical reconstruction-range computation shared through `xorb_utils.rs`.
4. **wiremock** unit tests — retry semantics (`retry_wrapper.rs:592-1062`: 500→retry, 400→no retry, 429 both modes, 403 both modes, truncated-JSON retry) and telemetry contract (`telemetry/sink.rs:325-439`: single POST, no retry on 429, exact JSON body match).
5. **`UnixSocketProxy`** (`simulation/socket_proxy.rs`) — client can route all CAS traffic through a Unix socket (`HF_XET_CLIENT_UNIX_SOCKET_PATH`), rewriting https→http (`common/http_client.rs:19-41`).

---

## 5. Gotchas checklist for a from-scratch Rust server

1. Bodies for xorb and `/v2/shards` uploads arrive **chunked with an explicit Content-Length** — accept both simultaneously.
2. `terms[].range`/`chunks` are end-**exclusive** chunk indexes; `url_range`/`bytes` are end-**inclusive** byte offsets; mixing these corrupts downloads silently until the unpacked-length check trips a retry loop.
3. Return **416** (not 404, not empty 200) for `Range` starts at/after EOF on reconstructions — it's the client's EOF signal.
4. Never return 501 casually: the client treats it as permanent-fatal (`retry_wrapper.rs:219-222`) except on the two V2 endpoints where it (like 404) means "fall back to V1".
5. `/v2/shards` failures after the stream starts **must** be in-stream `{"type":"error",...}` frames on a 200 — a mid-stream non-200 is impossible, and a stream ending without `result`/`error` is a client-side fatal error.
6. Verify the xorb hash from the uploaded chunk data (path hash is client-asserted); reject mismatch with 400.
7. Keep every NDJSON frame ≤ 1 MiB and emit some frame at least every ~300 s (client read timeout) — the production pattern is a heartbeat every ~20 s.
8. Presigned URLs receive **no Authorization header**; embed any auth in the URL, and answer expired URLs with **403** (triggers refresh+retry), not 401.
9. Accept prefix `default` (and ideally anything) on both `/v1/chunks/...` and `/v1/xorbs/...` despite the OpenAPI enums.
10. JSON field spellings are inconsistent by design: reconstruction/shard/xorb responses are snake_case; `/v2/file-chunk-hashes` is camelCase; telemetry accepts `user_agent`/`userAgent`. Copy them exactly as quoted above.