//! Global-dedup response bodies.
//!
//! A hit on the chunk dedup route must return a complete, parseable MDB shard
//! whose cas-info section names the xorb holding the chunk; the client feeds
//! the bytes straight into its shard cache
//! (docs/research/api-surface.md section 1.4).
//!
//! The shard is synthesized from our own stored xorb record rather than by
//! replaying the client's uploaded shard bytes, and its chunk hashes are HMAC
//! keyed with a fresh per-response key. Keying is what production HF CAS does;
//! the client reads the key out of the footer and applies it transparently, so
//! a keyed shard is a drop-in for an unkeyed one
//! (docs/research/binary-formats.md section 4.7).

use std::io::Cursor;
use std::time::Duration;

use xet_core_structures::merklehash::{HMACKey, MerkleHash};
use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
use xet_core_structures::metadata_shard::xorb_structs::{
    MDBXorbInfo, XorbChunkSequenceEntry, XorbChunkSequenceHeader,
};
use xet_core_structures::metadata_shard::MDBShardInfo;
use xetcas_contracts::constants::HASH_BYTES;
use xetcas_contracts::v1::XorbRecord;

/// How long a minted dedup key stays valid. The client honours the footer
/// expiry and stops using the shard once it lapses.
pub const DEDUP_KEY_VALIDITY: Duration = Duration::from_secs(24 * 60 * 60);

/// Decode one raw chunk hash out of a record packed hash blob.
pub fn chunk_hash_at(record: &XorbRecord, index: usize) -> Result<MerkleHash, String> {
    let start = index * HASH_BYTES;
    let end = start + HASH_BYTES;
    if record.chunk_hashes.len() < end {
        return Err(format!("chunk hash {index} missing from record"));
    }
    let bytes = &record.chunk_hashes[start..end];
    MerkleHash::from_slice(bytes).map_err(|e| format!("bad chunk hash {index}: {e}"))
}

/// Build the keyed dedup shard describing one stored xorb.
///
/// `disk_bytes` is the physical size of the stored object, reported to the
/// client as the xorb on-disk accounting.
pub fn build_keyed_dedup_shard(record: &XorbRecord, disk_bytes: u64) -> Result<Vec<u8>, String> {
    let xorb_hash =
        MerkleHash::from_hex(&record.xorb_hash).map_err(|e| format!("bad xorb hash: {e}"))?;

    let num_chunks = record.num_chunks as usize;
    let mut chunks = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let chunk_hash = chunk_hash_at(record, i)?;
        let cumulative_end = *record
            .unpacked_chunk_offsets
            .get(i)
            .ok_or_else(|| format!("missing unpacked offset {i}"))?;
        let cumulative_start = if i == 0 {
            0
        } else {
            record.unpacked_chunk_offsets[i - 1]
        };
        // Shard-side offsets are UNPACKED, unlike the xorb footer boundary
        // offsets, and the constructor takes the length before the offset
        // (docs/research/binary-formats.md section 4.3).
        chunks.push(XorbChunkSequenceEntry::new(
            chunk_hash,
            cumulative_end - cumulative_start,
            cumulative_start,
        ));
    }

    let info = MDBXorbInfo {
        metadata: XorbChunkSequenceHeader {
            xorb_hash,
            xorb_flags: 0,
            num_entries: record.num_chunks,
            num_bytes_in_xorb: record.unpacked_length as u32,
            num_bytes_on_disk: disk_bytes as u32,
        },
        chunks,
    };

    let mut shard = MDBInMemoryShard::default();
    shard
        .add_xorb_block(info)
        .map_err(|e| format!("dedup shard build: {e}"))?;
    let plain = shard
        .to_bytes()
        .map_err(|e| format!("dedup shard serialize: {e}"))?;

    let mut keyed = Vec::with_capacity(plain.len() + 256);
    MDBShardInfo::export_as_keyed_shard_streaming(
        &mut Cursor::new(&plain),
        &mut keyed,
        random_hmac_key(),
        DEDUP_KEY_VALIDITY,
        // The client only needs the xorb section to dedup against; the file
        // section is dropped, matching what the reference server returns.
        false,
        true,
        true,
    )
    .map_err(|e| format!("dedup shard keying: {e}"))?;

    Ok(keyed)
}

fn random_hmac_key() -> HMACKey {
    use rand::RngCore;
    let mut bytes = [0u8; HASH_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    MerkleHash::from(bytes)
}
