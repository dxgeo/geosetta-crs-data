//! `geoscribe` — CLI front end for the embedded CRS registry.
//!
//! ```text
//! geoscribe <AUTHORITY:CODE> [--wkt | --wkt2 | --projjson]
//! ```
//!
//! Resolves a trusted `(authority, code)` pair (e.g. `EPSG:4326`) to its
//! authoritative definition and writes exactly that string to stdout — no
//! extra decoration — so it composes with anything downstream that wants a
//! WKT/PROJJSON string on a pipe or in a file (`ogr2ogr -a_srs $(geoscribe
//! ...)`, `geoscribe EPSG:4326 --wkt2 > crs.wkt`, ...). Exit status: `0` on a
//! resolved match, `1` on any error (bad usage, unknown authority/code, or a
//! requested dialect the CRS doesn't have — e.g. `--wkt` for one of the ~4%
//! of entries with no WKT1; see [`geoscribe::CrsRecord`]).
//!
//! Deliberately does *not* expose [`geoscribe::resolve_by_name`] — that's a
//! weaker-evidence lookup (multiple authorities can share a name, so it
//! returns candidates, not an answer) that needs a caller-side trust policy
//! on top of it (see `public-api.org` § BOUNDARY). This CLI only does the
//! trusted-id lookup, so its output is always safe to pipe without a human
//! eyeballing it first.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dialect {
    Projjson,
    Wkt,
    Wkt2,
}

struct Args {
    authority: String,
    code: String,
    dialect: Dialect,
}

const USAGE: &str = "usage: geoscribe <AUTHORITY:CODE> [--wkt | --wkt2 | --projjson]\n  e.g. geoscribe EPSG:4326 --wkt2\n  default dialect is --projjson (the one every entry has)";

fn main() {
    if let Err(e) = run() {
        eprintln!("geoscribe: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse(std::env::args())?;
    let rec = geoscribe::resolve(&args.authority, &args.code)
        .ok_or_else(|| format!("no match for {}:{}", args.authority, args.code))?;

    let (out, dialect_name) = match args.dialect {
        Dialect::Projjson => (Some(rec.projjson), "PROJJSON"),
        Dialect::Wkt => (rec.wkt, "WKT1"),
        Dialect::Wkt2 => (rec.wkt2, "WKT2"),
    };
    let out = out.ok_or_else(|| {
        format!("{}:{} has no {dialect_name} representation", args.authority, args.code)
    })?;

    println!("{out}");
    Ok(())
}

/// Parse arguments from an iterator (typically `std::env::args()`), whose
/// first item is the program name.
fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
    let mut iter = args.into_iter();
    let _program = iter.next();

    let mut dialect = Dialect::Projjson;
    let mut id: Option<String> = None;

    for arg in iter {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "--projjson" => dialect = Dialect::Projjson,
            "--wkt" => dialect = Dialect::Wkt,
            "--wkt2" => dialect = Dialect::Wkt2,
            other if other.starts_with('-') => {
                return Err(format!("unknown option \"{other}\"\n{USAGE}"));
            }
            _ if id.is_none() => id = Some(arg),
            _ => return Err(format!("unexpected extra argument \"{arg}\"\n{USAGE}")),
        }
    }

    let id = id.ok_or_else(|| USAGE.to_string())?;
    let (authority, code) = id
        .split_once(':')
        .ok_or_else(|| format!("expected AUTHORITY:CODE (e.g. EPSG:4326), got \"{id}\""))?;
    if authority.is_empty() || code.is_empty() {
        return Err(format!("expected AUTHORITY:CODE (e.g. EPSG:4326), got \"{id}\""));
    }

    Ok(Args {
        authority: authority.to_string(),
        code: code.to_string(),
        dialect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Result<Args, String> {
        parse(items.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_authority_code() {
        let a = args(&["geoscribe", "EPSG:4326"]).unwrap();
        assert_eq!(a.authority, "EPSG");
        assert_eq!(a.code, "4326");
        assert_eq!(a.dialect, Dialect::Projjson);
    }

    #[test]
    fn string_codes_split_on_first_colon_only() {
        // IGNF/OGC/PROJ/NKG codes are alphanumeric strings, not numbers — the
        // split must not assume a numeric code, and must not choke if a code
        // ever contained a colon itself (none do today, but split_once keeps
        // everything after the first ':' rather than requiring exactly one).
        let a = args(&["geoscribe", "OGC:CRS84"]).unwrap();
        assert_eq!(a.authority, "OGC");
        assert_eq!(a.code, "CRS84");
    }

    #[test]
    fn parses_dialect_flags() {
        assert_eq!(args(&["geoscribe", "EPSG:4326", "--wkt"]).unwrap().dialect, Dialect::Wkt);
        assert_eq!(args(&["geoscribe", "EPSG:4326", "--wkt2"]).unwrap().dialect, Dialect::Wkt2);
        assert_eq!(args(&["geoscribe", "EPSG:4326", "--projjson"]).unwrap().dialect, Dialect::Projjson);
    }

    #[test]
    fn errors_on_bad_usage() {
        assert!(args(&["geoscribe"]).is_err());
        assert!(args(&["geoscribe", "--help"]).is_err());
        assert!(args(&["geoscribe", "EPSG4326"]).is_err()); // no colon
        assert!(args(&["geoscribe", "EPSG:"]).is_err()); // empty code
        assert!(args(&["geoscribe", ":4326"]).is_err()); // empty authority
        assert!(args(&["geoscribe", "EPSG:4326", "--bogus"]).is_err());
        assert!(args(&["geoscribe", "EPSG:4326", "OGC:CRS84"]).is_err()); // two positionals
    }
}
