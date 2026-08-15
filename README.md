# xetcas

**Git LFS with rsync-style transfer, on a server you own.**

`xetcasd` is a single-binary, self-hosted [Xet](https://github.com/huggingface/xet-core)
content-addressable storage server with a Git LFS front door. Point a plain git
repository at it and large files stop moving as whole blobs: the client splits
them into content-defined chunks, uploads only the chunks the server does not
already have, and the server reassembles the file on demand. Re-pushing a 48 MiB
model after editing 2% of it costs about 1 MiB of transfer and 1 MiB of storage,
not another 48 MiB. The client side is stock `git` and `git-lfs` plus
[`git-xet`](https://github.com/huggingface/xet-core/tree/main/git_xet), xet-core's
LFS custom transfer agent — no forks, no wrappers, no daemon on your laptop.

The scope is deliberately narrow: **storage and transfer, single node,
permissive by default.** xetcasd stores xorbs, shards and a chunk index in one
data directory, serves git smart HTTP for the repositories it hosts, and answers
the LFS batch API. It has no user model, no per-repository permissions and no
quotas. `XETCAS_TOKEN` is one optional coarse shared secret, **not**
authentication: the server advertises it to clients in the LFS batch response,
it gates only the two CAS write routes, and unset means anyone who can reach the
port can read and write. Put xetcasd behind an authenticating reverse proxy for
real access control; it is designed to sit on a private network and to be backed
up by copying one directory. There is no clustering, no replication and
no garbage collection of unreferenced chunks yet.

## Quickstart

The demo builds everything, runs a complete push/edit/push/verify cycle against
a throwaway server, and prints what the server actually stored:

```bash
git clone https://github.com/candacelabs/xetcas && cd xetcas
bash demo/demo.sh
```

It needs Docker and nothing else — no Rust, no Go, no git-lfs on the host. The
first run compiles `xetcasd` and `git-xet`, so give it a few minutes; afterwards
`SKIP_BUILD=1 bash demo/demo.sh` re-runs in seconds. The stack stays up when it
finishes so you can poke at it:

```bash
docker compose -f docker/compose.demo.yaml exec workbench bash   # the client
curl -s http://127.0.0.1:8080/health                             # the server
docker compose -f docker/compose.demo.yaml down --volumes        # clean up
```

[docs/demo.md](docs/demo.md) walks through the same run act by act, with the
output you should expect.

## Use it with your own repositories

**1. Run the server.** One binary, one data directory, one port:

```bash
docker build -f docker/Dockerfile.server -t xetcasd .
docker run -d --name xetcasd \
  -p 8080:8080 \
  -v xetcas-data:/data \
  -e XETCAS_PUBLIC_URL=http://your-host:8080 \
  xetcasd
```

(There is no published image; build it from this repository, or
`cargo build --release -p xetcasd` and run the binary directly.)

`XETCAS_PUBLIC_URL` matters: the server mints the CAS and download URLs it hands
to clients from it, so it must be the address *clients* use, with no trailing
slash.

| Variable | Default | Purpose |
|---|---|---|
| `XETCAS_DATA_DIR` | `./xetcas-data` | xorbs, shards, chunk index, git repositories |
| `XETCAS_LISTEN` | `0.0.0.0:8080` | listen address |
| `XETCAS_PUBLIC_URL` | `http://localhost:8080` | base URL used to mint CAS/LFS/fetch URLs |
| `XETCAS_GIT_ROOT` | `$XETCAS_DATA_DIR/git` | where bare repositories live |
| `XETCAS_GIT_AUTOCREATE` | `true` | create a bare repo on first push/clone |
| `XETCAS_TOKEN` | unset | coarse shared write secret, not auth; unset = fully permissive |

**2. Set up the client once per machine.** Install `git-lfs` and `git-xet`, then:

```bash
git xet install     # registers the "xet" LFS custom transfer agent, globally
```

That writes three `lfs.customtransfer.xet.*` keys to `~/.gitconfig` and nothing
else. `git lfs env` should now list `xet` under `UploadTransfers`.

**3a. Host the repository on xetcasd.** Push to it like any other git remote;
the bare repo is created on first contact when `XETCAS_GIT_AUTOCREATE` is on:

```bash
git clone http://your-host:8080/git/models/llama-finetune.git
cd llama-finetune
git lfs track "*.safetensors"
git add .gitattributes && git commit -m "Track weights with LFS"
git add model.safetensors && git commit -m "Add weights"
git push
```

**3b. Or keep your existing git host** (GitHub, Gitea, whatever) and send only
the large objects to xetcasd, by overriding the LFS endpoint per repository:

```bash
git config lfs.url http://your-host:8080/git/models/llama-finetune.git/info/lfs
```

The repository path in the URL is just a namespace for the LFS objects; it does
not have to match your git host's path, and the git history still lives with
your existing host.

**Anonymous access.** Against a permissive server, tell git-lfs there are no
credentials to look for, or it will try to prompt on the first 401:

```bash
git config --global "lfs.http://your-host:8080/git/models/llama-finetune.git/info/lfs.access" none
```

## How it works

Contracts first: the wire format lives in
[`proto/xetcas/v1/`](proto/xetcas/v1) as Liquid Proto (protobuf plus refinement
predicates), and both the Rust server types and the Go client types are
generated from it — `protox`/`prost` for Rust, a pinned `protoc` toolchain in a
container for Go. The proto files are the normative description of every JSON
body, header spelling and hash encoding on the wire.

```
  developer's machine                        your server
  ───────────────────                        ───────────
  git push
    └─ git-lfs pre-push
        ├─ POST .../info/lfs/objects/batch ──────────▶  LFS bridge
        │   { "operation":"upload",                     answers transfer "xet",
        │     "transfers":["xet",...] }   ◀──────────   returns X-Xet-Cas-Url +
        │                                               a token and its expiry
        └─ spawns `git-xet transfer`
             ├─ chunk (content-defined, ~64 KiB) + hash
             ├─ GET  /v1/chunks/{prefix}/{hash} ─────▶  "do you already have
             │                                          chunks near this one?"
             ├─ POST /v1/xorbs/{prefix}/{hash} ──────▶  only the new chunks,
             │                                          packed into xorbs
             └─ POST /v1/shards ────────────────────▶   the file's recipe
                                                        (sha256 -> chunk list)

  git clone / git lfs pull
    └─ git-lfs batch (download) ────────────────────▶  answers transfer "basic"
        └─ GET /lfs/objects/{oid} ──────────────────▶  server reconstructs the
                                                       file from its chunks and
                                                       streams the bytes back
```

Two consequences worth knowing:

- **Uploads are chunked and deduplicated; downloads are whole files.** git-xet
  implements the upload half of the LFS transfer protocol only — it explicitly
  refuses download and tells git-lfs to use its standard basic transfer. So the
  *server* does the reconstruction work on the way out. That is why the demo's
  verification clone is a genuine test of the storage layer.
- **The pointer file is stock git-lfs.** Nothing about the format changes:
  `version`/`oid sha256:…`/`size`. A repository pushed through xetcas is a
  normal LFS repository whose objects happen to live in a CAS, and the LFS oid
  (the file's sha256) is recorded in the shard, which is what lets the server
  serve a plain LFS download by oid.

## Protocol compatibility

Written against **xet-core `77fc84d3d`** (git-xet 0.2.1, hf-xet 1.6.0). The
workbench image builds `git-xet` from exactly that revision, so the demo is a
conformance test against the real client, not against a mock.

Implemented (the surface the real client uses):

| Route | Notes |
|---|---|
| `GET /v1/reconstructions/{file_id}` | with `Range`; `416` past EOF |
| `GET /v1/chunks/{prefix}/{hash}` | global dedup query; `404` = not tracked |
| `POST /v1/xorbs/{prefix}/{hash}` | xorb upload, footer reconstructed server-side |
| `GET /v1/xorbs/{prefix}/{hash}/data` | the fetch URL handed out in reconstructions, single `Range` |
| `POST /v1/shards` | shard upload |
| `POST /v1/telemetry` | accepted and discarded |
| `GET /health` | liveness |
| `GET /git/<path>.git/...` | git smart HTTP (`git http-backend`) |
| `POST /git/<path>.git/info/lfs/objects/batch` | LFS batch: `xet` for upload, `basic` for download |
| `GET /lfs/objects/{oid}` | reconstructed bytes for an LFS oid |
| `GET /xet-token` | `{"casUrl","exp","accessToken"}` token refresh route |

`404` by design: `/v2/reconstructions/{id}`, `/v2/shards`,
`/v2/file-chunk-hashes/{id}`. The client probes v2 first and falls back to v1 on
`404`/`501`, caching the answer for the session — a v1-only server is a
supported configuration, not a degraded one.

Not implemented: SSH remotes (`git-lfs-authenticate`), the batch
reconstruction query, and multipart range responses. Only http(s) remotes are
supported, which also sidesteps git-xet's one Hugging Face-ism (a non-http
remote *with an explicit port* requires `HF_ENDPOINT` to be set to something).

The reverse-engineered protocol notes this implementation is built from live in
[`docs/research/`](docs/research): the CAS HTTP contract
([api-surface.md](docs/research/api-surface.md)), the xorb/shard binary formats
([binary-formats.md](docs/research/binary-formats.md)), the upload/download
pipelines ([dataplane.md](docs/research/dataplane.md)), client configuration
([config-selfhost.md](docs/research/config-selfhost.md)), and the git
integration ([git-xet.md](docs/research/git-xet.md)).

## Repository layout

| Path | What |
|---|---|
| `proto/xetcas/v1/` | the contracts: [transfer](proto/xetcas/v1/transfer.proto) (CAS wire), [storage](proto/xetcas/v1/storage.proto) (persisted records), [bridge](proto/xetcas/v1/bridge.proto) (Git LFS) |
| `proto/generate.sh` | containerized Go codegen (`write`, `check`, or `check-drift`) |
| `crates/` | the Rust workspace: generated contracts, validators, and `xetcasd` |
| `xtask/` | `cargo xtask gen-proto [--check]` — Rust codegen via protox, no host `protoc` |
| `go/` | generated Go types and their validators (module `github.com/candacelabs/xetcas/go`) |
| `docker/` | [server image](docker/Dockerfile.server), [workbench image](docker/Dockerfile.workbench), [demo stack](docker/compose.demo.yaml) |
| `demo/` | [the demo](demo/demo.sh), the [narrated steps](demo/steps.sh), the synthetic model generator |
| `docs/research/` | protocol dossiers, with citations into xet-core |
| `.dis/` | dev container definition (Rust 1.91 + just) |

## Development

The Rust toolchain runs on the host (via `cargo`/`cargo xtask`); only Go and
protoc are containerized, in the pinned codegen image, so you never install
Go or protoc locally.

```bash
just --list                  # the available targets
cargo xtask gen-proto        # regenerate the Rust contracts (protox, no protoc)
cargo xtask gen-proto --check
bash proto/generate.sh       # regenerate the Go contracts (pinned protoc image)
bash proto/generate.sh check # fail on drift, in contents AND in the file set
bash proto/generate.sh check-drift  # ...and prove that check can actually fail
```

`.dis/Dockerfile` is the dev container: Rust 1.91 with `rustfmt`, `clippy`,
`just` and `git`. Codegen output is committed, and CI fails if regenerating
produces a diff — change the `.proto` files, regenerate, commit both.

The demo doubles as the acceptance test: `bash demo/demo.sh` exits non-zero if
either version fails to verify or if the incremental push transfers more than
its budget. [`.github/workflows/ci.yaml`](.github/workflows/ci.yaml) runs
formatting, clippy, tests, codegen drift checks and the workbench image build on
every push.

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). This project
implements the storage and transfer protocol defined by
[xet-core](https://github.com/huggingface/xet-core) (Copyright Hugging Face,
Inc., Apache 2.0); the workbench image links against xet-core crates fetched at
a pinned revision.
