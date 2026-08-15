# xetcas component tasks.
#
# The Rust side runs on the host toolchain. Everything Go or protoc runs inside
# the pinned code-generation image (proto/Dockerfile.codegen): there is no host
# Go and no host protoc, by design.

set shell := ["bash", "-euo", "pipefail", "-c"]

component_root := justfile_directory()
codegen_image := env_var_or_default("XETCAS_PROTO_TOOLCHAIN_IMAGE", "xetcas/proto-codegen:go1.26.5-protoc35.1")

_default:
    @just --list

# Regenerate both language bindings from proto/xetcas/v1/*.proto.
gen: gen-rust gen-go

# Regenerate the committed Rust types (protox + prost, no protoc needed).
gen-rust:
    cd "{{ component_root }}" && cargo xtask gen-proto

# Regenerate the committed Go types and Liquid Proto validators.
gen-go:
    bash "{{ component_root }}/proto/generate.sh" write

# Fail if either committed codegen output has drifted from the schemas.
gen-check: gen-check-rust gen-check-go gen-check-go-drift

gen-check-rust:
    cd "{{ component_root }}" && cargo xtask gen-proto --check

# Also diffs proto/vendor/liquidproto against the pinned candacelib in the image.
gen-check-go:
    bash "{{ component_root }}/proto/generate.sh" check

# Prove the Go drift check is not vacuous: plants an obsolete committed
# *.pb.go, requires check mode to reject it, and removes it again.
gen-check-go-drift:
    bash "{{ component_root }}/proto/generate.sh" check-drift

build:
    cd "{{ component_root }}" && cargo build --all-targets

test: test-rust test-go
    @echo "xetcas: rust + go tests passed"

test-rust:
    cd "{{ component_root }}" && cargo test

# Go tests run in the pinned image. GOPATH/GOCACHE/HOME point at writable
# paths because the container runs as the invoking (non-root) user, and the
# image's own /go tree is root-owned (module cache and sumdb included).
test-go: _codegen-image
    docker run --rm \
      --platform linux/amd64 \
      --user "$(id -u):$(id -g)" \
      --volume "{{ component_root }}:/workspace" \
      --workdir /workspace/go \
      --env HOME=/tmp \
      --env GOPATH=/tmp/gopath \
      --env GOCACHE=/tmp/gocache \
      "{{ codegen_image }}" \
      go test ./...

fmt:
    cd "{{ component_root }}" && cargo fmt --all

clippy:
    cd "{{ component_root }}" && cargo clippy --all-targets -- -D warnings

lint:
    cd "{{ component_root }}" && cargo fmt --all --check
    cd "{{ component_root }}" && cargo clippy --all-targets -- -D warnings

# Arbitrary Go command inside the pinned image, e.g. `just go-run "go mod tidy"`.
go-run command: _codegen-image
    docker run --rm \
      --platform linux/amd64 \
      --user "$(id -u):$(id -g)" \
      --volume "{{ component_root }}:/workspace" \
      --workdir /workspace/go \
      --env HOME=/tmp \
      --env GOFLAGS=-mod=mod \
      --env GOPATH=/tmp/gopath \
      --env GOCACHE=/tmp/gocache \
      "{{ codegen_image }}" \
      bash -c "{{ command }}"

_codegen-image:
    docker build \
      --platform linux/amd64 \
      --file "{{ component_root }}/proto/Dockerfile.codegen" \
      --tag "{{ codegen_image }}" \
      "{{ component_root }}/proto"

# --- xetcasd (the server binary) ---

# Run the server against a local data directory.
serve:
    cd "{{ component_root }}" && cargo run -p xetcasd

# Build the server container image.
docker-server:
    cd "{{ component_root }}" && docker build -f docker/Dockerfile.server -t xetcasd .

# Server tests only (unit + real-client integration).
test-server:
    cd "{{ component_root }}" && cargo test -p xetcasd
