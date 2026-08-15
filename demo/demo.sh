#!/usr/bin/env bash
#
# The xetcas demo, end to end, in one command:
#
#     bash demo/demo.sh          # from the xetcas/ component root
#
# It builds the two images, brings up an isolated compose stack (a xetcasd
# server and a workbench with git + git-lfs + git-xet), and then drives a real
# git workflow inside the workbench: track a 48 MiB file with git-lfs, push it
# to your own CAS, change ~2% of it, push again, throw the clone away, clone
# from scratch and verify both versions byte-for-byte.
#
# Between the acts this script measures the server's data directory, so the
# dedup claim is a measurement, not a slogan.
#
# Knobs (all optional):
#   SKIP_BUILD=1                  reuse existing images
#   RESET=0                       keep the server's data volume from a previous
#                                 run (the first push then dedups against it)
#   TEARDOWN=1                    remove the stack and volumes when finished
#   XETCAS_DEMO_SIZE_MIB=48       size of the synthetic model file
#   XETCAS_DEMO_MAX_GROWTH_RATIO  fail if the second push grows the store by
#                                 more than this fraction of the file (0.5)
#
# Exit code is 0 only if both versions verify and the second push stayed under
# the growth budget, so this is usable as a CI check.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
component_root="$(cd "${here}/.." && pwd)"
compose_file="${component_root}/docker/compose.demo.yaml"

SKIP_BUILD="${SKIP_BUILD:-0}"
RESET="${RESET:-1}"
TEARDOWN="${TEARDOWN:-0}"
SIZE_MIB="${XETCAS_DEMO_SIZE_MIB:-48}"
MAX_GROWTH_RATIO="${XETCAS_DEMO_MAX_GROWTH_RATIO:-0.5}"

if [ -t 1 ]; then
  BOLD=$'\033[1m'; CYAN=$'\033[1;36m'; RED=$'\033[1;31m'; GREEN=$'\033[1;32m'; RESET_C=$'\033[0m'
else
  BOLD=""; CYAN=""; RED=""; GREEN=""; RESET_C=""
fi

note() { printf '%s>> %s%s\n' "$CYAN" "$*" "$RESET_C"; }
fail() { printf '%s!! %s%s\n' "$RED" "$*" "$RESET_C" >&2; }

compose() { docker compose -f "${compose_file}" "$@"; }

human() {
  awk -v bytes="${1:-0}" 'BEGIN {
    split("B KiB MiB GiB TiB", unit, " ")
    i = 1
    while (bytes >= 1024 && i < 5) { bytes /= 1024; i++ }
    if (i == 1) printf "%d %s", bytes, unit[i]
    else printf "%.2f %s", bytes, unit[i]
  }'
}

# Bytes used at a path inside the server container. /data and /data/xorbs are
# created at server startup, so an empty or failed measurement is a real error,
# not a "path absent yet" 0. Fail the run rather than coerce it to 0 -- a
# swallowed measurement would otherwise let the ledger print "100% dedup / PASS".
server_bytes() {
  local raw out
  if ! raw="$(compose exec -T xetcasd sh -c "du -sb '$1' 2>/dev/null | cut -f1")"; then
    fail "measuring $1: 'docker compose exec xetcasd' failed"
    return 1
  fi
  out="$(printf '%s' "$raw" | tr -dc '0-9')"
  if [ -z "$out" ]; then
    fail "measuring $1: empty result (is the xetcasd container up?)"
    return 1
  fi
  printf '%s' "$out"
}

# Measure the store into SNAP_XORBS and SNAP_TOTAL. Returns nonzero (so the
# caller aborts) if either measurement fails; never coerces a failure to 0.
snapshot() {
  SNAP_XORBS="$(server_bytes /data/xorbs)" || return 1
  SNAP_TOTAL="$(server_bytes /data)" || return 1
}

workbench() {
  compose exec -T \
    -e XETCAS_DEMO_SIZE_MIB="${SIZE_MIB}" \
    workbench bash /demo/steps.sh "$@"
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

command -v docker >/dev/null 2>&1 || { fail "docker is required"; exit 1; }
docker compose version >/dev/null 2>&1 || { fail "docker compose v2 is required"; exit 1; }

printf '\n%s xetcas demo — git lfs with rsync-style transfer, on your own server%s\n' "$BOLD" "$RESET_C"
printf '   component root : %s\n' "${component_root}"
printf '   compose project: xetcas-demo (%s)\n\n' "${compose_file}"

if [ "${RESET}" != "0" ]; then
  note "Removing any previous demo stack and its volumes (RESET=0 to keep them)"
  compose down --volumes --remove-orphans >/dev/null 2>&1 || true
fi

if [ "${SKIP_BUILD}" = "1" ]; then
  note "SKIP_BUILD=1 — using existing images"
else
  note "Building images (the workbench compiles git-xet from pinned xet-core; first run is slow)"
  if ! compose build; then
    fail "image build failed"
    fail "if the xetcasd build failed, make sure crates/xetcasd exists and 'cargo build --release -p xetcasd' works"
    exit 1
  fi
fi

note "Starting the stack and waiting for xetcasd to report healthy"
if ! compose up -d --wait --wait-timeout 180; then
  fail "the stack did not become healthy; recent server logs:"
  compose logs --tail 50 xetcasd || true
  exit 1
fi

# ---------------------------------------------------------------------------
# The demo itself
# ---------------------------------------------------------------------------

# If a phase blows up, the server log is almost always the reason.
on_error() {
  local rc=$?
  fail "demo aborted (exit ${rc}). Last 40 lines of the server log:"
  compose logs --tail 40 xetcasd || true
  fail "the stack is still up; 'docker compose -f docker/compose.demo.yaml down --volumes' to clean up"
}
trap on_error ERR

snapshot || { fail "baseline store measurement failed"; exit 1; }
base_xorbs="$SNAP_XORBS"; base_total="$SNAP_TOTAL"

status=0

workbench setup
workbench push-v1
snapshot || { fail "post-push-1 store measurement failed"; exit 1; }
after1_xorbs="$SNAP_XORBS"; after1_total="$SNAP_TOTAL"

workbench mutate-push-v2
snapshot || { fail "post-push-2 store measurement failed"; exit 1; }
after2_xorbs="$SNAP_XORBS"; after2_total="$SNAP_TOTAL"

if ! workbench verify; then
  status=1
fi

trap - ERR

# ---------------------------------------------------------------------------
# The ledger
# ---------------------------------------------------------------------------

state_file="$(mktemp)"
trap 'rm -f "${state_file}"' EXIT
if ! workbench state >"${state_file}"; then
  fail "could not read the demo state from the workbench"
  exit 1
fi
get_state() { awk -F= -v k="$1" '$1 == k { print $2 }' "${state_file}"; }

v1_size="$(get_state v1.size)"
v2_size="$(get_state v2.size)"
push1_ms="$(get_state push1.ms)"
push2_ms="$(get_state push2.ms)"
# A missing measurement is a failed run, not a 0: fail loudly instead of
# printing a vacuous "100% dedup / PASS".
for pair in "v1.size=${v1_size}" "v2.size=${v2_size}" "push1.ms=${push1_ms}" "push2.ms=${push2_ms}"; do
  if [ -z "${pair#*=}" ]; then
    fail "demo state is missing ${pair%%=*}; the run did not finish its measurements"
    exit 1
  fi
done

grew1=$((after1_total - base_total))
grew2=$((after2_total - after1_total))
grew1_xorbs=$((after1_xorbs - base_xorbs))
grew2_xorbs=$((after2_xorbs - after1_xorbs))

printf '\n%s────────────────────────────────────────────────────────────%s\n' "$CYAN" "$RESET_C"
printf '%s  What the server actually stored%s\n' "$CYAN" "$RESET_C"
printf '%s────────────────────────────────────────────────────────────%s\n' "$CYAN" "$RESET_C"
printf '  push 1: %-12s of model  ->  %-12s of data (%s in xorbs)  in %s s\n' \
  "$(human "${v1_size}")" "$(human "${grew1}")" "$(human "${grew1_xorbs}")" \
  "$(awk -v ms="${push1_ms}" 'BEGIN { printf "%.1f", ms / 1000 }')"
printf '  push 2: %-12s of model  ->  %-12s of data (%s in xorbs)  in %s s\n' \
  "$(human "${v2_size}")" "$(human "${grew2}")" "$(human "${grew2_xorbs}")" \
  "$(awk -v ms="${push2_ms}" 'BEGIN { printf "%.1f", ms / 1000 }')"

if [ "${v1_size}" -gt 0 ] && [ "${grew1}" -gt 0 ]; then
  printf '  dedup on the first push (the file repeats its own blocks): %s%%\n' \
    "$(awk -v s="${v1_size}" -v g="${grew1}" 'BEGIN { printf "%.1f", 100 * (1 - g / s) }')"
fi
if [ "${v2_size}" -gt 0 ]; then
  printf '  dedup on the second push (~2%% of the file changed)      : %s%%\n' \
    "$(awk -v s="${v2_size}" -v g="${grew2}" 'BEGIN { printf "%.1f", 100 * (1 - g / s) }')"
fi

# The headline claim, checked: pushing a 48 MiB file whose content is 98%
# unchanged must not cost another 48 MiB of storage. v2_size is guaranteed
# non-empty above, so this contract check ALWAYS runs -- it is never skipped.
if [ "${v2_size}" -ge 0 ]; then
  budget="$(awk -v s="${v2_size}" -v r="${MAX_GROWTH_RATIO}" 'BEGIN { printf "%d", s * r }')"
  if [ "${grew2}" -le "${budget}" ]; then
    printf '\n  %sPASS%s second push grew the store by %s, under the %s budget\n' \
      "$GREEN" "$RESET_C" "$(human "${grew2}")" "$(human "${budget}")"
  else
    printf '\n  %sFAIL%s second push grew the store by %s, over the %s budget\n' \
      "$RED" "$RESET_C" "$(human "${grew2}")" "$(human "${budget}")"
    status=1
  fi
fi

# ---------------------------------------------------------------------------
# Curtain call
# ---------------------------------------------------------------------------

if [ "${status}" -eq 0 ]; then
  printf '\n%s  DEMO PASSED%s\n' "$GREEN" "$RESET_C"
else
  printf '\n%s  DEMO FAILED%s\n' "$RED" "$RESET_C"
fi

if [ "${TEARDOWN}" = "1" ]; then
  note "TEARDOWN=1 — removing the stack and its volumes"
  compose down --volumes --remove-orphans
elif [ "${XETCAS_DEMO_HINTS:-}" = "candace" ]; then
  # Driven through `candace xetcas demo` inside the candace-server monorepo,
  # where every one of these has a CLI primitive.
  cat <<EOF

The stack is still running, so you can play with it:

  # a shell on the client, with git, git-lfs and git-xet already set up
  candace xetcas shell

  # the server's API and how much deduplicated content it holds
  candace xetcas health
  candace xetcas usage

  # server logs
  candace xetcas logs

  # when you are done
  candace xetcas down
EOF
else
  cat <<EOF

The stack is still running, so you can play with it:

  # a shell on the client, with git, git-lfs and git-xet already set up
  docker compose -f docker/compose.demo.yaml exec workbench bash

  # the server's API and data directory
  curl -s http://127.0.0.1:8080/health
  docker compose -f docker/compose.demo.yaml exec xetcasd du -sh /data
  docker compose -f docker/compose.demo.yaml exec xetcasd du -sh /data/xorbs

  # server logs
  docker compose -f docker/compose.demo.yaml logs -f xetcasd

  # when you are done
  docker compose -f docker/compose.demo.yaml down --volumes
EOF
fi

exit "${status}"
