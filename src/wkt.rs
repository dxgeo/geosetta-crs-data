//! A shallow WKT lexer plus the two extractors [`identify`](crate::identify)
//! needs: the outer CRS name and the first ellipsoid's constants.
//!
//! This crate *stores* WKT but had never parsed one until `identify_from_wkt`
//! (see `plans/wkt-identify.org`). What it needs is not a WKT parser: WKT is a
//! tree of `KEYWORD[value, value, ...]` nodes, and recovering a CRS's identity
//! reads only the root keyword, the first quoted string, and one numeric pair.
//! Nothing here interprets the projection a WKT describes.
//!
//! **Ported, not shared**, from `geosetta`'s `src/crs.rs` (`tokenize_wkt` /
//! `wkt_crs_name_toks` / `wkt_ellipsoid_params_toks`). Geosetta keeps its own
//! copy regardless — `wkt_authority_code` and `is_crs_wkt` both need it — and
//! factoring the lexer into a shared crate would reintroduce exactly the
//! dependency geosetta removed in its 0.24.0. Two copies of a shallow,
//! spec-stable lexer is the same maintenance fact already accepted for the
//! zstd decoder (`plans/public-api.org` § MODULE LAYOUT / § RISKS), for the
//! same reason.

/// A lexical WKT token. Both WKT1 and WKT2 tokenize identically at this depth.
#[derive(Debug, PartialEq)]
pub(crate) enum Tok {
    Open,
    Close,
    Comma,
    Str(String),
    Word(String),
}

/// Tokenize WKT into brackets, commas, quoted strings, and bare words
/// (keywords like `SPHEROID`, or numbers like `6378137.0`). Deliberately
/// lenient: `[`/`(` and `]`/`)` are treated alike, and anything that isn't a
/// delimiter or whitespace runs into a single [`Tok::Word`].
pub(crate) fn tokenize(s: &str) -> Vec<Tok> {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'[' | b'(' => {
                toks.push(Tok::Open);
                i += 1;
            }
            b']' | b')' => {
                toks.push(Tok::Close);
                i += 1;
            }
            b',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            b'"' => {
                i += 1;
                let start = i;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                toks.push(Tok::Str(s[start..i].to_string()));
                i += 1; // skip the closing quote (a no-op if it was unterminated)
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len()
                    && !matches!(b[i], b'[' | b']' | b'(' | b')' | b',' | b'"')
                    && !b[i].is_ascii_whitespace()
                {
                    i += 1;
                }
                toks.push(Tok::Word(s[start..i].to_string()));
            }
        }
    }
    toks
}

/// The root keyword — `GEOGCS`, `PROJCS`, `GEOGCRS`, … — uppercased.
pub(crate) fn root_keyword(toks: &[Tok]) -> Option<String> {
    match (toks.first(), toks.get(1)) {
        (Some(Tok::Word(kw)), Some(Tok::Open)) => Some(kw.to_ascii_uppercase()),
        _ => None,
    }
}

/// The outermost CRS's own name — the first quoted string right after the root
/// keyword, e.g. `GCS_WGS_1984` in `GEOGCS["GCS_WGS_1984", ...]`.
///
/// This is exactly the spelling `proj.db`'s `alias_name` (Esri) or `name`
/// (every other authority) columns carry — what the generator built
/// [`NAMES`](crate::NAMES) from — so no normalization is needed before looking
/// it up.
pub(crate) fn crs_name(toks: &[Tok]) -> Option<&str> {
    match (toks.first(), toks.get(1), toks.get(2)) {
        (Some(Tok::Word(_)), Some(Tok::Open), Some(Tok::Str(name))) => Some(name),
        _ => None,
    }
}

/// The CRS's ellipsoid as `(semi_major_axis, inverse_flattening)`, from its
/// first `SPHEROID`/`ELLIPSOID` node — WKT1 and WKT2 spell the leading
/// `name, semi-major axis, inverse flattening` triple identically.
///
/// "First" is what makes this work for a `PROJCS` too: the only ellipsoid a
/// projected CRS carries is its base geographic CRS's, nested inside.
pub(crate) fn ellipsoid_params(toks: &[Tok]) -> Option<(f64, f64)> {
    for i in 0..toks.len() {
        let Tok::Word(w) = &toks[i] else { continue };
        if !(w.eq_ignore_ascii_case("SPHEROID") || w.eq_ignore_ascii_case("ELLIPSOID")) {
            continue;
        }
        if let (Some(Tok::Open), Some(Tok::Str(_)), Some(Tok::Comma)) =
            (toks.get(i + 1), toks.get(i + 2), toks.get(i + 3))
        {
            let a = match toks.get(i + 4) {
                Some(Tok::Word(v)) | Some(Tok::Str(v)) => v.parse::<f64>().ok(),
                _ => None,
            };
            let rf = match toks.get(i + 5) {
                Some(Tok::Comma) => match toks.get(i + 6) {
                    Some(Tok::Word(v)) | Some(Tok::Str(v)) => v.parse::<f64>().ok(),
                    _ => None,
                },
                _ => None,
            };
            if let (Some(a), Some(rf)) = (a, rf) {
                return Some((a, rf));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESRI_WGS84: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

    #[test]
    fn reads_the_outer_name() {
        assert_eq!(crs_name(&tokenize(ESRI_WGS84)), Some("GCS_WGS_1984"));
        assert_eq!(crs_name(&tokenize("")), None);
    }

    #[test]
    fn reads_the_first_spheroid() {
        assert_eq!(ellipsoid_params(&tokenize(ESRI_WGS84)), Some((6378137.0, 298.257223563)));
        assert_eq!(ellipsoid_params(&tokenize(r#"GEOGCRS["x"]"#)), None);
    }

    #[test]
    fn reads_a_projected_crs_base_ellipsoid() {
        // The first SPHEROID inside a PROJCS is the base geographic CRS's —
        // there is no other, which is why "first" is the right rule.
        let wkt = r#"PROJCS["p",GEOGCS["g",DATUM["d",SPHEROID["GRS_1980",6378137.0,298.257222101]]],PROJECTION["Transverse_Mercator"],UNIT["Meter",1.0]]"#;
        assert_eq!(ellipsoid_params(&tokenize(wkt)), Some((6378137.0, 298.257222101)));
    }

    #[test]
    fn reads_wkt2_spelling() {
        // WKT2:2019 renames the node and adds LENGTHUNIT, but the leading
        // `name, a, rf` triple is identical — the reason one extractor covers
        // both dialects.
        let wkt = r#"GEOGCRS["WGS 84",ENSEMBLE["World Geodetic System 1984 ensemble",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]]]"#;
        assert_eq!(crs_name(&tokenize(wkt)), Some("WGS 84"));
        assert_eq!(ellipsoid_params(&tokenize(wkt)), Some((6378137.0, 298.257223563)));
    }

    #[test]
    fn reads_the_root_keyword() {
        assert_eq!(root_keyword(&tokenize(ESRI_WGS84)).as_deref(), Some("GEOGCS"));
        assert_eq!(root_keyword(&tokenize(r#"projcs["p",..."#)).as_deref(), Some("PROJCS"));
        assert_eq!(root_keyword(&tokenize("")), None);
        assert_eq!(root_keyword(&tokenize("GEOGCS")), None); // keyword with no body
    }

    #[test]
    fn parenthesized_and_unterminated_input_do_not_panic() {
        assert_eq!(crs_name(&tokenize(r#"GEOGCS("n","#)), Some("n"));
        assert_eq!(crs_name(&tokenize(r#"GEOGCS["unterminated"#)), Some("unterminated"));
    }
}
