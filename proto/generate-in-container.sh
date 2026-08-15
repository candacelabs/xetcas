#!/usr/bin/env bash
# Runs inside the pinned code-generation image. Use generate.sh from the host.
set -euo pipefail

proto_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
component_root="$(cd "${proto_dir}/.." && pwd)"
mode="${1:-write}"
case "${mode}" in
  write)
    output_root="${component_root}"
    ;;
  check | check-drift)
    output_root="$(mktemp -d /tmp/xetcas-proto-output.XXXXXX)"
    ;;
  *)
    echo "usage: $0 [write|check|check-drift]" >&2
    exit 2
    ;;
esac

test "$(protoc --version)" = "libprotoc 35.1"
test "$(protoc-gen-go --version)" = "protoc-gen-go v1.36.11"

# The Rust codegen path (protox) cannot read /opt/candacelib, so it compiles
# against proto/vendor/liquidproto/v1/refinement.proto instead. Fail loudly if
# that vendored copy has drifted from the pinned candacelib in this image.
vendored_refinement="${proto_dir}/vendor/liquidproto/v1/refinement.proto"
vendor_marker='// ---8<--- upstream verbatim below this line ---8<---'
vendored_body="$(mktemp /tmp/xetcas-vendored-refinement.XXXXXX)"
drift_probe=""
trap 'rm -f "${vendored_body}" ${drift_probe:+"${drift_probe}"}' EXIT
awk -v marker="${vendor_marker}" '
  seen { print }
  $0 == marker { seen = 1 }
' "${vendored_refinement}" >"${vendored_body}"
if [[ ! -s "${vendored_body}" ]]; then
  echo "vendored refinement.proto is missing its '${vendor_marker}' marker" >&2
  exit 1
fi
diff -u \
  --label "candacelib (pinned in image) liquidproto/v1/refinement.proto" \
  --label "proto/vendor/liquidproto/v1/refinement.proto (below marker)" \
  /opt/candacelib/liquidproto/v1/refinement.proto "${vendored_body}"

proto_files=(
  xetcas/v1/transfer.proto
  xetcas/v1/storage.proto
  xetcas/v1/bridge.proto
)

protoc \
  -I "${component_root}/proto" \
  -I /opt/candacelib \
  -I /usr/local/include \
  "--go_out=module=github.com/candacelabs/xetcas:${output_root}" \
  "--liquidproto_out=module=github.com/candacelabs/xetcas:${output_root}" \
  "${proto_files[@]}"

# List every generated Go file under a tree, relative to it and sorted.
generated_set() {
  (cd "$1" && find go -name '*.pb.go' -type f | LC_ALL=C sort)
}

# Compare the committed Go bindings against a freshly generated tree.
#
# Both the FILE SET and the contents are compared. Comparing only the contents
# of files the fresh run happens to produce would miss the case a check exists
# for: a .proto removed or renamed so generation stops emitting one of the
# committed *.pb.go files. That obsolete file is never regenerated, so it is
# never examined, and the exported Go module keeps publishing stale messages and
# validators while drift reports clean.
#
# Every failure is returned explicitly rather than left to `set -e`: errexit is
# suppressed inside a function whose status is being tested, which is exactly
# how check-drift calls this.
run_check() {
  local fresh committed rel
  fresh="$(generated_set "${output_root}")"
  committed="$(generated_set "${component_root}")"
  if [[ -z "${fresh}" ]]; then
    echo "code generation produced no *.pb.go files" >&2
    return 1
  fi
  if ! diff -u \
    --label "committed go/**/*.pb.go" \
    --label "generated go/**/*.pb.go" \
    <(printf '%s\n' "${committed}") <(printf '%s\n' "${fresh}"); then
    return 1
  fi
  while IFS= read -r rel; do
    if ! diff -u "${component_root}/${rel}" "${output_root}/${rel}"; then
      return 1
    fi
  done <<<"${fresh}"
  return 0
}

case "${mode}" in
  check)
    run_check
    ;;
  check-drift)
    # A drift check that cannot fail is worse than none, so prove this one
    # notices the exact case it was extended for: an obsolete committed file
    # that generation no longer produces. The probe is planted in the working
    # tree and removed again (also on failure, via the EXIT trap).
    run_check
    drift_probe="${component_root}/go/xetcasv1/zz_drift_probe.pb.go"
    printf 'package xetcasv1\n' >"${drift_probe}"
    if run_check >/dev/null 2>&1; then
      rm -f "${drift_probe}"
      drift_probe=""
      echo "drift check did not notice an obsolete committed *.pb.go" >&2
      exit 1
    fi
    rm -f "${drift_probe}"
    drift_probe=""
    echo "codegen drift check rejects an obsolete committed *.pb.go, as intended"
    ;;
esac
