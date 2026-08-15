#!/usr/bin/env bash
#
# The client side of the xetcas demo. Every command below is one a human would
# type on a laptop; nothing here knows about docker. It runs inside the
# workbench container, driven phase by phase from demo/demo.sh so the host can
# measure the server's on-disk footprint between acts.
#
#   bash /demo/steps.sh setup           # Act 0/1: install the agent, get a repo
#   bash /demo/steps.sh push-v1         # Act 2:   first push of a 48 MiB model
#   bash /demo/steps.sh mutate-push-v2  # Act 3:   change ~2%, push again
#   bash /demo/steps.sh verify          # Act 4:   re-clone from scratch, verify
#
# State (branch name, commit ids, checksums, push timings) is kept in
# $WORK/state so the phases can run as separate `docker compose exec` calls.

set -euo pipefail

REPO_URL="${XETCAS_DEMO_REPO_URL:-http://xetcasd:8080/git/models/demo.git}"
WORK="${XETCAS_DEMO_WORK:-$HOME/work}"
SIZE_MIB="${XETCAS_DEMO_SIZE_MIB:-48}"

STATE="$WORK/state"
CLONE="$WORK/demo"
VERIFY_CLONE="$WORK/verify"
MODEL="model.safetensors"
LFS_ENDPOINT="$REPO_URL/info/lfs"

export GIT_TERMINAL_PROMPT=0

# ---------------------------------------------------------------------------
# Narration helpers
# ---------------------------------------------------------------------------

if [ -t 1 ]; then
  BOLD=$'\033[1m'; CYAN=$'\033[1;36m'; DIM=$'\033[2m'; RESET=$'\033[0m'
else
  BOLD=""; CYAN=""; DIM=""; RESET=""
fi

banner() {
  printf '\n%s────────────────────────────────────────────────────────────%s\n' "$CYAN" "$RESET"
  printf '%s %s%s\n' "$CYAN" "$*" "$RESET"
  printf '%s────────────────────────────────────────────────────────────%s\n' "$CYAN" "$RESET"
}

say() { printf '%s%s%s\n' "$DIM" "$*" "$RESET"; }

# Echo the command, then run it. The point of the demo is the commands, so
# arguments that would need quoting on a real command line get quoted here.
run() {
  local rendered="" arg
  for arg in "$@"; do
    case "$arg" in
      *[[:space:]\*\?\"\'\$\&\|\;\(\)]*) rendered="$rendered \"$arg\"" ;;
      *) rendered="$rendered $arg" ;;
    esac
  done
  printf '\n%s$%s%s\n' "$BOLD" "$rendered" "$RESET"
  "$@"
}

# Same, for a command line with a pipe in it.
run_sh() {
  printf '\n%s$ %s%s\n' "$BOLD" "$1" "$RESET"
  bash -c "$1"
}

now_ms() { date +%s%3N; }

fmt_secs() { awk -v ms="$1" 'BEGIN { printf "%.1f", ms / 1000 }'; }

state_put() { printf '%s\n' "$2" >"$STATE/$1"; }
state_get() { cat "$STATE/$1"; }

# ---------------------------------------------------------------------------
# Act 0 + 1 — a workstation that speaks xet, and an empty repo to push to
# ---------------------------------------------------------------------------

phase_setup() {
  banner "Act 0 — the toolchain"

  run git --version
  run git lfs version
  run git-xet --version

  # Identity and defaults, so the demo never stops to ask.
  git config --global user.name "xetcas demo"
  git config --global user.email "demo@xetcas.invalid"
  git config --global init.defaultBranch main

  # The demo server is fully permissive, so tell git-lfs (and git-xet, which
  # reads the same key) that this endpoint takes no credentials. Without it a
  # client that meets a 401 would try to prompt for a password.
  git config --global "lfs.${LFS_ENDPOINT}.access" none
  # xetcasd implements the batch API, not the LFS file-locking API; without
  # this, git-lfs prints a "does not support the Git LFS locking API" advisory
  # on every push.
  git config --global "lfs.${LFS_ENDPOINT}.locksverify" false

  banner "Act 1 — teach git-lfs to hand uploads to Xet"

  # This is the whole client-side install: it writes the lfs.customtransfer.xet
  # keys that make git-lfs spawn `git-xet transfer` for uploads, and runs
  # `git lfs install` if git-lfs was not set up yet.
  run git xet install

  run git config --global --get-regexp '^lfs\.(customtransfer\.xet|concurrenttransfers)'

  say ""
  say "git-lfs now offers 'xet' as a transfer to the server:"
  run_sh "git lfs env | grep -E 'Transfers='"

  banner "Act 1b — a git repo whose large files live in your CAS"

  rm -rf "$CLONE" "$VERIFY_CLONE"
  mkdir -p "$STATE"

  # The server auto-creates the bare repo the first time it is asked for, so
  # this clones an empty repository. If a server is configured without
  # autocreate, fall back to init + remote — the rest of the demo is identical.
  if run git clone "$REPO_URL" "$CLONE"; then
    cd "$CLONE"
  else
    say "clone failed; falling back to 'git init' + 'git remote add'"
    rm -rf "$CLONE"
    mkdir -p "$CLONE"
    cd "$CLONE"
    run git init -b main
    run git remote add origin "$REPO_URL"
  fi

  # An empty clone leaves HEAD on an unborn branch; that name is the branch the
  # demo pushes.
  branch="$(git symbolic-ref --short HEAD)"
  state_put branch "$branch"
  say "working on branch '$branch'"

  run git lfs track "*.safetensors"
  run cat .gitattributes

  run git add .gitattributes
  run git commit -m "Track *.safetensors with git-lfs"
  run git push -u origin "$branch"

  say ""
  say "The repository exists on the server and the LFS endpoint is:"
  run_sh "git lfs env | grep -E 'Endpoint=' | head -1"
}

# ---------------------------------------------------------------------------
# Act 2 — push a 48 MiB "model"
# ---------------------------------------------------------------------------

phase_push_v1() {
  banner "Act 2 — a 48 MiB model, pushed to your own CAS"

  cd "$CLONE"
  branch="$(state_get branch)"

  say "Generating a deterministic synthetic model file. It repeats some of its"
  say "own blocks on purpose, so chunk-level dedup has something to find."
  run python3 /demo/model.py create \
    --path "$MODEL" \
    --size-mib "$SIZE_MIB" \
    --seed 1337

  sha="$(sha256sum "$MODEL" | cut -d' ' -f1)"
  size="$(stat -c %s "$MODEL")"
  state_put v1.sha256 "$sha"
  state_put v1.size "$size"

  run git add "$MODEL"
  run git commit -m "Add model.safetensors (v1)"

  say ""
  say "What git actually stores in the commit is a pointer, not the weights:"
  run git show "HEAD:$MODEL"

  say ""
  say "The bytes go to the CAS during 'git push', via the xet transfer agent."
  start="$(now_ms)"
  run git push origin "$branch"
  elapsed="$(( $(now_ms) - start ))"
  state_put push1.ms "$elapsed"
  state_put v1.commit "$(git rev-parse HEAD)"

  say ""
  say "push completed in $(fmt_secs "$elapsed") s for $((size / 1048576)) MiB"
  run git lfs ls-files
}

# ---------------------------------------------------------------------------
# Act 3 — change ~2% of the file and push again
# ---------------------------------------------------------------------------

phase_mutate_push_v2() {
  banner "Act 3 — edit ~2% of the model and push again"

  cd "$CLONE"
  branch="$(state_get branch)"

  run python3 /demo/model.py mutate \
    --path "$MODEL" \
    --seed 4242

  sha="$(sha256sum "$MODEL" | cut -d' ' -f1)"
  size="$(stat -c %s "$MODEL")"
  state_put v2.sha256 "$sha"
  state_put v2.size "$size"

  run git add "$MODEL"
  run git commit -m "Update model.safetensors (v2)"

  say ""
  say "A new oid, because the file content changed:"
  run git show "HEAD:$MODEL"

  start="$(now_ms)"
  run git push origin "$branch"
  elapsed="$(( $(now_ms) - start ))"
  state_put push2.ms "$elapsed"
  state_put v2.commit "$(git rev-parse HEAD)"

  say ""
  say "push completed in $(fmt_secs "$elapsed") s"
  say "git-lfs sees a whole new $((size / 1048576)) MiB object; the CAS should"
  say "only have gained the chunks that actually changed. The host script"
  say "measures the server's data directory to check that."
}

# ---------------------------------------------------------------------------
# Act 4 — throw the clone away and get both versions back from the server
# ---------------------------------------------------------------------------

phase_verify() {
  banner "Act 4 — delete everything local, clone fresh, verify both versions"

  v1_sha="$(state_get v1.sha256)"
  v2_sha="$(state_get v2.sha256)"
  v1_commit="$(state_get v1.commit)"
  v2_commit="$(state_get v2.commit)"

  cd "$WORK"
  run rm -rf "$CLONE"

  # Downloads never touch the xet client cache — git-xet implements uploads
  # only, and git-lfs pulls objects over its basic transfer, which the server
  # answers by reconstructing the file from CAS chunks. Point the xet cache at
  # a throwaway directory anyway, so nothing in this phase can be served from
  # state the earlier pushes left behind.
  export HF_XET_CACHE="$WORK/verify-xet-cache"
  run rm -rf "$HF_XET_CACHE" "$VERIFY_CLONE"

  # A brand-new clone directory also means an empty .git/lfs/objects, so every
  # byte checked below came over the wire from xetcasd just now.
  run git clone "$REPO_URL" "$VERIFY_CLONE"
  cd "$VERIFY_CLONE"

  failures=0

  run git checkout --quiet "$v2_commit"
  run git lfs pull
  got_v2="$(sha256sum "$MODEL" | cut -d' ' -f1)"
  if [ "$got_v2" = "$v2_sha" ]; then
    printf '  %s v2 sha256 %s matches\n' "PASS" "$got_v2"
  else
    printf '  %s v2 sha256 %s != expected %s\n' "FAIL" "$got_v2" "$v2_sha"
    failures=$((failures + 1))
  fi

  run git checkout --quiet "$v1_commit"
  run git lfs pull
  got_v1="$(sha256sum "$MODEL" | cut -d' ' -f1)"
  if [ "$got_v1" = "$v1_sha" ]; then
    printf '  %s v1 sha256 %s matches\n' "PASS" "$got_v1"
  else
    printf '  %s v1 sha256 %s != expected %s\n' "FAIL" "$got_v1" "$v1_sha"
    failures=$((failures + 1))
  fi

  if [ "$failures" -ne 0 ]; then
    printf '\n%s content verification failed (%d mismatch(es))%s\n' "$BOLD" "$failures" "$RESET"
    return 1
  fi

  printf '\n%s Both versions were reconstructed byte-for-byte from the CAS.%s\n' "$BOLD" "$RESET"
}

# ---------------------------------------------------------------------------
# Convenience: report what the demo recorded (used by the host summary).
# ---------------------------------------------------------------------------

phase_state() {
  for key in branch v1.sha256 v1.size v1.commit push1.ms v2.sha256 v2.size v2.commit push2.ms; do
    if [ -f "$STATE/$key" ]; then
      printf '%s=%s\n' "$key" "$(state_get "$key")"
    fi
  done
}

main() {
  mkdir -p "$WORK" "$STATE"
  case "${1:-}" in
    setup) phase_setup ;;
    push-v1) phase_push_v1 ;;
    mutate-push-v2) phase_mutate_push_v2 ;;
    verify) phase_verify ;;
    state) phase_state ;;
    *)
      echo "usage: $0 {setup|push-v1|mutate-push-v2|verify|state}" >&2
      exit 2
      ;;
  esac
}

main "$@"
