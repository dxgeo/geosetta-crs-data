//! `GCR1` decode + lookup — the internals behind [`crate::resolve`],
//! [`crate::resolve_by_name`], and [`crate::all`].
//!
//! Ported from `geosetta`'s `src/crs/registry.rs` (where this used to live,
//! `pub(crate)`-private to that crate) as part of R6 — see
//! `public-api.org`. The wire format decoded here (`GCR1`) is specified in
//! `registry-format.org`; read that alongside this file. The image is
//! `(authority, code) -> {PROJJSON, WKT1, WKT2}`, sorted by
//! `(auth_id, code_bytes)` so lookup is a binary search + slice — no
//! per-entry allocation, no map build.
//!
//! Identity-*recovery* (WKT name -> code, validated against ellipsoid
//! parameters) is deliberately **not** here — that's policy about how much to
//! trust a candidate derived from untrusted input, specific to whichever
//! format reader is doing the recovering, and stays in each consumer (e.g.
//! `geosetta/src/crs/registry.rs`). This module is a dumb, honest lookup:
//! given a trusted `(authority, code)` or an exact name, return what the
//! registry has.

#![allow(dead_code)]

use crate::CrsRecord;

const MAGIC: &[u8; 4] = b"GCR1";
const HEADER_SIZE: usize = 64;
/// v2 (R5) 32-byte record: v1's 24 bytes plus `wkt2_off`/`wkt2_len`, with the
/// v1 `reserved` byte repurposed as `has_wkt2` — see `registry-format.org` §
/// V1 → V2. Only v2 blobs are produced or read; there was never a released v1
/// blob to keep parsing.
const RECORD_SIZE: usize = 32;
/// Highest `format_version` this reader understands. A blob stamped higher
/// declines cleanly (`None`) rather than partially reading — forward-compat per
/// `registry-format.org` § VERSIONING.
const MAX_FORMAT_VERSION: u16 = 2;

fn u16_at(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

/// Parsed header offsets (all byte offsets from the start of the decoded image).
struct Header {
    entry_count: usize,
    auth_count: usize,
    authorities_off: usize,
    index_off: usize,
    keys_off: usize,
    payload_off: usize,
    versions_off: usize,
}

fn parse_header(image: &[u8]) -> Option<Header> {
    if image.len() < HEADER_SIZE || &image[0..4] != MAGIC {
        return None;
    }
    if u16_at(image, 4) > MAX_FORMAT_VERSION {
        return None;
    }
    Some(Header {
        entry_count: u32_at(image, 8) as usize,
        auth_count: u16_at(image, 12) as usize,
        authorities_off: u32_at(image, 16) as usize,
        index_off: u32_at(image, 20) as usize,
        keys_off: u32_at(image, 24) as usize,
        payload_off: u32_at(image, 28) as usize,
        versions_off: u32_at(image, 32) as usize,
    })
}

/// One decoded index record — see `registry-format.org` § Index.
struct Record {
    auth_id: u8,
    key_off: usize,
    key_len: usize,
    has_wkt: bool,
    has_wkt2: bool,
    proj_off: usize,
    proj_len: usize,
    wkt_off: usize,
    wkt_len: usize,
    wkt2_off: usize,
    wkt2_len: usize,
}

fn parse_record(buf: &[u8]) -> Record {
    Record {
        auth_id: buf[0],
        key_len: buf[1] as usize,
        has_wkt: buf[2] != 0,
        has_wkt2: buf[3] != 0,
        key_off: u32_at(buf, 4) as usize,
        proj_off: u32_at(buf, 8) as usize,
        proj_len: u32_at(buf, 12) as usize,
        wkt_off: u32_at(buf, 16) as usize,
        wkt_len: u32_at(buf, 20) as usize,
        wkt2_off: u32_at(buf, 24) as usize,
        wkt2_len: u32_at(buf, 28) as usize,
    }
}

/// The decoded `GCR1` image plus its parsed header. Lookups slice directly into
/// `image`; nothing is copied or allocated per query.
struct Registry {
    image: Vec<u8>,
    header: Header,
}

impl Registry {
    fn record(&self, i: usize) -> Record {
        let off = self.header.index_off + i * RECORD_SIZE;
        parse_record(&self.image[off..off + RECORD_SIZE])
    }

    fn key_bytes(&self, rec: &Record) -> &[u8] {
        let start = self.header.keys_off + rec.key_off;
        &self.image[start..start + rec.key_len]
    }

    fn slice_str(&self, off: usize, len: usize) -> &str {
        let start = self.header.payload_off + off;
        // Generator guarantees UTF-8 (`registry-format.org` § LOOKUP ALGORITHM
        // step 5); trust it rather than paying a validation pass per lookup.
        std::str::from_utf8(&self.image[start..start + len]).unwrap_or_default()
    }

    /// Authority string -> its `auth_id` (position in the authority table).
    /// Linear scan; the table has ~10 entries.
    fn lookup_authority(&self, auth: &str) -> Option<u8> {
        let mut p = self.header.authorities_off;
        for id in 0..self.header.auth_count {
            let len = self.image[p] as usize;
            p += 1;
            if &self.image[p..p + len] == auth.as_bytes() {
                return Some(id as u8);
            }
            p += len;
        }
        None
    }

    /// `auth_id` -> its authority string. Same linear scan, the other direction.
    fn authority_name(&self, auth_id: u8) -> Option<&str> {
        let mut p = self.header.authorities_off;
        for id in 0..self.header.auth_count {
            let len = self.image[p] as usize;
            p += 1;
            if id as u8 == auth_id {
                return std::str::from_utf8(&self.image[p..p + len]).ok();
            }
            p += len;
        }
        None
    }

    /// Binary search the sorted `(auth_id, code_bytes)` index. Comparison order
    /// matches the generator's sort key exactly (`registry-format.org` § Index).
    fn find(&self, auth: &str, code: &str) -> Option<Record> {
        let auth_id = self.lookup_authority(auth)?;
        let code_b = code.as_bytes();
        let (mut lo, mut hi) = (0usize, self.header.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = self.record(mid);
            let key = (rec.auth_id, self.key_bytes(&rec));
            match key.cmp(&(auth_id, code_b)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(rec),
            }
        }
        None
    }
}

/// Decode the embedded blob once (only when the registry is actually used) and
/// parse its header. `None` if the crate has no data (generator hasn't run)
/// or the image fails to validate — callers degrade to `None` results.
fn registry() -> Option<&'static Registry> {
    static REGISTRY: std::sync::OnceLock<Option<Registry>> = std::sync::OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let zstd = crate::REGISTRY_BLOB_ZSTD;
            if zstd.is_empty() {
                return None;
            }
            let image = crate::zstd::decompress(zstd, crate::REGISTRY_BLOB_RAW_SIZE).ok()?;
            let header = parse_header(&image)?;
            Some(Registry { image, header })
        })
        .as_ref()
}

/// Build the public [`CrsRecord`] for a decoded `Record`, given its already
/// -resolved authority string (one lookup shared by both call sites below).
fn to_record(reg: &'static Registry, auth: &'static str, code: &'static str, rec: &Record) -> CrsRecord {
    CrsRecord {
        authority: auth,
        code,
        projjson: reg.slice_str(rec.proj_off, rec.proj_len),
        wkt: rec.has_wkt.then(|| reg.slice_str(rec.wkt_off, rec.wkt_len)),
        wkt2: rec.has_wkt2.then(|| reg.slice_str(rec.wkt2_off, rec.wkt2_len)),
    }
}

pub(crate) fn resolve(authority: &str, code: &str) -> Option<CrsRecord> {
    let reg = registry()?;
    let rec = reg.find(authority, code)?;
    let auth = reg.authority_name(rec.auth_id)?;
    let code = std::str::from_utf8(reg.key_bytes(&rec)).ok()?;
    Some(to_record(reg, auth, code, &rec))
}

pub(crate) fn resolve_by_name(name: &str) -> impl Iterator<Item = (&'static str, &'static str)> {
    // `NAMES` is sorted by `(name, authority, code)`, so its bounds for a
    // given name are two binary searches instead of a full scan.
    let names = crate::NAMES;
    let lo = names.partition_point(|(n, _, _)| *n < name);
    let hi = lo + names[lo..].partition_point(|(n, _, _)| *n == name);
    names[lo..hi].iter().map(|(_, auth, code)| (*auth, *code))
}

pub(crate) fn all() -> impl Iterator<Item = CrsRecord> {
    let reg = registry();
    let count = reg.map_or(0, |r| r.header.entry_count);
    (0..count).map(move |i| {
        let reg = reg.expect("count is 0 when reg is None");
        let rec = reg.record(i);
        let auth = reg.authority_name(rec.auth_id).unwrap_or_default();
        let code = std::str::from_utf8(reg.key_bytes(&rec)).unwrap_or_default();
        to_record(reg, auth, code, &rec)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_epsg_3857() {
        let rec = crate::resolve("EPSG", "3857").expect("EPSG:3857 present");
        assert_eq!(rec.authority, "EPSG");
        assert_eq!(rec.code, "3857");
        assert!(rec.projjson.contains("\"id\":{\"authority\":\"EPSG\",\"code\":3857}"), "{}", rec.projjson);
    }

    #[test]
    fn resolves_non_epsg_authorities() {
        // String codes (IGNF/OGC/PROJ/NKG) round-trip through the same path as
        // numeric-looking EPSG/ESRI codes — the key is always compared as bytes.
        let ignf = crate::resolve("IGNF", "LAMB93").expect("IGNF:LAMB93 present");
        assert!(ignf.projjson.contains("\"code\":\"LAMB93\""), "{}", ignf.projjson);

        let crs84 = crate::resolve("OGC", "CRS84").expect("OGC:CRS84 present");
        assert!(crs84.projjson.contains("\"code\":\"CRS84\""), "{}", crs84.projjson);

        let esri = crate::resolve("ESRI", "102100").expect("ESRI:102100 present");
        assert!(
            esri.projjson.contains("\"code\":102100") || esri.projjson.contains("\"code\":\"102100\""),
            "{}",
            esri.projjson
        );
    }

    #[test]
    fn unknown_authority_or_code_declines() {
        assert!(crate::resolve("NOPE", "1").is_none());
        assert!(crate::resolve("EPSG", "999999999").is_none());
    }

    #[test]
    fn wkt_present_and_absent_are_distinguishable() {
        // Every entry has PROJJSON; only ~96% have WKT1 (has_wkt=0 for datum
        // ensembles / dynamic / compound CRSes). `resolve` must return `wkt:
        // None` for those, not `Some("")`, and `Some` for the rest.
        let (mut saw_present, mut saw_absent) = (false, false);
        for rec in crate::all() {
            if saw_present && saw_absent {
                break;
            }
            match &rec.wkt {
                Some(w) if !saw_present => {
                    assert!(!w.is_empty());
                    saw_present = true;
                }
                None if !saw_absent => saw_absent = true,
                _ => {}
            }
        }
        assert!(saw_present && saw_absent, "expected both WKT-present and WKT-absent entries");
    }

    #[test]
    fn def_wkt2_covers_the_has_wkt_zero_gap() {
        // R5's actual point: EPSG:4979 (WGS 84, Geographic 3D) has no WKT1 —
        // `projinfo` itself declines ("WKT1 does not support Geographic 3D
        // CRS"), so `wkt` is `None` for it — but WKT2:2019 can express it, so
        // `wkt2` must be `Some`.
        let rec = crate::resolve("EPSG", "4979").expect("EPSG:4979 present");
        assert!(rec.wkt.is_none(), "sanity: still has_wkt=0");
        let wkt2 = rec.wkt2.expect("EPSG:4979 has WKT2");
        assert!(wkt2.contains("ID[\"EPSG\",4979]"), "{wkt2}");
    }

    #[test]
    fn def_wkt2_resolves_a_present_entry() {
        let rec = crate::resolve("EPSG", "3857").expect("EPSG:3857 present");
        let wkt2 = rec.wkt2.expect("EPSG:3857 has WKT2");
        assert!(wkt2.starts_with("PROJCRS[") || wkt2.starts_with("PROJCS["), "{wkt2}");
        assert!(wkt2.contains("ID[\"EPSG\",3857]"), "{wkt2}");
    }

    #[test]
    fn def_wkt2_unknown_declines() {
        assert!(crate::resolve("NOPE", "1").is_none());
        assert!(crate::resolve("EPSG", "999999999").is_none());
    }

    #[test]
    fn resolve_by_name_finds_wgs84_candidates() {
        let candidates: Vec<_> = crate::resolve_by_name("GCS_WGS_1984").collect();
        assert!(!candidates.is_empty());
        assert!(candidates.iter().any(|(auth, code)| *auth == "EPSG" && *code == "4326"));
    }

    #[test]
    fn resolve_by_name_unknown_declines() {
        assert_eq!(crate::resolve_by_name("Totally Made Up Name XYZ").count(), 0);
    }

    #[test]
    fn all_yields_every_entry() {
        assert_eq!(crate::all().count(), crate::CRS_COUNT);
    }

    #[test]
    fn format_self_check() {
        // Guards the embed/decode pipeline itself: magic, version, declared
        // entry count, index sortedness, and every (off, len) in-bounds.
        let reg = registry().expect("registry present");
        assert_eq!(reg.header.entry_count, crate::CRS_COUNT);

        let payload_end = reg.header.versions_off - reg.header.payload_off;
        let mut prev: Option<(u8, Vec<u8>)> = None;
        for i in 0..reg.header.entry_count {
            let rec = reg.record(i);
            let key = (rec.auth_id, reg.key_bytes(&rec).to_vec());
            if let Some(p) = &prev {
                assert!(*p <= key, "index not sorted at entry {i}");
            }
            prev = Some(key);

            assert!(rec.proj_len > 0, "entry {i}: empty PROJJSON");
            assert!(rec.proj_off + rec.proj_len <= payload_end, "entry {i}: PROJJSON out of bounds");
            if rec.has_wkt {
                assert!(rec.wkt_off + rec.wkt_len <= payload_end, "entry {i}: WKT1 out of bounds");
            } else {
                assert_eq!(rec.wkt_len, 0);
            }
            if rec.has_wkt2 {
                assert!(rec.wkt2_off + rec.wkt2_len <= payload_end, "entry {i}: WKT2 out of bounds");
            } else {
                assert_eq!(rec.wkt2_len, 0);
            }
            assert!(reg.authority_name(rec.auth_id).is_some(), "entry {i}: unknown auth_id");
        }
    }

    /// Entries where `projinfo --identify` cannot return `100 %` for reasons
    /// proven to be limitations of `projinfo` itself, not the registry data or
    /// pipeline — verified by feeding `projinfo`'s own unmodified, unstripped
    /// export of each code back into `--identify` and observing the identical
    /// result. Two classes, found by the first full run of this oracle:
    ///
    /// - `EPSG` `EngineeringCRS` (15/15 of that type, all of them): `--identify`
    ///   returns *zero* matches for this CRS type categorically.
    /// - `ESRI` UTM-zone / Alaska-system codes that are exact numeric duplicates
    ///   of an EPSG CRS: `--identify` reports the EPSG twin at 70–90 %
    ///   confidence rather than the ESRI code at 100 %.
    ///
    /// A code leaving this list (because a newer PROJ fixes the gap) is fine —
    /// the loop below tolerates over-performance. A code *entering* unexpected
    /// failure, or one on this list unexpectedly passing (so the list has gone
    /// stale), fails the test either way, so this stays an exhaustive guard
    /// against regressions rather than a blanket tolerance.
    const KNOWN_IDENTIFY_GAPS: &[(&str, &str)] = &[
        ("EPSG", "5800"),
        ("EPSG", "5801"),
        ("EPSG", "5802"),
        ("EPSG", "5803"),
        ("EPSG", "5808"),
        ("EPSG", "5809"),
        ("EPSG", "5810"),
        ("EPSG", "5811"),
        ("EPSG", "5812"),
        ("EPSG", "5813"),
        ("EPSG", "5814"),
        ("EPSG", "5815"),
        ("EPSG", "5816"),
        ("EPSG", "5817"),
        ("EPSG", "6715"),
        ("ESRI", "102124"),
        ("ESRI", "102125"),
        ("ESRI", "102126"),
        ("ESRI", "102127"),
        ("ESRI", "102128"),
        ("ESRI", "102129"),
        ("ESRI", "102130"),
        ("ESRI", "102131"),
        ("ESRI", "102570"),
        ("ESRI", "102571"),
        ("ESRI", "102572"),
        ("ESRI", "102573"),
        ("ESRI", "102574"),
        ("ESRI", "102575"),
        ("ESRI", "102576"),
        ("ESRI", "102577"),
        ("ESRI", "102578"),
        ("ESRI", "102579"),
        ("ESRI", "102580"),
    ];

    /// R5's own `KNOWN_IDENTIFY_GAPS` counterpart, for the WKT2:2019 oracle
    /// below. *Not* the same set as `KNOWN_IDENTIFY_GAPS` above — every one of
    /// these 21 codes self-identifies at 100% via its *PROJJSON* (verified
    /// individually, e.g. `ESRI:104009`), so this is a `--identify`
    /// discrepancy specific to *WKT2 serialization*, not a broader `projinfo`
    /// limitation shared with R1's list — the two lists must stay separate,
    /// since merging would make R1's oracle wrongly expect these (PROJJSON-
    /// clean) codes to fail too. Two classes, same shape as R1's: 20 deprecated
    /// ESRI codes reporting 25% (mostly geographic CRSes with a live EPSG/ESRI
    /// successor — `--identify` apparently weighs a WKT2 candidate against
    /// deprecation/duplicate status differently than a PROJJSON candidate);
    /// `ESRI:102113` (`WGS_1984_Web_Mercator`, deprecated) reporting 70% for
    /// its aux-sphere EPSG:3857 twin, the same aux-sphere ambiguity pattern as
    /// R1's ESRI-UTM-duplicate entries. Same exhaustive-allowlist discipline:
    /// a new unexpected failure or a stale (now-passing) entry both fail loud.
    const KNOWN_WKT2_IDENTIFY_GAPS: &[(&str, &str)] = &[
        ("ESRI", "102113"),
        ("ESRI", "104009"),
        ("ESRI", "104125"),
        ("ESRI", "104144"),
        ("ESRI", "104199"),
        ("ESRI", "104256"),
        ("ESRI", "104664"),
        ("ESRI", "37201"),
        ("ESRI", "37208"),
        ("ESRI", "37212"),
        ("ESRI", "37214"),
        ("ESRI", "37216"),
        ("ESRI", "37217"),
        ("ESRI", "37227"),
        ("ESRI", "37229"),
        ("ESRI", "37231"),
        ("ESRI", "37233"),
        ("ESRI", "37234"),
        ("ESRI", "37238"),
        ("ESRI", "37242"),
        ("ESRI", "37253"),
    ];

    // The registry oracle (`crs-registry.org` R1.4): every embedded definition
    // fed through `projinfo --identify` must return its own `(authority, code)`
    // at 100%, except the documented `KNOWN_IDENTIFY_GAPS`. For an authoritative
    // definition 100% holds by construction, so this mainly guards the
    // embed/serialize/zstd-decode pipeline and dataset version drift — not the
    // definitions themselves. Ignored by default: it shells out to PROJ's
    // `projinfo` once per entry (13 790 of them, ~7 minutes).
    #[test]
    #[ignore = "manual: full registry oracle, needs `projinfo` (PROJ) on PATH, ~13.8k invocations"]
    fn bulk_oracle_every_entry_identifies() {
        use std::process::Command;

        let (mut ok, mut unexpected, mut stale) = (0u32, Vec::new(), Vec::new());
        for rec in crate::all() {
            let expected_gap = KNOWN_IDENTIFY_GAPS.contains(&(rec.authority, rec.code));

            let out = Command::new("projinfo")
                .arg("--identify")
                .arg(rec.projjson)
                .output()
                .expect("run projinfo");
            let text = String::from_utf8_lossy(&out.stdout);
            let matched = text.contains(&format!("{}:{}: 100 %", rec.authority, rec.code));

            match (matched, expected_gap) {
                (true, _) => {
                    ok += 1;
                    if expected_gap {
                        stale.push(format!(
                            "{}:{}: on KNOWN_IDENTIFY_GAPS but now identifies at 100% — remove it",
                            rec.authority, rec.code
                        ));
                    }
                }
                (false, true) => {} // documented, tolerated
                (false, false) => {
                    // The confidence line (e.g. "EPSG:26701: 90 %") always
                    // contains '%'; a plain-error message has none, and its
                    // reason lands on stderr, not stdout.
                    let got = text.lines().find(|l| l.contains('%')).map(str::trim).unwrap_or("(no match)");
                    let err = String::from_utf8_lossy(&out.stderr);
                    let err = err.lines().next().unwrap_or("").trim();
                    unexpected.push(format!(
                        "{}:{}: expected 100%, got `{got}`{}",
                        rec.authority,
                        rec.code,
                        if err.is_empty() { String::new() } else { format!(" (stderr: {err})") }
                    ));
                }
            }
        }
        eprintln!(
            "registry oracle: {ok} identified@100%, {} known gaps, {} unexpected failures, {} stale exceptions",
            KNOWN_IDENTIFY_GAPS.len(),
            unexpected.len(),
            stale.len()
        );
        assert!(
            unexpected.is_empty() && stale.is_empty(),
            "{} unexpected failure(s):\n{}\n{} stale exception(s):\n{}",
            unexpected.len(),
            unexpected.join("\n"),
            stale.len(),
            stale.join("\n"),
        );
    }

    // R5's own full-registry oracle, parallel to `bulk_oracle_every_entry_
    // identifies` but over WKT2:2019 instead of PROJJSON, plus the coverage
    // claim R5 exists to make: every `has_wkt=0` entry (the ~4% WKT1 can't
    // express) must have `has_wkt2=1`. Checks against *both* `KNOWN_IDENTIFY_
    // GAPS` and `KNOWN_WKT2_IDENTIFY_GAPS`: R1's gaps (EngineeringCRS,
    // exact-duplicate ESRI codes) turn out to be `--identify` limitations that
    // hold regardless of serialization, so they fail under WKT2 too (verified
    // by this oracle's first run); `KNOWN_WKT2_IDENTIFY_GAPS` adds the ones
    // that are *specific* to WKT2 (self-identify fine via PROJJSON — see that
    // const's doc comment).
    #[test]
    #[ignore = "manual: full registry WKT2 oracle, needs `projinfo` (PROJ) on PATH, ~13.8k invocations"]
    fn bulk_oracle_every_entry_wkt2_identifies_and_covers_wkt1_gap() {
        use std::process::Command;

        let (mut ok, mut unexpected, mut stale, mut missing_wkt2) = (0u32, Vec::new(), Vec::new(), Vec::new());
        for rec in crate::all() {
            let Some(wkt2) = rec.wkt2 else {
                // The specific gap R5 exists to close: every entry lacking
                // WKT1 (`has_wkt=0`) must have WKT2 instead. Collected for
                // *all* entries missing WKT2, not just the WKT1-less ones —
                // a broader miss would be even more surprising.
                missing_wkt2.push(format!("{}:{}", rec.authority, rec.code));
                continue;
            };

            let expected_gap = KNOWN_IDENTIFY_GAPS.contains(&(rec.authority, rec.code))
                || KNOWN_WKT2_IDENTIFY_GAPS.contains(&(rec.authority, rec.code));
            let out = Command::new("projinfo")
                .arg("--identify")
                .arg(wkt2)
                .output()
                .expect("run projinfo");
            let text = String::from_utf8_lossy(&out.stdout);
            let matched = text.contains(&format!("{}:{}: 100 %", rec.authority, rec.code));

            match (matched, expected_gap) {
                (true, _) => {
                    ok += 1;
                    if expected_gap {
                        stale.push(format!(
                            "{}:{}: on a known-gaps list but now identifies at 100% (WKT2) — remove it",
                            rec.authority, rec.code
                        ));
                    }
                }
                (false, true) => {} // documented, tolerated
                (false, false) => {
                    let got = text.lines().find(|l| l.contains('%')).map(str::trim).unwrap_or("(no match)");
                    let err = String::from_utf8_lossy(&out.stderr);
                    let err = err.lines().next().unwrap_or("").trim();
                    unexpected.push(format!(
                        "{}:{}: expected 100%, got `{got}`{} (WKT2)",
                        rec.authority,
                        rec.code,
                        if err.is_empty() { String::new() } else { format!(" (stderr: {err})") }
                    ));
                }
            }
        }
        eprintln!(
            "R5 WKT2 oracle: {ok} identified@100%, {} known gaps, {} unexpected failures, {} stale exceptions, {} missing WKT2 entirely",
            KNOWN_IDENTIFY_GAPS.len() + KNOWN_WKT2_IDENTIFY_GAPS.len(),
            unexpected.len(),
            stale.len(),
            missing_wkt2.len(),
        );
        assert!(
            unexpected.is_empty() && stale.is_empty() && missing_wkt2.is_empty(),
            "{} unexpected failure(s):\n{}\n{} stale exception(s):\n{}\n{} entries with no WKT2 at all (R5's coverage claim broken):\n{}",
            unexpected.len(),
            unexpected.join("\n"),
            stale.len(),
            stale.join("\n"),
            missing_wkt2.len(),
            missing_wkt2.join("\n"),
        );
    }
}
