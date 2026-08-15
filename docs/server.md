# xetcasd

A self-hosted Xet CAS server. The real `xet-core` client (crates.io 1.6.0)
uploads to and downloads from it unmodified, and stock `git` + `git-lfs` +
`git-xet` push to it through the built-in LFS bridge.

Wire bodies use the generated contract types from `xetcas-contracts`. Binary
formats -- xorb chunk frames, at-rest footers, MDB shards -- and every hash in
them come from upstream `xet-core-structures`; none of it is reimplemented.

## Configuration

Every flag has an environment variable; the container image is driven purely
through the environment.

| Variable | Flag | Default | Meaning |
|---|---|---|---|
| `XETCAS_DATA_DIR` | `--data-dir` | `./xetcas-data` | Objects, index, git repos |
| `XETCAS_LISTEN` | `--listen` | `0.0.0.0:8080` | Bind address |
| `XETCAS_PUBLIC_URL` | `--public-url` | `http://localhost:8080` | Base for every URL handed out |
| `XETCAS_GIT_ROOT` | `--git-root` | `$XETCAS_DATA_DIR/git` | Bare repositories |
| `XETCAS_GIT_AUTOCREATE` | `--git-autocreate` | `true` | Create a repo on first touch |
| `XETCAS_TOKEN` | `--token` | unset | Coarse shared write secret (not authentication) |

`XETCAS_PUBLIC_URL` must be reachable **by the client**, not by the server: it
is spliced into reconstruction fetch URLs, LFS hrefs, and the CAS URL handed to
git-xet. It must not end in a slash -- xet-core builds request URLs by string
concatenation.

`XETCAS_TOKEN` is **not authentication** -- authentication is deliberately out
of scope here. It is a single coarse shared secret that the server also *hands
out*: the LFS batch response and `GET /xet-token` advertise it to any caller
that reaches them (git-xet needs it for its follow-up upload), so it is not
secret from anyone who can reach those endpoints. When set it gates only the two
CAS write routes (xorb upload and shard upload); the git bridge, downloads, the
token route and the LFS batch route stay open. Unset, the server is fully
permissive. For real access control put xetcasd behind an authenticating reverse
proxy (the same terminate-auth-at-the-proxy model the rest of this
infrastructure uses); do not treat `XETCAS_TOKEN` as a security boundary.

## Storage layout

```
$XETCAS_DATA_DIR/
  xorbs/<h0..2>/<h2..4>/<64 hex>   objects, stored WITH the canonical v1 footer
  staging/                         atomic-rename staging, outside the object tree
  index.sqlite                     metadata (WAL)
  git/                             bare repositories
```

Chunk frames stay at offset 0 in a stored object, so a reconstruction
`url_range` indexes the file directly. The footer follows the frames and is
never served.

The SQLite index holds `xorbs`, `files` (indexed by sha256 for the LFS bridge),
`chunks` (dedup-eligible only), `store_stats`, and `schema_meta`. Records are
prost-encoded contract messages, so the on-disk metadata shares the schema the
wire uses. `store_stats` accumulates the size of each object as it is indexed,
in the same transaction as its `xorbs` row, which is what keeps `/health`
constant time.

## Limits and back-pressure

| Bound | Value | Why |
|---|---|---|
| Xorb/shard body | 68 MiB | 64 MiB of content plus frame headers and footer |
| Concurrent xorb upload bodies | 16 | Each near-limit upload holds the body plus a second stored copy; the client documents 64 concurrent uploads, which unbounded is multiple GiB resident |
| Concurrent download decoders | 64 | Each parks a blocking-pool thread that sqlite shares |

The xorb upload permit is taken **before** the body is read, so the cap bounds
buffered bytes rather than just verification work. Requests over the cap queue;
V1 xorb upload has no client-side read timeout to trip.

## Routes

| Route | Notes |
|---|---|
| `GET /health` | `{status, xorbs, files, stored_bytes}`; constant time |
| `POST /v1/xorbs/{prefix}/{hash}` | Verifies the footerless body; idempotent |
| `GET /v1/xorbs/{prefix}/{hash}/data` | Ranged fetch target; **unauthenticated** |
| `GET /v1/reconstructions/{file_id}` | 416 when a range starts past EOF |
| `GET /v1/chunks/{prefix}/{hash}` | HMAC-keyed dedup shard, or 404 |
| `POST /v1/shards` | Derives both identifiers from stored content; idempotent |
| `POST /v1/telemetry` | Always 200, body dropped |
| `/v2/*` | Always 404, driving the client's v1 fallback |
| `POST /git/{repo}.git/info/lfs/objects/batch` | LFS batch |
| `GET /lfs/objects/{oid}` | Server-side reconstruction by sha256 |
| `GET /xet-token` | Token bootstrap and refresh |
| `ANY /git/{path}` | `git http-backend` over CGI |

Any prefix is accepted on the xorb and chunk routes; the client sends
`default` on both.

### What a shard upload is checked against

Everything in a shard is client-asserted metadata about content the server
already holds, so nothing in it is copied into the index unverified:

- every term must name a stored xorb, stay inside its chunk count, and declare
  the byte length its chunk range actually decodes to;
- each verification range hash is recomputed from our own stored chunk hashes;
- the **file hash** is recomputed as the aggregated hash of the file's ordered
  chunk `(hash, size)` pairs, read out of those xorbs;
- the **sha256** is checked by hashing the reconstructed content. It is the
  git-lfs oid and is not derivable from chunk hashes, so this costs one pass
  over the file — skipped when the same `(file_hash, sha256)` pair is already
  registered, which is the ordinary idempotent re-upload;
- cas-info global-dedup flags are corroborated against the named xorb and
  dropped, not fatal, when they cannot be.

A mismatch is a 400. This matters because `files` is keyed by file hash and
insert-only and `chunks` is keyed by chunk hash with `INSERT OR IGNORE`: an
unverified identifier is squatted permanently, and a squatted sha256 makes the
LFS batch report an object as already stored so its genuine upload never
happens.

### Why `/health` never walks the object tree

The container health-checks it every five seconds with a three-second timeout,
so nothing in it may scale with the store. `stored_bytes` is the total the index
accumulated as each xorb was registered — for any index this schema created it
equals the sum of object file sizes under `xorbs/`, and it deliberately excludes
orphan blobs left by a crash between the object write and the index insert,
which no reconstruction can reach. The object tree is still *probed* (the root
must exist, be a directory, and open), so a failed mount fails the health request
instead of reporting `"status":"ok"` while every download 500s.

### Why the v2 routes answer 404

404 is the documented "this server is v1 only" signal, and the client caches
the fallback. It must not be 501: the client treats 501 as permanently fatal
everywhere else.

## Git and LFS

Uploads negotiate the `xet` transfer, so git-xet performs the chunked,
deduplicated CAS upload and the objects land as real Xet content. Downloads are
always answered with the stock `basic` transfer, because git-xet implements no
download path -- the server reconstructs the file from CAS and serves the bytes,
which is why shards must carry the sha256 in their metadata.

An upload batch that does not offer `xet` is refused per object with a 422:
accepting a basic upload would store bytes the CAS cannot address.

### `authenticated` and reverse proxies

Git LFS reads `authenticated: true` on a batch object as "the action already
embeds authorization, do not apply your own", and git-lfs does **not** copy the
batch request's `Authorization` onto the action request it goes on to make. So
xetcasd sets the flag only when the action really carries an `Authorization`:
whatever the batch request arrived with is echoed into the action headers, and
`authenticated` is derived from whether that happened.

With no proxy in front there is nothing to forward, the flag is false (omitted
in JSON, which the LFS spec defines as equivalent), and git-lfs applies its
normal credential chain — for an open server, no credentials at all. Behind an
authenticating reverse proxy the credential rides along, so a clone's download
GET is accepted instead of 401ing. This is not an authentication mechanism:
xetcasd still does not authenticate anyone, it just stops lying to the client
about what its actions carry.

Repositories are created on first touch with `http.receivepack` enabled;
without it anonymous pushes fail.

## Running

```bash
cargo run -p xetcasd
docker build -f docker/Dockerfile.server -t xetcasd .

cargo test          # unit + real-client integration tests
cargo clippy --all-targets -- -D warnings
```

The integration tests drive the genuine xet-core client against an in-process
server, and the git tests shell out to the real `git` binary.
