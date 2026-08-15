# Xet-Core Storage & Binary Format Dossier — hashing, chunking, xorb, and MDB shard formats

**Source**: huggingface/xet-core @ 77fc84d3d (2026-08-11), cloned at `/tmp/claude-1000/-home-bertha-candace-server--claude-worktrees-xet-cas-storage-transfer-07271f/046ce30e-cd86-469f-9c94-1dbeda51d318/scratchpad/xet-core`. All paths below are relative to that root. All multi-byte integers on disk/wire are **little-endian** (`xet_core_structures/src/utils/serialization_utils.rs:10-79` — every writer uses `to_le_bytes`, every reader `from_le_bytes`; hashes are written as raw 32 bytes).

---

## 1. Hashes

### 1.1 `MerkleHash` / `DataHash` — representation and hex convention

- `DataHash` is `pub struct DataHash([u64; 4])` — a transparent 256-bit value (`xet_core_structures/src/merklehash/data_hash.rs:39-40`). `MerkleHash` is a type alias: `pub type MerkleHash = DataHash` (`src/merklehash/mod.rs`, `pub type MerkleHash = DataHash;`). `HMACKey = DataHash` too (`data_hash.rs:227`).
- Conversion `[u8;32] ↔ [u64;4]` is a raw transmute (no byte reordering) (`data_hash.rs:63-79`). On-disk/wire serialization is always the raw 32 memory bytes (`serialization_utils.rs:10-12, 53-58`).
- **Hex convention (critical for interop)** — `hex()` prints each of the 4 u64s as 16 zero-padded **lowercase** hex chars, interpreting each u64 in **little-endian** (`data_hash.rs:145-153`):
  ```
  format!("{:016x}{:016x}{:016x}{:016x}", h[0].to_le(), h[1].to_le(), h[2].to_le(), h[3].to_le())
  ```
  Equivalently: take the 32 raw bytes, reverse each 8-byte group, hex-encode. Reference vector (`data_hash.rs:533-547`): raw bytes `[22,175,58,132,4,75,131,214, 190,153,138,66,226,3,153,242, 204,86,80,234,249,153,80,99, 159,80,65,138,236,231,149,78]` ⇒ hex `"d6834b04843aaf16f29903e2428a99be635099f9ea5056cc4e95e7ec8a41509f"`. This is **not** plain byte-order hex.
- `from_hex` requires exactly 64 hex digits (`data_hash.rs:163-179`); parses each 16-char group as a u64. `from_be_bytes` builds from canonical-hex byte order (`data_hash.rs:208-216`).
- `base64()` = base64 **URL_SAFE_NO_PAD** over the raw 32 memory bytes (not hex order) (`data_hash.rs:157-159`, engine imported at line 10).
- `Ord` compares `[u64;4]` as native u64 array (`data_hash.rs:108-112`) — h[0] most significant for ordering (matters for shard BTreeMap-ordered lookup tables, §4.6).
- `hash % m` (used by aggregation cuts and global-dedup eligibility) = `h[3].to_le() % m` — the **fourth** u64, LE-interpreted (`data_hash.rs:120-126`).
- Marker/bookend value = all `0xFF` bytes: `DataHash([!0u64; 4])` (`data_hash.rs:238-240`).

### 1.2 Chunk (leaf) hash

`compute_data_hash(slice)` = **blake3 keyed hash** over the raw (uncompressed) chunk bytes with the fixed 32-byte key `DATA_KEY` (`data_hash.rs:272-275, 294-297`):

```
DATA_KEY = [102,151,245,119,91,149,80,222,49,53,203,172,165,151,24,28,
            157,228,33,16,155,235,43,88,180,208,176,75,147,173,242,41]
```

The 32-byte blake3 output bytes are transmuted directly into `[u64;4]`.

### 1.3 Internal-node hash

`compute_internal_node_hash(slice)` = blake3 keyed hash with key `INTERNAL_NODE_HASH` (`data_hash.rs:278-281, 320-323`):

```
INTERNAL_NODE_HASH = [1,126,197,199,165,71,41,150,253,148,102,102,180,138,2,230,
                      93,221,83,111,55,199,109,210,248,99,82,230,74,83,113,63]
```

### 1.4 HMAC of a hash

`DataHash::hmac(key)` = `blake3::keyed_hash(key_bytes, self.as_bytes())` — blake3 keyed hash where the message is the 32 raw bytes of the hash being protected (`data_hash.rs:232-235`). Used for (a) file-hash salting and (b) shard chunk-hash HMAC protection (§4.7).

### 1.5 Aggregated (merkle) hash — xorb hash and file hash

All in `xet_core_structures/src/merklehash/aggregated_hashes.rs`. Input: ordered list of `(MerkleHash, u64 size)` pairs — for a **xorb**, sizes are the *uncompressed* chunk lengths (`raw_xorb_data.rs:40-41`); for a **file**, the chunk sizes of the file's chunk sequence.

- Mean branching factor: `AGGREGATED_HASHES_MEAN_TREE_BRANCHING_FACTOR = 4` (`aggregated_hashes.rs:3`). Group sizes: min 2, max `2*4+1 = 9` (`:6-9`).
- **Cut rule** `next_merge_cut(hashes)` (`:37-53`): if `len <= 2` take all; otherwise scan i = 2..min(9, len): the group ends *after* the first element whose `hash % 4 == 0` (i.e., returns `i+1`); if none found, group size = min(9, len).
- **Group merge** `merged_hash_of_sequence` (`:104-136`): build an ASCII buffer with one line per node, exactly:
  ```
  {64-char lowercase hex of hash}{SPACE}{COLON}{SPACE}{decimal size}{LF}
  ```
  i.e. `"{hex} : {size}\n"` (space-colon-space, `\n` = 0x0A, no leading zeros on the decimal, `0` for zero) — then `compute_internal_node_hash(buffer)`. The merged node's size = sum of child sizes.
- **`aggregated_node_hash(chunks)`** (`:143-169`): repeatedly collapse the list level by level (each level: partition greedily with `next_merge_cut`, replace each group by its merged (hash,size)) until one hash remains. Empty input ⇒ `MerkleHash::default()` (all zero). **A single-element list is returned unchanged** — the while-loop only runs for len > 1, so `xorb_hash([(h, n)]) == h` (confirmed by reference vectors, `aggregated_hashes.rs:311-312`).
- **Xorb hash**: `xorb_hash(chunks) = aggregated_node_hash(chunks)` (`:172-179`).
- **File hash**: `file_hash_with_salt(chunks, salt) = aggregated_node_hash(chunks).hmac(salt)` (`:183-189`); the no-salt `file_hash(chunks)` uses salt `[0u8;32]` (`:192-195`) — **note the final blake3-keyed step is applied even with a zero salt**, so a 1-chunk file's file hash ≠ its chunk hash.
- **Reference vectors** for both xorb hash and salted file hash: `aggregated_hashes.rs:273-882` (`test_correctness`). E.g., 3 chunks `("cfc5d07f…",100),("c3e67584…",200),("0d2beb91…",300)` ⇒ xorb hash `71ec1275fca074724e2dd666921b3277c7cee603e4d025bcab2d4050015be2bc`, zero-salt file hash `54e55dccc6653c612bdb5576c5d3cb34bb31bc4e100248abccf4c908b3ef7715` (`:329-349`). Use these to validate a port.

### 1.6 Chunk-range verification hash

`range_hash_from_chunks(chunks)` = `blake3::keyed_hash(VERIFICATION_KEY, concat(raw 32-byte chunk hashes in order))` (`xet_core_structures/src/metadata_shard/chunk_verification.rs:9-16`), with fixed key (`:4-7`):

```
VERIFICATION_KEY = [127,24,87,214,206,86,237,102,18,127,249,19,231,165,195,243,
                    164,205,38,213,181,219,73,230,65,36,152,127,40,251,148,195]
```

This is the value stored in each shard `FileVerificationEntry.range_hash` for a file segment's chunk range `[chunk_index_start, chunk_index_end)`, and what `XorbObject::generate_chunk_range_hash` computes (`xorb_object_format.rs:1177-1192`). It proves the holder actually knows the chunk hashes of a claimed range (ranges are public; hashes are not). This is the closest thing to "auth" in this layer; a server must compute/verify these when producing/accepting file info with verification.

---

## 2. Chunking (CDC) — client-side, but needed for compat testing

Implementation: `xet_data/src/deduplication/chunking.rs` (crate `xet-data`); constants live canonically in `xet_core_structures/src/xorb_object/constants.rs`.

- **Algorithm**: gearhash rolling hash, crate `gearhash = "0.1"` (locked **0.1.3**, checksum `c8cf82cf76cd16485e56295a1377c775ce708c9f1a0be6b029076d60a245d213` — `Cargo.toml:65`, `Cargo.lock`). `gearhash::Hasher::default()` is used (`chunking.rs:52`), i.e. the crate's built-in 256-entry `DEFAULT_TABLE` of u64s (a from-scratch port must copy that exact table from the gearhash crate). Gear update per byte: `h = (h << 1) + TABLE[byte]`; `next_match(data, mask)` returns `Some(i+1)` at the first byte where `(h & mask) == 0`.
- **Constants** (`xet_core_structures/src/xorb_object/constants.rs:3-29`):
  - `TARGET_CHUNK_SIZE = 64 * 1024` (65536)
  - `MINIMUM_CHUNK_DIVISOR = 8` ⇒ minimum chunk = 8192
  - `MAXIMUM_CHUNK_MULTIPLIER = 2` ⇒ maximum chunk = 131072 = `MAX_CHUNK_SIZE`
- **Mask** (`chunking.rs:41-46`): `mask = (target - 1) as u64` shifted all the way left: `mask <<= mask.leading_zeros()`. For target 65536: `0xFFFF << 48 = 0xFFFF_0000_0000_0000`.
- **Boundary logic** (`chunking.rs:76-139`): hash window = 64 bytes (`:77`). Skip-ahead: while `buffered + 64 < min_chunk`, skip `min_chunk - buffered - 64 - 1` bytes without hashing (`:91-94`). A match before `min_chunk` total bytes is ignored (`:113-115`). At `max_chunk` total bytes a boundary is forced (`:126-129`). After each emitted boundary the hasher state resets to 0 (`:132`). Final flush emits the remainder as a chunk.
- **Compat vectors** (`chunking.rs`): 1,000,000 bytes of SplitMix64(seed=0) data (generator at `:559-565`, constant `0x9E3779B97F4A7C15`, mults `0xBF58476D1CE4E5B9`/`0x94D049BB133111EB`) ⇒ boundaries `[84493, 134421, 144853, 243318, 271793, 336457, 467529, 494581, 582000, 596735, 616815, 653164, 678202, 724510, 815591, 827760, 958832, 991092, 1000000]` (`:649-656`). Constant data `vec![59u8; 1000000]` ⇒ max-size chunks `[131072, 262144, …, 1000000]` (`:671`).
- Parallel-chunking partition finding and "stable boundary" rules (`chunking.rs:289-355`; `constants.rs:57-78`: stable chunk size ∈ `[2*min, max−min)`) are client-side only.

**Server relevance**: the server never chunks; it receives pre-chunked framed data. Chunk sizes it must *accept*: uncompressed length ∈ (0, 131072]; the last chunk of a small file may be arbitrarily small (< 8192).

---

## 3. Xorb serialization (`xet_core_structures/src/xorb_object/`)

### 3.1 What goes over the wire on upload

**The client uploads a xorb as a bare concatenation of chunk frames — no footer.** `SerializedXorbObject::from_xorb(xorb, /*serialize_footer=*/false, …)` with the comment "XORBs are sent without footer - the server/client reconstructs it from chunk data" (`xet_data/src/processing/file_upload_session.rs:386-400`). The server should run the logic of `reconstruct_xorb_with_footer` (`xorb_object_format.rs:1747-1798`): parse frames, decompress, hash each chunk, compute the xorb hash, and (optionally) append the V1 footer for at-rest storage.

Limits: `MAX_XORB_BYTES = 64 * 1024 * 1024` (uncompressed content), `MAX_XORB_CHUNKS = 8 * 1024` = 8192, `XORB_BLOCK_SIZE = 64 MiB` (`constants.rs:16-25`). Client enforces via debug asserts (`raw_xorb_data.rs:24,38`; `xet_data/src/deduplication/data_aggregator.rs:75-76`); a server should hard-enforce both.

### 3.2 Chunk frame (`xorb_chunk_format.rs`)

Every chunk is framed by an 8-byte packed header (`XorbChunkHeader`, `#[repr(C, packed)]`, `xorb_chunk_format.rs:14-21`; `XORB_CHUNK_HEADER_LENGTH = 8`):

| offset | size | field | notes |
|---|---|---|---|
| 0 | 1 | `version` (u8) | `CURRENT_VERSION = 0` (`:12`); reader rejects `version > 0` (`:66-71`) |
| 1 | 3 | `compressed_length` | u24 LE (low 3 bytes of u32, `:99-110`) — payload length that follows |
| 4 | 1 | `compression_scheme` (u8) | see §3.3 |
| 5 | 3 | `uncompressed_length` | u24 LE |

Followed immediately by `compressed_length` payload bytes. Validation (`:64-88`): `compressed_length ≤ 2 * MAX_CHUNK_SIZE` (= 262144), `uncompressed_length ≤ MAX_CHUNK_SIZE` (= 131072, strict). **Footer guard**: a "header" whose first 7 bytes equal `b"XETBLOB"` is rejected as `ChunkHeaderParse` (`:141-148`) — this is how streaming readers detect the start of a footer mid-stream. After decompression the byte count must equal `uncompressed_length` (`:182-186`).

Writer behavior (`serialize_chunk`, `:112-139`): compress with chosen scheme; **if compressed size ≥ raw size, fall back to scheme None with raw bytes** (`:129-133`). `Auto` (99) is never written into a frame (`:118-123`).

### 3.3 Compression schemes (`compression_scheme.rs`)

`#[repr(u8)] enum CompressionScheme` (`:22-30`), on-disk ids:

| id | name | string form | codec |
|---|---|---|---|
| 0 | `None` | `"none"` | raw bytes |
| 1 | `LZ4` | `"lz4"` | **LZ4 frame format** via `lz4_flex::frame::{FrameEncoder,FrameDecoder}` (crate lz4_flex 0.13.0), `:139-154` |
| 2 | `ByteGrouping4LZ4` | `"bg4-lz4"` | BG4 transform then LZ4 frame, `:156-198` |
| 99 | `Auto` | `"auto"` | client-side selector only; never on the wire |

Any other id ⇒ error (`:54-66`). ASCII capital-letter values 65–90 deliberately unused (`:21`).

**BG4 transform** (`byte_grouping/bg4.rs`): split the chunk into 4 planes by byte index mod 4. With `n = len`, `split = n/4`, `rem = n%4`, output = concatenation of groups g0..g3 where `g0 = bytes[0,4,8,…]` (length `split + min(1,rem)`), `g1 = bytes[1,5,…]` (length `split + min(1, rem.saturating_sub(1))`), `g2` (length `split + min(1, rem.saturating_sub(2))`), `g3` (length `split`) (`bg4.rs:3-77`). Decompress: LZ4-frame-decode the whole payload, then `bg4_regroup` = exact inverse interleave (`bg4.rs:120-159, 258-261`). Auto selection uses a KL-divergence heuristic over byte planes (`BG4Predictor`, `compression_scheme.rs:126-137`; per-file-block re-testing every `compression_scheme_retest_interval` chunks, `xorb_object_format.rs:1430-1458`) — server never needs it; it only needs to *decode* ids 0/1/2.

### 3.4 Xorb at-rest object layout (footer) — V1

Physical layout (`xorb_object_format.rs:914-937`):

```
<chunk frame 0> … <chunk frame N-1> <XorbObjectInfoV1> <info_length: u32 LE>
```

`XorbObjectInfoV1` serialized field order (`serialize`, `xorb_object_format.rs:434-511`; struct `:293-374`):

| bytes | field | value |
|---|---|---|
| 7 | `ident` | `b"XETBLOB"` (`XORB_OBJECT_FORMAT_IDENT`, `:21`) |
| 1 | `version` | `1` (`XORB_OBJECT_FORMAT_VERSION`, `:25`) |
| 32 | `xorb_hash` | raw hash bytes |
| 7 | `ident_hash_section` | `b"XBLBHSH"` (`:23`) |
| 1 | `hashes_version` | `0` (`:26`) |
| 4 | `num_chunks` (copy 2) | u32 LE |
| 32×n | `chunk_hashes` | per-chunk `compute_data_hash` values |
| 7 | `ident_boundary_section` | `b"XBLBBND"` (`:24`) |
| 1 | `boundaries_version` | `1` (`:30`; `0` = legacy, no unpacked offsets, `:29`) |
| 4 | `num_chunks` (copy 3) | u32 LE |
| 4×n | `chunk_boundary_offsets` | cumulative **physical** byte offset of the end of each chunk frame (header included); entry n = end of chunk n; first chunk starts at 0 (`:335-346`) |
| 4×n | `unpacked_chunk_offsets` | cumulative **uncompressed** byte offset of end of each chunk (`:348-353`) |
| 4 | `num_chunks` | u32 LE |
| 4 | `hashes_section_offset_from_end` | u32: bytes from end-of-footer back to `"XBLBHSH"` = `(12 + 32n) + boundary_section_offset_from_end` (`:902-906`) |
| 4 | `boundary_section_offset_from_end` | u32 = `44 + 8n` (`:891-900`) |
| 16 | `_nonce_buffer` | first 4 bytes = optional per-upload uniqueness nonce (`XORB_OBJECT_FORMAT_NONCE_LEN = 4`, `:47`), rest zero; **excluded from the xorb hash** (`:367-373`) |

Then the trailing `info_length: u32 LE` (not counted in itself). Footer length = `92 + 40n` bytes (`XORB_OBJECT_INFO_DEFAULT_LENGTH = 92` for n=0, `:32`; `serialized_length`, `:419-429`). Readers locate the footer by reading the last 4 bytes (`get_info_length`, `:953-962`) then seeking back `info_length + 4` (`deserialize`, `:967-981`). Deserializer cross-checks: ident/version match, all three `num_chunks` copies equal, and both `*_offset_from_end` fields consistent with actual byte counts (`:516-634`). A boundaries-only fast path seeks `-(4+16+4)` from the end to read `boundary_section_offset_from_end` (`deserialize_only_boundaries_section`, `:639-705`).

**Legacy V0 footer** (accepted on read; never write): `b"XETBLOB"`, version `0`, xorb_hash(32), num_chunks u32, `chunk_boundary_offsets` u32×n, `chunk_hashes` 32×n, 16-byte buffer; length 60 + 36n (`_XORB_OBJECT_INFO_DEFAULT_LENGTH_V0 = 60`, `:31`; struct `:63-95`, `deserialize_v0` `:179-225`). Note V0 field order differs (offsets before hashes; no unpacked offsets).

### 3.5 Server obligations

**(a) Verify a xorb upload** (mirror `reconstruct_xorb_with_footer` `:1747-1798` / `validate_xorb_object` `:1079-1164`):
1. Iterate frames from offset 0; stop at EOF or at a frame whose first 7 bytes are `XETBLOB`.
2. For each frame: validate header, read `compressed_length` bytes, decompress per scheme, check decompressed len == `uncompressed_length`, compute `compute_data_hash(decompressed)`, accumulate `(hash, uncompressed_len as u64)` plus physical and unpacked cumulative offsets.
3. `xorb_hash(list)` must equal the hash the client addressed the upload with (and the footer's `xorb_hash` if a footer is present). Enforce ≤ 8192 chunks / ≤ 64 MiB uncompressed.

**(b) Serve chunk subranges later**: persist (or reconstruct) `chunk_boundary_offsets` + `unpacked_chunk_offsets`. Chunk range `[s, e)` ⇒ physical byte range `[s==0 ? 0 : offsets[s-1], offsets[e-1])` (`get_byte_offset`, `:1260-1273`); the served bytes are whole frames (header+payload) that the client decompresses (`get_chunk_contents` loops `deserialize_chunk`, `:1247-1257`). Uncompressed length of chunk k = `unpacked[k] - unpacked[k-1]` (`:1277-1289`). Range-hash for verification = §1.6 over `chunk_hashes[s..e]`.

Streaming multi-chunk decode helper semantics (relevant to client behavior on download): `deserialize_chunks_to_writer` returns uncompressed bytes plus `chunk_byte_indices` beginning with 0 (`xorb_chunk_format.rs:236-256`); clean EOF at a frame boundary is success, truncation mid-header/mid-payload is an error (`:223-234`, tests `:406-439`).

---

## 4. MDB shard format (`xet_core_structures/src/metadata_shard/`)

Overall file layout (`shard_format.rs:226-282` doc block, `serialize_from` `:305-374`):

```
[Header 48B][File-info section][Xorb-info section][File lookup][Xorb lookup][Chunk lookup][Footer 200B]
```

All section entries are exactly **48 bytes**: `MDB_FILE_INFO_ENTRY_SIZE = 32 + 4*4 = 48` and `MDB_XORB_INFO_ENTRY_SIZE = 48`, with `const_assert`s pinning every struct to that size (`shard_format.rs:23-31`).

### 4.1 Header (48 bytes; `shard_format.rs:57-99`)

| bytes | field | value |
|---|---|---|
| 32 | `tag` | `MDB_SHARD_HEADER_TAG` = ASCII `"HFRepoMetaData"` + `[0, 85,105,103,69,106,123,129,87,131,165,189,217,92,205,209,74,169]` (`:43-46`) |
| 8 | `version` u64 | `MDB_SHARD_HEADER_VERSION = 2` (`:35`) |
| 8 | `footer_size` u64 | `200` (= `size_of::<MDBShardFileFooter>()`, `:33`) |

Readers reject on tag mismatch (`:88-91`); note version is *not* checked on read here.

### 4.2 File-info section

Starts at `footer.file_info_offset` (= 48). Per file (`file_structs.rs`):

- **`FileDataSequenceHeader`** (48B, serialize order `:82-97`): `file_hash` (32B), `file_flags` u32, `num_entries` u32, `_unused` u64. Flags (`:14-18`): `MDB_FILE_FLAG_WITH_VERIFICATION = 1<<31`, `MDB_FILE_FLAG_WITH_METADATA_EXT = 1<<30`, default 0.
- **`num_entries` × `FileDataSequenceEntry`** (48B, `:174-183`, serialize `:226-242`): `xorb_hash` (32B; serde JSON name `cas_hash`), `xorb_flags` u32 (JSON `cas_flags`), `unpacked_segment_bytes` u32, `chunk_index_start` u32, `chunk_index_end` u32 (end-exclusive chunk indices into the referenced xorb).
- If bit31 set: **`num_entries` × `FileVerificationEntry`** (48B, `:261-301`): `range_hash` (32B; §1.6 over that segment's chunk hashes), 16 zero bytes.
- If bit30 set: **one `FileMetadataExt`** (48B, `:305-345`): `sha256` (32B, the file's SHA-256; stored/rendered with the same LE-per-u64 hex convention), 16 zero bytes.

Section terminates with a **bookend** `FileDataSequenceHeader` whose `file_hash` is all `0xFF` (`:70-80`; written at `shard_format.rs:402-403`). Sequential deserialization: `MDBFileInfo::deserialize` returns `None` at bookend (`file_structs.rs:392-424`).

### 4.3 Xorb-info (CAS-info) section

Starts at `footer.xorb_info_offset`. Per xorb (`xorb_structs.rs`):

- **`XorbChunkSequenceHeader`** (48B, serialize `:57-73`): `xorb_hash` (32B), `xorb_flags` u32 (default 0, `:10`), `num_entries` u32, `num_bytes_in_xorb` u32 (uncompressed content bytes), `num_bytes_on_disk` u32 (compressed size; 0 if unknown).
- **`num_entries` × `XorbChunkSequenceEntry`** (48B; **serialize order** `:139-155`, which differs from constructor arg order): `chunk_hash` (32B), `chunk_byte_range_start` u32 (uncompressed start offset of this chunk within the xorb), `unpacked_segment_bytes` u32 (uncompressed chunk length), `flags` u32 (`MDB_CHUNK_WITH_GLOBAL_DEDUP_FLAG = 1<<31`, `:12`), `_unused` u32.

Terminated by a bookend `XorbChunkSequenceHeader` with all-`0xFF` hash (`:45-55`).

### 4.4 Lookup sections (all sorted ascending by u64 key)

- **File lookup**: `file_lookup_num_entry` records of `(u64 truncated file hash, u32 index)` — 12 bytes each (`shard_format.rs:328-332`). `index` counts 48-byte entries from `file_info_offset` to that file's header (each file consumes `1 + num_entries (+num_entries if verification) (+1 if metadata_ext)` slots).
- **Xorb lookup**: same 12-byte shape; `index` counts 48-byte entries from `xorb_info_offset` (each xorb consumes `1 + num_entries` slots) (`:339-345, 426-443`).
- **Chunk lookup**: `(u64 truncated chunk hash, u32 xorb_entry_index, u32 chunk_offset_within_xorb)` — 16 bytes each (`:350-355`), sorted by key (`:451-453`); used for dedup queries (`chunk_hash_dedup_query*`, `:673-757`, up to 8 truncated-collision probes).

`truncate_hash(h) = h[0]` — the first u64 of the hash, i.e. `u64::from_le_bytes(bytes[0..8])` on LE (`metadata_shard/utils.rs:30-33`). Because `DataHash::Ord` orders by `h[0]` first and file/xorb maps are BTreeMaps, those two lookup tables come out pre-sorted (`shard_format.rs:405, 448`). Lookup uses interpolation search over the fixed-width records (`interpolation_search.rs`, `search_on_sorted_u64s`); >8 truncated collisions on file/xorb lookup is an error (`:493-497, 515-520`).

### 4.5 Footer (200 bytes; `shard_format.rs:102-224`) — at file end

| offset | size | field |
|---|---|---|
| 0 | 8 | `version` u64 = `MDB_SHARD_FOOTER_VERSION = 1` (`:37`; strict check on read `:187-192`) |
| 8 | 8 | `file_info_offset` |
| 16 | 8 | `xorb_info_offset` |
| 24 | 8 | `file_lookup_offset` |
| 32 | 8 | `file_lookup_num_entry` |
| 40 | 8 | `xorb_lookup_offset` |
| 48 | 8 | `xorb_lookup_num_entry` |
| 56 | 8 | `chunk_lookup_offset` |
| 64 | 8 | `chunk_lookup_num_entry` |
| 72 | 32 | `chunk_hash_hmac_key` (`HMACKey`; all-zero = no key) |
| 104 | 8 | `shard_creation_timestamp` (unix secs) |
| 112 | 8 | `shard_key_expiry` (unix secs; default `u64::MAX` = never, `:151`; minimal-shard writer uses `0` for "no expiry", `streaming_shard.rs:417-419`) |
| 120 | 48 | `_buffer` `[u64; 6]` zeros |
| 168 | 8 | `stored_bytes_on_disk` (Σ xorb `num_bytes_on_disk`) |
| 176 | 8 | `materialized_bytes` (Σ file `unpacked_segment_bytes`) |
| 184 | 8 | `stored_bytes` (Σ xorb `num_bytes_in_xorb`) |
| 192 | 8 | `footer_offset` (byte offset where the footer begins) |

Read pattern: header from offset 0, footer from `SeekFrom::End(-200)` (`:291-303`).

### 4.6 Naming, sizes, expiry

- Shard file name = `<64-hex-hash>.mdb` (`utils.rs:12-13, 35-37`); regex `^[0-9a-fA-F]{64}\.mdb$`. The name hash is the shard's own content hash (computed over the file with `HashedWrite`, i.e., blake3-keyed with `DATA_KEY` over the full shard bytes — `data_hash.rs:369-401`; used by client `MDBShardFile`).
- Target/max shard size: `64 MiB` both (`xet_runtime/src/config/groups/shard.rs:10-17`; env `HF_XET_SHARD_TARGET_SIZE`, `HF_XET_SHARD_MAX_TARGET_SIZE`).
- Expiry handling (client reference): shard usable while `now <= shard_key_expiry`; deleted once `shard_key_expiry + MDB_SHARD_EXPIRATION_BUFFER (7 days) <= now` (`shard_file_handle.rs:216-222`; buffer at `metadata_shard/constants.rs:10-12`); local cache shards get 3-week expiry (`:14-16`).
- Global dedup eligibility: `chunk_hash % 1024 == 0` (uses h[3], §1.1) — `MDB_SHARD_GLOBAL_DEDUP_CHUNK_MODULUS = 1024` (`constants.rs:5-8, 20-22`) — or the entry's `1<<31` flag, or being a file's first chunk (`xorb_structs.rs:134-137`; `streaming_shard.rs:459+`).

### 4.7 HMAC-keyed chunk hashes (what a server returns for dedup)

`MDBShardInfo::export_as_keyed_shard[_streaming]` (`shard_format.rs:938-1204`) is the transformation a server applies before handing shards to clients for global dedup:

- Every `XorbChunkSequenceEntry.chunk_hash` is replaced by `chunk_hash.hmac(hmac_key)` = blake3-keyed-hash(key, raw 32 hash bytes) (`:1098-1100`).
- The footer's `chunk_hash_hmac_key` field is set to the key (`:1178`); zero key ⇒ unprotected (`chunk_hashes_protected`, `:465-467`).
- The chunk lookup table is rebuilt from truncated *keyed* hashes and re-sorted (`:1102-1104, 1160-1173`); file info and/or lookup tables may be omitted (flags `include_file_info`, `include_xorb_lookup_table`, `include_chunk_lookup_table`, with omitted-lookup counts 0).
- `shard_creation_timestamp = now`, `shard_key_expiry = now + key_valid_for` (`:1181-1189`).
- Clients query by computing `keyed_chunk_hash = raw_chunk_hash.hmac(footer.chunk_hash_hmac_key)` before lookup (`keyed_chunk_hash`, `:599-608`; used in `get_xorb_info_index_by_chunk` `:523-548`). File hashes and xorb hashes are **never** HMACed — only per-chunk hashes.

`MDBMinimalShard` (`streaming_shard.rs`) is the streaming subset writer used for dedup responses: same header/sections/footer format, file section optional, all three lookup counts 0 and offsets = footer start (`:405-426`), optional expiry, global-dedup flags stamped onto first-chunks-of-files (`:380-402`).

---

## 5. CRCs / checksums

There are **no CRCs anywhere in the on-wire or at-rest xorb/shard formats** — integrity is entirely blake3-keyed-hash based (chunk hashes, xorb hash, shard content hash, verification range hashes) plus the LZ4 frame format's own internal checksums. The only crc32 in the workspace is `crc32fast` in the client's *local* disk chunk cache (`xet_client/src/chunk_cache/disk.rs:341, 401, 488, 562-565`) — never observed by a server.

---

## 6. Crates a third-party Rust server can depend on (workspace v1.6.0)

Published package names (workspace members, root `Cargo.toml:3-13`; per-crate `[package] name`):

| package (crates.io name) | lib name | what it exports (server-relevant) |
|---|---|---|
| **`xet-core-structures`** | `xet_core_structures` | Everything in this dossier: `merklehash::{MerkleHash, DataHash, HMACKey, compute_data_hash, compute_internal_node_hash, xorb_hash, file_hash, file_hash_with_salt, HashedWrite, ChunkHashList}`; `xorb_object::{CompressionScheme, XorbChunkHeader, serialize_chunk, deserialize_chunk(s)(_to_writer), parse_chunk_header, XorbObject, XorbObjectInfoV1/V0, SerializedXorbObject, RawXorbData, Chunk, reconstruct_xorb_with_footer, constants::{TARGET_CHUNK_SIZE, MAX_CHUNK_SIZE, MAX_XORB_BYTES, MAX_XORB_CHUNKS}}`; `metadata_shard::{MDBShardInfo, MDBShardFileHeader, MDBShardFileFooter, file_structs::*, xorb_structs::*, chunk_verification::{VERIFICATION_KEY, range_hash_from_chunks}, streaming_shard::MDBMinimalShard, utils::{truncate_hash, parse_shard_filename, shard_file_name}}` (`src/lib.rs`, module `mod.rs` files) |
| **`xet-runtime`** | `xet_runtime` | required transitively (`test_configurable_constants!` macro backing the constants; config groups incl. shard sizes `src/config/groups/shard.rs`) |
| **`xet-data`** | `xet_data` | `deduplication::{Chunker, find_partitions, next_stable_chunk_boundary}` (`src/deduplication/mod.rs`) — only needed if the server wants to chunk/verify chunking itself |
| **`xet-client`** | `xet_client` | HTTP CAS client; also contains a **reference in-process server** under `src/cas_client/simulation/local_server/` (axum handlers) and `local_client.rs`/`memory_client.rs` upload validation — the best executable spec for server behavior |
| `hf-xet` (dir `xet_pkg`) | — | Python wheel binding; not useful for a server |

Key third-party pins (`Cargo.lock`): `blake3 1.8.3`, `gearhash 0.1.3`, `lz4_flex 0.13.0`, `base64 0.22` (URL_SAFE_NO_PAD).

---

## 7. Cross-cutting numeric summary (the numbers a server must hard-code)

- Chunk: target 65,536 B; min 8,192 B; max 131,072 B; gear mask `0xFFFF_0000_0000_0000`; window 64 B.
- Chunk frame header: 8 B; `version=0`; u24 LE lengths; compressed ≤ 262,144; uncompressed ≤ 131,072; schemes {0,1,2} on wire.
- Xorb: ≤ 8,192 chunks; ≤ 64 MiB uncompressed; uploaded **without** footer; footer idents `XETBLOB`/`XBLBHSH`/`XBLBBND`, versions 1/0/1, footer size `92 + 40·n_chunks` + 4-byte trailing length; 16-byte nonce buffer excluded from hashing.
- Shard: header 48 B (tag above, version 2, footer_size 200); all entries 48 B; bookends all-0xFF hash; footer 200 B version 1; lookups 12/12/16 B records keyed by `h[0]`; ≤ 64 MiB target size; verification key / data key / internal key as listed verbatim in §1; HMAC = blake3 keyed hash of the 32 raw hash bytes; expiry buffer 7 days; global-dedup modulus 1024 (on `h[3]`).