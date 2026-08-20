//! `identify_from_wkt` over the real Esri corpus — the tests that say this
//! behaves the way `geosetta` did with its `crs-registry` feature on.
//!
//! The fixtures (`tests/fixtures/esri_{geographic,projected}_wkt1.tsv`) are
//! real `projinfo -o WKT1_ESRI` exports of every native ESRI geographic 2D
//! (431) and projected (2 274) CRS in `proj.db`, produced by
//! `tools/gen_esri_*_fixtures.py`. They moved here from `geosetta/tests/`
//! along with the capability itself — see `plans/wkt-identify.org` § TESTING.
//!
//! # What "the same as geosetta" means here
//! Geosetta's deleted `src/crs/registry.rs` applied one predicate (name match,
//! right CRS type, ellipsoid within a relative 1e-6) and then returned the
//! *first* candidate that passed, in `NAMES` order. This crate applies the
//! identical predicate — [`geosetta_0_23_pick`] below reimplements the old rule
//! independently, and [`matches_the_geosetta_rule_over_the_whole_corpus`]
//! checks the two agree on all 2 705 fixtures — but stops short of the "first
//! wins" step, because that step was a tie-break, not evidence.
//!
//! So the divergence is exactly and only this: where several real CRSes fit a
//! WKT equally well, geosetta silently returned one of them and this crate
//! returns [`Identity::Ambiguous`] with all of them. That is the one deliberate
//! behavior change, and it is not small — see
//! [`the_ambiguity_rate_over_the_corpus`] for how often the old rule was
//! guessing.

use geoscribe::Identity;

fn fixtures(name: &str) -> Vec<(String, String)> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (code, wkt) = l.split_once('\t').expect("code<TAB>wkt");
            (code.to_string(), wkt.to_string())
        })
        .collect()
}

fn all_fixtures() -> Vec<(String, String)> {
    let mut v = fixtures("esri_geographic_wkt1.tsv");
    v.extend(fixtures("esri_projected_wkt1.tsv"));
    v
}

fn ids(id: &Identity) -> Vec<String> {
    id.clone()
        .into_candidates()
        .iter()
        .map(|r| format!("{}:{}", r.authority, r.code))
        .collect()
}

// --- the reference implementation of geosetta 0.23's rule -------------------

/// Geosetta's rule, reimplemented from its deleted `resolve_*_by_name`: the
/// first candidate in `NAMES` order whose PROJJSON is the right CRS type and
/// whose ellipsoid matches the WKT's within a relative 1e-6.
///
/// Deliberately written against this crate's *public* API plus its own crude
/// text scrapes, sharing none of `identify.rs`'s extraction or comparison code,
/// so agreement between the two is evidence rather than tautology.
fn geosetta_0_23_pick(wkt: &str) -> Option<String> {
    let name = scrape_wkt_name(wkt)?;
    let (wkt_a, wkt_rf) = scrape_wkt_ellipsoid(wkt)?;
    let want_projected = wkt.trim_start().to_ascii_uppercase().starts_with("PROJCS");
    for (auth, code) in geoscribe::resolve_by_name(&name) {
        let rec = geoscribe::resolve(auth, code)?;
        let is_projected = rec.projjson.contains(r#""type":"ProjectedCRS""#);
        let is_geographic = rec.projjson.contains(r#""type":"GeographicCRS""#);
        if want_projected != is_projected || (!want_projected && !is_geographic) {
            continue;
        }
        let Some((reg_a, reg_rf)) = scrape_projjson_ellipsoid(rec.projjson) else { continue };
        if rel_eq(wkt_a, reg_a) && rel_eq(wkt_rf, reg_rf) {
            return Some(format!("{auth}:{code}"));
        }
    }
    None
}

fn rel_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

/// The first quoted string in the WKT — its outer CRS name.
fn scrape_wkt_name(wkt: &str) -> Option<String> {
    let rest = wkt.split_once('"')?.1;
    Some(rest.split_once('"')?.0.to_string())
}

/// `(a, rf)` from the WKT's first `SPHEROID[...]`/`ELLIPSOID[...]` node.
fn scrape_wkt_ellipsoid(wkt: &str) -> Option<(f64, f64)> {
    let upper = wkt.to_ascii_uppercase();
    let at = upper.find("SPHEROID[").or_else(|| upper.find("ELLIPSOID["))?;
    let body = wkt[at..].split_once('[')?.1;
    let body = &body[..body.find(']')?];
    let mut fields = body.splitn(3, ',').skip(1); // skip the ellipsoid name
    let a = fields.next()?.trim().parse().ok()?;
    let rf = fields.next()?.split(',').next()?.trim().parse().ok()?;
    Some((a, rf))
}

/// `(a, rf)` from a PROJJSON's first `"ellipsoid":{...}` object — which is the
/// base geographic CRS's for a `ProjectedCRS` and the datum's for a
/// `GeographicCRS`.
fn scrape_projjson_ellipsoid(pj: &str) -> Option<(f64, f64)> {
    let at = pj.find(r#""ellipsoid":{"#)?;
    let body = &pj[at..];
    let end = body.find('}')?;
    let body = &body[..end];
    let a = scrape_number(body, "semi_major_axis")?;
    let rf = match scrape_number(body, "inverse_flattening") {
        Some(rf) => rf,
        None => a / (a - scrape_number(body, "semi_minor_axis")?),
    };
    Some((a, rf))
}

fn scrape_number(json: &str, key: &str) -> Option<f64> {
    let at = json.find(&format!("\"{key}\":"))?;
    let rest = &json[at + key.len() + 3..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

// --- equivalence with geosetta ---------------------------------------------

#[test]
fn matches_the_geosetta_rule_over_the_whole_corpus() {
    let mut disagreements = Vec::new();
    let mut newly_identified = 0usize;
    for (code, wkt) in all_fixtures() {
        let got = geoscribe::identify_from_wkt(&wkt);
        let mine = ids(&got);
        let theirs = geosetta_0_23_pick(&wkt);

        // The load-bearing direction: nothing geosetta could resolve may be
        // lost. A decline here where it answered is a regression.
        if theirs.is_some() && mine.is_empty() {
            disagreements.push(format!("ESRI:{code}: geosetta {theirs:?}, here declined"));
            continue;
        }
        // The other direction is an *improvement*, not a disagreement, and this
        // test used to forbid it by asserting strict equivalence. Geosetta's
        // rule reimplemented below (`geosetta_0_23_pick`) inherits the same
        // sphere and non-metre-axis blind spots this crate had until
        // 2026-08-19 — see `the_ambiguity_rate_over_the_corpus` — so it declines
        // on 182 fixtures that now identify. Count them; do not require them.
        if theirs.is_none() {
            if !mine.is_empty() {
                newly_identified += 1;
            }
            continue;
        }
        // Where it did answer, that answer is always the first of ours, since
        // "first in NAMES order" is precisely the step this crate stops short
        // of. Ambiguity was hidden, never a different CRS.
        if let Some(theirs) = theirs
            && mine.first() != Some(&theirs)
        {
            disagreements.push(format!("ESRI:{code}: geosetta {theirs}, here {mine:?}"));
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} fixtures where this crate's candidate set doesn't contain geosetta's answer first:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
    // Recorded, like the census: a drop here means the ellipsoid extractor lost
    // ground, which no change should do silently.
    assert_eq!(newly_identified, 182, "fixtures geosetta declined and this crate identifies");
}

#[test]
fn the_geosetta_unit_cases_still_give_the_same_answers() {
    // The four cases geosetta asserted by hand in its own `registry.rs` tests.
    // The first three are unchanged; the fourth is the deliberate divergence.
    let cases: &[(&str, &[&str])] = &[
        (r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#, &["EPSG:4326"]),
        (r#"GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#, &["EPSG:4269"]),
        (r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#, &["EPSG:4267"]),
        // Geosetta returned EPSG:6339 alone here, by tie-break.
        (r#"PROJCS["NAD_1983_2011_UTM_Zone_10N",GEOGCS["GCS_NAD_1983_2011",DATUM["D_NAD_1983_2011",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-123.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#, &["EPSG:6339", "ESRI:102057"]),
    ];
    for (wkt, want) in cases {
        assert_eq!(ids(&geoscribe::identify_from_wkt(wkt)), *want, "for {wkt}");
    }
}

// --- properties of the answer ----------------------------------------------

#[test]
fn a_candidate_set_is_never_a_single_crs_listed_twice() {
    // `NAMES` carries an authority's official name and its Esri aliases, which
    // can spell identically — the same `(authority, code)` reached twice is not
    // an ambiguity and must not be reported as one.
    for (code, wkt) in all_fixtures() {
        let got = ids(&geoscribe::identify_from_wkt(&wkt));
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), got.len(), "ESRI:{code} has duplicate candidates: {got:?}");
    }
}

#[test]
fn ambiguous_always_means_two_or_more_and_unique_exactly_one() {
    for (code, wkt) in all_fixtures() {
        match geoscribe::identify_from_wkt(&wkt) {
            Identity::Unique(_) => {}
            Identity::Ambiguous(recs) => {
                assert!(recs.len() >= 2, "ESRI:{code}: Ambiguous with {} candidates", recs.len())
            }
            Identity::Unidentified => {}
        }
    }
}

#[test]
fn every_candidate_is_a_resolvable_registry_entry() {
    // A candidate is offered to a human to pick from and then feed back in as
    // `geoscribe AUTHORITY:CODE`, so every one must round-trip through the
    // trusted-id path.
    for (code, wkt) in all_fixtures() {
        for rec in geoscribe::identify_from_wkt(&wkt).into_candidates() {
            let back = geoscribe::resolve(rec.authority, rec.code)
                .unwrap_or_else(|| panic!("ESRI:{code} offered {}:{}, which doesn't resolve", rec.authority, rec.code));
            assert_eq!(back.projjson, rec.projjson);
        }
    }
}

#[test]
fn the_ambiguity_rate_over_the_corpus() {
    // A recorded census, not a quality bar — it pins down how much of the old
    // behavior was a tie-break. Geosetta answered on unique + ambiguous alike
    // (327 of 431 geographic, 2 196 of 2 274 projected); of those answers, 109
    // and 890 respectively were one of several equally-supported CRSes, picked
    // by sort order. That is a third of the projected corpus, which is why the
    // ambiguity is surfaced now instead of resolved silently.
    //
    // Update these numbers deliberately when the registry is regenerated from a
    // newer `proj.db` — a change here is a real change in what the data
    // supports, worth looking at rather than absorbing.
    //
    // **Revised 2026-08-19, and the unidentified column is now zero.** It was
    // (218, 109, 104) and (1306, 890, 78). The 182 declines were not a property
    // of the data: `identify`'s shared ellipsoid extractor read
    // `semi_major_axis` only as a bare number and required an
    // `inverse_flattening` or `semi_minor_axis` to pair with it, so it silently
    // declined on two real shapes in the registry —
    //
    //   * **spheres**, where the two dialects state the same fact differently —
    //     WKT writes inverse flattening `0`, PROJJSON writes a `radius` and no
    //     flattening — so an Esri sphere could never match the registry's
    //     (the ESRI:104xxx planetary CRSes, EPSG:10346's authalic sphere).
    //     `canonical_rf` normalizes both to `INFINITY`, the true inverse of zero
    //     flattening, and `approx_eq` rejects non-finite mismatches so a sphere
    //     still never validates against a real ellipsoid;
    //   * **non-metre ellipsoids**, which wrap the axis as
    //     `{"value": …, "unit": {… "conversion_factor": F}}` (Clarke 1858 in
    //     Clarke's feet, Everest 1830, Clarke 1880).
    //
    // Both are now read (`identify.rs`'s `metres` and the `radius` arm), which
    // is why every one of these 2 705 real Esri rows identifies. Found by the
    // strip-id oracle in `identify_projjson_oracle.rs`, which is exactly the
    // class of gap it was built to surface. The ambiguous column moved by one in
    // each file, so the newly-reachable candidates are almost all unique.
    for (file, want) in [
        ("esri_geographic_wkt1.tsv", (321, 110, 0)),
        ("esri_projected_wkt1.tsv", (1383, 891, 0)),
    ] {
        let (mut unique, mut ambiguous, mut unidentified) = (0, 0, 0);
        for (_, wkt) in fixtures(file) {
            match geoscribe::identify_from_wkt(&wkt) {
                Identity::Unique(_) => unique += 1,
                Identity::Ambiguous(_) => ambiguous += 1,
                Identity::Unidentified => unidentified += 1,
            }
        }
        assert_eq!((unique, ambiguous, unidentified), want, "census for {file}");
    }
}

// --- the bulk oracles ------------------------------------------------------

/// Entries where `projinfo --identify` cannot return `100 %` for reasons proven
/// to be limitations of `projinfo` itself, not the registry data or pipeline.
///
/// **Duplicated from `src/registry.rs`'s own `KNOWN_IDENTIFY_GAPS`** (same
/// list, same provenance — see that file's doc comment for the two classes it
/// covers) because it is `cfg(test)`-private to the unit-test build and an
/// integration test cannot import it. Keep the two in sync by hand — the same
/// documented-duplication discipline as this crate's zstd decoder
/// (`plans/public-api.org` § MODULE LAYOUT).
const KNOWN_IDENTIFY_GAPS: &[(&str, &str)] = &[
    ("EPSG", "5800"), ("EPSG", "5801"), ("EPSG", "5802"), ("EPSG", "5803"),
    ("EPSG", "5808"), ("EPSG", "5809"), ("EPSG", "5810"), ("EPSG", "5811"),
    ("EPSG", "5812"), ("EPSG", "5813"), ("EPSG", "5814"), ("EPSG", "5815"),
    ("EPSG", "5816"), ("EPSG", "5817"), ("EPSG", "6715"),
    ("ESRI", "102124"), ("ESRI", "102125"), ("ESRI", "102126"), ("ESRI", "102127"),
    ("ESRI", "102128"), ("ESRI", "102129"), ("ESRI", "102130"), ("ESRI", "102131"),
    ("ESRI", "102570"), ("ESRI", "102571"), ("ESRI", "102572"), ("ESRI", "102573"),
    ("ESRI", "102574"), ("ESRI", "102575"), ("ESRI", "102576"), ("ESRI", "102577"),
    ("ESRI", "102578"), ("ESRI", "102579"), ("ESRI", "102580"),
];

/// The hard bar, run over both corpora: **every candidate this crate is willing
/// to offer must self-identify at 100%** via `projinfo --identify`. Declining
/// to identify is always acceptable; offering a CRS that isn't what it claims
/// to be never is.
///
/// Stricter than geosetta's oracle in one way that matters: geosetta checked
/// only the candidate it picked, so the runners-up it silently discarded were
/// never validated. Here every member of an ambiguous set is checked, because
/// every member is something a human may choose.
fn bulk_oracle(file: &str, label: &str) {
    use std::process::Command;

    let (mut checked, mut declined, mut known_gap, mut wrong) = (0u32, 0u32, 0u32, Vec::new());
    for (esri_code, wkt) in fixtures(file) {
        let candidates = geoscribe::identify_from_wkt(&wkt).into_candidates();
        if candidates.is_empty() {
            declined += 1;
            continue;
        }
        for rec in candidates {
            let out = Command::new("projinfo")
                .arg("--identify")
                .arg(rec.projjson)
                .output()
                .expect("run projinfo");
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains(": 100 %") {
                checked += 1;
            } else if KNOWN_IDENTIFY_GAPS.contains(&(rec.authority, rec.code)) {
                known_gap += 1;
            } else {
                let got = text.lines().find(|l| l.contains('%')).unwrap_or("(no match)");
                wrong.push(format!(
                    "ESRI:{esri_code}: offered {}:{}, which doesn't self-identify at 100%: {}",
                    rec.authority,
                    rec.code,
                    got.trim()
                ));
            }
        }
    }
    eprintln!(
        "{label} name-recovery oracle: {checked} candidates @100%, {declined} fixtures declined, \
         {known_gap} known projinfo --identify gaps, {} wrong",
        wrong.len()
    );
    assert!(
        wrong.is_empty(),
        "{} offered-but-wrong (a candidate that doesn't self-identify — must never happen):\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
#[ignore = "manual: bulk oracle over 431 native ESRI geographic CRSes, needs `projinfo` (PROJ) on PATH"]
fn bulk_oracle_esri_geographic() {
    bulk_oracle("esri_geographic_wkt1.tsv", "geographic");
}

#[test]
#[ignore = "manual: bulk oracle over 2274 native ESRI projected CRSes, needs `projinfo` (PROJ) on PATH"]
fn bulk_oracle_esri_projected() {
    bulk_oracle("esri_projected_wkt1.tsv", "projected");
}

