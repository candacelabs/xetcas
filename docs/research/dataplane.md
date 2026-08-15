# xet-core CAS dataplane dossier (rev `77fc84d3d077973dade91a216da5e6b4a1595ca0`, 2026-08-11)

All paths relative to the cloned repo root. Crates: `xet_data` (upload/download orchestration), `xet_client` (HTTP client, wire types, reference simulation server), `xet_core_structures` (hashes, xorb format, shard format), `xet_runtime` (config/env). A machine-readable API spec exists at `openapi/cas.openapi.yaml` (499 lines) and a **complete axum reference server** at `xet_client/src/cas_client/simulation/local_server/handlers.rs` + `server.rs`.

---

## 0. Hash primitives (get these wrong and nothing interoperates)

### 0.1 `MerkleHash` / `DataHash` representation and hex encoding
- `DataHash([u64; 4])` — `xet_core_structures/src/merklehash/data_hash.rs:40`. Constructed from 32 raw bytes by **memcpy/transmute** (`From<[u8;32]>`, data_hash.rs:63–73).
- **Hex encoding is NOT plain hex of the 32 bytes.** `hex()` prints `{:016x}` of each u64 after `.to_le()` (data_hash.rs:145–153); `from_hex` is the inverse (data_hash.rs:163–179). Because each u64 is a little-endian load of 8 bytes, **the byte order within each 8-byte group is reversed** in the hex string: blake3 output bytes `b0..b31` hex-print as `b7 b6 … b0 | b15 … b8 | b23 … b16 | b31 … b24`. `from_be_bytes` (data_hash.rs:208–216) exists to build a hash from canonical hex-order bytes. All URL path hashes, JSON `hash` fields, and file IDs use this 64-lowercase-hex form (`HexMerkleHash`, `xet_client/src/cas_types/key.rs:41–48`; serde via `hex::serde`, key.rs:42).
- `%` operator: `hash % n == hash[3].to_le() % n` (the 4th u64, bytes 24..32 LE) — data_hash.rs:120–126.
- `marker()` = all-1-bits hash (data_hash.rs:238–240); used as placeholder xorb hash during upload, never on the wire.
- `hmac(key)` = `blake3::keyed_hash(key, self.as_bytes())` (data_hash.rs:232–235).

### 0.2 Keyed blake3 keys (exact constants)
- **Chunk (leaf) hash key** `DATA_KEY` (data_hash.rs:272–275):
  `[102,151,245,119,91,149,80,222,49,53,203,172,165,151,24,28,157,228,33,16,155,235,43,88,180,208,176,75,147,173,242,41]`
  `compute_data_hash(slice) = blake3::keyed_hash(DATA_KEY, slice)` (data_hash.rs:294–297). Chunk hash = `compute_data_hash(chunk_bytes)` (`xet_core_structures/src/xorb_object/chunk.rs:12–18`). Shard file hash (its `.mdb` name) = streaming `HashedWrite` also keyed with `DATA_KEY` over full serialized shard bytes (data_hash.rs:369–390; `metadata_shard/shard_file_handle.rs:97–110`).
- **Internal node key** `INTERNAL_NODE_HASH` (data_hash.rs:278–281):
  `[1,126,197,199,165,71,41,150,253,148,102,102,180,138,2,230,93,221,83,111,55,199,109,210,248,99,82,230,74,83,113,63]`
- **Verification range-hash key** `VERIFICATION_KEY` (`metadata_shard/chunk_verification.rs:4–7`):
  `[127,24,87,214,206,86,237,102,18,127,249,19,231,165,195,243,164,205,38,213,181,219,73,230,65,36,152,127,40,251,148,195]`
  `range_hash_from_chunks(chunks) = blake3::keyed_hash(VERIFICATION_KEY, concat(chunk_hash.as_bytes()))` (chunk_verification.rs:9–16).

### 0.3 Xorb hash and file hash (aggregated merkle)
`xet_core_structures/src/merklehash/aggregated_hashes.rs`:
- Branching factor 4 (`AGGREGATED_HASHES_MEAN_TREE_BRANCHING_FACTOR = 4`, line 3); group size 2..=9 (lines 6–9).
- Merge of a group of `(hash, size)`: each entry formatted as `"{hash.hex()} : {size}\n"` (space-colon-space; decimal size), concatenated, then `compute_internal_node_hash` over the bytes; merged size = sum (lines 108–135). Cut rule `next_merge_cut` (lines 37–53): if ≤2 remain take all; else scan i in 2..min(9,len), cut after first i with `hashes[i] % 4 == 0` (i.e. hash[3] LE u64 % 4), else cut at min(9,len). Iterate `aggregated_node_hash` until 1 remains (lines 143–169).
- **xorb hash** = `aggregated_node_hash(&[(chunk_hash, uncompressed_len)])` (lines 172–179; applied in `xorb_object/raw_xorb_data.rs:40–41`).
- **file hash** = `aggregated_node_hash(chunks).hmac([0u8;32])` — i.e. the aggregated hash HMAC'd with an all-zero 32-byte salt (lines 183–195; called in `xet_data/src/deduplication/file_deduplication.rs:416`).
- `truncate_hash(h) = h[0]` (first u64) — used for shard lookup tables (`metadata_shard/utils.rs:31–33`).

---

## 1. Chunking (CDC)

`xet_data/src/deduplication/chunking.rs`:
- gearhash (`gearhash::Hasher::default()`, line 52), window 64 bytes (`HASH_WINDOW_SIZE = 64`, line 77).
- `TARGET_CHUNK_SIZE = 64*1024`; `MINIMUM_CHUNK_DIVISOR = 8` → min chunk 8 KiB; `MAXIMUM_CHUNK_MULTIPLIER = 2` → max chunk 128 KiB (`xet_core_structures/src/xorb_object/constants.rs:5–14`, `MAX_CHUNK_SIZE` line 29).
- Boundary mask: `mask = (target-1) << leading_zeros`, i.e. `0xFFFF` shifted fully left = `0xFFFF_0000_0000_0000` (chunking.rs:41–46). Boundary when `hasher.next_match(data, mask)` fires past min size; hash state reset to 0 after each cut (chunking.rs:100–138). Force cut at max chunk (line 126–129).
- Chunks are produced per 8 MiB ingestion block (`ingestion_block_size` default `8*1024*1024`, `xet_runtime` data config; also mirrored in `xet_data/src/processing/constants.rs:31`), final partial chunk flushed by `Chunker::finish` via `SingleFileCleaner::finish_inner` (`file_cleaner.rs:236–239`).

---

## 2. Wire formats

### 2.1 Serialized chunk (the *only* thing inside an uploaded xorb body)
`xet_core_structures/src/xorb_object/xorb_chunk_format.rs`:
- `XorbChunkHeader` — 8 bytes, `#[repr(C, packed)]` (lines 14–21):
  - byte 0: `version` (u8) — `CURRENT_VERSION = 0` (line 12)
  - bytes 1–3: `compressed_length` u24 LE (lines 99–110)
  - byte 4: `compression_scheme` (u8)
  - bytes 5–7: `uncompressed_length` u24 LE
- CompressionScheme values (`compression_scheme.rs:22–30`): `None=0`, `LZ4=1`, `ByteGrouping4LZ4=2`, `Auto=99` (Auto never serialized; `serialize_chunk` asserts, xorb_chunk_format.rs:117–123). LZ4 = **lz4_flex frame format** (`FrameEncoder`, compression_scheme.rs:139–143); BG4-LZ4 = byte-group-4 split then LZ4 frame (lines 156–173).
- If compressed ≥ raw, chunk stored uncompressed with scheme `None` (xorb_chunk_format.rs:127–133).
- Header validation on read (lines 64–88): scheme must parse; version ≤ 0; compressed ≤ `2*MAX_CHUNK_SIZE` (262144); uncompressed ≤ `MAX_CHUNK_SIZE` (131072). Also: a header whose first 7 bytes equal `b"XETBLOB"` is rejected as `ChunkHeaderParse` (footer sentinel; lines 141–148).
- `deserialize_chunks` returns `(uncompressed_bytes, chunk_byte_indices)` where indices start with 0 and end with total length (lines 236–256); `append_chunk_segment` concatenation rule for multi-segment responses (lines 203–216). Streaming variant: `xorb_chunk_format/deserialize_async.rs` (`deserialize_chunks_to_writer_from_stream`, used at `remote_client.rs:707`).

### 2.2 Xorb object footer (NOT sent over the wire by the client)
`xet_core_structures/src/xorb_object/xorb_object_format.rs`:
- Idents: `XORB_OBJECT_FORMAT_IDENT = b"XETBLOB"` (7 bytes), hashes section `b"XBLBHSH"`, boundaries section `b"XBLBBND"`; versions: object 1, hashes 0, boundaries 1 (lines 20–30). Footer buffer 16 bytes, first 4 = optional uniqueness nonce (lines 36–47).
- V1 footer serialize order (lines 434–511): `"XETBLOB"`, version u8, xorb_hash 32B; `"XBLBHSH"`, hashes_version u8, num_chunks u32, chunk_hashes[num_chunks]×32B; `"XBLBBND"`, boundaries_version u8, num_chunks u32, chunk_boundary_offsets[num_chunks]×u32 (physical end offsets incl. chunk headers), unpacked_chunk_offsets[num_chunks]×u32 (cumulative uncompressed ends); then num_chunks u32, hashes_section_offset_from_end u32, boundary_section_offset_from_end u32, 16-byte nonce buffer. The whole footer is followed by a trailing `info_length: u32 LE` (see `get_info_length`, line 953; `overwrite_uniqueness_nonce` lines 1044–1072).
- **Upload body = concatenated serialized chunks only, no footer**: `SerializedXorbObject::from_xorb(xorb, /*serialize_footer=*/false, …)` with comment "XORBs are sent without footer - the server/client reconstructs it from chunk data" (`xet_data/src/processing/file_upload_session.rs:386–399`; `from_xorb_with_compression` xorb_object_format.rs:1391–1477).
- Server-side validation recipe: `reconstruct_xorb_with_footer(writer, raw_data)` parses chunks until footer-or-EOF, decompresses each, recomputes chunk hashes and xorb hash, writes a canonical footer (lines 1747+); the reference `LocalClient::upload_xorb` does exactly that and rejects on hash mismatch (`xet_client/src/cas_client/simulation/local_client.rs:1595–1629`). Full validation of a footer-bearing xorb: `validate_xorb_object` (lines 1079–1164) — per-chunk hash equality, boundary offsets, unpacked offsets, footer position, xorb hash.
- Compression policy default `auto` with per-block re-testing every 32 chunks, reset at file boundaries (`xet_runtime/src/config/groups/xorb.rs:13,22`; from_xorb_with_compression, xorb_object_format.rs:1424–1458).

### 2.3 Shard format (upload body of `/v1/shards` & `/v2/shards`; also the global-dedup response body)
`xet_core_structures/src/metadata_shard/shard_format.rs` and `file_structs.rs` / `xorb_structs.rs`. Every record is 48 bytes (`MDB_FILE_INFO_ENTRY_SIZE = 8*4 + 4*4 = 48`, shard_format.rs:23–31). All ints LE.

- **Header** (48 bytes; lines 57–99): `tag[32]` = `MDB_SHARD_HEADER_TAG` (lines 43–46):
  `['H','F','R','e','p','o','M','e','t','a','D','a','t','a', 0, 85,105,103,69,106,123,129,87,131,165,189,217,92,205,209,74,169]`
  then `version: u64` = 2 (line 35), `footer_size: u64` (default `size_of::<MDBShardFileFooter>()` = 200; **rewritten to 0 for upload**, see §3.6).
- **File-info section** (offset in footer `file_info_offset`): per file — `FileDataSequenceHeader{ file_hash: 32B, file_flags: u32, num_entries: u32, _unused: u64 }` (file_structs.rs:30–36; flags bit31 = has verification `MDB_FILE_FLAG_WITH_VERIFICATION = 1<<31`, bit30 = has metadata ext `= 1<<30`, lines 14–18), then `num_entries` × `FileDataSequenceEntry{ xorb_hash: 32B, xorb_flags: u32, unpacked_segment_bytes: u32, chunk_index_start: u32, chunk_index_end: u32 }` (lines 174–183), then (if flagged) `num_entries` × `FileVerificationEntry{ range_hash: 32B, _unused: [u64;2] }` (lines 261–266), then (if flagged) one `FileMetadataExt{ sha256: 32B, _unused: [u64;2] }` (lines 305–310; sha256 stored via `DataHash` transmute, standard sha2 crate — `xet_data/src/processing/sha256.rs:1`). Section terminated by a **bookend header** whose file_hash is all-1 bits (file_structs.rs:70–80; written at shard_format.rs:402–404).
- **Xorb-info section** (`xorb_info_offset`): per xorb — `XorbChunkSequenceHeader{ xorb_hash: 32B, xorb_flags: u32, num_entries: u32, num_bytes_in_xorb: u32, num_bytes_on_disk: u32 }` (xorb_structs.rs:17–24) then `num_entries` × `XorbChunkSequenceEntry{ chunk_hash: 32B, chunk_byte_range_start: u32 (cumulative uncompressed start), unpacked_segment_bytes: u32, flags: u32 (bit31 = global-dedup candidate, `MDB_CHUNK_WITH_GLOBAL_DEDUP_FLAG = 1<<31`, line 12), _unused: u32 }` (lines 91–98). Terminated by all-1s bookend (lines 45–55).
- **Lookup tables** (client writes them; stripped from upload — see §3.6): file lookup `(truncate_hash(file_hash): u64, file_info_entry_index: u32)` sorted; xorb lookup `(truncate_hash(xorb_hash): u64, entry_index: u32)`; chunk lookup `(truncate_hash(keyed_chunk_hash): u64, (xorb_entry_index: u32, chunk_offset: u32))` sorted (shard_format.rs:325–355, 409–463). Indices count 48-byte records from the start of the respective section.
- **Footer** (200 bytes; lines 102–224): `version: u64 = 1` (line 37), `file_info_offset, xorb_info_offset, file_lookup_offset, file_lookup_num_entry, xorb_lookup_offset, xorb_lookup_num_entry, chunk_lookup_offset, chunk_lookup_num_entry` (u64 each), `chunk_hash_hmac_key: 32B` (zero = unkeyed), `shard_creation_timestamp: u64`, `shard_key_expiry: u64` (u64::MAX = none), `_buffer: [u64;6]`, `stored_bytes_on_disk, materialized_bytes, stored_bytes, footer_offset` (u64 each).
- HMAC-keyed chunk hashes: when footer `chunk_hash_hmac_key != 0`, chunk hashes in the shard are `chunk_hash.hmac(key)`; the client applies the key when doing lookups (`keyed_chunk_hash`, shard_format.rs:599–608). Server-issued global-dedup shards typically use this + an expiry.
- Dedup query against a shard: chunk-lookup on first hash (up to 8 candidate collisions, `get_xorb_info_index_by_chunk`, lines 523–549), then run-extend forward matching consecutive query hashes to consecutive `XorbChunkSequenceEntry`s until mismatch/end-of-xorb, returning `(n_matched, FileDataSequenceEntry{xorb_hash, unpacked bytes, chunk_index_start..end})` (lines 673–757).
- Streaming (footer-less) parse used server-side: `MDBMinimalShard::from_reader(reader, true, true)` (reference use: local_client.rs:1521–1523), which walks sections by bookends.

---

## 3. HTTP API — exact surface the client uses

Base endpoint string, e.g. `https://cas-server.xethub.hf.co` (client default constant `CAS_ENDPOINT = "http://localhost:8080"`, `xet_client/src/cas_client/remote_client.rs:40`). Reference server route table: `simulation/local_server/server.rs:197–222`. OpenAPI: `openapi/cas.openapi.yaml`.

Common headers on every CAS request (not presigned-URL fetches):
- `Authorization: Bearer <token>` via `AuthMiddleware` when `AuthConfig` present (`common/http_client.rs:331–344`); token auto-refreshed 30 s before expiry (`cas_client/auth.rs:30, 203–225`).
- `X-Xet-Session-Id: <session id>` via `SessionMiddleware` when non-empty (`cas_types/mod.rs:17`; http_client.rs:356–369).
- Server may return `X-Request-Id` (`cas_types/mod.rs:19`) — logged only.
- Custom default headers (should include `User-Agent`) via `custom_headers` (`remote_client.rs:76,84`).
- HTTP/1.1 only (`http1_only()`, http_client.rs:90); connect timeout 60 s, read timeout 300 s, pool idle 60 s / 16 conns (`xet_runtime/src/config/groups/client.rs:38–63`). Shard-v1 uploads use a separate client with **no read timeout** (http_client.rs:107–142; remote_client.rs:52–54,350–354).

| # | Method & path | Purpose |
|---|---|---|
| 1 | `GET /v1/reconstructions/{file_id_hex}` | file → terms + per-range presigned URLs (remote_client.rs:226, 743) |
| 2 | `GET /v2/reconstructions/{file_id_hex}` | V2 multi-range variant (remote_client.rs:226) |
| 3 | `GET /v1/reconstructions?file_id=<hex>&file_id=<hex>…` | batch (remote_client.rs:535–568) |
| 4 | `GET /v1/chunks/{prefix}/{chunk_hash_hex}` | global dedup: chunk → shard bytes (remote_client.rs:164–212) |
| 5 | `POST /v1/xorbs/{prefix}/{xorb_hash_hex}` | upload xorb (remote_client.rs:824–934) |
| 6 | `POST /v1/shards` | upload shard, JSON reply (remote_client.rs:337–381) |
| 7 | `POST /v2/shards` | upload shard, NDJSON progress stream (remote_client.rs:384–472) |
| 8 | `GET /v2/file-chunk-hashes/{file_id_hex}` + `X-Range-Dirty` header | range-upload support (remote_client.rs:936–976) |
| 9 | `POST /v1/telemetry` | fire-and-forget telemetry (openapi lines 217–262; can be a 200 stub) |
| 10 | `GET <presigned url>` + `Range` | xorb data fetch (§5.3) |

`{prefix}`: the client sends `ctx.config.data.default_prefix` = **`"default"`** for both xorbs and chunk-dedup queries (`xet_runtime/src/config/groups/data.rs:109`; used at remote_client.rs:41, `shard_interface/native.rs:150`, `file_upload_session.rs:404`). (The OpenAPI file documents hf-prod's `default-merkledb` for dedup, openapi lines 296–302 — but this client sends `default`; accept both.)

Retry engine (`cas_client/retry_wrapper.rs`): 5 attempts, base delay 3 s, exponential ×~3/step with jitter, per-sleep cap 6 min (client.rs config lines 12–31; `exponential_retry_delays` retry_wrapper.rs:485–497). Classification (`default_on_request_success`, lines 513–522): 5xx/408/429 transient-retryable; other 4xx fatal; **501 explicitly fatal-no-retry** (lines 219–222). Network: timeout/connect/incomplete-message/canceled/IO → transient (lines 530–572).

---

## 4. Upload session flow (step-by-step)

Entry: `FileUploadSession::new(Arc<TranslatorConfig>)` (`xet_data/src/processing/file_upload_session.rs:73–124`); per-file `SingleFileCleaner` from `start_clean` (lines 237–247) or `upload_files` (126–225) / `spawn_upload_from_path` (265–285) / `spawn_upload_bytes` (290–318).

1. **Ingest & chunk** — `SingleFileCleaner::add_data[_from_bytes]` splits input into ≤8 MiB blocks (`file_cleaner.rs:142–157`), chunks on a blocking thread (`file_cleaner.rs:168–177`), optionally streams SHA-256 (`Sha256Policy` Compute/Provided/Skip, lines 24–46). Concurrency across files: `file_ingestion_semaphore`, default 8 (high-perf 100) (`processing/constants.rs:19–22`).

2. **Per-chunk dedup** — `FileDeduper::process_chunks` (`deduplication/file_deduplication.rs:80–307`), two passes:
   - Pass 1: for each chunk position not yet resolved, `chunk_hash_dedup_query(&hashes[i..])` → `SessionShardInterface::chunk_hash_dedup_query` (`processing/shard_interface/native.rs:165–191`) which consults, in order: **resumed-session shards** (previously uploaded xorbs; returns `is_uploaded=true`), the **session shard manager** (xorbs cut in this session; `is_uploaded=false`), then the **local shard cache** (`shard-cache` dir: shards downloaded from global dedup + shards from prior successful uploads, ≤3-week expiry). A match consumes `n_deduped` chunks and yields a `FileDataSequenceEntry`.
   - **Global dedup query trigger** (file_deduplication.rs:156–176): only on pass 1, only for an *undeduplicated* chunk where `global_chunk_index == 0` (first chunk of the file) **or** `chunk_hash % 1024 == 0` (`hash_is_global_dedup_eligible`, `metadata_shard/constants.rs:5–22`; modulus = 4th u64 LE). Spawns background `GET /v1/chunks/default/{chunk_hash}`; the spacing counter `min_spacing_between_global_dedup_queries` is initialized to 0 at this rev (file_deduplication.rs:74) so every eligible chunk queries. Gated by `HF_XET_DEDUPLICATION_GLOBAL_DEDUP_QUERY_ENABLED` (default true; `deduplication_interface.rs:29–31`, `config/groups/deduplication.rs`).
   - After pass 1, `complete_global_dedup_queries` joins the background queries (deduplication_interface.rs:72–82). Response body (raw shard bytes) is imported into the cache shard manager (`native.rs:147–162`, `import_shard_from_bytes`). If any shard arrived, **pass 2** re-runs dedup over unresolved chunks (file_deduplication.rs:127–185).
   - Dedup-miss handling on the wire: `query_dedup_api` uses `.with_429_no_retry().with_expected_404()`; **any HTTP-status error (404, 429, 5xx-after-retries…) is coerced to “miss” (`Ok(None)`)** (remote_client.rs:184–212 + `native.rs:147–153`'s `let Ok(Some(..)) else return false`).
   - Local-window dedup: chunks matching chunks earlier in the *current un-cut xorb* dedupe against `MerkleHash::marker()` placeholder entries (file_deduplication.rs:190–199, 374–405).
   - **Defrag prevention**: a dedupe range is rejected (data re-added as new) when the rolling mean chunks-per-range over the last 128 ranges drops below 8.0 (hysteresis ×0.5) and the candidate run is smaller than the current mean (`deduplication/defrag_prevention.rs`, config `config/groups/deduplication.rs`: `nranges_in_streaming_fragmentation_estimator=128`, `min_n_chunks_per_range=8.0`, hysteresis 0.5). Continuations of the previous entry are always allowed (file_deduplication.rs:203–239).
   - File-entry construction: consecutive chunks from the same xorb extend the previous `FileDataSequenceEntry` (`unpacked_segment_bytes += …`, `chunk_index_end += …`); otherwise a new entry is pushed (file_deduplication.rs:262–296, 317–337).

3. **Xorb cut policy** — new-data buffer cut into a xorb when adding a chunk would exceed `MAX_XORB_BYTES = 64 MiB` **or** `MAX_XORB_CHUNKS = 8192` (file_deduplication.rs:251–259; constants `xorb_object/constants.rs:17,22`). Marker entries are back-patched with the just-computed xorb hash (`cut_new_xorb`, lines 340–372). Additionally, at file completion the leftover `DataAggregator` is merged with the session-wide aggregator; if the merge would exceed either limit the larger of the two is cut & uploaded immediately (`file_upload_session.rs:443–505`, swap logic lines 474–491). At `finalize`, the remaining aggregate is cut (lines 634–641).

4. **Xorb upload** — `register_new_xorb` (`file_upload_session.rs:343–439`): session-level duplicate xorbs skipped (completion tracker, lines 356–371); empty xorbs skipped (373–378); xorb metadata (`MDBXorbInfo`) added to the session shard manager *before* upload so other files can dedup against it (383–384); serialization (no footer) on a blocking thread (386–400); `client.acquire_upload_permit()` (adaptive concurrency: initial 2, min 1, max 64 — client.rs config lines 202–223); `POST /v1/xorbs/default/{hash}` with `Content-Length` set explicitly and a streamed body chunked at `upload_reporting_block_size = 512 KiB` (remote_client.rs:850–898); expects JSON `{"was_inserted": bool}` (`cas_types/mod.rs:21–24`; value only logged, remote_client.rs:906–931). After success: `add_uploaded_xorb_block` stages the xorb metadata into `shard_session_directory/xorb_metadata/` as small shards, flushed every 20 s or 64 xorbs (`session_xorb_metadata_flush_interval/…_max_count`; native.rs:201–226) — this is the **resume** record merged back on the next session (`native.rs:80–129`, `file_upload_session` resume via `merge_shards_background`).

5. **Per-file finalize** — `SingleFileCleaner::finish` → `FileDeduper::finalize(metadata_ext)` (`file_deduplication.rs:412–450`): computes `file_hash = file_hash(&chunk_hashes)`; builds `FileDataSequenceHeader::new(file_hash, n_entries, /*verification=*/true, metadata_ext.is_some())`; builds **one `FileVerificationEntry` per segment** = `range_hash_from_chunks(chunk hashes of exactly that segment's chunks in file order)` (lines 422–438); optional `FileMetadataExt(sha256)`. `MDBFileInfo` is registered in the session shard when its xorb is cut (`process_aggregated_data_as_xorb_impl` → `shard_interface.add_file_reconstruction_info`, file_upload_session.rs:542–575). Returned to caller: `XetFileInfo { hash: <file_hash hex>, file_size, sha256 }` (`file_cleaner.rs:251–255`; `processing/xet_file.rs:8–19` — also the JSON pointer-file schema, `{"hash": "...", "file_size": N, "sha256": "..."}`).

6. **Shard flush/upload timing** — only in `FileUploadSession::finalize` (`file_upload_session.rs:634–680`): cut last xorb → **join all xorb upload tasks** → `shard_interface.upload_and_register_session_shards()` (`native.rs:243–340`): flush session shard manager to disk, `consolidate_shards_in_directory` merging to ≤ `shard.max_target_size = 64 MiB` (`config/groups/shard.rs:17`), then per shard: acquire upload permit, **read the file up to `file_lookup_offset` only and rewrite the header with `footer_size = 0`** (`read_shard_to_bytes_remove_footer`, native.rs:343–361) — i.e. the upload body is header + file-info section (+bookend) + xorb-info section (+bookend), **no lookup tables, no footer** — `client.upload_shard(bytes, permit, progress_cb)`, then move the shard to the cache dir with a 3-week expiry stamp (`MDB_SHARD_LOCAL_CACHE_EXPIRATION = 3*7*24*3600 s`, metadata_shard/constants.rs:16) and register it for future dedup (native.rs:305–321). Invariant: **all xorbs are fully uploaded before any shard is sent** (comment native.rs:241–243).

7. **Shard upload wire** — `upload_shard` prefers V2, falls back to V1 on 404/501 with the detected version cached per client instance (`AtomicU32`, remote_client.rs:59–60, 475–519).
   - V1: `POST /v1/shards`, body = shard bytes, response JSON `{"result": 0|1}` (`UploadShardResponseType::Exists=0 / SyncPerformed=1`, serde_repr integers, cas_types/mod.rs:297–307).
   - V2: `POST /v2/shards`, `Content-Length` set, streamed body; response is `200 OK` + `application/x-ndjson` frames of `ShardUploadEvent` (cas_types/mod.rs:320–343):
     `{"type":"validating","verified":N,"total":M}`, `{"type":"committing","stage":"uploading"|"syncing"}`, `{"type":"result"}` (terminal success), `{"type":"error","message":"…","retryable":bool}` (terminal failure — HTTP status is already 200, the frame **is** the error signal; `retryable:true` re-drives the whole upload through the retry budget). Unknown `type` values are ignored/non-terminal (`#[serde(other)] Unknown`). Parser: `cas_client/shard_upload_v2.rs:18–110`; frame cap 1 MiB (`MAX_NDJSON_LINE_BYTES`, line 11); trailing frame without `\n` accepted (lines 52–65); **stream EOF without a terminal frame = fatal, non-retried** (lines 67–69). Server should re-emit the last frame ~every 20 s as heartbeat because the V2 path uses the 300 s read-timeout client (remote_client.rs:403–407).
8. Return: `finalize()` → `DeduplicationMetrics`; variants return `Vec<MDBFileInfo>` / `GroupProgressReport` (file_upload_session.rs:752–781).

---

## 5. Download flow (step-by-step)

Entry: `FileDownloadSession::new(config, chunk_cache: Option<Arc<dyn ChunkCache>>)` (`xet_data/src/processing/file_download_session.rs:40–69`) or `from_client` (76–90). Methods: `download_file(&XetFileInfo, &Path)` (347–352), `download_file_background` (328–343), `download_to_writer(file, RangeBounds, W)` (387–421), `download_stream`/`download_stream_range`/`download_unordered_stream` (140–202). Range convention: `FileRange` end-exclusive; unbounded → full file; open end = `u64::MAX` sentinel (424–450).

1. **Reconstruction query** — `FileReconstructor::run_impl` (`file_reconstruction/file_reconstructor.rs:261–341`) drives a `ReconstructionTermManager` (`reconstruction_terms/manager.rs:48–94`) which prefetches whole-file or ranged reconstruction metadata in **blocks**: first two blocks of 256 MB and 512 MB (`min_reconstruction_fetch_size = 256mb`, manager.rs:83–85; `config/groups/reconstruction.rs:13`), then adaptively sized to `target_block_completion_time = 15 min × observed byte rate` clamped to [256 MB, 8 GB] (manager.rs:198–237; reconstruction.rs:21,63). Each block: `client.get_reconstruction(file_hash, Some(FileRange))`.
   - HTTP: `GET /v{2|1}/reconstructions/{file_hash_hex}` with header `Range: bytes={start}-{end}` **inclusive-end** (`HttpRange::from(FileRange)` does `end-1`; remote_client.rs:250–254, `cas_types/mod.rs:91–96`). V2 preferred; 404/501 → V1 fallback + cached version (remote_client.rs:296–335). **416 → `Ok(None)`** = "range starts past EOF" (remote_client.rs:271–273; manager.rs:310–315 shrinks `known_final_byte_position`). A response covering less than requested ⇒ EOF discovered (manager.rs:299–309).
   - Response JSON (V1, `QueryReconstructionResponse`, cas_types/mod.rs:204–217): `offset_into_first_range: u64`, `terms: [{hash: hex, unpacked_length: u32, range: {start,end}}]` (chunk indices end-exclusive), `fetch_info: {xorb_hex: [{range:{start,end}, url, url_range:{start,end}}]}` (`url_range` inclusive-end byte offsets into the **serialized xorb**). V2 (`QueryReconstructionResponseV2`, lines 222–249): same `terms`, plus `xorbs: {xorb_hex: [{url, ranges: [{chunks:{start,end}, bytes:{start,end}}]}]}`; V1 auto-converted to V2 client-side (lines 251–277).
2. **Term/fetch_info consumption** — `retrieve_file_term_block` (`reconstruction_terms/file_term.rs:113–351`): for each term, find the fetch entry whose chunk range **contains** the term's range (`r.chunks.start <= term.range.start && term.range.end <= r.chunks.end`); missing containment ⇒ `CorruptedReconstruction` error (lines 166–237). With `enable_multirange_fetching = false` (default; client.rs config lines 276–285) each `XorbRangeDescriptor` becomes its own single-range `XorbBlock`; when true, one block per `XorbMultiRangeFetch` (all ranges → one multi-range HTTP request). `offset_into_first_range` applies only to term 0 (lines 241–249); the last term is trimmed to the requested end (lines 304–313). Blocks are deduped by `(xorb_hash, first_chunk_start)` so repeated terms share one download (lines 140–143, 166–232). `uncompressed_size_if_known` computed from term coverage DP (`xorb_block.rs:201–250`).
3. **Per-term fetch** — first term needing a block triggers `XorbBlock::retrieve_data` (single-flight `OnceCell`; `xorb_block.rs:97–184`): optional chunk-cache hit path; else `client.acquire_download_permit()` (adaptive: initial 4, min 1, max 64 — client.rs lines 225–247) then `Client::get_file_term_data(url_provider, permit, cb, size_hint)` (remote_client.rs:579–734):
   - GET on the presigned URL with `Range:` `bytes=S-E` (single, inclusive) or `bytes=S1-E1,S2-E2,…` for multi-range (lines 615–625). **Unauthenticated** plain client (no Bearer/session headers) — it's a presigned URL (line 587 uses `self.http_client`).
   - **403 handling**: on `403 Forbidden` the closure calls `url_info.refresh_url()` then the `RetryWrapper` (`with_retry_on_403`) retries (lines 599–642; retry_wrapper.rs:200–203). Refresh = re-issue the reconstruction query for the same block byte-range; the returned range must match exactly or `CorruptedReconstruction` (`retrieval_urls.rs:63–135`); single-flighted via acquisition-id compare (lines 69–90).
   - Response handling (lines 644–729): if `Content-Type` contains `multipart/byteranges` → RFC 7233 §4.1 parse (`cas_client/multipart.rs:16–117`; boundary from Content-Type, parts carry `Content-Range: bytes S-E/Total`, parts sorted by start), each part fed to `deserialize_chunks`, concatenated via `append_chunk_segment`. Otherwise the body stream is fed directly to the async chunk deserializer. **Status may be 200 or 206** — anything 2xx passes (`error_for_status` only rejects ≥400); the reference server returns 206 for ranged/multipart responses and 200 otherwise (handlers.rs:437,445,481,504). The body must be the exact serialized-chunk byte span `url_range` of the xorb (chunk-aligned).
   - **Client-side verification = length only**: decompressed total must equal `uncompressed_size_if_known` when derivable (remote_client.rs:688–695, 715–722); each chunk's decompressed size must match its header u24 (xorb_chunk_format.rs:182–186). **No chunk-hash / range-hash cryptographic verification on download**; final `n_bytes == file_size` check produces `DataError::SizeMismatch` (file_download_session.rs:361–368, 406–419).
4. **Assembly** — each `FileTerm` slices its bytes out of the block via `chunk_offsets` + `offset_into_first_range` (file_term.rs:46–53); `SequentialWriter` (background thread, optional `write_vectored`) or `UnorderedWriter` emits `(offset, Bytes)`; memory backpressure via a byte semaphore: 2 GB base + 512 MB per active download, cap 8 GB (`file_reconstructor.rs:292–324`; reconstruction.rs:31–47). Chunk cache `put` is fire-and-forget (xorb_block.rs:161–176).

---

## 6. Client-facing integration surface (custom endpoint)

High-level (recommended):
```rust
// xet_runtime::core::XetContext::default() -> XetContext (env-config via HF_XET_* vars)
let session_ctx = xet_data::configurations::SessionContext {   // configurations.rs:16-24
    endpoint: "https://my-cas.example".into(),                 // "local://<path>" and "memory://" special-cased
    auth: Option<xet_client::cas_client::auth::AuthConfig>,    // { token: String, token_expiration: u64 epoch-secs, token_refresher: Arc<dyn TokenRefresher> } auth.rs:143-151; AuthConfig::maybe_new auth.rs:163-186; None = no Authorization header
    custom_headers: Option<Arc<http::HeaderMap>>,              // should carry User-Agent
    repo_paths: vec!["".into()],
    session_id: Option<String>,                                // becomes X-Xet-Session-Id; None => random UniqueId (file_upload_session.rs:83-88)
};
let cfg = Arc::new(xet_data::configurations::TranslatorConfig::new(&ctx, session_ctx)?); // computes shard cache/session dirs under xet cache root keyed by first-16-chars + hash of endpoint (configurations.rs:98-142, 194-209)
let up = xet_data::FileUploadSession::new(cfg.clone()).await?;   // or ::dry_run
let (id, mut cleaner) = up.start_clean(Some("name".into()), Some(len), xet_data::Sha256Policy::Skip)?;
cleaner.add_data(&bytes).await?; let (xet_file_info, metrics) = cleaner.finish().await?;
up.finalize().await?;
let dl = xet_data::FileDownloadSession::new(cfg, None /*chunk cache: xet_client::chunk_cache::get_cache*/).await?;
dl.download_file(&xet_file_info, path).await?;
```
Exports: `xet_data/src/lib.rs` + `processing/mod.rs` (re-exports `FileUploadSession`, `FileDownloadSession`, `SingleFileCleaner`, `Sha256Policy`, `XetFileInfo`, `create_remote_client`, `upload_ranges`, `CasClient = xet_client::cas_client::Client`).

Low-level: `RemoteClient::new(ctx, endpoint, &auth, session_id, dry_run, custom_headers) -> Arc<RemoteClient>` (remote_client.rs:143–152; unix-socket variant `new_with_socket` 77–130) implements `trait Client` (`cas_client/interface.rs:50–127`): `get_reconstruction`, `batch_get_reconstruction`, `get_file_reconstruction_info`, `query_for_global_dedup_shard`, `acquire_upload_permit`/`acquire_download_permit` → `ConnectionPermit`, `upload_xorb(prefix, SerializedXorbObject, Option<ProgressCallback>, permit) -> u64`, `upload_shard(Bytes, permit, Option<ShardUploadProgressCallback>)`, `get_file_term_data(Box<dyn URLProvider>, permit, cb, size_hint) -> (Bytes, Vec<u32>)`, `get_file_chunk_hashes`. `URLProvider` (interface.rs:33–41): `retrieve_url() -> (String, Vec<HttpRange>)`, `refresh_url()` (called on 403). Endpoint dispatch: `create_remote_client` (`processing/remote_client_interface.rs:8–39`): `local://` → `LocalClient`, `memory://` → `MemoryClient`, else `RemoteClient`. Adaptive-concurrency controllers are shared per (ctx, endpoint) (remote_client.rs:991–1011).

Range-append flow (optional): `xet_data::upload_ranges` / `DirtyInput` + `GET /v2/file-chunk-hashes/{file_id}` with `X-Range-Dirty: bytes=A-B,C-D` (inclusive ends; `cas_types/mod.rs:389`, remote_client.rs:946–960) → `FileChunkHashesResponse` (camelCase JSON: `totalChunks`, `fileSize`, `windows[].dirtyByteRange:[u64;2]`, `hashRanges: [MerkleHashSubtree|null]`, `gapVerification: [hex]`; cas_types/mod.rs:395–418).

---

## 7. Progress / telemetry hooks (all no-op-able)

- `ProgressCallback = Arc<dyn Fn(u64 delta, u64 completed, u64 total) + Send + Sync>` (`progress_tracked_streams.rs:11`) — optional on `upload_xorb` / `get_file_term_data`; pass `None`.
- `ShardUploadProgressCallback = Arc<dyn Fn(ShardUploadProgressType)>` with `Transfer(u64) / DecrementTransfer(u64) / Response(&ShardUploadEvent)` (interface.rs:20–29) — optional; pass `None`. V1 synthesizes `Transfer(total)+Result` (remote_client.rs:373–380).
- Session-level progress is internal (`CompletionTracker` / `GroupProgress`); consumers poll `session.report() / item_report(id)` (file_upload_session.rs:732–750) — ignore freely.
- Transfer telemetry (`cas_client/telemetry`): built only when `HF_XET_TELEMETRY_ENABLED` (default true), not dry-run, and endpoint scheme is http/https (`telemetry/mod.rs:102–143`). Posts JSON to `POST {endpoint}/v1/telemetry`, fire-and-forget, never retried (openapi lines 217–262; envelope fields `time`, `event` ∈ {`xet_upload_summary`,`xet_download_summary`,`xet_transfer_heartbeat`}, `session_id`, `userAgent`, flat `metrics` map — openapi lines 464–499). A stub server can always return 200 (reference: handlers.rs:1022–1035). Heartbeats only after 300 s, every 300 s (`config/groups/telemetry.rs:28,35`). Disable wholesale with `HF_XET_TELEMETRY_ENABLED=false`.
- All tracing/log output is `tracing`-based; `INFORMATION_LOG_LEVEL` is DEBUG unless the `elevated_information_level` feature is on (`cas_client/mod.rs:33–38`).

---

## 8. Server behavior assumptions & easy-to-get-wrong details

1. **Xorb POST body has no footer.** Validate by re-parsing chunks (8-byte headers + payloads), decompressing, recomputing chunk hashes and the aggregated xorb hash, and comparing against the URL-path hash (`reconstruct_xorb_with_footer`, xorb_object_format.rs:1747+; LocalClient rejects mismatch, local_client.rs:1613–1628). Reply JSON `{"was_inserted": bool}`; the client treats any 200 as success regardless of the boolean → **xorb upload must be idempotent** (re-uploads happen on retry, resume, and across sessions).
2. **Shard POST body is truncated**: header (with `footer_size=0`) + file-info + bookend + xorb-info + bookend; **no lookup tables, no footer** (native.rs:343–361). Parse it streaming/bookend-terminated (`MDBMinimalShard::from_reader(_, true, true)`). Duplicate shard upload must succeed (V1 `result:0` Exists is fine).
3. **Shards may reference xorbs not uploaded in this session** (dedup against cached/global shards, and resumed sessions). Do not require every referenced xorb in the same request; do verify each `FileDataSequenceEntry`'s chunk range and each `FileVerificationEntry` range-hash against stored xorb metadata if you want hf-equivalent "validating" semantics — the client always includes verification entries (header flag bit31) and usually `metadata_ext` (bit30) unless `Sha256Policy::Skip`.
4. **`/v2/shards` failure contract**: once you've sent 200 and started streaming, errors MUST be an in-stream `{"type":"error",...}` frame (cas_types/mod.rs:330–339; handlers.rs:609–654). Ending the stream without a terminal frame is a **fatal, non-retried** client error (shard_upload_v2.rs:67–69). Keep frames <1 MiB; heartbeat by re-emitting the last frame ≤ every ~20 s (client read timeout 300 s applies on V2, remote_client.rs:403–407). V1 has no read timeout — long silent processing OK.
5. **404/501 drive protocol negotiation**: the first `/v2/reconstructions` or `/v2/shards` 404/501 flips the client permanently (per `RemoteClient` instance) to V1 (remote_client.rs:296–335, 475–519). A file-not-found on a *V2-capable* server must therefore be distinguishable — the client will *also* interpret a 404 for a nonexistent file as "V2 unsupported", fall back to V1, get 404 again, and only then fail. Any other status on V2 (e.g. 400) is a hard error with no fallback.
6. **416 semantics**: reconstruction with a `Range` starting at/after EOF ⇒ `416` (not 404). The client uses this to discover file length for open-ended ranges (remote_client.rs:271–273; manager.rs:310–315). Reference implementation: handlers.rs:275, 332. Range header on reconstructions is inclusive-end `bytes=S-E`; also accept `bytes=S-` and `bytes=-N` (handlers.rs:84–137) though this client only emits `S-E`.
7. **Dedup miss = 404 with empty/any body**; success = 200 whose body is a complete shard file (with footer + lookup tables; typically HMAC-keyed chunk hashes + `shard_key_expiry` set). The client coerces *any* status error (including 429/5xx) to a miss and never retries this call (remote_client.rs:184–212) — global dedup is purely best-effort.
8. **fetch_info / presigned URL invariants**: every term's chunk range must be contained in exactly one advertised range for that xorb, else `CorruptedReconstruction` (file_term.rs:166–237). `url_range`/`bytes` must be exactly the byte span of those serialized chunks in the stored xorb (inclusive end); the client sends that exact `Range` header and by default splits V2 multi-range entries into parallel single-range GETs (`enable_multirange_fetching=false`, client.rs:276–285). Multi-range responses must be `multipart/byteranges` with `Content-Range` per part (multipart.rs:16–117). 200 or 206 both accepted; the fetch is unauthenticated (no Bearer/session headers).
9. **URL expiry/refresh**: expired presigned URLs must return **403** — that (and only that) triggers refresh-then-retry; the refreshed reconstruction for the identical byte range must return the identical range (retrieval_urls.rs:108–113) with fresh URLs. Retry budget: 5 attempts/3 s base/6 min cap (client.rs:12–31).
10. **Eventual consistency tolerance**: after a shard upload succeeds, the client caches that shard locally for **3 weeks** and dedups future uploads against it without re-asking the server — a later upload may produce shards referencing xorbs the server saw weeks ago; deleting xorbs referenced by recently-issued shards breaks uploads. (`MDB_SHARD_LOCAL_CACHE_EXPIRATION`, metadata_shard/constants.rs:16; native.rs:305–321.) The per-upload xorb uniqueness nonce (xorb_object_format.rs:36–47) exists precisely for a snapshot-vs-delete race on hf-prod; footer bytes differing while the hash is unchanged must be tolerated.
11. **`offset_into_first_range` / trimming**: servers may return chunk-aligned terms that over-cover the requested range; the client discards `offset_into_first_range` bytes at the front (first term only) and trims the tail (file_term.rs:241–249, 304–313). Don't return terms that start after `range.start`'s chunk.
12. **Prefix**: accept `default` for both `/v1/xorbs/{prefix}/…` and `/v1/chunks/{prefix}/…` (data.rs:109); hf-docs' `default-merkledb` appears only in the OpenAPI text.
13. **429**: retried (transient) everywhere except the dedup endpoint (explicit `with_429_no_retry`); 408 transient; other 4xx fatal; 501 fatal (retry_wrapper.rs:216–226, 513–522).
14. **No cryptographic verification client-side on download** (lengths only, §5.3) — server integrity is trusted; corrupted-but-length-consistent data reaches the user. Conversely the server *is* expected to verify uploads (hash-addressed xorbs, shard verification entries).
15. Reference server for byte-exact behavior: routes `server.rs:197–222`; handlers for every endpoint incl. NDJSON success sequence (`validating 0/1 → validating 1/1 → committing uploading → committing syncing → result`, handlers.rs:656–698) and the multipart fetch encoding (handlers.rs:450–482).