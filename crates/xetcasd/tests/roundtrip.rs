//! End-to-end tests driving the REAL xet-core client against xetcasd.

mod common;

use common::{download_bytes, download_range, pseudo_random_bytes, upload_all, TestServer};
use tempfile::TempDir;

/// Roughly 6 MiB, comfortably many chunks at the 64 KiB target chunk size.
const BIG_LEN: usize = 6 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread")]
async fn real_client_roundtrips_files_of_varied_sizes() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();

    let big = pseudo_random_bytes(1, BIG_LEN);
    let small = pseudo_random_bytes(2, 100 * 1024);
    let tiny = vec![0x42u8];

    let files = vec![
        ("big.bin", big.clone()),
        ("small.bin", small.clone()),
        ("tiny.bin", tiny.clone()),
    ];
    let infos = upload_all(&server.base_url, cache.path(), &files).await;
    assert_eq!(infos.len(), 3);

    for (info, (name, expected)) in infos.iter().zip(files.iter()) {
        assert_eq!(info.file_size(), Some(expected.len() as u64), "{name} size");
        let dl_cache = TempDir::new().unwrap();
        let got = download_bytes(&server.base_url, dl_cache.path(), info).await;
        assert_eq!(got.len(), expected.len(), "{name} downloaded length");
        assert!(got == *expected, "{name} bytes differ");
    }

    let (xorbs, stored_files) = server.counts().await;
    assert!(xorbs >= 1, "expected stored xorbs, got {xorbs}");
    assert_eq!(stored_files, 3, "expected three registered files");
}

#[tokio::test(flavor = "multi_thread")]
async fn re_uploading_the_same_content_stores_nothing_new() {
    let server = TestServer::start().await;
    let data = pseudo_random_bytes(7, BIG_LEN);

    let first = TempDir::new().unwrap();
    upload_all(&server.base_url, first.path(), &[("f.bin", data.clone())]).await;
    let (xorbs_before, files_before) = server.counts().await;
    let bytes_before = server.stored_bytes().await;

    // A brand new cache means the client has no local dedup state and will
    // re-offer the same xorbs; the server must absorb them idempotently.
    let second = TempDir::new().unwrap();
    upload_all(&server.base_url, second.path(), &[("f.bin", data.clone())]).await;
    let (xorbs_after, files_after) = server.counts().await;
    let bytes_after = server.stored_bytes().await;

    assert_eq!(xorbs_before, xorbs_after, "re-upload created new xorbs");
    assert_eq!(files_before, files_after, "re-upload created new files");
    assert_eq!(bytes_before, bytes_after, "re-upload grew the store");
}

/// A mutated copy of a stored file must reuse most of its chunks, and with a
/// fresh client cache the ONLY way to discover them is the server global-dedup
/// route -- so this exercises the chunk dedup endpoint end to end.
///
/// The eligibility modulus makes a random 6 MiB file unlikely to contain any
/// hash-eligible chunk, but the client always probes a file very first chunk.
/// Both versions share that chunk, and the shard answering the probe describes
/// the whole owning xorb, so one probe recovers every shared chunk.
#[tokio::test(flavor = "multi_thread")]
async fn mutating_a_file_reuses_chunks_through_global_dedup() {
    let server = TestServer::start().await;

    let v1 = pseudo_random_bytes(11, BIG_LEN);
    let first = TempDir::new().unwrap();
    upload_all(&server.base_url, first.path(), &[("v1.bin", v1.clone())]).await;
    let bytes_after_v1 = server.stored_bytes().await;

    // Flip a few KiB in the middle, then append a fresh tail.
    let mut v2 = v1.clone();
    let middle = v2.len() / 2;
    for byte in v2[middle..middle + 4096].iter_mut() {
        *byte ^= 0xFF;
    }
    v2.extend_from_slice(&pseudo_random_bytes(12, 64 * 1024));

    let second = TempDir::new().unwrap();
    let infos = upload_all(&server.base_url, second.path(), &[("v2.bin", v2.clone())]).await;
    let bytes_after_v2 = server.stored_bytes().await;

    let new_bytes = bytes_after_v2 - bytes_after_v1;
    let budget = (v2.len() as u64 * 30) / 100;
    assert!(
        new_bytes < budget,
        "second upload stored {new_bytes} new bytes for a {} byte file; expected under {budget}",
        v2.len()
    );

    let dl = TempDir::new().unwrap();
    let got = download_bytes(&server.base_url, dl.path(), &infos[0]).await;
    assert!(got == v2, "mutated file did not round-trip");
}

/// Ranged reads must match plain slices of the original, including ranges that
/// start and end inside a chunk (the server returns chunk-aligned supersets and
/// the client trims using offset_into_first_range).
#[tokio::test(flavor = "multi_thread")]
async fn ranged_downloads_match_the_original_slices() {
    let server = TestServer::start().await;
    let data = pseudo_random_bytes(21, BIG_LEN);
    let cache = TempDir::new().unwrap();
    let infos = upload_all(&server.base_url, cache.path(), &[("r.bin", data.clone())]).await;
    let info = &infos[0];
    let total = data.len() as u64;

    for (start, end) in [
        (100u64, 5_000u64),
        (70_000, 200_000),
        (1_048_576, 1_048_577),
        (total - 10, total),
    ] {
        let dl = TempDir::new().unwrap();
        let got = download_range(&server.base_url, dl.path(), info, start..end)
            .await
            .unwrap_or_else(|e| panic!("range {start}..{end} failed: {e}"));
        let expected = &data[start as usize..end as usize];
        assert_eq!(got.len(), expected.len(), "range {start}..{end} length");
        assert!(got == expected, "range {start}..{end} bytes differ");
    }

    // Open-ended range runs to EOF.
    let dl = TempDir::new().unwrap();
    let got = download_range(&server.base_url, dl.path(), info, 1_000u64..)
        .await
        .expect("open ended range");
    assert!(got == data[1_000..], "open ended range bytes differ");

    // A range starting at EOF is NOT exercised through the client here: xet-data
    // 1.6.0 sets the item size to the requested length and then finalizes it at
    // zero, tripping a debug_assert in its own progress tracker
    // (progress_tracking/progress_types.rs update_item_size). The server-side
    // contract for that case -- 416 -- is asserted directly over HTTP in
    // tests/protocol.rs instead.
}

#[tokio::test(flavor = "multi_thread")]
async fn real_client_roundtrips_a_zero_byte_file_alongside_others() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let big = pseudo_random_bytes(101, BIG_LEN);
    let files = vec![("empty.txt", Vec::new()), ("big.bin", big.clone())];
    let infos = upload_all(&server.base_url, cache.path(), &files).await;
    assert_eq!(infos.len(), 2);
    for (info, (name, expected)) in infos.iter().zip(files.iter()) {
        assert_eq!(info.file_size(), Some(expected.len() as u64), "{name} size");
        let dl = TempDir::new().unwrap();
        let got = download_bytes(&server.base_url, dl.path(), info).await;
        assert!(got == *expected, "{name} bytes differ");
    }
    let (_x, files_n) = server.counts().await;
    assert_eq!(files_n, 2, "both the empty and the big file registered");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_client_roundtrips_an_all_empty_upload() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let files = vec![("a", Vec::new()), ("b", Vec::new())];
    let infos = upload_all(&server.base_url, cache.path(), &files).await;
    assert_eq!(infos.len(), 2);
    for (info, (name, _)) in infos.iter().zip(files.iter()) {
        assert_eq!(info.file_size(), Some(0), "{name} size");
        let dl = TempDir::new().unwrap();
        let got = download_bytes(&server.base_url, dl.path(), info).await;
        assert!(got.is_empty(), "{name} should download empty");
    }
    let (xorbs, files_n) = server.counts().await;
    assert_eq!(xorbs, 0, "an all-empty upload stores no xorbs");
    // Both empty files share the same content hash, so they dedupe into a
    // single FileRecord -- the point is that finalize accepted them at all.
    assert_eq!(files_n, 1, "the empty content registered exactly once");
}
