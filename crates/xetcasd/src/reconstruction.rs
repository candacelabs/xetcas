//! Reconstruction range computation.
//!
//! This is a faithful port of xet-core's `compute_reconstruction_ranges`
//! (`xet_client/src/cas_client/simulation/xorb_utils.rs`), the algorithm both
//! in-repo reference servers share. The output shape is specified in
//! docs/research/api-surface.md section 1.1.
//!
//! Two conventions collide here and must not be mixed up: chunk ranges are
//! end-EXCLUSIVE chunk indexes, while the `url_range` handed to the client is
//! an end-INCLUSIVE byte range. This module works exclusively in half-open
//! ranges; the conversion to inclusive happens once, at the HTTP boundary.

use std::collections::{BTreeMap, HashMap};

use xetcas_contracts::v1::{FileRecord, XorbRecord};

/// Per-xorb chunk geometry, derived from a stored [`XorbRecord`].
#[derive(Debug, Clone)]
pub struct XorbMeta {
    /// Cumulative physical end offset of each chunk frame, header included.
    /// There is no leading zero: entry `k` is the end of chunk `k`.
    boundary: Vec<u32>,
    /// Cumulative uncompressed end offset of each chunk, same convention.
    unpacked: Vec<u32>,
}

impl XorbMeta {
    /// Build from a stored record.
    pub fn from_record(record: &XorbRecord) -> Self {
        Self {
            boundary: record.chunk_boundary_offsets.clone(),
            unpacked: record.unpacked_chunk_offsets.clone(),
        }
    }

    /// Number of chunk frames.
    pub fn num_chunks(&self) -> u32 {
        self.unpacked.len() as u32
    }

    /// Uncompressed length of one chunk.
    pub fn chunk_len(&self, index: u32) -> Result<u32, String> {
        let i = index as usize;
        let end = *self
            .unpacked
            .get(i)
            .ok_or_else(|| format!("chunk index {index} out of range"))?;
        let start = if i == 0 { 0 } else { self.unpacked[i - 1] };
        Ok(end - start)
    }

    /// Uncompressed length of a half-open chunk range.
    pub fn unpacked_len(&self, start: u32, end: u32) -> Result<u64, String> {
        if end > self.num_chunks() || start > end {
            return Err(format!("chunk range {start} to {end} out of range"));
        }
        let hi = if end == 0 {
            0
        } else {
            self.unpacked[end as usize - 1]
        } as u64;
        let lo = if start == 0 {
            0
        } else {
            self.unpacked[start as usize - 1]
        } as u64;
        Ok(hi - lo)
    }

    /// Physical half-open byte range of a half-open chunk range in the stored
    /// object (docs/research/binary-formats.md section 3.5b).
    pub fn byte_offset(&self, start: u32, end: u32) -> Result<(u64, u64), String> {
        if end > self.num_chunks() || start >= end {
            return Err(format!("chunk range {start} to {end} out of range"));
        }
        let lo = if start == 0 {
            0
        } else {
            self.boundary[start as usize - 1]
        } as u64;
        let hi = self.boundary[end as usize - 1] as u64;
        Ok((lo, hi))
    }
}

/// One ordered reconstruction term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// Xorb the term reads from.
    pub xorb: String,
    /// First chunk index, inclusive.
    pub chunk_start: u32,
    /// Last chunk index, exclusive.
    pub chunk_end: u32,
    /// Uncompressed bytes this term contributes.
    pub unpacked_length: u32,
}

/// One merged fetch range for a xorb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRange {
    /// First chunk index, inclusive.
    pub chunk_start: u32,
    /// Last chunk index, exclusive.
    pub chunk_end: u32,
    /// Physical byte offset in the stored object, inclusive.
    pub byte_start: u64,
    /// Physical byte offset in the stored object, EXCLUSIVE. The wire form is
    /// inclusive; the HTTP layer subtracts one.
    pub byte_end: u64,
}

/// A complete reconstruction answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Bytes to discard from the front of the first term.
    pub offset_into_first_range: u64,
    /// Ordered terms.
    pub terms: Vec<Term>,
    /// Per-xorb merged fetch ranges, sorted by chunk start.
    pub fetch: BTreeMap<String, Vec<FetchRange>>,
}

impl Plan {
    fn empty() -> Self {
        Self {
            offset_into_first_range: 0,
            terms: Vec::new(),
            fetch: BTreeMap::new(),
        }
    }
}

/// Compute the reconstruction for `file`, optionally restricted to a half-open
/// byte range.
///
/// `Ok(None)` means the range starts at or past EOF, which the HTTP layer turns
/// into 416 -- the client's end-of-file signal
/// (docs/research/dataplane.md section 8.6).
pub fn plan_reconstruction(
    file: &FileRecord,
    range: Option<(u64, u64)>,
    metas: &HashMap<String, XorbMeta>,
) -> Result<Option<Plan>, String> {
    let total = file.file_length;

    let (range_start, range_end) = match range {
        Some((start, _)) if start >= total => {
            // A zero-length file queried at offset zero is legitimately empty
            // rather than past EOF.
            if total == 0 && start == 0 {
                return Ok(Some(Plan::empty()));
            }
            return Ok(None);
        }
        Some((start, end)) => (start, end.min(total)),
        None => {
            if total == 0 {
                return Ok(Some(Plan::empty()));
            }
            (0, total)
        }
    };

    // Skip whole segments that end before the range starts.
    let mut seg_idx = 0usize;
    let mut cumulative = 0u64;
    let first_chunk_byte_start;
    loop {
        let seg = file
            .terms
            .get(seg_idx)
            .ok_or_else(|| "range extends past the file's terms".to_string())?;
        let n = u64::from(seg.unpacked_segment_bytes);
        if cumulative + n > range_start {
            first_chunk_byte_start = cumulative;
            break;
        }
        cumulative += n;
        seg_idx += 1;
    }
    let mut first_chunk_byte_start = first_chunk_byte_start;

    let mut terms = Vec::new();
    let mut per_xorb: BTreeMap<String, Vec<FetchRange>> = BTreeMap::new();

    while seg_idx < file.terms.len() && cumulative < range_end {
        let seg = &file.terms[seg_idx];
        let meta = metas
            .get(&seg.xorb_hash)
            .ok_or_else(|| format!("unknown xorb {}", seg.xorb_hash))?;

        let mut chunk_start = seg.chunk_index_start;
        let mut chunk_end = seg.chunk_index_end;
        let mut unpacked = seg.unpacked_segment_bytes;

        // Trim leading whole chunks that fall entirely before the range.
        if cumulative < range_start {
            while chunk_start < chunk_end {
                let next = u64::from(meta.chunk_len(chunk_start)?);
                if cumulative + next > range_start {
                    break;
                }
                cumulative += next;
                first_chunk_byte_start += next;
                unpacked = unpacked
                    .checked_sub(next as u32)
                    .ok_or_else(|| "segment shorter than its chunks".to_string())?;
                chunk_start += 1;
            }
        }

        // Trim trailing whole chunks that fall entirely past the range.
        if cumulative + u64::from(unpacked) > range_end {
            while chunk_end > chunk_start {
                let last = meta.chunk_len(chunk_end - 1)?;
                let without = unpacked
                    .checked_sub(last)
                    .ok_or_else(|| "segment shorter than its chunks".to_string())?;
                if cumulative + u64::from(without) < range_end {
                    break;
                }
                chunk_end -= 1;
                unpacked = without;
            }
        }

        let (byte_start, byte_end) = meta.byte_offset(chunk_start, chunk_end)?;

        terms.push(Term {
            xorb: seg.xorb_hash.clone(),
            chunk_start,
            chunk_end,
            unpacked_length: unpacked,
        });
        per_xorb
            .entry(seg.xorb_hash.clone())
            .or_default()
            .push(FetchRange {
                chunk_start,
                chunk_end,
                byte_start,
                byte_end,
            });

        cumulative += u64::from(unpacked);
        seg_idx += 1;
    }

    // Per xorb: sort by chunk start, then merge adjacent or overlapping ranges
    // into one entry. Every term's chunk range must end up contained in exactly
    // one advertised range or the client rejects the whole reconstruction
    // (docs/research/dataplane.md section 8.8).
    let mut fetch: BTreeMap<String, Vec<FetchRange>> = BTreeMap::new();
    for (hash, mut ranges) in per_xorb {
        ranges.sort_by_key(|r| r.chunk_start);
        let mut merged: Vec<FetchRange> = Vec::new();
        let mut i = 0usize;
        while i < ranges.len() {
            let mut cur = ranges[i].clone();
            while i + 1 < ranges.len() && ranges[i + 1].chunk_start <= cur.chunk_end {
                cur.chunk_end = cur.chunk_end.max(ranges[i + 1].chunk_end);
                cur.byte_end = cur.byte_end.max(ranges[i + 1].byte_end);
                i += 1;
            }
            merged.push(cur);
            i += 1;
        }
        fetch.insert(hash, merged);
    }

    Ok(Some(Plan {
        offset_into_first_range: range_start - first_chunk_byte_start,
        terms,
        fetch,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xetcas_contracts::v1::FileTermRecord;

    /// A xorb whose chunks have the given uncompressed lengths. Physical frames
    /// are modelled as the payload plus the 8-byte chunk header.
    fn meta(chunk_lens: &[u32]) -> XorbMeta {
        let mut boundary = Vec::new();
        let mut unpacked = Vec::new();
        let (mut physical, mut logical) = (0u32, 0u32);
        for &len in chunk_lens {
            physical += len + 8;
            logical += len;
            boundary.push(physical);
            unpacked.push(logical);
        }
        XorbMeta { boundary, unpacked }
    }

    /// A file made of (xorb, chunk_start, chunk_end, unpacked_bytes) terms.
    fn file(terms: &[(&str, u32, u32, u32)]) -> FileRecord {
        let terms: Vec<FileTermRecord> = terms
            .iter()
            .map(|(xorb, start, end, bytes)| FileTermRecord {
                xorb_hash: (*xorb).to_string(),
                chunk_index_start: *start,
                chunk_index_end: *end,
                unpacked_segment_bytes: *bytes,
            })
            .collect();
        FileRecord {
            file_hash: "f".repeat(64),
            file_length: terms
                .iter()
                .map(|t| u64::from(t.unpacked_segment_bytes))
                .sum(),
            sha256: String::new(),
            terms,
            verification_range_hashes: Vec::new(),
            created_at: 0,
        }
    }

    fn metas(entries: &[(&str, XorbMeta)]) -> HashMap<String, XorbMeta> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn whole_file_single_segment() {
        let m = metas(&[("a", meta(&[100, 200, 300]))]);
        let f = file(&[("a", 0, 3, 600)]);
        let plan = plan_reconstruction(&f, None, &m).unwrap().unwrap();

        assert_eq!(plan.offset_into_first_range, 0);
        assert_eq!(plan.terms.len(), 1);
        assert_eq!(plan.terms[0].chunk_start, 0);
        assert_eq!(plan.terms[0].chunk_end, 3);
        assert_eq!(plan.terms[0].unpacked_length, 600);

        let ranges = &plan.fetch["a"];
        assert_eq!(ranges.len(), 1);
        // Physical span covers all three frames including their headers.
        assert_eq!((ranges[0].byte_start, ranges[0].byte_end), (0, 624));
    }

    #[test]
    fn range_aligned_to_a_chunk_boundary_trims_without_offset() {
        let m = metas(&[("a", meta(&[100, 200, 300]))]);
        let f = file(&[("a", 0, 3, 600)]);
        let plan = plan_reconstruction(&f, Some((100, 300)), &m)
            .unwrap()
            .unwrap();

        assert_eq!(plan.offset_into_first_range, 0);
        assert_eq!(plan.terms.len(), 1);
        assert_eq!((plan.terms[0].chunk_start, plan.terms[0].chunk_end), (1, 2));
        assert_eq!(plan.terms[0].unpacked_length, 200);

        // url_range is the physical span of chunk 1 alone.
        let ranges = &plan.fetch["a"];
        assert_eq!((ranges[0].byte_start, ranges[0].byte_end), (108, 316));
    }

    #[test]
    fn range_inside_a_chunk_reports_the_leading_offset() {
        let m = metas(&[("a", meta(&[100, 200, 300]))]);
        let f = file(&[("a", 0, 3, 600)]);
        let plan = plan_reconstruction(&f, Some((150, 600)), &m)
            .unwrap()
            .unwrap();

        // Chunk 1 starts at byte 100, so 50 bytes of it are discarded.
        assert_eq!(plan.offset_into_first_range, 50);
        assert_eq!(plan.terms[0].chunk_start, 1);
    }

    #[test]
    fn adjacent_ranges_of_one_xorb_are_merged() {
        let m = metas(&[("a", meta(&[100, 100, 100, 100]))]);
        let f = file(&[("a", 0, 2, 200), ("a", 2, 4, 200)]);
        let plan = plan_reconstruction(&f, None, &m).unwrap().unwrap();

        assert_eq!(plan.terms.len(), 2, "terms stay separate");
        let ranges = &plan.fetch["a"];
        assert_eq!(ranges.len(), 1, "fetch ranges merge into one");
        assert_eq!((ranges[0].chunk_start, ranges[0].chunk_end), (0, 4));
    }

    #[test]
    fn non_contiguous_terms_across_xorbs_merge_per_xorb() {
        let m = metas(&[("a", meta(&[100, 100, 100, 100])), ("b", meta(&[150, 150]))]);
        let f = file(&[("a", 0, 2, 200), ("b", 0, 2, 300), ("a", 2, 4, 200)]);
        let plan = plan_reconstruction(&f, None, &m).unwrap().unwrap();

        assert_eq!(plan.terms.len(), 3);
        assert_eq!(plan.fetch["a"].len(), 1);
        assert_eq!(
            (plan.fetch["a"][0].chunk_start, plan.fetch["a"][0].chunk_end),
            (0, 4)
        );
        assert_eq!(plan.fetch["b"].len(), 1);
        assert_eq!(
            (plan.fetch["b"][0].chunk_start, plan.fetch["b"][0].chunk_end),
            (0, 2)
        );
    }

    #[test]
    fn range_past_eof_is_unsatisfiable() {
        let m = metas(&[("a", meta(&[100]))]);
        let f = file(&[("a", 0, 1, 100)]);
        assert!(plan_reconstruction(&f, Some((200, 300)), &m)
            .unwrap()
            .is_none());
        assert!(plan_reconstruction(&f, Some((100, 300)), &m)
            .unwrap()
            .is_none());
    }

    #[test]
    fn range_beyond_the_end_is_clamped_to_the_file() {
        let m = metas(&[("a", meta(&[500]))]);
        let f = file(&[("a", 0, 1, 500)]);
        let plan = plan_reconstruction(&f, Some((0, 10_000)), &m)
            .unwrap()
            .unwrap();
        assert_eq!(plan.terms.len(), 1);
        assert_eq!(plan.terms[0].unpacked_length, 500);
    }

    #[test]
    fn empty_file_yields_an_empty_plan() {
        let m = HashMap::new();
        let f = file(&[]);
        let plan = plan_reconstruction(&f, None, &m).unwrap().unwrap();
        assert!(plan.terms.is_empty());
        assert!(plan.fetch.is_empty());

        let plan = plan_reconstruction(&f, Some((0, 100)), &m)
            .unwrap()
            .unwrap();
        assert!(plan.terms.is_empty());

        assert!(plan_reconstruction(&f, Some((1, 100)), &m)
            .unwrap()
            .is_none());
    }
}
