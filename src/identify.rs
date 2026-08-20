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
    // A sphere's inverse flattening is `INFINITY` (it has no flattening at all),
    // and that needs both guards below. Exact equality is how two spheres agree:
    // `inf - inf` is `NaN`, which fails every inequality. And the non-finite
    // rejection is how a sphere fails to agree with an ellipsoid: the tolerance
    // scales with the operands, so `SNAP_TOLERANCE * inf` is `inf` and the
    // comparison would otherwise accept *any* finite value against a sphere.
    if a == b {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
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

    identify_by_name(name, kind, wkt_a, canonical_rf(wkt_rf))
}

/// Which evidence produced an identification — what [`identify`] reports
/// alongside the answer.
///
/// [`Evidence::InlineId`] implies [`Identity::Unique`]: a stated id either
/// resolves to exactly one record or it is not evidence at all, in which case
/// the name path runs and reports itself. So an [`Identity::Ambiguous`] or
/// [`Identity::Unidentified`] answer is always [`Evidence::ValidatedName`].
///
/// This exists because the mode's guarantee is *validated*, not *weak*
/// (`plans/projjson-identify.org` § DECISIONS): once `--identify` began using an
/// inline id when the input carries one, a caller could no longer infer from the
/// mode alone which kind of evidence produced the answer. Reporting it keeps the
/// § BOUNDARY distinction checkable by anyone who cares to check it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The definition stated its own authority and code, and that pair resolved
    /// — the same trusted lookup [`crate::resolve`] does, reached from a
    /// definition rather than a command line.
    InlineId,
    /// The definition's name matched a registry entry whose own ellipsoid agrees
    /// within this module's snap tolerance. Weaker than an id, which is why the
    /// ellipsoid check exists and why this never picks between equal candidates.
    ValidatedName,
}

/// Identify a CRS definition in either dialect, by the strongest evidence it
/// carries.
///
/// The entry point a caller with text of unknown provenance wants — piping a
/// file's CRS out of some other tool, the user does not know in advance whether
/// it is WKT or PROJJSON, or whether it states an id. This answers all three
/// questions rather than making the caller answer them:
///
/// 1. **Sniff the dialect.** First non-whitespace byte `{` means PROJJSON;
///    anything else means WKT. No flag, because `--identify` already composes
///    with `--wkt`/`--wkt2`/`--projjson` and those select the *output* dialect —
///    two flag families sharing three names with opposite meanings would be a
///    real footgun, and the sniff is unambiguous since no WKT production begins
///    with `{`.
/// 2. **Use an inline id if there is one.** A stated authority and code is
///    strictly stronger evidence than a name; ignoring it would be deliberately
///    doing worse work, and erroring would put the conditional back in the
///    user's shell.
/// 3. **Otherwise recover by name**, validated against the definition's own
///    ellipsoid, exactly as [`identify_from_wkt`] and
///    [`identify_from_projjson`] do.
///
/// An id that does *not* resolve — an unknown authority, a retired code — falls
/// through to step 3 rather than failing: it turned out to carry no evidence,
/// and the name may still.
///
/// The sniff is total. A JSON document that is not a CRS is
/// [`Identity::Unidentified`], never retried as WKT, because "this is not a CRS"
/// is a clearer answer than a WKT tokenizer's complaint about a `{`.
pub fn identify(text: &str) -> (Identity, Evidence) {
    if text.trim_start().starts_with('{') {
        if let Some(rec) = json::parse(text).as_ref().and_then(projjson_inline_id) {
            return (Identity::Unique(rec), Evidence::InlineId);
        }
        return (identify_from_projjson(text), Evidence::ValidatedName);
    }
    let toks = wkt::tokenize(text);
    if let Some(rec) = wkt::authority_code(&toks).and_then(|(a, c)| crate::resolve(a, c)) {
        return (Identity::Unique(rec), Evidence::InlineId);
    }
    (identify_from_wkt(text), Evidence::ValidatedName)
}

/// The record a PROJJSON's root `id` names, when it states one that resolves.
///
/// PROJJSON spells `id.code` as a JSON number for the numeric authorities (EPSG,
/// ESRI, IAU_2015 — the overwhelming majority) and as a string for the
/// alphanumeric ones (IGNF's `LAMB93`, OGC's `CRS84`), so both are read. Only
/// the *root* id counts: the ones nested in `datum_ensemble.members` describe
/// the datum's realizations, not this CRS.
fn projjson_inline_id(pj: &Json) -> Option<CrsRecord> {
    let id = pj.get("id")?;
    let authority = id.get("authority")?.as_str()?;
    let code = id.get("code")?;
    match code.as_str() {
        Some(c) => crate::resolve(authority, c),
        // An authority code is always an integer when written as a number;
        // formatting through `{}` would render a spurious `.0`.
        None => crate::resolve(authority, &format!("{}", code.as_f64()? as i64)),
    }
}

/// Steps 2 and 3 — look the name up, then validate every candidate against the
/// `kind` and ellipsoid the *input* declared, whichever dialect it was written
/// in.
///
/// Shared by [`identify_from_wkt`] and [`identify_from_projjson`] so a second
/// input dialect cannot drift from the first on what the evidence is worth: only
/// step 1 (getting `name`, `kind`, and the ellipsoid out of the text) differs
/// between them, which is exactly the split `plans/projjson-identify.org`
/// § Extraction argues for.
fn identify_by_name(name: &str, kind: Kind, a: f64, rf: f64) -> Identity {
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
        if approx_eq(a, reg_a) && approx_eq(rf, reg_rf) {
            matches.push(rec);
        }
    }

    match matches.len() {
        0 => Identity::Unidentified,
        1 => Identity::Unique(matches.remove(0)),
        _ => Identity::Ambiguous(matches),
    }
}

/// Identify an id-less PROJJSON definition — the sibling of
/// [`identify_from_wkt`], same method and same trust policy, differing only in
/// how step 1 reads the input.
///
/// This is the dialect a container format hands over: GeoParquet records its CRS
/// as PROJJSON, so a definition printed out of one arrives here rather than at
/// the WKT entry point. Steps 2 and 3 run the same code either dialect reaches,
/// so only the extraction differs.
///
/// A JSON document that parses but is not a CRS — no `type`, no `name`, or no
/// ellipsoid to check a name against — is [`Identity::Unidentified`]. It is
/// never retried as WKT: "this is not a CRS" is a clear answer, and turning it
/// into a WKT tokenizer complaint would be a worse one.
///
/// ```
/// let projjson = r#"{"type":"ProjectedCRS","name":"WGS 84 / Pseudo-Mercator",
///     "base_crs":{"type":"GeographicCRS","name":"WGS 84",
///     "datum_ensemble":{"name":"World Geodetic System 1984 ensemble",
///     "ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,
///     "inverse_flattening":298.257223563}}}}"#;
/// let rec = geoscribe::identify_from_projjson(projjson).unique().expect("identifies");
/// assert_eq!((rec.authority, rec.code), ("EPSG", "3857"));
/// ```
///
/// Note what a *bare* `"WGS 84"` does instead: it fits EPSG:4326 (2D) and
/// EPSG:4979 (3D) equally, same family and same ellipsoid, so it is
/// [`Identity::Ambiguous`] carrying both rather than a guess. That is the trust
/// policy working, not a shortcoming — see [`Identity`].
pub fn identify_from_projjson(text: &str) -> Identity {
    let Some(pj) = json::parse(text) else {
        return Identity::Unidentified;
    };
    let Some(kind) = pj.get("type").and_then(Json::as_str).and_then(Kind::from_projjson_type)
    else {
        return Identity::Unidentified;
    };
    let (Some(name), Some((a, rf))) =
        (pj.get("name").and_then(Json::as_str), projjson_ellipsoid(&pj))
    else {
        return Identity::Unidentified;
    };

    identify_by_name(name, kind, a, rf)
}

/// One spelling for "this is a sphere", across both dialects.
///
/// A sphere has no flattening, and the two dialects say so differently: WKT
/// writes the inverse flattening as `0` (`SPHEROID["Sphere_EMEP",6370000.0,0.0]`
/// — the shape all the `ESRI:104xxx` planetary CRSes take), while PROJJSON omits
/// flattening entirely and states a `radius`. Neither is *literally* an inverse
/// flattening of zero, which would mean infinite flattening; both mean the same
/// physical fact.
///
/// Canonicalizing to `INFINITY` — the true inverse of zero flattening — is what
/// lets an Esri `.prj` sphere validate against the registry's PROJJSON sphere.
/// Comparing `0` against a `radius`-only record never matched, which is why 182
/// real Esri fixtures used to decline; see
/// `tests/identify_esri.rs`'s census.
fn canonical_rf(rf: f64) -> f64 {
    if rf == 0.0 { f64::INFINITY } else { rf }
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
    /// WKT's `GEODCRS` / `GEOCCS` — the general geodetic spelling, which covers
    /// both the geocentric-Cartesian and the lat/lon cases, so both PROJJSON
    /// types may validate and the ellipsoid check is what discriminates. This
    /// looseness is a property of the *WKT keyword*, not of the family: a
    /// PROJJSON input says which it means and gets [`Kind::Geodetic`].
    GeodeticOrGeographic,
    /// PROJJSON's `GeodeticCRS` — the geocentric case, stated unambiguously by
    /// an input that had a word for it.
    Geodetic,
    /// `PROJCS` / `PROJCRS`.
    Projected,
}

impl Kind {
    fn from_keyword(kw: &str) -> Option<Kind> {
        match kw {
            "GEOGCS" | "GEOGCRS" => Some(Kind::Geographic),
            "GEODCRS" | "GEOCCS" => Some(Kind::GeodeticOrGeographic),
            "PROJCS" | "PROJCRS" => Some(Kind::Projected),
            _ => None,
        }
    }

    /// The family a PROJJSON input's own `type` declares.
    ///
    /// Simpler than [`Kind::from_keyword`] because both sides of the comparison
    /// are then the same vocabulary — PROJJSON has distinct spellings for the
    /// two cases WKT's `GEODCRS` conflates, so nothing here needs to be loose.
    ///
    /// The families outside this set decline, which is deliberate and not a
    /// gap: `CompoundCRS`, `VerticalCRS`, and `EngineeringCRS` (1,326 of the
    /// 13,790 embedded records) have no single base ellipsoid to validate a
    /// name against, and name recovery was only ever measured for the three
    /// below. See `plans/wkt-identify.org` § Contracts.
    fn from_projjson_type(ty: &str) -> Option<Kind> {
        match ty {
            "GeographicCRS" => Some(Kind::Geographic),
            "GeodeticCRS" => Some(Kind::Geodetic),
            "ProjectedCRS" => Some(Kind::Projected),
            _ => None,
        }
    }

    fn accepts(self, projjson_type: Option<&str>) -> bool {
        match self {
            Kind::Geographic => projjson_type == Some("GeographicCRS"),
            Kind::GeodeticOrGeographic => {
                matches!(projjson_type, Some("GeographicCRS" | "GeodeticCRS"))
            }
            Kind::Geodetic => projjson_type == Some("GeodeticCRS"),
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

    // A sphere states a `radius` and nothing else — it has no flattening, so its
    // *inverse* flattening is infinite. Spelling it that way rather than
    // special-casing keeps one comparison for both shapes: two spheres agree via
    // `approx_eq`'s exact-equality arm, and a sphere never agrees with an
    // ellipsoid.
    if let Some(radius) = metres(ellipsoid.get("radius")) {
        return Some((radius, f64::INFINITY));
    }

    let a = metres(ellipsoid.get("semi_major_axis"))?;
    let rf = match ellipsoid.get("inverse_flattening").and_then(Json::as_f64) {
        Some(rf) => canonical_rf(rf),
        None => {
            let b = metres(ellipsoid.get("semi_minor_axis"))?;
            // `a == b` is a sphere stated as two equal axes; the division would
            // yield `inf` anyway, but say so through the one helper.
            if a == b { f64::INFINITY } else { a / (a - b) }
        }
    };
    Some((a, rf))
}

/// A PROJJSON linear measure, in metres.
///
/// PROJJSON writes a length either as a bare number — metres, the default linear
/// unit — or as `{"value": N, "unit": {…, "conversion_factor": F}}` when it is in
/// something else. The historical ellipsoids defined in Clarke's or Indian feet
/// (Clarke 1858 at 20926348 Clarke's feet, Everest 1830, Clarke 1880) take the
/// second form, and reading `value` while ignoring `unit` would compare feet
/// against metres and reject every one of them.
///
/// Normalizing rather than comparing units pairwise is what makes an input in one
/// unit match a registry record in another, which is the whole reason to convert
/// instead of requiring the spellings to agree.
///
/// `inverse_flattening` deliberately does *not* go through here: it is a ratio,
/// dimensionless by definition, and PROJJSON always writes it bare.
fn metres(v: Option<&Json>) -> Option<f64> {
    let v = v?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let value = v.get("value")?.as_f64()?;
    let factor = v.get("unit").and_then(|u| u.get("conversion_factor")).and_then(Json::as_f64);
    // A `unit` object with no factor is malformed; treating it as metres would be
    // a guess, so decline instead of inventing one.
    Some(value * factor?)
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

    // --- the PROJJSON entry point ------------------------------------------

    fn pj_ids(text: &str) -> Vec<String> {
        identify_from_projjson(text)
            .into_candidates()
            .iter()
            .map(|r| format!("{}:{}", r.authority, r.code))
            .collect()
    }

    fn pj_unique(text: &str) -> String {
        let rec = identify_from_projjson(text).unique().expect("identifies uniquely");
        format!("{}:{}", rec.authority, rec.code)
    }

    /// WGS 84 with its root `id` stripped — the shape `geosetta --print-crs`
    /// emits from an id-less GeoParquet, and the `datum_ensemble` case that an
    /// extractor looking only at `datum` would miss entirely.
    const IDLESS_WGS84: &str = r#"{"type":"GeographicCRS","name":"WGS 84",
        "datum_ensemble":{"name":"World Geodetic System 1984 ensemble",
        "ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,
        "inverse_flattening":298.257223563}}}"#;

    #[test]
    fn recovers_an_id_less_projjson_using_datum_ensemble() {
        // `datum_ensemble`, not `datum` — every WGS 84-based CRS in the registry
        // uses it, so an extractor reading only `datum` would miss the single
        // most common input in existence.
        //
        // The answer is a candidate set, not a pick: the catalog name "WGS 84"
        // fits EPSG:4326 (2D) and EPSG:4979 (3D) with the same family and the
        // same ellipsoid, and nothing in the definition distinguishes them. The
        // bar here is the recovery bar — the true code must be *among* the
        // candidates — not the trusted-id bar.
        assert_eq!(pj_ids(IDLESS_WGS84), vec!["EPSG:4326", "EPSG:4979"]);
        assert!(matches!(identify_from_projjson(IDLESS_WGS84), Identity::Ambiguous(_)));
    }

    #[test]
    fn recovers_a_plain_datum_rather_than_an_ensemble() {
        // NAD83 uses a single-realization `datum`, not an ensemble — both spellings
        // have to work or the extractor covers only half the registry.
        let nad83 = r#"{"type":"GeographicCRS","name":"NAD83",
            "datum":{"type":"GeodeticReferenceFrame","name":"North American Datum 1983",
            "ellipsoid":{"name":"GRS 1980","semi_major_axis":6378137,
            "inverse_flattening":298.257222101}}}"#;
        assert_eq!(pj_unique(nad83), "EPSG:4269");
    }

    #[test]
    fn recovers_a_projected_crs_through_its_base_crs() {
        // A ProjectedCRS carries no ellipsoid of its own; the one that identifies
        // it sits under `base_crs`. Base ellipsoid only — never the projection
        // method or parameters (`plans/wkt-identify.org` § Contracts).
        let webmerc = r#"{"type":"ProjectedCRS","name":"WGS 84 / Pseudo-Mercator",
            "base_crs":{"type":"GeographicCRS","name":"WGS 84",
            "datum_ensemble":{"name":"World Geodetic System 1984 ensemble",
            "ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,
            "inverse_flattening":298.257223563}}}}"#;
        assert_eq!(pj_unique(webmerc), "EPSG:3857");
    }

    #[test]
    fn converts_semi_minor_axis_to_inverse_flattening() {
        // Clarke 1866 (NAD27's ellipsoid) is expressed with `semi_minor_axis` in
        // the registry; an input spelling it the same way must still compare.
        let nad27 = r#"{"type":"GeographicCRS","name":"NAD27",
            "datum":{"type":"GeodeticReferenceFrame","name":"North American Datum 1927",
            "ellipsoid":{"name":"Clarke 1866","semi_major_axis":6378206.4,
            "semi_minor_axis":6356583.8}}}"#;
        assert_eq!(pj_unique(nad27), "EPSG:4267");
    }

    #[test]
    fn rejects_a_lying_name_in_projjson_too() {
        // The trust policy does not change with the dialect: the right spelling
        // over a fabricated ellipsoid must decline, never snap.
        let lying = r#"{"type":"GeographicCRS","name":"WGS 84",
            "datum_ensemble":{"name":"e","ellipsoid":{"name":"f",
            "semi_major_axis":6300000,"inverse_flattening":290}}}"#;
        assert_eq!(identify_from_projjson(lying), Identity::Unidentified);
    }

    #[test]
    fn a_wrong_kind_does_not_validate() {
        // PROJJSON says which family it is, so a name that matches a CRS of a
        // different type is not a match. WKT's `GEODCRS` conflates geographic and
        // geocentric; PROJJSON does not, and `Kind` reflects that.
        let as_geodetic = IDLESS_WGS84.replace("GeographicCRS", "GeodeticCRS");
        assert!(!pj_ids(&as_geodetic).contains(&"EPSG:4326".to_string()));
        assert_eq!(Kind::from_projjson_type("GeodeticCRS"), Some(Kind::Geodetic));
        assert!(!Kind::Geodetic.accepts(Some("GeographicCRS")));
        // The WKT spelling stays deliberately loose, which is the case that
        // rename was made to keep distinct.
        assert!(Kind::GeodeticOrGeographic.accepts(Some("GeographicCRS")));
        assert!(Kind::GeodeticOrGeographic.accepts(Some("GeodeticCRS")));
    }

    #[test]
    fn json_that_is_not_a_crs_is_unidentified_not_an_error() {
        for text in [
            r#"{"hello":"world"}"#,                          // valid JSON, no CRS
            r#"{"type":"CompoundCRS","name":"WGS 84"}"#,     // a family with no ellipsoid
            r#"{"type":"GeographicCRS"}"#,                   // no name
            r#"{"name":"WGS 84"}"#,                          // no type
            "{",                                             // malformed
            "",                                              // empty
        ] {
            assert_eq!(
                identify_from_projjson(text),
                Identity::Unidentified,
                "for {text:?}"
            );
        }
    }

    #[test]
    fn identifies_a_sphere_which_has_no_flattening() {
        // A sphere states a `radius` and stops. Requiring `semi_major_axis` plus a
        // flattening silently declined on every one of these — the ESRI:104xxx
        // planetary CRSes and EPSG's authalic spheres.
        let sphere = r#"{"type":"GeographicCRS","name":"NSIDC Authalic Sphere",
            "datum":{"type":"GeodeticReferenceFrame","name":"NSIDC International 1924 Authalic Sphere",
            "ellipsoid":{"name":"International 1924 Authalic Sphere","radius":6371228}}}"#;
        assert_eq!(pj_unique(sphere), "EPSG:10346");

        // And a sphere never validates against an ellipsoid: `INFINITY` compares
        // equal only to itself.
        assert!(approx_eq(f64::INFINITY, f64::INFINITY));
        assert!(!approx_eq(f64::INFINITY, 298.257223563));
    }

    #[test]
    fn identifies_an_ellipsoid_defined_in_a_non_metre_unit() {
        // Clarke 1858 is defined in Clarke's feet, so PROJJSON wraps the axis as
        // `{"value":…,"unit":{…,"conversion_factor":…}}`. Reading `value` and
        // ignoring the unit would compare feet against metres and reject it;
        // normalizing is also what lets an input in one unit match a record in
        // another.
        let ft = r#"{"type":"GeographicCRS","name":"Mount Dillon",
            "datum":{"type":"GeodeticReferenceFrame","name":"Mount Dillon",
            "ellipsoid":{"name":"Clarke 1858",
            "semi_major_axis":{"value":20926348,"unit":{"type":"LinearUnit",
            "name":"Clarke's foot","conversion_factor":0.3047972654}},
            "semi_minor_axis":{"value":20855233,"unit":{"type":"LinearUnit",
            "name":"Clarke's foot","conversion_factor":0.3047972654}}}}}"#;
        assert!(
            pj_ids(ft).contains(&"EPSG:4157".to_string()),
            "got {:?}",
            pj_ids(ft)
        );
    }

    #[test]
    fn a_unit_object_with_no_conversion_factor_declines_rather_than_guessing() {
        // Assuming metres would be inventing a fact about the input.
        let bad = r#"{"type":"GeographicCRS","name":"WGS 84",
            "datum":{"name":"d","ellipsoid":{"name":"e",
            "semi_major_axis":{"value":6378137,"unit":{"name":"mystery"}},
            "inverse_flattening":298.257223563}}}"#;
        assert_eq!(identify_from_projjson(bad), Identity::Unidentified);
    }

    #[test]
    fn the_two_dialects_agree_on_the_same_crs() {
        // Cross-dialect agreement: where the extraction differs, this catches it.
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]]"#;
        assert_eq!(ids(wkt), pj_ids(IDLESS_WGS84));
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
