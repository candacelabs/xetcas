FROM rust:1.91-bookworm

LABEL dis.ports="8080"
LABEL dis.shell="/bin/bash"

RUN apt-get update && apt-get install -y --no-install-recommends \
        git just sqlite3 \
    && rm -rf /var/lib/apt/lists/*

RUN rustup component add rustfmt clippy

WORKDIR /workspace
