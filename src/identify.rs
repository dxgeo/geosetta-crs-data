//! Identify an *id-less* WKT by name, validated against its own ellipsoid.
//!
//! The one capability that did not survive `geosetta` dropping its
//! `crs-registry` feature (and this dependency) in 0.24.0. Esri-flavor
//! Shapefile `.prj` text carries no `AUTHORITY`/`ID` node anywhere, so there
//! is no code for geosetta's `--print-crs-code` to report and nothing for this
//! crate's trusted-id CLI to look up. See `plans/wkt-identify.org`, and read
//! its § BOUNDARY: this is a deliberate, argued reversal of
//! `plans/public-api.org` § BOUNDARY, which had assigned this policy to
//! whichever format reader was doing the recovering — a reader that, as of
//! geosetta 0.24.0, no longer exists.
//!
//! Three steps, only the middle one previously this crate's:
//!
//! 1. **Extract** the outer CRS name and the first ellipsoid's
//!    `(semi_major_axis, inverse_flattening)` ([`crate::wkt`]).
//! 2. **Look up** the name via [`resolve_by_name`](crate::resolve_by_name).
//! 3. **Validate before snapping** — require each candidate's PROJJSON `type`
//!    to match the WKT's own root keyword, and its ellipsoid to agree within a
//!    relative [`SNAP_TOLERANCE`]. This is what rejects a *lying name*: the
//!    right spelling over a fabricated ellipsoid.
//!
//! A name is weaker evidence than an inline id, which is why step 3 exists at
//! all and why this never guesses. It also never *picks*: when more than one
//! candidate validates, the answer is [`Identity::Ambiguous`] carrying every
//! one of them, for a caller (or a human) to adjudicate. Silently taking the
//! first would hand a caller a specific `(authority, code)` label it has no
//! evidence for over the others.

use crate::json::{self, Json};
use crate::{wkt, CrsRecord};

/// Relative tolerance for validating a name match before snapping to it, per
/// `plans/crs-registry.org` § Validation ("relative 1e-6 on values"). Applies
/// only to recovery weaker than a trusted inline id; [`crate::resolve`] never
/// uses it.
const SNAP_TOLERANCE: f64 = 1e-6;

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= SNAP_TOLERANCE * a.abs().max(b.abs()).max(1.0)
}

/// What a WKT's name and ellipsoid could be identified as.
///
/// Deliberately three-valued rather than `Option<CrsRecord>`: "several real
/// CRSes share this catalog name and this ellipsoid" is a different fact from
/// "no CRS matches", and collapsing them would mean picking one arbitrarily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// Exactly one candidate validated — a real answer, safe to use.
    Unique(CrsRecord),
    /// More than one validated, in [`resolve_by_name`](crate::resolve_by_name)
    /// order. Every one of them fits the evidence the WKT carries; choosing
    /// between them needs information the WKT does not have, so this crate
    /// does not choose.
    Ambiguous(Vec<CrsRecord>),
    /// No extractable name/ellipsoid, no candidate for the name, or no
    /// candidate whose type and ellipsoid validate. Never a guess.
    Unidentified,
}

impl Identity {
    /// The single unambiguous answer, or `None` for both
    /// [`Ambiguous`](Identity::Ambiguous) and
    /// [`Unidentified`](Identity::Unidentified).
    ///
    /// This is the accessor a caller that must not guess should reach for:
    /// ambiguity collapses to `None` rather than to an arbitrary pick.
    pub fn unique(self) -> Option<CrsRecord> {
        match self {
            Identity::Unique(rec) => Some(rec),
            _ => None,
        }
    }

    /// Every validating candidate — one for [`Unique`](Identity::Unique),
    /// several for [`Ambiguous`](Identity::Ambiguous), none for
    /// [`Unidentified`](Identity::Unidentified). For a caller doing its own
    /// adjudication (a prompt, a preference order, an error message).
    pub fn into_candidates(self) -> Vec<CrsRecord> {
        match self {
            Identity::Unique(rec) => vec![rec],
            Identity::Ambiguous(recs) => recs,
            Identity::Unidentified => Vec::new(),
        }
    }
}

/// Identify an id-less WKT definition — a *validated* answer, not a lookup.
///
/// The WKT's outer name is looked up in the registry and each candidate is
/// confirmed against the WKT's own ellipsoid before being accepted. Covers
/// geographic and projected CRSes; see [`Identity`] for what the three
/// outcomes mean, and the module header for why ambiguity is not resolved
/// here.
///
/// A WKT that *does* carry an `AUTHORITY`/`ID` node should be read with
/// [`resolve`](crate::resolve) on that pair instead: an inline id is stronger
/// evidence than a name, and guessing a different code than the source stated
/// is a mislabel, not a recovery.
///
/// ```
/// let esri_prj = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",
///     SPHEROID["WGS_1984",6378137.0,298.257223563]],
///     PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
/// let rec = geoscribe::identify_from_wkt(esri_prj).unique().expect("identifies");
/// assert_eq!((rec.authority, rec.code), ("EPSG", "4326"));
/// ```
pub fn identify_from_wkt(text: &str) -> Identity {
    let toks = wkt::tokenize(text);
    let Some(kind) = wkt::root_keyword(&toks).as_deref().and_then(Kind::from_keyword) else {
        return Identity::Unidentified;
    };
    let (Some(name), Some((wkt_a, wkt_rf))) =
        (wkt::crs_name(&toks), wkt::ellipsoid_params(&toks))
    else {
        return Identity::Unidentified;
    };

    let mut matches: Vec<CrsRecord> = Vec::new();
    for (auth, code) in crate::resolve_by_name(name) {
        // `NAMES` can carry the same pair twice (an authority's official name
        // and an alias that spell identically); the same CRS listed twice is
        // not an ambiguity.
        if matches.iter().any(|r| r.authority == auth && r.code == code) {
            continue;
        }
        let Some(rec) = crate::resolve(auth, code) else { continue };
        let Some(pj) = json::parse(rec.projjson) else { continue };
        if !kind.accepts(pj.get("type").and_then(Json::as_str)) {
            continue;
        }
        let Some((reg_a, reg_rf)) = projjson_ellipsoid(&pj) else { continue };
        if approx_eq(wkt_a, reg_a) && approx_eq(wkt_rf, reg_rf) {
            matches.push(rec);
        }
    }

    match matches.len() {
        0 => Identity::Unidentified,
        1 => Identity::Unique(matches.remove(0)),
        _ => Identity::Ambiguous(matches),
    }
}

/// Which family of CRS a WKT's root keyword declares, and which PROJJSON
/// `type` values may therefore validate against it.
///
/// A name match that lands on the wrong *kind* of CRS is not a match at all —
/// this is the cheap check that runs before the ellipsoid comparison. A root
/// keyword outside these families (`COMPOUNDCRS`, `VERT_CS`, `TIMECRS`, …)
/// yields `None`: name recovery was only ever measured for the families below,
/// and declining is the safe outcome for the rest.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// `GEOGCS` / `GEOGCRS` — a latitude/longitude CRS, which PROJJSON always
    /// spells `GeographicCRS`. Kept narrow deliberately: widening it to
    /// `GeodeticCRS` would let a geocentric twin sharing the same name and
    /// ellipsoid (e.g. `EPSG:4978` beside `EPSG:4326`) validate against a
    /// plainly 2D input.
    Geographic,
    /// `GEODCRS` / `GEOCCS` — the general geodetic spelling, which covers both
    /// the geocentric-Cartesian and the lat/lon cases, so both PROJJSON types
    /// may validate and the ellipsoid check is what discriminates.
    Geodetic,
    /// `PROJCS` / `PROJCRS`.
    Projected,
}

impl Kind {
    fn from_keyword(kw: &str) -> Option<Kind> {
        match kw {
            "GEOGCS" | "GEOGCRS" => Some(Kind::Geographic),
            "GEODCRS" | "GEOCCS" => Some(Kind::Geodetic),
            "PROJCS" | "PROJCRS" => Some(Kind::Projected),
            _ => None,
        }
    }

    fn accepts(self, projjson_type: Option<&str>) -> bool {
        match self {
            Kind::Geographic => projjson_type == Some("GeographicCRS"),
            Kind::Geodetic => matches!(projjson_type, Some("GeographicCRS" | "GeodeticCRS")),
            Kind::Projected => projjson_type == Some("ProjectedCRS"),
        }
    }
}

/// A CRS's ellipsoid as `(semi_major_axis, inverse_flattening)`, from its
/// `datum` (a plain single-realization datum) or `datum_ensemble` (the modern
/// form WGS 84 and similar use — the ellipsoid sits once at the ensemble
/// level, not per member).
///
/// A `ProjectedCRS` nests these under `base_crs` (its underlying geographic
/// CRS) rather than at the top level, so that is checked first; falling
/// through to the top level covers geographic callers unchanged. PROJJSON
/// expresses flattening either as `inverse_flattening` directly or as
/// `semi_minor_axis` (e.g. Clarke 1866, NAD27's ellipsoid); the latter is
/// converted (`a / (a - b)`) so callers always compare the same quantity.
fn projjson_ellipsoid(pj: &Json) -> Option<(f64, f64)> {
    let base = pj.get("base_crs").unwrap_or(pj);
    let datum = base.get("datum").or_else(|| base.get("datum_ensemble"))?;
    let ellipsoid = datum.get("ellipsoid")?;
    let a = ellipsoid.get("semi_major_axis")?.as_f64()?;
    let rf = match ellipsoid.get("inverse_flattening").and_then(Json::as_f64) {
        Some(rf) => rf,
        None => {
            let b = ellipsoid.get("semi_minor_axis")?.as_f64()?;
            a / (a - b)
        }
    };
    Some((a, rf))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidates, as `AUTHORITY:CODE`, for compact assertions.
    fn ids(wkt: &str) -> Vec<String> {
        identify_from_wkt(wkt)
            .into_candidates()
            .iter()
            .map(|r| format!("{}:{}", r.authority, r.code))
            .collect()
    }

    fn unique_id(wkt: &str) -> String {
        let rec = identify_from_wkt(wkt).unique().expect("identifies uniquely");
        format!("{}:{}", rec.authority, rec.code)
    }

    // Real `projinfo -o WKT1_ESRI` exports — id-less, Esri-flavor spellings
    // (`GCS_WGS_1984`, `D_North_American_1983`), exactly the Shapefile-`.prj`
    // shape `plans/crs-registry.org` § WHY measured as mis-identifying (WGS 84
    // at 70% confidence, NAD83/NAD27 outright wrong) under structural
    // translation alone. All three carried over verbatim from geosetta's
    // deleted `src/crs/registry.rs`, so the answers below are literally the
    // ones that crate produced with its `crs-registry` feature on.
    const ESRI_WGS84: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    const ESRI_NAD83: &str = r#"GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    // Esri's inverse_flattening (294.978698213898) is derived from the same
    // ellipsoid the registry expresses as `semi_minor_axis` (6356583.8) —
    // exercises the conversion path in [`projjson_ellipsoid`].
    const ESRI_NAD27: &str = r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

    #[test]
    fn recovers_esri_wgs84() {
        assert_eq!(unique_id(ESRI_WGS84), "EPSG:4326");
    }

    #[test]
    fn recovers_esri_nad83() {
        // Structural translation alone gets this wrong (EPSG:9309, per
        // `plans/crs-registry.org`); name recovery gets the real code.
        assert_eq!(unique_id(ESRI_NAD83), "EPSG:4269");
    }

    #[test]
    fn recovers_esri_nad27() {
        // Structural translation alone gets this wrong (EPSG:4169); also the
        // `semi_minor_axis` -> `inverse_flattening` case.
        assert_eq!(unique_id(ESRI_NAD27), "EPSG:4267");
    }

    #[test]
    fn rejects_a_lying_name() {
        // Same name as WGS 84, but a fabricated ellipsoid matching no real
        // registry entry: a name is weaker evidence than an id, so a
        // mismatched structure must not snap — decline, never guess.
        let lying = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6300000.0,290.0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(identify_from_wkt(lying), Identity::Unidentified);
    }

    #[test]
    fn declines_an_unknown_name() {
        let wkt = r#"GEOGCS["Totally Made Up Datum XYZ",DATUM["d",SPHEROID["e",6378137.0,298.257223563]]]"#;
        assert_eq!(identify_from_wkt(wkt), Identity::Unidentified);
    }

    #[test]
    fn declines_a_wkt_with_no_name_or_no_ellipsoid() {
        assert_eq!(identify_from_wkt(""), Identity::Unidentified);
        assert_eq!(identify_from_wkt("not wkt at all"), Identity::Unidentified);
        // A name but no SPHEROID/ELLIPSOID node: nothing to validate against,
        // and an unvalidated name match is exactly what this must not return.
        assert_eq!(identify_from_wkt(r#"GEOGCS["GCS_WGS_1984"]"#), Identity::Unidentified);
    }

    #[test]
    fn declines_a_crs_family_it_does_not_cover() {
        // A vertical CRS's name could well be in `NAMES`, but name recovery was
        // only ever measured for geographic and projected CRSes — and a
        // `VERT_CS` has no ellipsoid to validate against in the first place.
        let vert = r#"VERT_CS["NAVD_1988",VERT_DATUM["North_American_Vertical_Datum_1988",2005],UNIT["Meter",1.0]]"#;
        assert_eq!(identify_from_wkt(vert), Identity::Unidentified);
    }

    // Real `projinfo -o WKT1_ESRI` export, ESRI:102057
    // (`tools/gen_esri_projected_fixtures.py`).
    const ESRI_NAD83_2011_UTM10N: &str = r#"PROJCS["NAD_1983_2011_UTM_Zone_10N",GEOGCS["GCS_NAD_1983_2011",DATUM["D_NAD_1983_2011",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-123.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;

    #[test]
    fn reports_the_esri_epsg_twin_as_ambiguous_rather_than_picking() {
        // ESRI:102057 is deprecated in `proj.db` with EPSG:6339 as its live
        // replacement, and the two share an Esri-style catalog name. Geosetta
        // resolved this to EPSG:6339 by taking the first candidate in `NAMES`
        // order — a tie-break, not evidence. The registry carries no
        // deprecation flag (checked: no entry's PROJJSON has the field), so
        // there is nothing here that *can* prefer one honestly, and both are
        // reported instead.
        assert_eq!(ids(ESRI_NAD83_2011_UTM10N), ["EPSG:6339", "ESRI:102057"]);
        assert_eq!(identify_from_wkt(ESRI_NAD83_2011_UTM10N).unique(), None);
    }

    #[test]
    fn rejects_a_lying_projected_ellipsoid() {
        // Same name, fabricated base ellipsoid — same strict-fallback
        // discipline as the geographic path.
        let lying = r#"PROJCS["NAD_1983_2011_UTM_Zone_10N",GEOGCS["GCS_NAD_1983_2011",DATUM["D_NAD_1983_2011",SPHEROID["GRS_1980",6300000.0,290.0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],UNIT["Meter",1.0]]"#;
        assert_eq!(identify_from_wkt(lying), Identity::Unidentified);
    }

    #[test]
    fn declines_an_unknown_projected_name() {
        let wkt = r#"PROJCS["Totally Made Up Projection XYZ",GEOGCS["g",DATUM["d",SPHEROID["e",6378137.0,298.257223563]]],PROJECTION["Transverse_Mercator"]]"#;
        assert_eq!(identify_from_wkt(wkt), Identity::Unidentified);
    }

    #[test]
    fn recovers_a_projected_crs_uniquely_when_no_twin_exists() {
        // A real `.prj`-shaped projected export (ESRI:24721, from
        // `tests/fixtures/esri_projected_wkt1.tsv`) whose catalog name no other
        // authority answers to, so the projected path returns a single real
        // answer. This is the case geosetta's `resolve_projected_by_name` was
        // written and bulk-oracle-tested for — and never wired to a call site,
        // so it never ran in production there.
        let wkt = r#"PROJCS["La_Canoa_UTM_Zone_21N",GEOGCS["GCS_La_Canoa",DATUM["D_La_Canoa",SPHEROID["International_1924",6378388.0,297.0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-57.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        assert_eq!(unique_id(wkt), "ESRI:24721");
    }

    #[test]
    fn a_projected_name_does_not_match_a_geographic_crs() {
        // The kind check runs before the ellipsoid comparison: `GCS_WGS_1984`
        // is a real name with a real ellipsoid, but nothing projected answers
        // to it, so a `PROJCS` claiming it identifies as nothing.
        let wkt = r#"PROJCS["GCS_WGS_1984",GEOGCS["g",DATUM["d",SPHEROID["WGS_1984",6378137.0,298.257223563]]],PROJECTION["Mercator"]]"#;
        assert_eq!(identify_from_wkt(wkt), Identity::Unidentified);
    }

    #[test]
    fn a_geogcs_does_not_match_its_geocentric_twin() {
        // EPSG:4326 (geographic), 4978 (geocentric) and 4979 (3D geographic)
        // all answer to "WGS 84" over the same ellipsoid. A `GEOGCRS` root is
        // 2D/3D lat-lon by construction, so the geocentric one must not be a
        // candidate at all — see [`Kind::Geographic`].
        let wkt = r#"GEOGCRS["WGS 84",ENSEMBLE["World Geodetic System 1984 ensemble",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],CS[ellipsoidal,2]]"#;
        let got = ids(wkt);
        assert!(got.contains(&"EPSG:4326".to_string()), "{got:?}");
        assert!(!got.contains(&"EPSG:4978".to_string()), "geocentric twin leaked in: {got:?}");
    }

    #[test]
    fn identity_accessors_agree_with_the_variant() {
        assert_eq!(identify_from_wkt(ESRI_WGS84).into_candidates().len(), 1);
        assert!(identify_from_wkt(ESRI_NAD83_2011_UTM10N).into_candidates().len() > 1);
        assert_eq!(identify_from_wkt("").into_candidates(), vec![]);
        assert_eq!(identify_from_wkt("").unique(), None);
    }

    #[test]
    fn snap_tolerance_is_relative_and_tight() {
        assert!(approx_eq(6378137.0, 6378137.0 + 1e-3)); // ~1.6e-10 relative
        assert!(!approx_eq(6378137.0, 6378137.0 + 100.0)); // ~1.6e-5 relative
        assert!(approx_eq(0.0, 0.0));
    }
}
