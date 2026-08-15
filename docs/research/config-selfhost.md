# Dossier: xet-core configuration, env vars, versioning, and self-hosting affordances

Repo: huggingface/xet-core @ `77fc84d3d` (2026-08-11).
Root path (call it `$XC` below; every cite is `$XC/<relpath>:<line>`):
`/tmp/claude-1000/-home-bertha-candace-server--claude-worktrees-xet-cas-storage-transfer-07271f/046ce30e-cd86-469f-9c94-1dbeda51d318/scratchpad/xet-core`

---

## 0. Config system mechanics (read this first)

- All runtime config lives in `XetConfig`, composed of groups `data, shard, deduplication, chunk_cache, client, log, reconstruction, xorb, session, telemetry` (+ `system_monitor` on non-wasm) — `$XC/xet_runtime/src/config/macros.rs:7-9`, `$XC/xet_runtime/src/config/xet_config.rs:4-15`.
- Env var name pattern is **`HF_XET_{GROUP}_{FIELD}`** (group = module name uppercased), generated at compile time — `$XC/xet_runtime/src/config/macros.rs:86-113`. `XetConfig::new()` = defaults + env overrides + high-performance overrides if enabled — `$XC/xet_runtime/src/config/xet_config.rs:140-146`.
- Backward-compat **aliases** (checked only when the primary var is unset) — `$XC/xet_runtime/src/config/aliases.rs:5-39`. Notable: `HF_XET_FIXED_UPLOAD_CONCURRENCY` / `HF_XET_FIXED_DOWNLOAD_CONCURRENCY` alias *simultaneously* to initial/min/max AC concurrency (lines 33-38), i.e. they pin concurrency to a fixed value.
- Programmatic override by dotted path: `XetConfig::with_config("group.field", value)` / `get(path)` / `all_keys()` — `$XC/xet_runtime/src/config/xet_config.rs:87-110,179-184`. Python: `hf_xet.XetConfig().with_config("data.max_concurrent_file_ingestion", 2)` or dict batch form; `get`, `__getitem__`, `items`, `keys` — `$XC/hf_xet/src/config.rs:41-89`; passed into `hf_xet.XetSession(config=...)` — `$XC/hf_xet/src/py_xet_session.rs:47-55`.
- `ByteSize` parsing: decimal suffixes `kb/mb/gb/tb/pb` = powers of 1000, binary `kib/mib/gib/tib/pib` = powers of 1024, bare number = bytes, float accepted, rounded — `$XC/xet_runtime/src/utils/byte_size.rs:45-80`. So the `"8mb"` default = 8,000,000 bytes (confirmed by test: `"16MB"` → 16_000_000, `$XC/xet_runtime/src/config/xet_config.rs:216`). Durations parse humantime-style (`"500ms"`, `"90s"`, `"30s"` — xet_config.rs:205-215, telemetry tests 127-131).
- **High-performance mode**: `HF_XET_HIGH_PERFORMANCE` or `HF_XET_HP` (truthy `1`/`true`) — `$XC/xet_runtime/src/utils/configuration_utils.rs:328-346`. Effects (`$XC/xet_runtime/src/config/xet_config.rs:159-177`): `data.max_concurrent_file_ingestion` 8→100; AC max up/down 64→124, min 1→4, initial up 2→16 / down 4→16; reconstruction min/max fetch 256MB/8GB→1GB/16GB; download buffers 2GB/512MB/8GB→16GB/2GB/64GB.

---

## 1. Every env var / config knob (name, default, effect)

### Endpoint selection
| Env var | Default | Effect | Cite |
|---|---|---|---|
| `HF_XET_DATA_DEFAULT_CAS_ENDPOINT` | `"http://localhost:8080"` | CAS endpoint used when none is passed per-operation | `$XC/xet_runtime/src/config/groups/data.rs:88-93`; consumed at `$XC/xet_pkg/src/xet_session/common.rs:62` and `$XC/xet_pkg/src/legacy/data_client.rs:38,98,149` |
| `HF_XET_DATA_LOCAL_CAS_SCHEME` (alias `HF_XET_LOCAL_CAS_SCHEME`) | `"local://"` | Prefix that routes an endpoint string to the **local filesystem client** instead of HTTP | `$XC/xet_runtime/src/config/groups/data.rs:16-21`; dispatch `$XC/xet_data/src/processing/remote_client_interface.rs:16-38` |
| `HF_XET_DATA_DEFAULT_PREFIX` | `"default"` | Prefix segment for CAS/shard ops (`/v1/xorbs/{prefix}/{hash}` etc.) | `$XC/xet_runtime/src/config/groups/data.rs:104-109`; also consts `CAS_ENDPOINT`/`PREFIX_DEFAULT` at `$XC/xet_client/src/cas_client/remote_client.rs:40-41` |
| `HF_XET_CLIENT_UNIX_SOCKET_PATH` | None (TCP) | All CAS HTTP over a Unix domain socket; https URLs rewritten to http for the proxy | `$XC/xet_runtime/src/config/groups/client.rs:249-256`; `$XC/xet_client/src/common/http_client.rs:21-41,78-95` |
| `HF_ENDPOINT` | `https://huggingface.co` (xtool default) | **Hub** endpoint (not CAS) for token minting; used by `xtool` and `git_xet`; not read by the core library itself | `$XC/xet_pkg/src/bin/xtool/main.rs:20,100`, `$XC/git_xet/src/constants.rs:17` |
| `HF_TOKEN` | – | Hub bearer token for `xtool`/`git_xet` | `$XC/xet_pkg/src/bin/xtool/main.rs:104`, `$XC/git_xet/src/constants.rs:16` |

Special endpoint `"memory://"` = in-memory client — `$XC/xet_data/src/processing/configurations.rs:38-41,56-65`.

### Concurrency / HTTP client (`client` group, `$XC/xet_runtime/src/config/groups/client.rs`)
| Field (env = `HF_XET_CLIENT_<UPPER>`) | Default | Line |
|---|---|---|
| `retry_max_attempts` | 5 | 12 |
| `retry_base_delay` | 3000ms | 20 |
| `retry_max_duration` (per-attempt backoff cap) | 6min | 31 |
| `idle_connection_timeout` | 60s | 38 |
| `max_idle_connections` | 16 | 45 |
| `connect_timeout` | 60s | 53 |
| `read_timeout` (between packets; shard-upload client has none) | 300s | 63 |
| `upload_reporting_block_size` (alias `HF_XET_UPLOAD_REPORTING_BLOCK_SIZE`) | 524288 | 70 |
| `enable_adaptive_concurrency` | true | 77 |
| `ac_min_adjustment_window_ms` | 500 | 84 |
| `ac_min_bytes_required_for_adjustment` | 20mb | 94 |
| `ac_num_transmissions_required_for_adjustment` | 1 | 102 |
| `ac_latency_rtt_half_life` | 64.0 | 116 |
| `ac_success_tracking_half_life` | 8.0 | 127 |
| `ac_target_rtt` | 60s | 135 |
| `ac_max_healthy_rtt` | 90s | 143 |
| `ac_rtt_success_max_quantile` | 0.95 | 156 |
| `ac_healthy_success_ratio_threshold` | 0.8 | 166 |
| `ac_unhealthy_success_ratio_threshold` | 0.5 | 175 |
| `ac_max_reference_transmission_size` | 64mb | 183 |
| `ac_min_reference_transmission_size` | 1mb | 192 |
| `ac_logging_interval_ms` | 10000 | 199 |
| `ac_max_upload_concurrency` | 64 | 207 |
| `ac_min_upload_concurrency` | 1 | 215 |
| `ac_initial_upload_concurrency` | 2 | 223 |
| `ac_max_download_concurrency` | 64 | 231 |
| `ac_min_download_concurrency` | 1 | 239 |
| `ac_initial_download_concurrency` | 4 | 247 |
| `reconstruction_api_version` | None (auto: try V2, fall back V1 on 404/501) | 258-265 |
| `shard_api_version` | None (same auto-detect) | 267-274 |
| `enable_multirange_fetching` | false (split V2 multi-range into parallel single-range requests) | 276-285 |

Fixed-concurrency shortcuts: `HF_XET_FIXED_UPLOAD_CONCURRENCY`, `HF_XET_FIXED_DOWNLOAD_CONCURRENCY` — aliases.rs:33-38.

### Data pipeline (`data` group, `$XC/xet_runtime/src/config/groups/data.rs`)
| Field | Default | Line |
|---|---|---|
| `min_spacing_between_global_dedup_queries` | 256 chunks (~4MB) | 14 |
| `local_cas_scheme` | `local://` | 21 |
| `max_concurrent_file_ingestion` | 8 (100 in HP mode) | 30 |
| `max_concurrent_file_downloads` | 8 | 37 |
| `ingestion_block_size` | 8mb | 44 |
| `progress_update_interval` | 200ms | 52 |
| `progress_update_speed_sampling_window` | 10s | 61 |
| `progress_update_speed_min_observations` | 4 | 71 |
| `session_xorb_metadata_flush_interval` | 20s | 78 |
| `session_xorb_metadata_flush_max_count` | 64 | 86 |
| `default_cas_endpoint` | `http://localhost:8080` | 93 |
| `aggregate_progress` | true | 102 |
| `default_prefix` | `default` | 109 |
| `staging_subdir` | `staging` | 116 |

### Dedup (`deduplication` group, `$XC/xet_runtime/src/config/groups/deduplication.rs`)
`nranges_in_streaming_fragmentation_estimator`=128 (:7), `min_n_chunks_per_range_hysteresis_factor`=0.5 (:19), `min_n_chunks_per_range`=8.0 (:25), `global_dedup_query_enabled`=true (`HF_XET_DEDUPLICATION_GLOBAL_DEDUP_QUERY_ENABLED`, :34).

### Shard (`shard` group, `$XC/xet_runtime/src/config/groups/shard.rs`)
`target_size`=67108864 (:10), `max_target_size`=67108864 (:17), `cache_size_limit`=16gb (:30), `chunk_index_table_max_size`=67108864 (:38), `cache_subdir`=`"shard-cache"` (:45). Aliases: old `HF_XET_MDB_SHARD_*` and `HF_XET_CHUNK_INDEX_TABLE_MAX_SIZE` — aliases.rs:21-25.

### Reconstruction/download buffers (`reconstruction` group, `$XC/xet_runtime/src/config/groups/reconstruction.rs`)
`min_reconstruction_fetch_size`=256mb (:13), `max_reconstruction_fetch_size`=8gb (:21), `download_buffer_size`=2gb (:31), `download_buffer_perfile_size`=512mb (:39), `download_buffer_limit`=8gb (:47), `completion_rate_estimator_half_life`=4.0 (:55), `target_block_completion_time`=15min (:63), `min_prefetch_buffer`=1gb (:72), `use_vectored_write`=true (:81).

### Compression (`xorb` group, `$XC/xet_runtime/src/config/groups/xorb.rs`)
- `HF_XET_XORB_COMPRESSION_POLICY` — valid values **`""`, `"auto"`, `"none"`, `"lz4"`, `"bg4-lz4"`**, default `"auto"` (:22). Alias `HF_XET_DATA_XORB_COMPRESSION_POLICY` (aliases.rs:31).
- `HF_XET_XORB_COMPRESSION_SCHEME_RETEST_INTERVAL` — default 32; 0 = once per file block per xorb (:13). Alias `HF_XET_DATA_XORB_COMPRESSION_SCHEME_RETEST_INTERVAL` (aliases.rs:27-30).
- Simulation-feature-only cutting overrides: `HF_XET_XORB_SIMULATION_MAX_BYTES` / `HF_XET_XORB_SIMULATION_MAX_CHUNKS` (default None, :32,:42).

### Chunk/xorb size constants (NOT runtime-configurable in release builds)
Declared via `test_configurable_constants!` — env override `HF_XET_<NAME>` works **only under `cfg(debug_assertions)`**; release builds use the literal (`$XC/xet_runtime/src/utils/configuration_utils.rs:166-192`). Values at `$XC/xet_core_structures/src/xorb_object/constants.rs:3-26`:
- `TARGET_CHUNK_SIZE` = 64·1024 (64 KiB)
- `MINIMUM_CHUNK_DIVISOR` = 8 → min chunk 8 KiB
- `MAXIMUM_CHUNK_MULTIPLIER` = 2 → `MAX_CHUNK_SIZE` = 128 KiB (:29)
- `MAX_XORB_BYTES` = 64·1024·1024 (64 MiB)
- `MAX_XORB_CHUNKS` = 8·1024
- `XORB_BLOCK_SIZE` = 64·1024·1024

Shard constants (same debug-only override): `MDB_SHARD_GLOBAL_DEDUP_CHUNK_MODULUS`=1024 (chunk eligible for global dedup when `hash % 1024 == 0`), `MDB_SHARD_EXPIRATION_BUFFER`=7d, `MDB_SHARD_LOCAL_CACHE_EXPIRATION`=21d — `$XC/xet_core_structures/src/metadata_shard/constants.rs:5-22`.
(Note: `$XC/xet_data/src/processing/constants.rs` is dead code — not declared in `$XC/xet_data/src/processing/mod.rs:1-13`.)

### Cache locations
- Root: `HF_XET_CACHE` ▸ else `HF_HOME`/xet ▸ else `XDG_CACHE_HOME`/huggingface/xet ▸ else `~/.cache/huggingface/xet` — `$XC/xet_runtime/src/core/cache_dir.rs:10-37`. Verified precedence tests in `$XC/xet_data/src/processing/data_client.rs:205-265`.
- Per-endpoint subdir: first 16 chars of endpoint (non-alphanumerics → `_`) + `-` + first 16 chars of base64 hash of endpoint — `$XC/xet_data/src/processing/configurations.rs:194-209`. Under it: `staging/` (data.staging_subdir), `shard-cache/` (shard.cache_subdir), `staging/shard-session/` (session.dir_name, `$XC/xet_runtime/src/config/groups/session.rs:8`) — configurations.rs:102-125. `local://` endpoints put these under `<path>/xet/`; xorbs at `<path>/xet/xorbs` (`$XC/xet_data/src/processing/remote_client_interface.rs:19`).
- Chunk cache: `HF_XET_CHUNK_CACHE_SIZE_BYTES`, default 10 GB **or 0 when built with feature `no-default-cache`** — `$XC/xet_runtime/src/config/groups/chunk_cache.rs:1-11`. The shipped Python wheel enables `no-default-cache` by default, i.e. **hf_xet disables the disk chunk cache** — `$XC/hf_xet/Cargo.toml` `[features] default = ["no-default-cache", "elevated_information_level"]`.

### Logging (`log` group, `$XC/xet_runtime/src/config/groups/log.rs`)
`HF_XET_LOG_DEST` (None; dir path or empty=console, :18), `HF_XET_LOG_FORMAT` ("json" or text, :26), `HF_XET_LOG_PREFIX` ("xet", :34), `HF_XET_LOG_DIR_DISABLE_CLEANUP` (false, :41), `HF_XET_LOG_DIR_MAX_SIZE` (250mb, :51), `HF_XET_LOG_DIR_MIN_DELETION_AGE` (1day, :58), `HF_XET_LOG_DIR_MAX_RETENTION_AGE` (14day, :65).

### Telemetry (`telemetry` group, `$XC/xet_runtime/src/config/groups/telemetry.rs`)
`HF_XET_TELEMETRY_ENABLED`=true (:16), `..._HEARTBEAT_AFTER`=300s (:28), `..._HEARTBEAT_INTERVAL`=300s (:35), `..._REQUEST_TIMEOUT`=5s (:42), `..._FINAL_FLUSH_TIMEOUT`=2s (:56), `..._MAX_IN_FLIGHT`=32 process-wide (:73). Client POSTs one summary per transfer to **`POST /v1/telemetry`**, best-effort, never retried. **A self-hosted server can just 404/ignore this** — failures only log at DEBUG (telemetry.rs:8-9). Telemetry is auto-disabled when the endpoint scheme isn't http/https — `$XC/xet_client/src/cas_client/telemetry/mod.rs:117-120`. ⚠️ The telemetry URL uses `base.join("/v1/telemetry")` (absolute), so it **ignores any path prefix on the endpoint** — telemetry/mod.rs:123.

### System monitor (`system_monitor` group, `$XC/xet_runtime/src/config/groups/system_monitor.rs`)
`HF_XET_SYSTEM_MONITOR_ENABLED`=false (:14), `..._SAMPLE_INTERVAL`=5s (:21), `..._LOG_PATH`=None, `{PID}`/`{TIMESTAMP}` templates (:37).

Test-only: `HF_XET_TEST_PATH` (privilege-context tests only, `$XC/xet_runtime/src/file_utils/privilege_context.rs:262`).

---

## 2. How hf_xet receives endpoint + token; arbitrary endpoint strings

### Legacy module-level API (what older huggingface_hub calls)
`hf_xet.upload_files(file_paths, endpoint, token_info, token_refresher, progress_updater, _repo_type, request_headers=None, sha256s=None, skip_sha256=False)` and likewise `upload_bytes`, `download_files(files, endpoint, token_info, token_refresher, progress_updater, request_headers=None)` — `$XC/hf_xet/src/legacy/functions.rs:52-66,133-148,233-244`.
- `endpoint: Option[str]` — falls back to `data.default_cas_endpoint` (`$XC/xet_pkg/src/legacy/data_client.rs:38,98,149`).
- `token_info: Option[(str, int)]` = (CAS JWT, expiry unix secs).
- `token_refresher`: Python **callable taking no args returning `(str, int)`** (token, unix expiry); raises on failure — `$XC/hf_xet/src/legacy/token_refresh.rs:12-67`.
- `request_headers: dict[str,str]` merged with the binding's own User-Agent (see §7).

### New XetSession API (current huggingface_hub)
`XetSession(config=None)`; then `session.new_upload_commit(...)`, `new_file_download_group(...)`, `new_download_stream_group(...)` with kwargs `endpoint`, `token`, `token_expiry_unix_secs`, `token_refresh_url`, `token_refresh_headers`, `custom_headers`, `progress_callback`, `progress_interval_ms=100` — `$XC/hf_xet/src/py_xet_session.rs:98-262`.

**Endpoint resolution order** (`$XC/xet_pkg/src/xet_session/common.rs:14-62`):
1. explicit `endpoint`;
2. else if `token_refresh_url` set: eager `GET` on that URL once, use response `casUrl` as endpoint (and seed token from `accessToken`/`exp` if none pre-seeded);
3. else `data.default_cas_endpoint`.

**Token refresh URL contract** (server you must stand up if you want hub-style refresh; otherwise pass `endpoint` + long-lived `token`, or nothing at all): plain GET (the `token_refresh_headers`, typically `Authorization: Bearer hf_…`, are attached), response JSON exactly:
```json
{"casUrl": "<CAS base URL>", "exp": 1756489133, "accessToken": "<JWT string>"}
```
— camelCase per `#[serde(rename_all = "camelCase")]` on `CasJWTInfo {cas_url, exp, access_token}` — `$XC/xet_client/src/hub_client/types.rs:9-15`, deser test :84-95; refresher `$XC/xet_client/src/cas_client/auth.rs:100-140`. HF Hub's real route: `GET {hub}/api/{repo_type}s/{repo_id}/xet-{read|write}-token/{rev}` (rev URL-encoded, `?create_pr=1` appended for uploads without a rev) — `$XC/xet_client/src/hub_client/client.rs:68-91`. Refresh fires when `expiry <= now + 30s` (`REFRESH_BUFFER_SEC`, auth.rs:30, 213-225). Token is sent on every CAS request as **`Authorization: Bearer <token>`** — `$XC/xet_client/src/common/http_client.rs:340-342`. With no token and no refresher, `AuthConfig::maybe_new` returns `None` and **no Authorization header is ever sent** — auth.rs:162-185, http_client.rs:166. So a self-hosted server can simply not require auth, or accept any bearer.

**Arbitrary endpoint strings**:
- Dispatch: starts with `local://` (configurable scheme) → local filesystem client; equals `memory://` → in-memory; anything else → `RemoteClient` — `$XC/xet_data/src/processing/remote_client_interface.rs:16-38`, `$XC/xet_data/src/processing/configurations.rs:27-41`.
- `RemoteClient` stores the string verbatim (`$XC/xet_client/src/cas_client/remote_client.rs:106`) and builds URLs by **string concatenation** then `Url::parse`: `format!("{}/v1/xorbs/{key}", self.endpoint)` etc. — remote_client.rs:172,226,348,393,536,743,837,946. Consequences:
  - Any parseable absolute http/https URL works, **including one with a path prefix** (`http://host:8080/cas` → `http://host:8080/cas/v1/...`) — except telemetry, which joins to root (§1).
  - A **trailing slash produces a double slash** (`http://host:8080//v1/...`) — there is no trimming anywhere in the client (only test helpers trim, `$XC/simulation/src/scenario.rs:249`). Your server should tolerate `//v1` or callers must not pass trailing slashes.
  - Scheme must be http/https in practice (reqwest transport; telemetry gate at telemetry/mod.rs:117-120; HTTP/1.1 only — `.http1_only()` at http_client.rs:90).
- Python tests use `local://{tmp}/cas` endpoints throughout — `$XC/hf_xet/tests/conftest.py:13-16`; Rust tests use `format!("local://{}", dir)` — `$XC/xet_pkg/tests/test_xet_session.rs:33`.
- `xtool` normalizes: bare absolute path → `local://` prefixed; anything containing `://` passed through — `$XC/xet_pkg/src/bin/xtool/main.rs:145-153`; refuses uploads to non-local/non-loopback endpoints (main.rs:155+, doc at :81-92).

---

## 3. api_changes/ — protocol changes, version gates, capability negotiation

Folder purpose: agent-readable change log — `$XC/api_changes/README.md`. Protocol-relevant entries:

- **`update_260304_cas_xorb_name_clarification.md`** — pure rename `cas_object`→`xorb_object` etc.; explicitly states **no wire/JSON/binary format changes**: shard serialization is positional; cas_types JSON field names unchanged; XORB format ident/version constants keep the same byte values (:274-279). Kept names: `cas_client`, `cas_types`, `CAS_ENDPOINT`, `UploadXorbResponse`.
- **`update_260316_v2_reconstruction_multirange.md`** — adds `GET /v2/reconstructions/{file_id}` returning `QueryReconstructionResponseV2` with keys `terms`, `offset_into_first_range`, `xorbs` (map hex-hash → array of `{url, ranges:[{chunks:{start,end}, bytes:{start,end}}]}`) (:13-31). **Negotiation: client tries V2 first; on HTTP 404 or 501 falls back to V1 and caches the choice** (:36-39). Force with `HF_XET_CLIENT_RECONSTRUCTION_API_VERSION=1|2`. `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` (default false) governs whether multi-range presigned fetches are issued as real multi-range HTTP (`multipart/byteranges`, RFC 7233 parser at `xet_client::cas_client::multipart`) or split into parallel single-range requests (:54-59,79-84). Also bumped default AC initial concurrency 1→2 (upload), 1→4 (download).
- **`update_260318_optional_file_size.md`** — `XetFileInfo.file_size` is now `Option<u64>`; serde: `Some(n)`→`n`, `None` omitted; downloads may be hash-only and discover size from the reconstruction; `DataError::SizeMismatch{expected,actual}` after full-file download.
- **`update_260316_xet_file_info_sha256.md`** — `XetFileInfo.sha256: Option<String>` (`skip_serializing_if none`), populated on upload.
- **`update_260330_token_refresh_url.md`** — per-commit/group auth (session-level auth removed); documents the refresh **JSON contract** `{"accessToken": "<string>", "exp": <unix_secs>, "casUrl": "<string>"}` (:134-142); session id is UUIDv7 (:48-76).
- **`update_260402_unified_auth_group_builder.md`** — endpoint moved off `XetSessionBuilder` onto per-operation builders (`with_endpoint`, `with_custom_headers`, `with_token_info`, `with_token_refresh_url`).
- **`update_260424_next_stable_chunk_boundary.md`** — stable-boundary rule (two consecutive chunk sizes in `[2*min_chunk, max_chunk-min_chunk)`); server-side `build_file_chunk_hashes_response` extends dirty ranges to stable boundaries. Relates to `GET /v2/file-chunk-hashes/{file_id}` (client call at `$XC/xet_client/src/cas_client/remote_client.rs:946`).
- **`update_260320_chunk_hash_range_composable_aggregation.md`** — `MerkleHashSubtree` for streaming hash composition; hashes serialize as hex strings in JSON; `xorb_hash`/`file_hash` outputs unchanged.
- **`update_260604_xorb_uniqueness_nonce.md`** — xorb footer buffer: `XORB_OBJECT_FORMAT_FOOTER_BUFFER_LEN` = **16** bytes, `XORB_OBJECT_FORMAT_NONCE_LEN` = **4**; nonce sits immediately before the trailing 4-byte `info_length`; excluded from the xorb hash; remaining 12 bytes reserved zero; pre-existing objects have all-zero nonce and stay valid.
- **`update_260728_client_transfer_telemetry.md`** — `POST /v1/telemetry`; envelope has **exactly five snake_case keys**: `time`, `event`, `session_id`, `user_agent`, `metrics`; server accepts legacy `userAgent` as serde alias but both-at-once = 400; upload metrics = 40 keys, download = 23 (source of truth `$XC/xet_data/src/telemetry/payload.rs`); no PII; identity derived from JWT server-side.
- **`update_260328_simulation_global_dedup_expiration_and_config_controls.md`** — simulation `/simulation/set_config?config=<k>&value=<v>` keys: `global_dedup_shard_expiration` (secs, 0=off), `max_ranges_per_fetch`, `disable_v2_reconstruction` (status code, 0 re-enables), `api_delay` (`(50ms, 50ms)`), `url_expiration` (ms). Dedup-shard responses can carry `shard_key_expiry` with the file-section stripped.
- **`update_260407_simulation_conditional_tag_deletion.md`** — GC control routes `GET /simulation/xorbs_with_tags`, `POST /simulation/xorbs/{hash}/tag_delete` `{"tag":[u8;32]}` → `{"deleted": bool}` (same for shards).
- **`update_260309_package_restructure.md`** — the 5-package consolidation and name mapping (see §6).
- Others are client-API only (progress redesign 260313, sync/async unify 260316, anyhow removal 260318, runtime bridge 260319 & 260402, streaming downloads 260319, TaskRuntime 260326, wasm 260515).

**Capability negotiation:** there is **no `xet-version` (or similar) request header**. Versioning is entirely (a) URL path prefixes `/v1` vs `/v2`, (b) client-side auto-probe with fallback on **404/501** for reconstructions and shard upload, cached in `detected_reconstruction_api_version` / `detected_shard_api_version` atomics — `$XC/xet_client/src/cas_client/remote_client.rs:57-60,296-335,475-519` — and (c) the env force-overrides. **A minimal server can serve only V1 and return 404 for `/v2/...`; the real client degrades automatically.**

---

## 4. docs/ and other protocol documentation

- `$XC/docs/` contains exactly one file: **`simulation-client-gc-fixes.md`** — four correctness fixes in the simulation stack, notable for design invariants a server must respect: **a shard's identity is its content hash and must stay stable** (soft-delete file entries in an LMDB `file_status_table` rather than rewriting shards, since rewriting changes the hash and breaks GC epoch snapshots) — lines 1-50+.
- **`$XC/openapi/cas.openapi.yaml`** (17.9 KB, OpenAPI 3.1.0, title "Xet CAS API" v1.0.0) — machine-readable spec of the CAS HTTP API including a full V1 reconstruction example (`offset_into_first_range`, `terms[].hash/unpacked_length/range{start,end}`, `fetch_info` map with `url`, `url_range{start,end}`), bearerAuth security, `x-required-scope: read/write` per route, and a pointer to `https://huggingface.co/docs/xet/api`. `$XC/openapi/Makefile` generates rust/ts/python/java/go clients via openapi-generator.
- Dedup design breadcrumbs: `next_stable_chunk_boundary` proof sketch + reference to "parallel chunking.lyx" — `$XC/xet_core_structures/src/xorb_object/constants.rs:31-79`; global-dedup eligibility `hash % 1024 == 0` — metadata_shard/constants.rs:19-22.

---

## 5. Existing server implementations / mocks (minimal viable server behavior)

### `LocalServer` (axum) — the canonical reference server
`$XC/xet_client/src/cas_client/simulation/local_server/server.rs`. Route table (:195-232):

```
GET  /health
GET  /v1/reconstructions            (batch)
GET  /v1/reconstructions/{file_id}
GET  /v1/chunks/{prefix}/{hash}     (global-dedup shard query)
HEAD /v1/xorbs/{prefix}/{hash}
POST /v1/xorbs/{prefix}/{hash}
POST /v1/shards                     (JSON response)
HEAD /v1/files/{file_id}            (file size)
GET  /v1/get_xorb/{prefix}/{hash}/  (legacy xorb download)
GET  /v1/fetch_term?term=<base64>   (presigned-URL stand-in)
POST /v1/telemetry
GET  /v2/reconstructions/{file_id}
POST /v2/shards                     (NDJSON progress stream)
/simulation/*  (ping, set_config, dummy_upload, direct-access + deletion routes)
```
Handlers live in `$XC/xet_client/src/cas_client/simulation/local_server/handlers*` (same dir). Batch reconstruction is `GET /v1/reconstructions?...` built as `format!("{}/v1/reconstructions?", endpoint)` — remote_client.rs:536. Shard upload V1 response JSON is `UploadShardResponse{result: Exists|SyncPerformed}` — remote_client.rs:356-370; xorb upload response is `{"was_inserted": bool}` (`UploadXorbResponse`, `$XC/xet_client/src/cas_types/mod.rs:21-24`). V2 shard upload streams the body with mandatory `Content-Length` and reads an NDJSON progress/heartbeat/Result stream back; the local server can inject in-stream `error` frames for testing (server.rs:107-108, remote_client.rs:384-472). Reconstruction `Range` request header supports `bytes=100-` and suffix `bytes=-128` (server tests :1084-1112); 416 RANGE_NOT_SATISFIABLE maps to `Ok(None)` client-side (remote_client.rs:271-273). The server's "presigned URLs" are just `http://<self>/v1/fetch_term?term=<base64>` honoring `Range` (tests :761-787).
- **Auth/token endpoints: none.** The reference server has no auth routes and no Authorization checking; `LocalTestServer` builds its `RemoteClient` with `auth = &None` (server.rs:389,399,406). Minimal viable = unauthenticated CAS + client given a direct `endpoint` (+ optional dummy `token`).
- Standalone binary: **`local_cas_server`** (`cargo run -p xet-client --features simulation --bin local_cas_server`), flags `--data-directory ./local_cas_data`, `--host 127.0.0.1`, `--port 8080`, `--in-memory`; endpoint doc-block lists the same API — `$XC/xet_client/src/cas_client/simulation/local_server/main.rs:26-45,61-97`; `[[bin]]` gate `$XC/xet_client/Cargo.toml:92-95`.
- In-process clients that skip HTTP entirely: `LocalClient` (disk: xorbs under `<dir>`, LMDB indices), `MemoryClient` — `$XC/xet_client/src/cas_client/simulation/{local_client.rs,memory_client.rs}`; dispatched by the `local://`/`memory://` endpoint schemes.

### Other mocks
- `simulation/` crate (excluded from workspace, v1.4.0): upload-concurrency scenario runner; test server exposes `/simulation/ping` and `POST /simulation/dummy_upload` — `$XC/simulation/src/scenario.rs:249`, `$XC/simulation/src/upload_concurrency/upload_simulation_client.rs:357`.
- Token-refresh endpoint mocking with wiremock returning the `{"casUrl","exp","accessToken"}` body — `$XC/xet_pkg/src/xet_session/common.rs:92-192` (the best stub template for a token route).
- `xtool` (`cargo run -p hf-xet --features internal-tools --bin xtool`) can upload/download against `local://` dirs or loopback http servers — `$XC/xet_pkg/src/bin/xtool/{main.rs,upload.rs,download.rs}`.

---

## 6. Crate names + versions (crates.io) at this rev

Workspace version **1.6.0** — `$XC/Cargo.toml:22-24`; members list :1-19.

| Dir | crates.io name | Rust import name | Version | Published? |
|---|---|---|---|---|
| `xet_runtime/` | `xet-runtime` | `xet_runtime` | 1.6.0 | yes (crates.io) |
| `xet_core_structures/` | `xet-core-structures` | `xet_core_structures` | 1.6.0 | yes |
| `xet_client/` | `xet-client` | `xet_client` | 1.6.0 | yes |
| `xet_data/` | `xet-data` | `xet_data` | 1.6.0 | yes |
| `xet_pkg/` | `hf-xet` | **`xet`** (`[lib] name = "xet"`) | 1.6.0 | yes |
| `git_xet/` | `git_xet` | – | 0.2.1 | **`publish = false`** |
| `simulation/` | `simulation` | – | 1.4.0 | **`publish = false`** |
| `hf_xet/` (Python cdylib) | – (PyPI `hf-xet`) | `hf_xet` | 1.6.0 | PyPI only, workspace-excluded |

Cites: `$XC/{xet_runtime,xet_core_structures,xet_client,xet_data,xet_pkg,git_xet,simulation}/Cargo.toml` `[package]` sections; `$XC/xet_pkg/Cargo.toml` `[lib] name = "xet"`; `$XC/hf_xet/Cargo.toml:1-4`. Inter-crate deps are `{ version = "1.6.0", path = "..." }` (e.g. `$XC/xet_pkg/Cargo.toml:39-42`) — the standard publishable pattern, and the READMEs confirm crates.io publication with badges/links: `$XC/README.md:44-52`, `$XC/xet_pkg/README.md:3-27`. So a third-party Cargo project can depend on `hf-xet = "1.6"` (import as `xet`) or the lower-level `xet-client`/`xet-core-structures`/`xet-data`/`xet-runtime` = "1.6.0" from crates.io; git deps are not required. Naming trap: crates.io `hf-xet` is the Rust `xet_pkg` crate, while PyPI `hf-xet` is the `hf_xet/` PyO3 wheel. Useful features: `xet-client` `simulation` (LocalServer/LocalClient/MemoryClient + `local_cas_server` bin), `hf-xet` `internal-tools` (xtool), `python`, `no-default-cache`.

---

## 7. User-Agent / version headers a server might see or key on

- **Python binding UA**: `const USER_AGENT = concat!(CARGO_PKG_NAME, "/", CARGO_PKG_VERSION)` = **`hf_xet/1.6.0`** — `$XC/hf_xet/src/headers.rs:7`. If huggingface_hub supplies its own `User-Agent` in `request_headers`/`custom_headers`, the binding **appends**: `"<hub UA>; hf_xet/1.6.0"` (headers.rs:13-24, test :87-99). These become reqwest `default_headers` on every CAS request (http_client.rs:97-99).
- **Rust `xet-client` default** (only in the telemetry payload's `user_agent` field when no custom UA given): **`xet-client/1.6.0`** — `$XC/xet_client/src/cas_client/telemetry/mod.rs:36`.
- **xtool UA**: `xtool/<CARGO_PKG_VERSION>` — `$XC/xet_pkg/src/bin/xtool/endpoint.rs:11`.
- **Request headers on every CAS call**: `Authorization: Bearer <jwt>` (when auth configured, http_client.rs:341), **`X-Xet-Session-Id`** (UUIDv7 session id; only when non-empty) — `$XC/xet_client/src/cas_types/mod.rs:17`, http_client.rs:365-367. Response header the client reads for logging: **`X-Request-Id`** — cas_types/mod.rs:19, http_client.rs:397-402.
- git-xet/hub protocol headers (LFS agent side, not CAS): `X-Xet-Cas-Url`, `X-Xet-Access-Token`, `X-Xet-Token-Expiration`, `X-Xet-Session-Id` — `$XC/git_xet/src/constants.rs:10-13`.
- **Do not key behavior on UA**: version selection is purely URL-prefix + 404/501 fallback (§3). No client sends a protocol-version header; the only version strings on the wire are inside the User-Agent and the telemetry `user_agent` field. Client version constant available to code as `git_xet::CURRENT_VERSION` (constants.rs:7); the old `HF_XET_CURRENT_VERSION` env (alias `HF_XET_DATA_CURRENT_VERSION`, aliases.rs:13) belonged to the removed legacy constants file and is dead at this rev.

### Implementation checklist distilled for a from-scratch server
1. Serve `/v1/reconstructions/{file_id}` (+`Range`), `/v1/chunks/{prefix}/{hash}`, `HEAD+POST /v1/xorbs/{prefix}/{hash}` (`{"was_inserted":bool}`), `POST /v1/shards` (`{"result":...}` Exists/SyncPerformed), `HEAD /v1/files/{file_id}`; return 404 on all `/v2/*` and `/v1/telemetry` if you don't want them — the client falls back / shrugs.
2. Accept and ignore (or validate) `Authorization: Bearer …`, `X-Xet-Session-Id`; optionally emit `X-Request-Id`.
3. Point the real client at you via `new_upload_commit(endpoint="http://host:8080", token="anything", token_expiry_unix_secs=9999999999)` (or `HF_XET_DATA_DEFAULT_CAS_ENDPOINT`), no trailing slash; or implement one GET token route returning `{"accessToken","exp","casUrl"}` and hand out `token_refresh_url` instead.
4. Chunk geometry the client will send: chunks 8–128 KiB targeting 64 KiB, xorbs ≤ 64 MiB and ≤ 8192 chunks, shards targeted at 64 MiB; xorb footer carries a 4-byte nonce inside a 16-byte buffer excluded from the hash.