#!/usr/bin/env python3
"""Deterministic synthetic "model weights" for the xetcas demo.

The demo needs a file that is (a) big enough for chunking to be interesting,
(b) byte-for-byte reproducible so two runs of the demo produce identical
numbers, and (c) *internally repetitive*, so content-defined chunking has real
duplicate chunks to collapse.

Layout produced by `create`:

    block 0 .. block U-1     U distinct pseudorandom blocks (the "unique" part)
    block U .. block N-1     each one is a verbatim copy of an earlier block

With the defaults (48 blocks of 1 MiB, 32 unique) a 48 MiB file carries only
32 MiB of distinct content, so a server that deduplicates should store roughly
two thirds of the nominal size — before any compression.

`mutate` makes the small, realistic edit a second training run would: it
rewrites one region in place and appends a little new data, touching about 2%
of the file. Everything outside that region is untouched, so the second push
should transfer only the chunks around the edit.

Both subcommands are seeded; nothing here reads the system RNG or the clock.
"""

from __future__ import annotations

import argparse
import hashlib
import random
import sys

KIB = 1024
MIB = 1024 * KIB


def _human(n: int) -> str:
    if n >= MIB:
        return f"{n / MIB:.2f} MiB"
    if n >= KIB:
        return f"{n / KIB:.2f} KiB"
    return f"{n} B"


def _sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(4 * MIB), b""):
            digest.update(block)
    return digest.hexdigest()


def create(args: argparse.Namespace) -> int:
    block_size = args.block_kib * KIB
    total_blocks = (args.size_mib * MIB) // block_size
    if total_blocks < 1:
        print("size-mib is smaller than one block", file=sys.stderr)
        return 2
    unique_blocks = min(args.unique_blocks, total_blocks)

    rng = random.Random(args.seed)
    pool = [rng.randbytes(block_size) for _ in range(unique_blocks)]

    # Blocks 0..U-1 are the pool in order; the tail reuses pool entries on a
    # fixed stride that is coprime with the pool size, so the copies are spread
    # through the file instead of forming one contiguous mirror image.
    stride = 7
    with open(args.path, "wb") as handle:
        for index in range(total_blocks):
            if index < unique_blocks:
                handle.write(pool[index])
            else:
                handle.write(pool[(index * stride) % unique_blocks])

    total_bytes = total_blocks * block_size
    unique_bytes = unique_blocks * block_size
    print(f"  file            : {args.path}")
    print(f"  size            : {_human(total_bytes)} ({total_bytes} bytes)")
    print(f"  block size      : {_human(block_size)}")
    print(f"  distinct content: {_human(unique_bytes)}"
          f" ({100.0 * unique_bytes / total_bytes:.1f}% of the file)")
    print(f"  repeated content: {_human(total_bytes - unique_bytes)}"
          f" ({100.0 * (total_bytes - unique_bytes) / total_bytes:.1f}% of the file)")
    print(f"  sha256          : {_sha256(args.path)}")
    return 0


def mutate(args: argparse.Namespace) -> int:
    overwrite = args.overwrite_kib * KIB
    append = args.append_kib * KIB

    rng = random.Random(args.seed)
    patch = rng.randbytes(overwrite)
    tail = rng.randbytes(append)

    with open(args.path, "r+b") as handle:
        handle.seek(0, 2)
        original_size = handle.tell()
        if overwrite > original_size:
            print("overwrite region is larger than the file", file=sys.stderr)
            return 2
        # A deliberately unaligned offset roughly a third of the way in: the
        # edit must not land on a tidy block boundary, or the demo would be
        # flattering itself.
        offset = min((original_size // 3) + 12_345, original_size - overwrite)
        handle.seek(offset)
        handle.write(patch)
        handle.seek(0, 2)
        handle.write(tail)
        new_size = handle.tell()

    touched = overwrite + append
    print(f"  file            : {args.path}")
    print(f"  rewrote         : {_human(overwrite)} in place at offset {offset}")
    print(f"  appended        : {_human(append)} at the end")
    print(f"  changed         : {_human(touched)}"
          f" ({100.0 * touched / original_size:.2f}% of the previous version)")
    print(f"  size            : {_human(original_size)} -> {_human(new_size)}")
    print(f"  sha256          : {_sha256(args.path)}")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    create_parser = sub.add_parser("create", help="write a fresh synthetic model file")
    create_parser.add_argument("--path", required=True)
    create_parser.add_argument("--size-mib", type=int, default=48)
    create_parser.add_argument("--block-kib", type=int, default=1024)
    create_parser.add_argument("--unique-blocks", type=int, default=32)
    create_parser.add_argument("--seed", type=int, default=1337)
    create_parser.set_defaults(func=create)

    mutate_parser = sub.add_parser("mutate", help="edit ~2% of an existing model file")
    mutate_parser.add_argument("--path", required=True)
    mutate_parser.add_argument("--overwrite-kib", type=int, default=512)
    mutate_parser.add_argument("--append-kib", type=int, default=512)
    mutate_parser.add_argument("--seed", type=int, default=4242)
    mutate_parser.set_defaults(func=mutate)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
