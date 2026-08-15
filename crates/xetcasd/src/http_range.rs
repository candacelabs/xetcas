//! HTTP `Range` header parsing.
//!
//! The real client only ever emits the two-sided `bytes=S-E` form, but the
//! reference server accepts open-ended and suffix forms too
//! (docs/research/dataplane.md section 8.6), so this does as well.
//!
//! HTTP ranges are end-INCLUSIVE; every resolved range this module returns is
//! end-EXCLUSIVE, matching xet-core's internal `FileRange`. Conflating the two
//! silently corrupts downloads (docs/research/api-surface.md section 5.2).

/// A parsed but unresolved single byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// Both bounds present, end-inclusive.
    FromTo(u64, u64),
    /// Open ended: from the given offset to the end of the object.
    From(u64),
    /// The final N bytes of the object.
    Suffix(u64),
}

/// Parse a single-range `Range` header value. Multi-range values and any
/// unit other than bytes are rejected.
pub fn parse_range_header(value: &str) -> Option<RangeSpec> {
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());

    match (start.is_empty(), end.is_empty()) {
        (true, false) => end.parse().ok().map(RangeSpec::Suffix),
        (false, true) => start.parse().ok().map(RangeSpec::From),
        (false, false) => {
            let s = start.parse().ok()?;
            let e = end.parse().ok()?;
            Some(RangeSpec::FromTo(s, e))
        }
        (true, true) => None,
    }
}

impl RangeSpec {
    /// Resolve against a known total length, returning a half-open range.
    ///
    /// `None` means unsatisfiable, which callers turn into 416.
    pub fn resolve(self, total: u64) -> Option<(u64, u64)> {
        match self {
            Self::FromTo(s, e) => {
                if s >= total || e < s {
                    return None;
                }
                // The end is inclusive on the wire and exclusive here, and is
                // clamped: a client may legitimately ask past EOF on the last
                // block of a segmented download.
                Some((s, total.min(e.saturating_add(1))))
            }
            Self::From(s) => {
                if s >= total {
                    return None;
                }
                Some((s, total))
            }
            Self::Suffix(n) => {
                if n == 0 {
                    return None;
                }
                Some((total.saturating_sub(n), total))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_accepted_forms() {
        assert_eq!(
            parse_range_header("bytes=0-99"),
            Some(RangeSpec::FromTo(0, 99))
        );
        assert_eq!(parse_range_header("bytes=10-"), Some(RangeSpec::From(10)));
        assert_eq!(parse_range_header("bytes=-64"), Some(RangeSpec::Suffix(64)));
    }

    #[test]
    fn rejects_multi_range_and_other_units() {
        assert_eq!(parse_range_header("bytes=0-9,20-29"), None);
        assert_eq!(parse_range_header("items=0-9"), None);
        assert_eq!(parse_range_header("bytes=-"), None);
        assert_eq!(parse_range_header("bytes=abc-def"), None);
    }

    #[test]
    fn inclusive_end_becomes_exclusive_and_clamps() {
        assert_eq!(RangeSpec::FromTo(0, 99).resolve(1000), Some((0, 100)));
        assert_eq!(RangeSpec::FromTo(0, 9999).resolve(1000), Some((0, 1000)));
        assert_eq!(RangeSpec::From(500).resolve(1000), Some((500, 1000)));
        assert_eq!(RangeSpec::Suffix(100).resolve(1000), Some((900, 1000)));
        assert_eq!(RangeSpec::Suffix(5000).resolve(1000), Some((0, 1000)));
    }

    #[test]
    fn start_at_or_past_eof_is_unsatisfiable() {
        assert_eq!(RangeSpec::FromTo(1000, 1099).resolve(1000), None);
        assert_eq!(RangeSpec::From(1000).resolve(1000), None);
        assert_eq!(RangeSpec::FromTo(5, 4).resolve(1000), None);
    }
}
