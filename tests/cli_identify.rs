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
    // stderr is *not* empty any more, by design: since `--identify` began using
    // an inline id when the input carries one, a caller can no longer infer from
    // the mode which evidence produced the answer, so the mode says. One line,
    // and stdout is untouched — see `plans/projjson-identify.org` § DECISIONS.
    assert_eq!(out.stderr.lines().count(), 1, "{:?}", out.stderr);
    assert!(out.stderr.contains("EPSG:4326"), "{}", out.stderr);
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

// --- PROJJSON input --------------------------------------------------------

/// The definition `geosetta --print-crs` emits from an id-less GeoParquet: an
/// Esri-flavor name, no root `id`, `datum` rather than `datum_ensemble`. Built by
/// converting a Shapefile whose `.prj` has no `AUTHORITY` node, which is how such
/// a file comes to exist at all.
const IDLESS_PROJJSON: &str = r#"{"type":"GeographicCRS","name":"GCS_WGS_1984",
    "datum":{"type":"GeodeticReferenceFrame","name":"D_WGS_1984",
    "ellipsoid":{"name":"WGS_1984","semi_major_axis":6378137.0,
    "inverse_flattening":298.257223563}}}"#;

#[test]
fn projjson_input_identifies_the_same_as_its_wkt_spelling() {
    // The dialect gap this mode was extended to close: the same CRS, written the
    // way a container format records it, must reach the same answer.
    let from_projjson = run(&["--identify", "--projjson"], Some(IDLESS_PROJJSON));
    let from_wkt = run(&["--identify", "--projjson"], Some(UNIQUE_PRJ));
    assert_eq!(from_projjson.status, 0, "stderr: {}", from_projjson.stderr);
    assert_eq!(from_projjson.stdout, from_wkt.stdout, "dialects must agree");
    assert!(from_projjson.stderr.contains("by name"), "{}", from_projjson.stderr);
}

#[test]
fn projjson_reads_from_a_file_and_from_stdin_alike() {
    let dir = tmpdir("projjson");
    let path = write_prj(&dir, "idless.projjson", IDLESS_PROJJSON);
    let from_file = run(&["--identify", &path], None);
    let from_stdin = run(&["--identify"], Some(IDLESS_PROJJSON));
    assert_eq!(from_file.status, 0, "stderr: {}", from_file.stderr);
    assert_eq!(from_file.stdout, from_stdin.stdout);
    assert_eq!(run(&["--identify", "-"], Some(IDLESS_PROJJSON)).stdout, from_stdin.stdout);
}

#[test]
fn every_dialect_flag_composes_with_projjson_input_too() {
    // Input dialect is sniffed, output dialect is chosen, and the two never
    // interact — which is why there is no input-dialect flag to collide with.
    for (flag, starts) in [("--projjson", "{"), ("--wkt", "GEOGCS["), ("--wkt2", "GEOGCRS[")] {
        let out = run(&["--identify", flag], Some(IDLESS_PROJJSON));
        assert_eq!(out.status, 0, "{flag} stderr: {}", out.stderr);
        assert!(out.stdout.starts_with(starts), "{flag} gave {:?}", out.stdout);
    }
}

#[test]
fn a_projjson_with_an_inline_id_is_identified_by_it() {
    // Option (a) on the PROJJSON side, and the case that makes the pipeline one
    // unconditional command: the caller does not have to know whether the file's
    // CRS carries an id.
    let with_id = r#"{"type":"GeographicCRS","name":"Anything At All",
        "datum":{"name":"d","ellipsoid":{"name":"e","semi_major_axis":1,
        "inverse_flattening":1}},"id":{"authority":"EPSG","code":4326}}"#;
    let out = run(&["--identify", "--all"], Some(with_id));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim_end(), "EPSG:4326");
    assert!(out.stderr.contains("trusted"), "{}", out.stderr);
    // Note the name and ellipsoid here are nonsense — proof the id was used and
    // name recovery never ran, since that would have declined.
}

#[test]
fn an_unresolvable_inline_id_falls_through_to_name_recovery() {
    // An id that resolves to nothing carried no evidence after all; the name may
    // still. Failing outright would be worse than the name answer we can give.
    let bad_id = r#"{"type":"GeographicCRS","name":"GCS_WGS_1984",
        "datum":{"type":"GeodeticReferenceFrame","name":"D_WGS_1984",
        "ellipsoid":{"name":"WGS_1984","semi_major_axis":6378137.0,
        "inverse_flattening":298.257223563}},
        "id":{"authority":"EPSG","code":999999}}"#;
    let out = run(&["--identify", "--all"], Some(bad_id));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim_end(), "EPSG:4326");
    assert!(out.stderr.contains("by name"), "and it says so: {}", out.stderr);
}

#[test]
fn an_ambiguous_projjson_exits_two_with_empty_stdout() {
    // The trust policy does not change with the dialect. "WGS 84" fits EPSG:4326
    // and EPSG:4979 equally — same family, same ellipsoid, nothing to choose on.
    let ambiguous = r#"{"type":"GeographicCRS","name":"WGS 84",
        "datum_ensemble":{"name":"World Geodetic System 1984 ensemble",
        "ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,
        "inverse_flattening":298.257223563}}}"#;
    let out = run(&["--identify"], Some(ambiguous));
    assert_eq!(out.status, 2, "stdout: {:?}", out.stdout);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(out.stderr.contains("EPSG:4326") && out.stderr.contains("EPSG:4979"), "{}", out.stderr);
}

#[test]
fn json_that_is_not_a_crs_is_refused_as_json_not_as_wkt() {
    // The sniff is total: a JSON document takes the JSON path and stays there.
    // Falling back to the WKT tokenizer would turn a clear "this is not a CRS"
    // into a confusing complaint about a `{`.
    for text in [r#"{"hello":"world"}"#, "{}", r#"{"type":"GeographicCRS"}"#] {
        let out = run(&["--identify"], Some(text));
        assert_eq!(out.status, 1, "for {text:?}: {:?}", out.stdout);
        assert!(out.stdout.is_empty(), "for {text:?}: {:?}", out.stdout);
        assert!(
            !out.stderr.to_lowercase().contains("wkt"),
            "for {text:?} the message must not blame WKT: {}",
            out.stderr
        );
    }
}

#[test]
fn the_sniff_reads_leading_whitespace_and_inner_braces_correctly() {
    // Leading whitespace is what a tool's stdout actually looks like, and a `{`
    // anywhere after the first token does not make a WKT into JSON.
    let padded = format!("\n\t  {IDLESS_PROJJSON}\n");
    assert_eq!(run(&["--identify", "--all"], Some(&padded)).stdout.trim_end(), "EPSG:4326");

    let wkt_with_brace = r#"GEOGCS["GCS_WGS_1984{not json}",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    let out = run(&["--identify"], Some(wkt_with_brace));
    // The name no longer matches anything, but it must fail as *WKT* — declining
    // to identify — not as malformed JSON.
    assert_eq!(out.status, 1);
    assert!(out.stderr.contains("could not identify"), "{}", out.stderr);
}

#[test]
fn binary_input_says_it_is_not_a_definition() {
    // Pointing --identify at a data file is an easy mistake — data files are
    // where CRSes live. `std`'s "stream did not contain valid UTF-8" is accurate
    // and no help; this names the actual mistake.
    let dir = tmpdir("binary");
    let path = dir.join("parcels.parquet");
    // Invalid UTF-8, as any real container's bytes would be.
    std::fs::write(&path, [0x50, 0x41, 0x52, 0x31, 0xff, 0xfe, 0x00, 0x80]).unwrap();

    let out = run(&["--identify", path.to_str().unwrap()], None);
    assert_eq!(out.status, 1);
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(out.stderr.contains("not text"), "{}", out.stderr);
    assert!(out.stderr.contains("data file"), "{}", out.stderr);
    assert!(!out.stderr.contains("UTF-8"), "the std wording must not leak: {}", out.stderr);
    // Tool-agnostic: it says what to do, never who should do it.
    assert!(!out.stderr.contains("geosetta"), "{}", out.stderr);
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
fn a_wkt_that_carries_an_id_is_identified_by_that_id() {
    // Option (a): a stated id is strictly stronger evidence than a name, so it
    // is used rather than ignored. The user piping a file's CRS does not know in
    // advance whether it has one, and this is the tool answering that instead of
    // the shell.
    //
    // The nested ids here are the point of the "shallowest wins" rule: the
    // ellipsoid (7030), datum (6326), prime meridian (8901), and unit (9122) all
    // carry their own, and only the root 4326 identifies the CRS.
    let with_id = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]]"#;
    let out = run(&["--identify", "--all"], Some(with_id));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim_end(), "EPSG:4326", "the id, not a candidate list");
    assert!(out.stderr.contains("trusted"), "provenance must say so: {}", out.stderr);

    // And the bare name "WGS 84" is genuinely ambiguous (EPSG:4326 vs 4979), so
    // this is also proof the id path ran instead of name recovery.
    let id_less = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]]"#;
    let out = run(&["--identify", "--all"], Some(id_less));
    assert_eq!(out.stdout.lines().count(), 2, "{:?}", out.stdout);
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
