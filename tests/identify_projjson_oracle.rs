//! The bulk oracle for [`geoscribe::identify_from_projjson`]: every CRS that
//! exists, with its identity removed, fed back in to see what comes out.
//!
//! This crate can test this capability against itself, which nothing else in it
//! can. The registry embeds PROJJSON for all 13,790 CRSes and `--projjson` is
//! the one dialect every entry has, so *stripping the root `id` from each yields
//! a perfect id-less corpus with known-correct answers* — no fixtures, no
//! `projinfo`, no network. `plans/wkt-identify.org` had nothing comparable and
//! had to import 2,705 real Esri rows from geosetta.
//!
//! **The bar is the recovery bar, not the trusted-id bar.** Deliberately weaker
//! than `plans/crs-registry.org`'s R1 oracle (100% identification via a trusted
//! id), for the reason that governs this whole module: a name is weaker evidence
//! than an id. So:
//!
//! - Zero *wrong* answers. A [`Identity::Unique`] naming a different CRS is a
//!   bug, full stop.
//! - Declining is acceptable. So is [`Identity::Ambiguous`], provided the true
//!   code is among the candidates.
//!
//! See § RESULTS at the bottom for what the corpus actually does, which is the
//! finding this file exists to produce.

use std::collections::BTreeMap;

use geoscribe::Identity;

/// Remove the root-level `id` member from a PROJJSON document, leaving the text
/// otherwise byte-identical.
///
/// **Structural, not positional, and that matters.** The obvious implementation
/// — chop a trailing `,"id":{...}}` — is wrong for 966 of the 13,790 records,
/// which carry a `remarks` string *after* their root `id` (`EPSG:10158`,
/// `10175`, `10176`, …). This walks the root object's members instead and
/// removes the one named `id` wherever it sits.
///
/// **Only the root id.** The ids nested inside `datum_ensemble.members` describe
/// the datum's realizations, not the CRS's identity, and real id-less PROJJSON
/// in the wild keeps them. Stripping those too would test a shape no writer
/// emits.
///
/// Returns `None` when there is no root `id` to remove, which the caller treats
/// as a corpus problem rather than silently testing the un-stripped text.
fn strip_root_id(pj: &str) -> Option<String> {
    let b = pj.as_bytes();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let first_member = i;
    // Byte index of the comma introducing the member under examination, or
    // `None` while looking at the first one.
    let mut prev_comma: Option<usize> = None;

    loop {
        i = skip_ws(b, i);
        if b.get(i) != Some(&b'"') {
            return None; // `}` on an empty object, or malformed
        }
        let (key, after_key) = read_string(b, i)?;
        i = skip_ws(b, after_key);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let value_end = skip_value(b, skip_ws(b, i + 1))?;

        if key == "id" {
            let mut out = String::with_capacity(pj.len());
            match prev_comma {
                // Not the first member: swallow the comma before it.
                Some(c) => {
                    out.push_str(&pj[..c]);
                    out.push_str(&pj[value_end..]);
                }
                // The first member: swallow the comma after it, if any.
                None => {
                    let next = skip_ws(b, value_end);
                    let resume = if b.get(next) == Some(&b',') { next + 1 } else { value_end };
                    out.push_str(&pj[..first_member]);
                    out.push_str(&pj[resume..]);
                }
            }
            return Some(out);
        }

        i = skip_ws(b, value_end);
        match b.get(i) {
            Some(&b',') => {
                prev_comma = Some(i);
                i += 1;
            }
            _ => return None, // `}` reached with no root id
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The string starting at `b[i] == '"'`, and the index just past its closing
/// quote. Handles escapes only well enough to find that quote, which is all a
/// key comparison needs.
fn read_string(b: &[u8], i: usize) -> Option<(String, usize)> {
    let mut j = i + 1;
    let start = j;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some((String::from_utf8_lossy(&b[start..j]).into_owned(), j + 1)),
            _ => j += 1,
        }
    }
    None
}

/// The index just past the JSON value starting at `i`, skipping over nesting.
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => read_string(b, i).map(|(_, end)| end),
        open @ (b'{' | b'[') => {
            let close = if *open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => j = read_string(b, j)?.1,
                    c if c == *open => {
                        depth += 1;
                        j += 1;
                    }
                    c if c == close => {
                        depth -= 1;
                        j += 1;
                        if depth == 0 {
                            return Some(j);
                        }
                    }
                    _ => j += 1,
                }
            }
            None
        }
        // A number, `true`, `false`, or `null`: runs to the next delimiter.
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

// --- the stripper's own tests ----------------------------------------------
// The oracle is only as trustworthy as this, so it is checked directly rather
// than assumed from the corpus passing.

#[test]
fn strips_a_trailing_root_id() {
    let out = strip_root_id(r#"{"type":"GeographicCRS","id":{"authority":"EPSG","code":4326}}"#);
    assert_eq!(out.unwrap(), r#"{"type":"GeographicCRS"}"#);
}

#[test]
fn strips_a_root_id_followed_by_another_member() {
    // The 966-record case: `remarks` sits after the id.
    let out = strip_root_id(
        r#"{"type":"GeographicCRS","id":{"authority":"EPSG","code":10158},"remarks":"why"}"#,
    );
    assert_eq!(out.unwrap(), r#"{"type":"GeographicCRS","remarks":"why"}"#);
}

#[test]
fn strips_a_root_id_that_comes_first() {
    let out = strip_root_id(r#"{"id":{"authority":"EPSG","code":4326},"type":"GeographicCRS"}"#);
    assert_eq!(out.unwrap(), r#"{"type":"GeographicCRS"}"#);
}

#[test]
fn leaves_nested_ids_alone() {
    // A datum ensemble's member ids are part of the datum's description, and a
    // real id-less definition keeps them.
    let pj = r#"{"type":"GeographicCRS","datum_ensemble":{"members":[{"name":"a","id":{"authority":"EPSG","code":1166}}]},"id":{"authority":"EPSG","code":4326}}"#;
    let out = strip_root_id(pj).unwrap();
    assert!(out.contains(r#""code":1166"#), "nested id must survive: {out}");
    assert!(!out.contains(r#""code":4326"#), "root id must go: {out}");
}

#[test]
fn reports_no_root_id_rather_than_returning_the_input() {
    assert_eq!(strip_root_id(r#"{"type":"GeographicCRS"}"#), None);
    assert_eq!(strip_root_id("{}"), None);
    assert_eq!(strip_root_id("[]"), None);
}

// --- the oracle -------------------------------------------------------------

/// Every embedded CRS, stripped of its id and fed back in.
///
/// Not `#[ignore]`-gated: it needs no external tool, and whole-corpus runtime is
/// a few seconds. `plans/projjson-identify.org` § TESTING says to gate on cost
/// if it ever stops being, not on tooling.
#[test]
fn every_embedded_crs_survives_having_its_id_removed() {
    let mut wrong: Vec<String> = Vec::new();
    let mut missing_id: Vec<String> = Vec::new();
    let mut unique = 0usize;
    let mut ambiguous_hit = 0usize;
    let mut declined_by_family: BTreeMap<String, usize> = BTreeMap::new();
    let mut ambiguity_sizes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut total = 0usize;

    for rec in geoscribe::all() {
        total += 1;
        let id = format!("{}:{}", rec.authority, rec.code);
        let Some(stripped) = strip_root_id(rec.projjson) else {
            missing_id.push(id);
            continue;
        };
        // The strip must actually have changed something, or this record is
        // silently being tested with its answer still attached.
        assert_ne!(stripped, rec.projjson, "{id}: strip was a no-op");

        let family = root_type(rec.projjson).unwrap_or("(none)").to_string();
        match geoscribe::identify_from_projjson(&stripped) {
            Identity::Unique(found) => {
                if (found.authority, found.code) == (rec.authority, rec.code) {
                    unique += 1;
                } else {
                    wrong.push(format!("{id} -> {}:{}", found.authority, found.code));
                }
            }
            Identity::Ambiguous(cands) => {
                *ambiguity_sizes.entry(cands.len()).or_default() += 1;
                if cands.iter().any(|c| (c.authority, c.code) == (rec.authority, rec.code)) {
                    ambiguous_hit += 1;
                } else {
                    wrong.push(format!("{id} -> ambiguous without itself"));
                }
            }
            Identity::Unidentified => *declined_by_family.entry(family).or_default() += 1,
        }
    }

    let declined: usize = declined_by_family.values().sum();
    eprintln!("--- strip-id oracle over {total} embedded CRSes ---");
    eprintln!("  uniquely recovered:        {unique}");
    eprintln!("  ambiguous, true code in:   {ambiguous_hit}");
    eprintln!("  declined:                  {declined}");
    for (family, n) in &declined_by_family {
        eprintln!("      {family}: {n}");
    }
    eprintln!("  ambiguity sizes (n candidates -> records): {ambiguity_sizes:?}");

    assert!(missing_id.is_empty(), "records with no root id to strip: {missing_id:?}");
    assert!(
        wrong.is_empty(),
        "{} wrong answers (the one thing that must never happen); first 20: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(20)]
    );
    assert_eq!(unique + ambiguous_hit + declined, total, "every record accounted for");
}

/// A record's root `type`, read off the text — enough to bucket declines by
/// family in the report above.
fn root_type(pj: &str) -> Option<&str> {
    let i = pj.find(r#""type":""#)? + 8;
    let rest = &pj[i..];
    Some(&rest[..rest.find('"')?])
}
