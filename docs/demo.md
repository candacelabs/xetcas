# The demo, act by act

```bash
bash demo/demo.sh        # from the xetcas/ component root
```

One command. It builds two images, brings up an isolated compose stack
(`xetcas-demo`), and then runs a real git workflow inside a container that has
nothing but `git`, `git-lfs` and `git-xet` on it. Everything you see below is a
command a person could type; the only thing the host script does that a person
would not is measure the server's data directory between the acts.

- **Requirements:** Docker (with compose v2). Nothing else — no Rust, no Go, no
  git-lfs on the host.
- **First run:** slow, because it compiles `xetcasd` and `git-xet` from source.
  Later runs: `SKIP_BUILD=1 bash demo/demo.sh`.
- **Exit code:** 0 only if both file versions verify and the incremental push
  stays inside its storage budget, so this doubles as an acceptance test.

The pieces: [`demo/demo.sh`](../demo/demo.sh) (host orchestration and
measurement), [`demo/steps.sh`](../demo/steps.sh) (everything that happens
inside the client), [`demo/model.py`](../demo/model.py) (the synthetic model
file), [`docker/compose.demo.yaml`](../docker/compose.demo.yaml) (the stack).

## Act 0 — the toolchain

The workbench prints its versions. The `git-xet` binary is built from the
xet-core revision this component targets, so the demo is a conformance test
against the real client:

```
$ git --version
git version 2.39.5

$ git lfs version
git-lfs/3.7.1 (GitHub; linux amd64; go 1.25.3; git b84b3384)

$ git-xet --version
git-xet 0.2.1
```

## Act 1 — teach git-lfs to hand uploads to Xet

`git xet install` is the entire client-side installation. It writes three keys
to `~/.gitconfig` and, if git-lfs was never set up on the machine, runs
`git lfs install` for you:

```
$ git xet install
git-xet installed to global config!

$ git config --global --get-regexp ^lfs\.(customtransfer\.xet|concurrenttransfers)
lfs.customtransfer.xet.path git-xet
lfs.customtransfer.xet.args transfer
lfs.customtransfer.xet.concurrent true

$ git lfs env | grep -E 'Transfers='
ConcurrentTransfers=8
TusTransfers=false
DownloadTransfers=basic,lfs-standalone-file,ssh,xet
UploadTransfers=basic,lfs-standalone-file,ssh,xet
```

`xet` now appears in the transfer list git-lfs offers the server. Whether it is
used is the *server's* choice, made in its batch response — and the server picks
it for uploads only. Downloads stay on `basic`, because git-xet implements the
upload half of the protocol only.

The demo also sets two per-endpoint keys, both because the server is a
permissive single-node service: `access = none` (there are no credentials to go
looking for, so nothing prompts) and `locksverify = false` (xetcasd implements
the batch API, not the file-locking API).

## Act 1b — a repository whose large files live in your CAS

The server creates the bare repository the first time it is asked for it, so
this clones an empty repo, tracks a pattern, and pushes:

```
$ git clone http://xetcasd:8080/git/models/demo.git /home/xet/work/demo
Cloning into '/home/xet/work/demo'...
warning: You appear to have cloned an empty repository.

$ git lfs track "*.safetensors"
Tracking "*.safetensors"

$ cat .gitattributes
*.safetensors filter=lfs diff=lfs merge=lfs -text

$ git push -u origin main
To http://xetcasd:8080/git/models/demo.git
 * [new branch]      main -> main

$ git lfs env | grep -E 'Endpoint=' | head -1
Endpoint=http://xetcasd:8080/git/models/demo.git/info/lfs (auth=none)
```

That `Endpoint` line is worth a look: git-lfs derived the LFS endpoint from the
git remote by appending `/info/lfs`, with no extra configuration. This is the
same derivation that makes the "keep your git host, override `lfs.url`" setup in
the README work.

## Act 2 — push a 48 MiB model

The file is synthetic but deliberate: 48 MiB of pseudorandom data of which only
32 MiB is distinct, because the generator reuses some of its own 1 MiB blocks.
Random data does not compress, so anything the server saves here it saved by
recognising duplicate chunks, not by zipping bytes.

```
$ python3 /demo/model.py create --path model.safetensors --size-mib 48 --seed 1337
  file            : model.safetensors
  size            : 48.00 MiB (50331648 bytes)
  block size      : 1.00 MiB
  distinct content: 32.00 MiB (66.7% of the file)
  repeated content: 16.00 MiB (33.3% of the file)
  sha256          : 5be63357e3bd24d7544fc2fa68144f1ffbe6cd354559f1f62b12615fcc272173
```

What git records is a pointer, not the weights:

```
$ git show HEAD:model.safetensors
version https://git-lfs.github.com/spec/v1
oid sha256:5be63357e3bd24d7544fc2fa68144f1ffbe6cd354559f1f62b12615fcc272173
size 50331648
```

The bytes move during `git push`: git-lfs runs its pre-push hook, asks the
server for a batch, the server answers `"transfer":"xet"` with a CAS URL and a
token, and git-lfs spawns `git-xet transfer`, which chunks the file, uploads the
chunks it needs to as xorbs, and finally uploads the shard — the recipe that
maps this sha256 to its chunk list.

## Act 3 — change 2% of the model and push again

```
$ python3 /demo/model.py mutate --path model.safetensors --seed 4242
  file            : model.safetensors
  rewrote         : 512.00 KiB in place at offset 16789561
  appended        : 512.00 KiB at the end
  changed         : 1.00 MiB (2.08% of the previous version)
  size            : 48.00 MiB -> 48.50 MiB
```

The rewrite lands on a deliberately unaligned offset, and the append shifts
nothing — which is the point of content-defined chunking: chunk boundaries
follow the data, so an edit in the middle does not invalidate everything after
it.

To git-lfs this is a brand-new object with a brand-new oid and 48.5 MiB of
bytes. To the CAS it is mostly chunks it already has.

## Act 4 — delete everything local and get both versions back

The clone is deleted, the xet cache is pointed at a throwaway directory, and a
fresh clone starts from an empty `.git/lfs/objects`. Every byte checked here
came off the server in that moment, and it came through git-lfs's *basic*
transfer — meaning the server reconstructed each file from its chunks:

```
$ git clone http://xetcasd:8080/git/models/demo.git /home/xet/work/verify
$ git checkout --quiet <v2 commit> && git lfs pull
  PASS v2 sha256 … matches
$ git checkout --quiet <v1 commit> && git lfs pull
  PASS v1 sha256 … matches
```

## The ledger

Finally the host prints what the server actually stored, measured with `du`
inside the server container before and after each push:

```
────────────────────────────────────────────────────────────
  What the server actually stored
────────────────────────────────────────────────────────────
  push 1: 48.00 MiB    of model  ->  ~32 MiB      of data (… in xorbs)  in … s
  push 2: 48.50 MiB    of model  ->  ~1-2 MiB     of data (… in xorbs)  in … s
  dedup on the first push (the file repeats its own blocks): ~33%
  dedup on the second push (~2% of the file changed)      : ~97%

  PASS second push grew the store by …, under the 24.25 MiB budget

  DEMO PASSED
```

The exact byte counts depend on chunk boundaries and the server's compression
policy, so treat the numbers above as the shape of the result rather than
constants. The two claims the script *enforces* are the ones that matter: both
versions come back byte-for-byte identical, and the second push does not cost
another whole file (`XETCAS_DEMO_MAX_GROWTH_RATIO`, default 0.5).

## Playing with it afterwards

The stack is left running:

```bash
# a client shell, already configured
docker compose -f docker/compose.demo.yaml exec workbench bash

# the server
curl -s http://127.0.0.1:8080/health
docker compose -f docker/compose.demo.yaml exec xetcasd du -sh /data
docker compose -f docker/compose.demo.yaml exec xetcasd du -sh /data/xorbs
docker compose -f docker/compose.demo.yaml logs -f xetcasd

# clean up
docker compose -f docker/compose.demo.yaml down --volumes
```

Inside the workbench, `~/work/demo` is the repository and everything is already
installed, so you can push your own files at it.

## Knobs and troubleshooting

| Variable | Default | Effect |
|---|---|---|
| `SKIP_BUILD` | `0` | `1` reuses the images already built |
| `RESET` | `1` | `0` keeps the server's data volume from the previous run |
| `TEARDOWN` | `0` | `1` removes the stack and volumes at the end |
| `XETCAS_DEMO_SIZE_MIB` | `48` | size of the synthetic model |
| `XETCAS_DEMO_MAX_GROWTH_RATIO` | `0.5` | storage budget for the second push |

- **`RESET=0` makes the first push look too good.** With the previous run's
  data still there, push 1 dedups against chunks the server already had. That is
  cross-run dedup working correctly, not a measurement error — but if you want
  the honest first-push number, leave `RESET` alone.
- **Port 8080 already in use.** The stack publishes `127.0.0.1:8080`; change the
  `ports` entry in `docker/compose.demo.yaml` if something else owns it. Nothing
  inside the demo uses the published port — the workbench talks to `xetcasd:8080`
  on the compose network.
- **A phase fails.** The script prints the tail of the server log and leaves the
  stack up. `docker compose -f docker/compose.demo.yaml logs xetcasd` has the
  rest.
- **`git push` hangs asking for credentials.** It should not: `GIT_TERMINAL_PROMPT=0`
  is set and the endpoint is marked `access = none`. If you see it against your
  *own* server, that server is answering 401 somewhere the demo server does not.
