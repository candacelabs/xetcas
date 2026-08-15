#!/usr/bin/env bash
# Build the pinned protobuf toolchain and regenerate or verify the Go side of
# the xetcas contracts (types + Liquid Proto validation boundaries). No host
# Go/protoc installation is used. Rust generation is separate: see
# `just gen-rust` (protox + prost, no protoc required).
set -euo pipefail

proto_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
component_root="$(cd "${proto_dir}/.." && pwd)"
mode="${1:-write}"

case "${mode}" in
  # check-drift is check plus a self-test that the check actually fails on an
  # obsolete committed *.pb.go.
  write | check | check-drift) ;;
  *)
    echo "usage: $0 [write|check|check-drift]" >&2
    exit 2
    ;;
esac

toolchain_image="${XETCAS_PROTO_TOOLCHAIN_IMAGE:-xetcas/proto-codegen:go1.26.5-protoc35.1}"

docker build \
  --platform linux/amd64 \
  --file "${proto_dir}/Dockerfile.codegen" \
  --tag "${toolchain_image}" \
  "${proto_dir}"

docker run --rm \
  --platform linux/amd64 \
  --user "$(id -u):$(id -g)" \
  --volume "${component_root}:/workspace" \
  "${toolchain_image}" \
  ./proto/generate-in-container.sh "${mode}"
