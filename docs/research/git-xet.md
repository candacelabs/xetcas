All source read. Compiling the dossier now.

# git_xet Dossier — the Git integration of xet-core

Repo: huggingface/xet-core @ 77fc84d3d (2026-08-11), local clone base `/tmp/claude-1000/-home-bertha-candace-server--claude-worktrees-xet-cas-storage-transfer-07271f/046ce30e-cd86-469f-9c94-1dbeda51d318/scratchpad/xet-core/`. All citations below are relative to that base. Crate: `git_xet`, package name `git_xet`, binary `git-xet`, version `0.2.1` (`git_xet/Cargo.toml:2-9`).

## 0. Architecture TL;DR (read this first)

git_xet is **NOT a clean/smudge filter and NOT a git hook**. It is a **Git LFS custom transfer agent** registered under the name `"xet"` (`git_xet/src/constants.rs:3`, README `git_xet/README.md:68-71`). The normal git-lfs machinery (clean/smudge filters, pointer files, pre-push hook, Batch API) is untouched; git_xet only replaces the *upload byte-transfer step*. Downloads deliberately fall back to standard git-lfs basic transfer (`git_xet/src/app/xet_agent.rs:70-75`). Flow:

1. `git push` → git-lfs pre-push hook → git-lfs Batch API POST to the LFS server with `"transfers"` including `"xet"` (git-lfs's own request; git_xet never issues the batch call — no batch code exists in the crate; the only reference is a doc comment `git_xet/src/app.rs:124-131`).
2. Server replies choosing transfer `"xet"` and returns per-object `actions.upload = {href, header}`.
3. git-lfs spawns `git-xet transfer` and drives it over stdin/stdout with line-delimited JSON (the [custom-transfers](https://github.com/git-lfs/git-lfs/blob/main/docs/custom-transfers.md) protocol, implemented in `git_xet/src/lfs_agent_protocol.rs`).
4. For each object, git_xet reads the CAS URL + JWT out of the batch action's `header` map, runs the Xet chunk/dedupe upload (`clean_file`) against the CAS server, finalizes the shard, and reports `complete`.

---

## 1. How a repo gets "xet enabled"

### 1.1 CLI surface (clap)

Parser struct `XetAgentApp`, clap name = `"git-xet"`, version = `CARGO_PKG_VERSION` = `0.2.1` (`git_xet/src/app.rs:132-140`, `git_xet/src/constants.rs:4,7`). Subcommands (`git_xet/src/app.rs:14-50`):

| Subcommand | Args | Purpose |
|---|---|---|
| `install` | `--system`, `--local`, `--path <PATH>`, `--concurrency <u32>` (`app.rs:52-71`) | Register the agent in git config |
| `uninstall` | `--all`, `--system`, `--local`, `--path <PATH>` (`app.rs:73-93`) | Remove registration |
| `transfer` | none | Run the LFS custom transfer agent loop on stdin/stdout (`app.rs:222-231`); "not meant to be used directly by users" (`app.rs:38-40`) |
| `track` | trailing var-args, hyphen values allowed (`app.rs:95-101`) | Pure passthrough: spawns `git-lfs track <args...>` (`app.rs:233-239`) |
| `run-any` | `program`, `args` — only with feature `git-xet-for-integration-test` (`app.rs:46-49,103-108,241-249`) | Test-only arbitrary exec |

Global flags: `-v/--verbose` (counted), `-l/--log <PATH>` (`app.rs:110-119`) — declared but not consumed in command dispatch.

### 1.2 Exact git config keys written by `git xet install`

`install_impl` (`git_xet/src/app/install.rs:51-117`) runs `git config [--system|--global|--local]` setting:

- `lfs.customtransfer.xet.path` = `git-xet` (`install.rs:74-82`)
- `lfs.customtransfer.xet.args` = `transfer` (`install.rs:84-92`)
- `lfs.customtransfer.xet.concurrent` = `true` unless `--concurrency 1` (then `false`) (`install.rs:60-72,94-102`)
- `lfs.concurrenttransfers` = `<N>` only when `--concurrency N` with N>1 (`install.rs:67`) — git-lfs's default is 8 when unset (asserted `install.rs:174`; the `--concurrency` help says "Default 8." `app.rs:68`)

Default location is `--global` (~/.gitconfig) (`app.rs:191-193`). `--concurrency 0` is rejected (`app.rs:178-183`, `install.rs:61-63`).

Then, if `git config --get filter.lfs.process` fails (git-lfs not configured), it runs `git lfs install` (which is what installs git-lfs's global filter config **and** its repo hooks), warning on failure (`install.rs:104-114`).

`git xet uninstall` runs `git config <loc> --remove-section lfs.customtransfer.xet` and `git config <loc> --unset lfs.concurrenttransfers`, ignoring errors (`git_xet/src/app/uninstall.rs:34-56`); `--all` does system+global+local (`uninstall.rs:25-29`).

### 1.3 .gitattributes and hooks

git_xet writes **no** .gitattributes and **no** hooks itself. Tracking patterns come from stock `git lfs track` (optionally via the `git xet track` passthrough, `app.rs:233-239`), producing standard lines like `*.bin filter=lfs diff=lfs merge=lfs -text`. Hooks (pre-push, post-checkout, post-commit, post-merge) are git-lfs's own, installed by the `git lfs install` fallback (`install.rs:104-114`). There is no clean/smudge/process filter registration anywhere in the crate — `grep filter.lfs` only hits the read at `install.rs:108`.

### 1.4 Installers

- `git_xet/install.sh`: downloads the platform zip (release tag `git-xet-v0.2.1`, `install.sh:7-10`), installs to `/usr/local/bin` (`install.sh:17-18`), optionally installs git-lfs v3.7.1 (`install.sh:12-15,119-148`), then runs `git-xet install --concurrency 3` (`install.sh:151`).
- Windows MSI custom actions: `ExeCommand="install --concurrency 3"` on install, `ExeCommand="uninstall --all"` on removal (`git_xet/windows_installer/Package.wxs:58-73`).
- macOS entitlements: `com.apple.security.cs.allow-unsigned-executable-memory`, `disable-library-validation` (`git_xet/entitlements.xml`).

---

## 2. Pointer file format

git_xet writes **no pointer files** — the pointer stays the stock **git-lfs pointer spec v1** produced by git-lfs's clean filter (git_xet never registers a filter; README `git_xet/README.md:1` — "follow your regular workflow to `git lfs track` … `git push`"). For reference the standard pointer blob is exactly:

```
version https://git-lfs.github.com/spec/v1
oid sha256:<64 lowercase hex chars>
size <byte length>
```

(That format is defined by git-lfs, not by this repo.) What this crate *does* pin down about the pointer:

- The LFS `oid` handed to the agent must be a 64-char string: `const OID_LEN: usize = 64` with validation `req.oid.len() != OID_LEN` → protocol argument error (`git_xet/src/lfs_agent_protocol/protocol_spec.rs:16,114-116`).
- The oid is interpreted as the file's SHA-256: `Sha256Policy::from_hex(&req.oid)` is passed to `clean_file` (`git_xet/src/app/xet_agent.rs:160`), which stores it as the shard's `FileMetadataExt.sha256` (`xet_data/src/processing/file_cleaner.rs:25-49`; `xet_core_structures/src/metadata_shard/file_structs.rs:304-315`). This is the sha256→xet-hash linkage a server needs to serve LFS downloads later.
- `size` must be > 0 or the agent rejects the transfer request (`protocol_spec.rs:118-120,137-139` — a server/repo with 0-byte LFS objects would fail, but git-lfs stores empty files inline anyway).

---

## 3. Transfer protocol (there is no pkt-line filter protocol)

git_xet does **not** implement git's long-running filter process protocol (no packet-line, no `git-filter-client`/`capabilities` handshake). Its wire protocol with git-lfs is the **LFS custom transfer agent protocol**: newline-delimited JSON messages over the agent's stdin/stdout, three stages (init / 0..N transfers / terminate), strictly sequential per process (`git_xet/src/lfs_agent_protocol.rs:46-62`).

### 3.1 Requests parsed (stdin, one JSON object per line)

Tagged by `"event"` (serde `tag = "event", rename_all = "lowercase"`, `protocol_spec.rs:18-25`):

- **init**: `{ "event": "init", "operation": "upload"|"download", "remote": "<name-or-url>", "concurrent": <bool>, "concurrenttransfers": <u32, optional> }` (`protocol_spec.rs:35-57`; examples in tests `protocol_spec.rs:239-262`). `remote` may be a remote *name* or a remote *URL* (`protocol_spec.rs:44`). Empty `remote` → argument error (`protocol_spec.rs:107-109`).
- **upload**: `{ "event": "upload", "oid": "<64 hex>", "size": <u64 >0>, "path": "/abs/file", "action": { "href": "<url>", "header": { "<k>": "<v>", ... } } }` (`protocol_spec.rs:59-66,85-89`; validation 113-131: oid len 64, size>0, path required, `action.href` non-empty).
- **download**: same minus `path` (path present → syntax error, `protocol_spec.rs:141-143`).
- **terminate**: `{ "event": "terminate" }` — no response; agent exits loop (`protocol_spec.rs:151`, `lfs_agent_protocol.rs:128-133`).

`action` is "the action copied from the response from the batch API" (`protocol_spec.rs:65`). Note `header` is a plain JSON `HashMap<String,String>` — lookups in git_xet are **exact-case string matches**, so the server must emit the exact header spellings listed in §5.

### 3.2 Responses written (stdout, one JSON per line, `to_line_delimited_json_string` `protocol_spec.rs:215-217`)

- init success: `{}` (`protocol_spec.rs:167-169`).
- init error: `{"error":{"code":32,"message":"<msg>"}}` — code **32** fixed (`protocol_spec.rs:171-173,205-208`; test 292-308).
- progress: `{"event":"progress","oid":"<oid>","bytesSoFar":<u64>,"bytesSinceLast":<u64>}` — camelCase via serde (`protocol_spec.rs:68-74`; test 433-451).
- complete (upload ok): `{"event":"complete","oid":"<oid>"}`; (download ok): adds `"path":"<file>"`; error: `{"event":"complete","oid":"...","error":{"code":2,"message":"..."}}` — code **2** fixed (`protocol_spec.rs:76-83,186-213`; tests 395-419,454-496).

### 3.3 State machine

`PendingInit → InitedForUpload → Uploading → Uploading…` (or the Download mirror); mixing upload/download or double-init is a protocol State error (`git_xet/src/lfs_agent_protocol/agent_state.rs:8-48`). Driver loop: `lfs_protocol_loop` reads a line, validates, dispatches to the `TransferAgent` trait (`lfs_agent_protocol.rs:63-144`; trait at 21-44).

### 3.4 Progress semantics

`ProgressUpdater` guarantees monotonically increasing `bytesSoFar` and skips messages when the stdout lock is contended (wait-free, `git_xet/src/lfs_agent_protocol/progress_updater.rs:16-69`). Immediately at the start of each upload the agent sends a dummy `bytesSoFar: 1` progress so git-lfs releases its "first worker gates the rest" login serialization (`git_xet/src/app/xet_agent.rs:104-113`, referencing git-lfs `tq/adapterbase.go#L156`).

---

## 4. Push path (upload)

Upload happens at **`git push` time, inside the git-lfs pre-push flow** — not at clean time. Sequence per agent process (`git_xet/src/app/xet_agent.rs`):

1. **init (upload)** (`xet_agent.rs:41-68`): opens the repo from CWD (`GitRepo::open_from_cur_dir`, `git_xet/src/git_repo.rs:17-19`); resolves `req.remote` as a remote name via `remote_name_to_url` (`git_repo.rs:96-109`) else parses it as a URL. If the remote URL scheme is not http/https **and** it has an explicit port (e.g. `ssh://git@host:2222/...`), the env var `HF_ENDPOINT` **must be set** or init fails with a config error (`xet_agent.rs:51-61`, const `HF_ENDPOINT_ENV = "HF_ENDPOINT"` `constants.rs:17`); the value is stored but otherwise unused (`xet_agent.rs:37,65`).
2. **upload (per object, sequential within a process)** (`xet_agent.rs:77-182`):
   - Builds a token refresher *before* any progress (so at most one credential prompt, `xet_agent.rs:82-102`): `new_git_token_refresher(ctx, repo, remote_url, refresh_route = req.action.href, Operation::Upload, session_id, headers)` (`git_xet/src/token_refresher.rs:16-37`).
   - Reads from `req.action.header` (exact keys, `constants.rs:10-13`):
     - `X-Xet-Cas-Url` → CAS endpoint; missing → error "Hugging Face Hub didn't provide a CAS URL" (`xet_agent.rs:119-124`).
     - `X-Xet-Access-Token` → initial JWT (`xet_agent.rs:125-130`).
     - `X-Xet-Token-Expiration` → parsed to `u64` epoch-seconds (a JSON *string* that must parse, `xet_agent.rs:131-137`).
     - `X-Xet-Session-Id` → optional session id (`xet_agent.rs:93`); when nonempty it is set as `config.session.session_id` (`xet_agent.rs:149-151`) and attached to every outbound HTTP request as header `X-Xet-Session-Id` (`SESSION_ID_HEADER`, `xet_client/src/cas_types/mod.rs:17`; `SessionMiddleware` `xet_client/src/common/http_client.rs:346-366`).
   - `User-Agent: git_xet/0.2.1` on all HTTP calls (`xet_agent.rs:17,87-91`).
   - Builds `default_config(ctx, cas_url, Some((token, expiry)), Some(refresher), headers)` (`xet_agent.rs:141-148`; `xet_data/src/processing/data_client.rs:27-46` — session gets a UUIDv7 session id if the server provided none) with `.disable_progress_aggregation()`.
   - `FileUploadSession::new`, then `clean_file(session, req.path, Sha256Policy::from_hex(&req.oid))` — content-defined chunking, dedupe, xorb upload to CAS (`xet_agent.rs:152-160`; `xet_data/src/processing/data_client.rs:62-90`). Returned `XetFileInfo` is discarded — the LFS oid (sha256) remains the object's public identity.
   - **`session.finalize()` after every single file** — the shard is uploaded per object, because the agent protocol is sequential, git-lfs never says how many files are coming, and after `terminate` git-lfs SIGKILLs the agent after 30s, so batching shards would risk data loss (verbatim rationale comment, `xet_agent.rs:162-173`).
   - Progress bridged from the Xet session via `GroupProgressCallbackUpdater` → `ProgressUpdater` (`xet_agent.rs:115-117,153,178`, wrapper 197-206).
3. **Concurrency**: not inside the agent — git-lfs spawns up to `lfs.concurrenttransfers` agent *processes* (because `lfs.customtransfer.xet.concurrent=true`) and splits objects evenly among them; each process is strictly sequential (`protocol_spec.rs:46-56`, `lfs_agent_protocol.rs:54-57`). Users can override ad hoc with `git -c lfs.concurrenttransfers=<n> push` (`protocol_spec.rs:50-56`).
4. **Enumeration of objects** is entirely git-lfs's job (pre-push ref scan + batch API); git_xet sees only the per-object `upload` events.

---

## 5. Endpoint discovery & repo identity

### 5.1 Where the CAS URL comes from

Exclusively from the LFS **batch response** that the *server* controls: `action.header["X-Xet-Cas-Url"]` (§4). There is no hardcoded huggingface.co in the transfer path and git_xet performs **no** Hub API discovery call of its own.

### 5.2 Token refresh route

`DirectRefreshRouteTokenRefresher` GETs **`req.action.href`** (the batch action's href, verbatim) whenever the JWT expires mid-session (`git_xet/src/token_refresher.rs:24-37`; `xet_client/src/cas_client/auth.rs:70-140`). Request: `GET <action.href>` with credentials filled by the credential chain (§5.4), `User-Agent: git_xet/0.2.1`, optional `X-Xet-Session-Id`. Expected response: HTTP 200 with JSON body deserialized as `CasJWTInfo` — **camelCase**:

```json
{"casUrl":"https://cas-server.xethub.hf.co","exp":1756489133,"accessToken":"ey...jQ"}
```

(`xet_client/src/hub_client/types.rs:9-15`, exact sample test 84-95.) Only `accessToken` and `exp` are consumed on refresh (`auth.rs:134-140`); `casUrl` is ignored there (the CAS endpoint is fixed at session start). Retries: 5xx, 408, 429 are retryable (`xet_client/src/cas_client/retry_wrapper.rs:515`); 501 aborts (`retry_wrapper.rs:219`).

### 5.3 The HF Hub token API (context: NOT called by git_xet)

The shared `HubClient::get_cas_jwt` builds `GET {endpoint}/api/{repo_type}s/{repo_id}/xet-{write|read}-token/{urlencoded rev, default "main"}` plus `?create_pr=1` when uploading with no rev; Bearer auth only ("doesn't take a Basic auth") (`xet_client/src/hub_client/client.rs:67-111`; `Operation::token_type()` maps Upload→`write`, Download→`read`, `client.rs:30-36`). git_xet links `hub_client` only for `Operation` and the credential-helper types; its refresher uses the direct route in §5.2. So a from-scratch server does **not** need this API for the git flow.

### 5.4 Credential chain (what auth the refresh GET will carry)

`get_credential` order (`git_xet/src/auth.rs:95-135`):

1. `lfs.<lfs-endpoint>.access` == `none` → no auth at all (Noop) (`auth.rs:100-102`); key format `lfs.{derived-http-url}.git/info/lfs.access`, values parsed case-insensitively: `none|basic|private|negotiate|""` (`auth.rs:35-74`).
2. Credentials embedded in the remote URL (`user:token@host`) → `Authorization: Bearer <token>` (`auth.rs:104-109`).
3. Env `HF_TOKEN` → `Authorization: Bearer <token>` (`auth.rs:111-114`; const `constants.rs:16`).
4. netrc (`NETRC` env respected; host match includes port, matched against `derived_host_url` minus scheme) → Bearer of the netrc `password` (`auth.rs:116-126`, test 263-285).
5. SSH-scheme remote → `SSHCredentialHelper`: runs `ssh [-p PORT] user@host git-lfs-authenticate <full_repo_path> <upload|download>` (`git_xet/src/auth/ssh.rs:51-72`; command assembly honoring `GIT_SSH_COMMAND`/`GIT_SSH`/`core.sshCommand`/`GIT_SSH_VARIANT`/`ssh.variant`, putty `-P`, tortoiseplink `-batch`, shell wrapping via `sh -c`, `git_xet/src/utils/ssh_connect.rs:57-172`). Expected stdout JSON: `{"header":{"Authorization":"<verbatim value, e.g. Basic xxx>"},"href":"...","expires_in":3600}` (`ssh.rs:16-30`); the `Authorization` value is copied **verbatim** onto the refresh GET (`ssh.rs:76-85`). Not cached (TTL shorter than CAS JWT, `ssh.rs:33-35`).
6. Fallback: `git credential fill` with input `url=<derived host url>\n\n`; the returned `password=` value becomes `Authorization: Bearer <password>` (`git_xet/src/auth/git.rs:47-80`). May prompt via GIT_ASKPASS/terminal (`git.rs:36-44`).

`BearerCredentialHelper` = `req.bearer_auth(token)` (`xet_client/src/common/auth/basics.rs:43-52`).

### 5.5 Remote → LFS endpoint derivation

- Remote selection: `branch.<branch>.remote` → `remote.lfsdefault` → the single defined remote → `"origin"` (`git_xet/src/git_repo.rs:58-93`).
- URL canonicalization (`git_xet/src/git_url.rs:43-119`): LFS endpoint = `{derived-http-url}/info/lfs` where derived-http-url = `scheme://[auth@]host[:port]/path.git`; `git|ssh|git+ssh` schemes translate to `https` **dropping the port** (`git_url.rs:69-74,106-119`; tests: `ssh://git@localhost:2222/foo/bar` → `https://localhost/foo/bar.git`, `git_url.rs:338-363`); `file|ftp|ftps` unsupported (errors).
- Repo identity: `full_repo_path()` strips leading `/` and trailing `.git` → `[{models|datasets|spaces}/]owner/name` (`git_url.rs:192-194`); parsed into `RepoInfo{repo_type, full_name}` with empty type defaulting to `model` (`git_url.rs:204-211`; `xet_client/src/hub_client/types.rs:25-47`).

---

## 6. Minimum surface for a NON-HuggingFace server

To make stock git-lfs + git_xet work end-to-end against your own stack you need:

1. **A git remote** reachable over http(s) (simplest; ssh works but see #6).
2. **LFS Batch API** at `{remote-url}.git/info/lfs/objects/batch` (default derivation §5.5; standard git-lfs config `lfs.url` can point it elsewhere — that's git-lfs's server-discovery, outside this crate). Behavior:
   - For `"operation":"upload"` requests whose `transfers` array contains `"xet"`: respond `"transfer":"xet"` and, per object, `"actions":{"upload":{"href":"<token-refresh-URL>","header":{"X-Xet-Cas-Url":"<cas base url>","X-Xet-Access-Token":"<jwt>","X-Xet-Token-Expiration":"<epoch-secs>","X-Xet-Session-Id":"<optional id>"}}}`. Exact key casing is mandatory (JSON map lookup, §3.1/§4). Every listed header except Session-Id is mandatory or the agent errors (`xet_agent.rs:119-137`).
   - For `"operation":"download"`: **do not select `"xet"`** — the agent's `init_download` hard-fails ("custom transfer for download is not implemented yet…", `xet_agent.rs:70-75`). Reply with basic transfer (`"transfer":"basic"` or omit) and a normal `actions.download.href` serving the full file bytes for the sha256 oid.
3. **Token endpoint** = whatever URL you put in `action.href`: must answer `GET` with `{"casUrl":..., "exp":<epoch-secs>, "accessToken":...}` (camelCase, §5.2). It will be called only when the initial token expires (and retried on 5xx/408/429). Auth arriving: `Authorization: Bearer <token from §5.4>` for http remotes, or the verbatim `Authorization` from your `git-lfs-authenticate` for ssh remotes; if you configure `lfs.<endpoint>.access none` client-side, no auth header at all. A permissive server can simply ignore it (stub).
4. **A CAS server** at the `X-Xet-Cas-Url` base implementing the Xet upload surface used by `FileUploadSession`/`clean_file` (xorb upload, shard/chunk-dedupe APIs — covered by the storage/transfer dossier, entry points `xet_data/src/processing/data_client.rs:27-90`). Uploaded shards carry `FileMetadataExt.sha256` = the LFS oid (§2), which is exactly what you need server-side to answer #2's basic-download by oid.
5. **Cleanliness of pointing at arbitrary URLs**: fully clean for http(s) remotes — everything (CAS URL, token, session id, refresh route) is server-supplied via the batch response; no env var and no HF API is required, and `HF_TOKEN` is only an optional client-side credential source. The single HF-ism: with a non-http remote URL *that has an explicit port* (typical self-hosted ssh), `HF_ENDPOINT` must merely be *set* to any value or init fails (`xet_agent.rs:51-61`).
6. **Only if you serve git over SSH**: implement the `git-lfs-authenticate <repo-path> <upload|download>` SSH command returning `{"header":{"Authorization":"..."},"href":"...","expires_in":N}` (§5.4.5) — git-lfs itself also calls it for the batch API.
7. **Client-side setup** on each machine: `git xet install` (writes the three `lfs.customtransfer.xet.*` keys) + git-lfs installed. Nothing repo-side beyond normal `git lfs track` patterns.

Auth note (scope limit per brief): the JWT content is opaque to git_xet — it's stored and replayed as `Authorization: Bearer` toward CAS by the cas_client auth middleware; a stub server may accept any token.

---

## 7. Smudge / download path

There is none in git_xet, by design:

- `init_download` → error, code 32 to git-lfs: "custom transfer for download is not implemented yet. Downloads should operate through standard git-lfs download protocol." (`xet_agent.rs:70-75`); `download_one` is `unimplemented!()` (`xet_agent.rs:184-190`). (The download half of the agent protocol — `download` event, `complete` with `path` — is nevertheless fully implemented in the protocol layer, `protocol_spec.rs:132-150,186-192`, ready for a future agent.)
- Actual smudge = stock git-lfs: `filter.lfs.process` filter + Batch API `download` + basic transfer GET of full bytes; caching is git-lfs's standard local store (`.git/lfs/objects/…`). The server, not the client, does Xet reconstruction for these downloads (hence the sha256 in the shard, §2/§6.4).

---

## 8. Tests and server test doubles

- **Protocol conformance (exact JSON)**: `git_xet/src/lfs_agent_protocol/protocol_spec.rs:225-497` (init/upload/download/terminate parsing incl. bad-input cases; exact error/progress/complete serializations). State machine: `agent_state.rs:51-122`. Progress concurrency/monotonicity: `progress_updater.rs:139-265`.
- **Install/uninstall end-to-end against real git+git-lfs** (asserts `git lfs env` shows `xet` in `UploadTransfers`/`DownloadTransfers`, concurrency values, local-over-global precedence): `git_xet/src/app/install.rs:119-314`, `git_xet/src/app/uninstall.rs:58-143`. Harness: `TestRepo` (temp repo + temp `$HOME`, `git_xet/src/test_utils/test_repo.rs:12-113`, `temp_home.rs:14-46`).
- **Server test double (SSH)**: `start_local_ssh_server` — russh-based server with a throwaway Ed25519 host key, accepts any auth, answers only the `git-lfs-authenticate <repo> <upload|download>` exec with `{"header":{"Authorization":"Basic 38vcn391nv=="},"href":"https://huggingface.co/<repo>.git/info/lfs","expires_in":3600}` (`git_xet/src/test_utils/ssh_server.rs:52-186`). Used by integration tests `git_xet/tests/test_ssh.rs:90-136` (ssh direct and via `sh -c`, driven through `git xet run-any` to prove PATH inheritance through the git→git-lfs→git-xet chain, esp. Windows MinGW; `test_ssh.rs:44-60`), gated by feature `git-xet-for-integration-test` (`Cargo.toml:40`).
- **Credential chain selection**: `git_xet/src/auth.rs:171-352` (url/env/netrc/ssh/git-store/noop, each asserting `whoami()`); `auth/git.rs:83-124` (real `git credential-store` round-trip); `auth/ssh.rs:87-132` (ignored tests against local ssh server / hf.co).
- **URL derivation matrix**: `git_xet/src/git_url.rs:214-591`.
- **There is no full `git push`-through-batch-API e2e in this crate** and no HTTP LFS batch mock anywhere in the workspace (only doc references, `git_xet/src/app.rs:124-131`); the closest full-stack coverage lives in the CAS-side crates (`xet_data/tests/test_clean_smudge.rs` etc.), which exercise upload/download against in-process CAS stubs, not git.

### Key constants index

`"xet"` agent name / `"git-xet"` program (`constants.rs:3-4`); headers `X-Xet-Cas-Url`, `X-Xet-Access-Token`, `X-Xet-Token-Expiration`, `X-Xet-Session-Id` (`constants.rs:10-13`, `cas_types/mod.rs:17`); envs `HF_TOKEN`, `HF_ENDPOINT` (`constants.rs:16-17`); OID length 64 (`protocol_spec.rs:16`); LFS error codes init=32, transfer=2 (`protocol_spec.rs:205-212`); token API path template `/api/{repo_type}s/{repo_id}/xet-{write|read}-token/{rev}[?create_pr=1]` (`hub_client/client.rs:83-90`); CasJWTInfo fields `casUrl`/`exp`/`accessToken` (`hub_client/types.rs:9-15`); `git-lfs-authenticate` response fields `header.Authorization`/`href`/`expires_in` (`auth/ssh.rs:16-30`); User-Agent `git_xet/0.2.1` (`xet_agent.rs:17`); LFS endpoint suffix `/info/lfs` (`git_url.rs:43-46`); access-mode config key pattern `lfs.<endpoint>.access` (`auth.rs:67-74`).