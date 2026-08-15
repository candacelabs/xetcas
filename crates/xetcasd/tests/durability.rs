//! Durability: acknowledged state survives a fresh reopen of the same data dir.

use tempfile::TempDir;
use xetcas_contracts::v1::XorbRecord;
use xetcasd::index::Index;
use xetcasd::xorbstore::XorbStore;

#[tokio::test(flavor = "multi_thread")]
async fn stored_xorb_and_record_survive_a_reopen() {
    let dir = TempDir::new().unwrap();
    let xorb_dir = dir.path().join("xorbs");
    let staging = dir.path().join("staging");
    let index_path = dir.path().join("index.sqlite");
    let hash = "a".repeat(64);
    let bytes = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    {
        let store = XorbStore::new(xorb_dir.clone(), staging.clone());
        store.write_atomic(&hash, bytes.clone()).await.unwrap();
        let index = Index::open(&index_path).await.unwrap();
        let record = XorbRecord {
            xorb_hash: hash.clone(),
            num_chunks: 1,
            frames_length: bytes.len() as u64,
            unpacked_length: bytes.len() as u64,
            chunk_boundary_offsets: vec![bytes.len() as u32],
            unpacked_chunk_offsets: vec![bytes.len() as u32],
            chunk_hashes: vec![0u8; 32],
            created_at: 0,
        };
        index
            .put_xorb(record, vec![], bytes.len() as u64)
            .await
            .unwrap();
    }
    // Reopen from disk: both the blob and its record must still be present.
    let store = XorbStore::new(xorb_dir, staging);
    let got = store.read_range(&hash, 0, bytes.len()).await.unwrap();
    assert_eq!(got, bytes, "stored blob did not survive reopen");
    let index = Index::open(&index_path).await.unwrap();
    let record = index
        .get_xorb(&hash)
        .await
        .unwrap()
        .expect("record survived");
    assert_eq!(record.frames_length, bytes.len() as u64);
}
