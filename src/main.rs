//! `geoscribe` — CLI front end for the embedded CRS registry.
//!
//! ```text
//! geoscribe <AUTHORITY:CODE> [--wkt | --wkt2 | --projjson]
//! geoscribe --identify [--all] [--wkt | --wkt2 | --projjson] [FILE | -]
//! ```
//!
//! The first form resolves a trusted `(authority, code)` pair (e.g.
//! `EPSG:4326`) to its authoritative definition and writes exactly that string
//! to stdout — no extra decoration — so it composes with anything downstream
//! that wants a WKT/PROJJSON string on a pipe or in a file (`ogr2ogr -a_srs
//! $(geoscribe ...)`, `geoscribe EPSG:4326 --wkt2 > crs.wkt`, ...).
//!
//! The second form covers the case the first cannot: a definition with no pair
//! to look up — an Esri-flavor Shapefile `.prj`, which carries no `AUTHORITY`
//! node, or the PROJJSON a container format hands over. It reads that text
//! (from `FILE`, or stdin) and identifies it *by the strongest evidence the
//! definition carries* ([`geoscribe::identify`]): the dialect is sniffed, an
//! inline id is used when there is one, and otherwise the name is recovered and
//! validated against the definition's own ellipsoid. The payoff, on the
//! `geosetta` side:
//!
//! ```text
//! geoscribe --identify --projjson parcels.prj \
//!   | geosetta parcels.shp parcels.parquet --crs -
//!
//! geosetta parcels.parquet --print-crs \
//!   | geoscribe --identify --projjson \
//!   | geosetta parcels.parquet out.parquet --crs -
//! ```
//!
//! Because the caller cannot tell from the mode alone which evidence produced
//! an answer, `--identify` says so on one line of stderr. stdout is unchanged
//! — the definition, or nothing — so this costs a pipeline nothing.
//!
//! `--identify` makes a *validated* guarantee, not a weak one: where it falls
//! back to a name it still checks that name against the definition's own
//! ellipsoid, which is why it never guesses and never picks. Where several
//! real CRSes share a name and an ellipsoid it writes *nothing* to stdout, lists the candidates on stderr, and exits `2`, leaving
//! the choice to whoever can actually make it. `--all` prints those candidates
//! as `AUTHORITY:CODE` lines on stdout for a human to adjudicate; that output
//! is a list of codes, not a definition, so it is for reading, not for piping
//! into a `--crs` flag.
//!
//! Exit status: `0` on success, `2` on an ambiguous `--identify`, `1` on any
//! other error (bad usage, unknown authority/code, unreadable input, no
//! identification, or a requested dialect the CRS doesn't have — e.g. `--wkt`
//! for one of the ~4% of entries with no WKT1; see [`geoscribe::CrsRecord`]).
//!
//! Deliberately does *not* expose [`geoscribe::resolve_by_name`] — that's a
//! raw lookup returning candidates with no validation at all, which needs a
//! caller-side trust policy on top of it (`plans/public-api.org` § BOUNDARY).
//! `--identify` is that policy, applied and documented as the weaker mode;
//! `resolve_by_name` itself stays library-only.

use std::io::Read;

use geoscribe::{Evidence, Identity};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dialect {
    Projjson,
    Wkt,
    Wkt2,
}

/// Where `--identify` reads its WKT from.
#[derive(Clone, Debug, PartialEq)]
enum Source {
    Stdin,
    Path(String),
}

#[derive(Debug, PartialEq)]
enum Mode {
    /// Trusted-id lookup — the default form.
    Resolve { authority: String, code: String },
    /// Identify an id-less WKT. `list_all` swaps the definition on stdout for
    /// the `AUTHORITY:CODE` list of every validating candidate.
    Identify { source: Source, list_all: bool },
}

#[derive(Debug, PartialEq)]
struct Args {
    mode: Mode,
    dialect: Dialect,
}

/// Exit status for an ambiguous `--identify`, distinct from `1` so a script
/// can tell "several CRSes fit, pick one" from "nothing fits" without parsing
/// stderr.
const EXIT_AMBIGUOUS: i32 = 2;

const USAGE: &str = "usage: geoscribe <AUTHORITY:CODE> [--wkt | --wkt2 | --projjson]\n       geoscribe --identify [--all] [--wkt | --wkt2 | --projjson] [FILE | -]\n  e.g. geoscribe EPSG:4326 --wkt2\n       geoscribe --identify --projjson parcels.prj\n  default dialect is --projjson (the one every entry has)\n  --identify reads a CRS definition (WKT or PROJJSON, sniffed) from FILE or\n  stdin and identifies it by the strongest evidence it carries -- an inline id\n  if present, else its name validated against its own ellipsoid. It exits 2\n  without printing when several CRSes fit; --all lists those candidates.\n  Which evidence was used is reported on stderr.";

fn main() {
    match run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("geoscribe: {}", e.message);
            std::process::exit(e.status);
        }
    }
}

/// A CLI failure: what to print, and what to exit with.
struct Failure {
    message: String,
    status: i32,
}

impl From<String> for Failure {
    fn from(message: String) -> Failure {
        Failure { message, status: 1 }
    }
}

fn run() -> Result<(), Failure> {
    let args = parse(std::env::args())?;
    match args.mode {
        Mode::Resolve { authority, code } => {
            let rec = geoscribe::resolve(&authority, &code)
                .ok_or_else(|| format!("no match for {authority}:{code}"))?;
            println!("{}", dialect_of(&rec, args.dialect, &authority, &code)?);
            Ok(())
        }
        Mode::Identify { source, list_all } => identify(source, list_all, args.dialect),
    }
}

fn identify(source: Source, list_all: bool, dialect: Dialect) -> Result<(), Failure> {
    let text = read_source(&source)?;
    if text.trim().is_empty() {
        return Err(
            format!("{}: no definition to identify (input was empty)", source_label(&source))
                .into(),
        );
    }

    let (identity, evidence) = geoscribe::identify(&text);
    // Provenance on stderr, before the answer on stdout: a caller can no longer
    // infer from the mode alone whether a trusted id or a validated name
    // produced this, and the distinction is the whole point of the mode.
    if let Identity::Unique(rec) = &identity {
        match evidence {
            Evidence::InlineId => eprintln!(
                "identified {}:{} from the definition's own id (trusted)",
                rec.authority, rec.code
            ),
            Evidence::ValidatedName => eprintln!(
                "identified {}:{} by name, validated against the definition's ellipsoid",
                rec.authority, rec.code
            ),
        }
    }

    match identity {
        Identity::Unique(rec) if list_all => {
            println!("{}:{}", rec.authority, rec.code);
            Ok(())
        }
        Identity::Unique(rec) => {
            println!("{}", dialect_of(&rec, dialect, rec.authority, rec.code)?);
            Ok(())
        }
        Identity::Ambiguous(recs) if list_all => {
            for rec in recs {
                println!("{}:{}", rec.authority, rec.code);
            }
            Ok(())
        }
        // Nothing on stdout: a pipeline that feeds this onward must fail
        // loudly rather than receive one of several equally-supported answers.
        Identity::Ambiguous(recs) => {
            let list = recs
                .iter()
                .map(|r| format!("  {}:{}", r.authority, r.code))
                .collect::<Vec<_>>()
                .join("\n");
            Err(Failure {
                message: format!(
                    "{} CRSes match this WKT's name and ellipsoid equally well; \
                     re-run with the one you want (or --all to list them):\n{list}",
                    recs.len()
                ),
                status: EXIT_AMBIGUOUS,
            })
        }
        Identity::Unidentified => Err(format!(
            "could not identify {}: no registry CRS matches its name and ellipsoid",
            source_label(&source)
        )
        .into()),
    }
}

fn dialect_of(
    rec: &geoscribe::CrsRecord,
    dialect: Dialect,
    authority: &str,
    code: &str,
) -> Result<&'static str, Failure> {
    let (out, name) = match dialect {
        Dialect::Projjson => (Some(rec.projjson), "PROJJSON"),
        Dialect::Wkt => (rec.wkt, "WKT1"),
        Dialect::Wkt2 => (rec.wkt2, "WKT2"),
    };
    out.ok_or_else(|| format!("{authority}:{code} has no {name} representation").into())
}

fn read_source(source: &Source) -> Result<String, Failure> {
    let mut text = String::new();
    match source {
        Source::Stdin => std::io::stdin()
            .read_to_string(&mut text)
            .map(|_| ())
            .map_err(|e| not_text("stdin", &e)),
        Source::Path(path) => std::fs::read_to_string(path)
            .map(|s| text = s)
            .map_err(|e| not_text(path, &e)),
    }?;
    Ok(text)
}

/// The message for a failed read, naming the actual mistake when the input is
/// not text at all.
///
/// Pointing `--identify` at a `.parquet` or a `.gpkg` is an easy thing to do —
/// they are where CRSes live — and `std`'s own words for it ("stream did not
/// contain valid UTF-8") are accurate and no help whatever in explaining that
/// this mode reads a *definition*, not a data file.
///
/// Deliberately names no tool. Which reader the user should reach for is their
/// business; this crate's tool-agnosticism cuts both ways, and it has no more
/// standing to assert that than the tools on the other end have to assume it.
fn not_text(label: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::InvalidData {
        return format!(
            "\"{label}\": not text — --identify reads a CRS definition (WKT or PROJJSON), \
             not a data file. To identify the CRS inside a data file, have the tool that \
             reads that format print its definition first."
        );
    }
    format!("reading \"{label}\": {e}")
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Stdin => "stdin".to_string(),
        Source::Path(p) => format!("\"{p}\""),
    }
}

/// Parse arguments from an iterator (typically `std::env::args()`), whose
/// first item is the program name.
fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
    let mut iter = args.into_iter();
    let _program = iter.next();

    let mut dialect = Dialect::Projjson;
    let mut identify = false;
    let mut list_all = false;
    let mut positional: Option<String> = None;

    for arg in iter {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "--projjson" => dialect = Dialect::Projjson,
            "--wkt" => dialect = Dialect::Wkt,
            "--wkt2" => dialect = Dialect::Wkt2,
            "--identify" => identify = true,
            "--all" => list_all = true,
            // `-` alone is the conventional "stdin" positional, not a flag.
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option \"{other}\"\n{USAGE}"));
            }
            _ if positional.is_none() => positional = Some(arg),
            _ => return Err(format!("unexpected extra argument \"{arg}\"\n{USAGE}")),
        }
    }

    if !identify {
        if list_all {
            return Err(format!("--all applies to --identify only\n{USAGE}"));
        }
        let id = positional.ok_or_else(|| USAGE.to_string())?;
        let (authority, code) = id
            .split_once(':')
            .ok_or_else(|| format!("expected AUTHORITY:CODE (e.g. EPSG:4326), got \"{id}\""))?;
        if authority.is_empty() || code.is_empty() {
            return Err(format!("expected AUTHORITY:CODE (e.g. EPSG:4326), got \"{id}\""));
        }
        return Ok(Args {
            mode: Mode::Resolve {
                authority: authority.to_string(),
                code: code.to_string(),
            },
            dialect,
        });
    }

    let source = match positional {
        None => Source::Stdin,
        Some(p) if p == "-" => Source::Stdin,
        Some(p) => Source::Path(p),
    };
    Ok(Args {
        mode: Mode::Identify { source, list_all },
        dialect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Result<Args, String> {
        parse(items.iter().map(|s| s.to_string()))
    }

    fn resolve_mode(items: &[&str]) -> (String, String) {
        match args(items).unwrap().mode {
            Mode::Resolve { authority, code } => (authority, code),
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    #[test]
    fn parses_authority_code() {
        assert_eq!(resolve_mode(&["geoscribe", "EPSG:4326"]), ("EPSG".into(), "4326".into()));
        assert_eq!(args(&["geoscribe", "EPSG:4326"]).unwrap().dialect, Dialect::Projjson);
    }

    #[test]
    fn string_codes_split_on_first_colon_only() {
        // IGNF/OGC/PROJ/NKG codes are alphanumeric strings, not numbers — the
        // split must not assume a numeric code, and must not choke if a code
        // ever contained a colon itself (none do today, but split_once keeps
        // everything after the first ':' rather than requiring exactly one).
        assert_eq!(resolve_mode(&["geoscribe", "OGC:CRS84"]), ("OGC".into(), "CRS84".into()));
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

    #[test]
    fn identify_defaults_to_stdin() {
        let a = args(&["geoscribe", "--identify"]).unwrap();
        assert_eq!(a.mode, Mode::Identify { source: Source::Stdin, list_all: false });
        assert_eq!(a.dialect, Dialect::Projjson);
    }

    #[test]
    fn identify_takes_a_path_or_a_dash() {
        assert_eq!(
            args(&["geoscribe", "--identify", "parcels.prj"]).unwrap().mode,
            Mode::Identify { source: Source::Path("parcels.prj".into()), list_all: false }
        );
        // `-` is the conventional stdin positional, not an unknown flag.
        assert_eq!(
            args(&["geoscribe", "--identify", "-"]).unwrap().mode,
            Mode::Identify { source: Source::Stdin, list_all: false }
        );
    }

    #[test]
    fn identify_composes_with_every_dialect_flag() {
        // `--identify` says where the *input* comes from; the dialect flags say
        // what the output looks like. They are orthogonal by design.
        for (flag, want) in [("--wkt", Dialect::Wkt), ("--wkt2", Dialect::Wkt2), ("--projjson", Dialect::Projjson)] {
            let a = args(&["geoscribe", "--identify", flag, "parcels.prj"]).unwrap();
            assert_eq!(a.dialect, want);
            assert!(matches!(a.mode, Mode::Identify { .. }));
        }
    }

    #[test]
    fn all_requires_identify() {
        assert_eq!(
            args(&["geoscribe", "--identify", "--all"]).unwrap().mode,
            Mode::Identify { source: Source::Stdin, list_all: true }
        );
        assert!(args(&["geoscribe", "EPSG:4326", "--all"]).is_err());
        assert!(args(&["geoscribe", "--all"]).is_err());
    }

    #[test]
    fn identify_does_not_parse_its_positional_as_an_authority_code() {
        // A path is a path, even one with a colon in it.
        assert_eq!(
            args(&["geoscribe", "--identify", "a:b.prj"]).unwrap().mode,
            Mode::Identify { source: Source::Path("a:b.prj".into()), list_all: false }
        );
    }
}
