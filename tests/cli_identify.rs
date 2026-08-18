//! The `--identify` CLI mode end to end — what lands on stdout, what lands on
//! stderr, and what the exit status is.
//!
//! The contracts these pin down exist so the mode composes safely with
//! `geosetta --crs -` (`plans/wkt-identify.org` § DESIGN / Contracts):
//!
//! - a definition on stdout and *nothing else*, so it can be piped verbatim;
//! - **nothing at all** on stdout when the answer is ambiguous or absent, with
//!   a nonzero exit, so a pipeline fails loudly instead of feeding empty or
//!   arbitrarily-chosen text onward. Geosetta hard-errors on an empty `--crs`
//!   (its `an_empty_override_is_rejected_rather_than_treated_as_no_override`),
//!   so the two contracts compose: an empty identification cannot silently
//!   become "no override".

use std::io::Write;
use std::process::{Command, Stdio};

/// A real `projinfo -o WKT1_ESRI` export with a name only one CRS answers to.
const UNIQUE_PRJ: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
/// Same shape, but a name shared by a live EPSG CRS and its deprecated ESRI
/// twin — the ambiguous case.
const AMBIGUOUS_PRJ: &str = r#"PROJCS["NAD_1983_2011_UTM_Zone_10N",GEOGCS["GCS_NAD_1983_2011",DATUM["D_NAD_1983_2011",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-123.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
/// The right spelling over a fabricated ellipsoid — must never identify.
const LYING_PRJ: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6300000.0,290.0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

fn bin() -> std::path::PathBuf {
    // The integration-test binary sits beside the CLI it exercises.
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("geoscribe")
}

/// Run the CLI with `args`, feeding `stdin_text` (if any) on stdin.
fn run(args: &[&str], stdin_text: Option<&str>) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn geoscribe");
    if let Some(text) = stdin_text {
        child.stdin.take().unwrap().write_all(text.as_bytes()).expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    Output {
        status: out.status.code().expect("exited normally"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write_prj(dir: &std::path::Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write fixture");
    path.to_string_lossy().into_owned()
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("geoscribe-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create tmpdir");
    dir
}

// --- the identifying path --------------------------------------------------

#[test]
fn a_unique_identification_prints_the_definition_and_exits_zero() {
    let out = run(&["--identify"], Some(UNIQUE_PRJ));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.starts_with('{'), "expected PROJJSON, got {:?}", out.stdout);
    assert!(out.stdout.contains(r#""code":4326"#), "{}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr should be quiet: {}", out.stderr);
}

#[test]
fn stdout_carries_the_definition_alone() {
    // Exactly one line, no banner, no trailing commentary — the property that
    // makes `--crs -` on the other end work.
    let out = run(&["--identify", "--wkt"], Some(UNIQUE_PRJ));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.lines().count(), 1, "{:?}", out.stdout);
    assert!(out.stdout.trim_end().starts_with("GEOGCS["), "{}", out.stdout);
}

#[test]
fn every_dialect_flag_composes_with_identify() {
    // `--identify` says where the input comes from; the dialect flags say what
    // the output looks like. Orthogonal, so all three work.
    for (flag, starts) in [("--projjson", "{"), ("--wkt", "GEOGCS["), ("--wkt2", "GEOGCRS[")] {
        let out = run(&["--identify", flag], Some(UNIQUE_PRJ));
        assert_eq!(out.status, 0, "{flag} stderr: {}", out.stderr);
        assert!(out.stdout.starts_with(starts), "{flag} gave {:?}", out.stdout);
    }
}

#[test]
fn it_reads_from_a_file_as_well_as_stdin() {
    let dir = tmpdir("file");
    let path = write_prj(&dir, "unique.prj", UNIQUE_PRJ);
    let from_file = run(&["--identify", &path], None);
    let from_stdin = run(&["--identify"], Some(UNIQUE_PRJ));
    assert_eq!(from_file.status, 0, "stderr: {}", from_file.stderr);
    assert_eq!(from_file.stdout, from_stdin.stdout);
    // `-` is the explicit spelling of the stdin default.
    assert_eq!(run(&["--identify", "-"], Some(UNIQUE_PRJ)).stdout, from_stdin.stdout);
}

// --- the ambiguous path ----------------------------------------------------

#[test]
fn an_ambiguous_identification_prints_nothing_and_exits_two() {
    let out = run(&["--identify"], Some(AMBIGUOUS_PRJ));
    assert_eq!(out.status, 2, "stdout: {:?} stderr: {}", out.stdout, out.stderr);
    assert!(out.stdout.is_empty(), "stdout must stay empty: {:?}", out.stdout);
}

#[test]
fn the_ambiguity_message_names_every_candidate() {
    // The point of refusing to pick is that the user can act on it, which means
    // the message has to say what the alternatives were.
    let out = run(&["--identify"], Some(AMBIGUOUS_PRJ));
    assert!(out.stderr.contains("EPSG:6339"), "{}", out.stderr);
    assert!(out.stderr.contains("ESRI:102057"), "{}", out.stderr);
    assert!(out.stderr.contains("--all"), "should point at the listing mode: {}", out.stderr);
}

#[test]
fn all_lists_the_candidates_on_stdout() {
    let out = run(&["--identify", "--all"], Some(AMBIGUOUS_PRJ));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.lines().collect::<Vec<_>>(), ["EPSG:6339", "ESRI:102057"]);
}

#[test]
fn all_lists_a_single_code_for_an_unambiguous_wkt() {
    let out = run(&["--identify", "--all"], Some(UNIQUE_PRJ));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.lines().collect::<Vec<_>>(), ["EPSG:4326"]);
}

#[test]
fn a_listed_candidate_feeds_straight_back_into_the_trusted_id_form() {
    // The two-step the ambiguity message asks the user to perform: list, pick,
    // resolve. The second step must accept the first step's output verbatim.
    let listed = run(&["--identify", "--all"], Some(AMBIGUOUS_PRJ));
    let chosen = listed.stdout.lines().next().expect("at least one candidate").to_string();
    let out = run(&[&chosen, "--wkt2"], None);
    assert_eq!(out.status, 0, "resolving {chosen}: {}", out.stderr);
    assert!(out.stdout.starts_with("PROJCRS["), "{}", out.stdout);
}

// --- refusing to identify --------------------------------------------------

#[test]
fn a_lying_ellipsoid_is_refused_with_empty_stdout() {
    let out = run(&["--identify"], Some(LYING_PRJ));
    assert_eq!(out.status, 1, "stdout: {:?}", out.stdout);
    assert!(out.stdout.is_empty(), "stdout must stay empty: {:?}", out.stdout);
    assert!(out.stderr.contains("could not identify"), "{}", out.stderr);
}

#[test]
fn an_unidentifiable_wkt_exits_one_not_two() {
    // "nothing fits" and "several fit" are different situations and get
    // different statuses, so a script can tell them apart without reading
    // stderr.
    let unknown = r#"GEOGCS["Totally Made Up Datum XYZ",DATUM["d",SPHEROID["e",6378137.0,298.257223563]]]"#;
    assert_eq!(run(&["--identify"], Some(unknown)).status, 1);
    assert_eq!(run(&["--identify"], Some(AMBIGUOUS_PRJ)).status, 2);
}

#[test]
fn empty_input_is_an_error_rather_than_empty_output() {
    for text in ["", "   \n\t\n"] {
        let out = run(&["--identify"], Some(text));
        assert_eq!(out.status, 1, "for {text:?}");
        assert!(out.stdout.is_empty(), "for {text:?}: {:?}", out.stdout);
        assert!(out.stderr.contains("empty"), "for {text:?}: {}", out.stderr);
    }
}

#[test]
fn a_missing_file_is_reported_not_silently_empty() {
    let out = run(&["--identify", "/nonexistent/parcels.prj"], None);
    assert_eq!(out.status, 1);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(out.stderr.contains("parcels.prj"), "{}", out.stderr);
}

#[test]
fn text_that_is_not_wkt_at_all_is_refused() {
    for text in ["hello", "{\"type\":\"GeographicCRS\"}", "EPSG:4326"] {
        let out = run(&["--identify"], Some(text));
        assert_eq!(out.status, 1, "for {text:?}: {:?}", out.stdout);
        assert!(out.stdout.is_empty(), "for {text:?}: {:?}", out.stdout);
    }
}

#[test]
fn a_wkt_that_already_carries_an_id_still_identifies_by_name() {
    // `--identify` is for id-less text, but it must not choke on WKT that does
    // carry an AUTHORITY node — a caller with such text should use the
    // trusted-id form, and this just confirms the name path still lands on the
    // same CRS rather than erroring.
    let with_id = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]]"#;
    let out = run(&["--identify", "--all"], Some(with_id));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("EPSG:4326"), "{}", out.stdout);
}

// --- the untouched trusted-id form ----------------------------------------

#[test]
fn the_trusted_id_form_is_unchanged() {
    let out = run(&["EPSG:4326", "--wkt"], None);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.starts_with("GEOGCS["), "{}", out.stdout);
    assert_eq!(run(&["EPSG:999999"], None).status, 1);
    assert_eq!(run(&["--all", "EPSG:4326"], None).status, 1, "--all needs --identify");
}
