# geoscribe

A **geospatial metadata reader/writer**: it owns the metadata a consumer
like [Geosetta](https://github.com/dxgeo/geosetta) needs but can't just
carry through unchanged from a source — anything that has to be *resolved
or computed*, not copied. Two capabilities today:

- An embedded **CRS registry** — coordinate reference system definitions
  (PROJJSON + WKT1 + WKT2:2019) plus a name → (authority, code) index,
  covering **all 13,790 CRSes in PROJ's `proj.db`** — every authority
  (EPSG, ESRI, IGNF, OGC, IAU_2015 planetary, PROJ, NKG) and type. The
  original reason this crate exists at all: PROJ/EPSG/Esri/IGN/IAU/NKG's
  data terms don't belong bundled into a pure-MIT library by default (see
  [`NOTICE`](NOTICE)), so the *data* lives here, isolated. On top of the
  trusted-id lookup sits **identification**: recovering a CRS from an
  *id-less* WKT — an Esri-flavor Shapefile `.prj`, which carries no
  `AUTHORITY` node — by its name, validated against that WKT's own
  ellipsoid ([`plans/wkt-identify.org`](plans/wkt-identify.org)).
- Dimension-aware geometry **envelope (bounding-box) computation** —
  scoping only so far, see [`plans/envelope-computation.org`](plans/envelope-computation.org).
  Unlike the CRS registry, this needs no embedded dataset at all — it's
  pure arithmetic over a geometry's own coordinates — but it's the same
  *shape* of problem (a value the consumer must derive, not one the source
  handed it), which is why it lands here rather than in `geosetta` itself.

Originally built as just the CRS registry for Geosetta; broadened to this
wider charter on 2026-08-17 once envelope computation showed the "isolate
third-party data" rationale was really one instance of a bigger one: *own
whatever geospatial metadata isn't a simple pass-through.* Kept as a
standalone, reusable crate rather than data/logic owned by Geosetta itself.

This crate exists to **own that metadata so consumers don't have to
re-derive or re-implement it themselves**:

- CRS decoding (zstd) and lookup are **owned here**
  (`resolve`/`resolve_by_name`/`all`, see § Library usage below) —
  `geosetta`'s own copy of this logic was deleted when it switched to
  calling through. This crate still has **no external dependencies**: its
  zstd decoder is its own from-scratch port, independent of `geosetta`'s (a
  deliberate, documented duplication — see `plans/public-api.org` § MODULE
  LAYOUT — since `geosetta` also needs zstd for an unrelated purpose,
  decoding GeoParquet's compressed pages).
- The core `geosetta` crate stays **pure-MIT and dependency-free**, which is
  why the data is isolated here at all. As of its 0.24.0 it doesn't link
  against this crate *at all* — its `crs-registry` feature is gone — and
  composes over a **pipe** instead (see § Integration with Geosetta below).
  The CLI is therefore a first-class interface here, not a convenience
  wrapper. (Whether a future non-CRS capability like envelope computation
  changes that calculus is open — see `plans/envelope-computation.org`
  § OPEN QUESTIONS.)
- The embedded CRS **data** specifically is a *derived* representation of
  PROJ/EPSG/Esri/IGN/IAU/NKG and is governed by those sources' terms — see
  [`NOTICE`](NOTICE). The Rust code throughout this crate is MIT (see
  [`LICENSE`](LICENSE)).

## Library usage

```toml
# Cargo.toml
[dependencies]
geoscribe = "0.4.0"
```

```rust
let rec = geoscribe::resolve("EPSG", "4326").expect("EPSG:4326 present");
assert_eq!(rec.projjson.contains("WGS 84"), true);
assert!(rec.wkt.is_some());  // None for the ~4% of entries WKT1 can't express
assert!(rec.wkt2.is_some()); // present for effectively every entry

for (authority, code) in geoscribe::resolve_by_name("GCS_WGS_1984") {
    // multiple authorities can share a catalog name — this is a raw lookup
    // that validates nothing; see plans/public-api.org § BOUNDARY
}
```

Or let the crate do the validating — identify an id-less `.prj` by name,
checked against its own ellipsoid. It never guesses, and where several real
CRSes fit equally well it never picks one:

```rust
use geoscribe::Identity;

match geoscribe::identify_from_wkt(&std::fs::read_to_string("parcels.prj")?) {
    Identity::Unique(rec) => println!("{}:{}", rec.authority, rec.code),
    Identity::Ambiguous(recs) => { /* several fit equally well — you choose */ }
    Identity::Unidentified => { /* nothing fits — decline, don't guess */ }
}
```

The full public surface is `resolve`, `resolve_by_name`, `identify_from_wkt`,
`all`, `CrsRecord`, `Identity`, `CRS_COUNT`, and `DATASET_VERSIONS` (see
`src/lib.rs` doc comments, `plans/public-api.org`, and
`plans/wkt-identify.org`).

## CLI

```
$ geoscribe EPSG:4326                      # PROJJSON (default — the one every entry has)
$ geoscribe EPSG:3857 --wkt2               # WKT2:2019
$ geoscribe OGC:CRS84 --wkt                # WKT1 (GDAL flavor); errors if absent
```

Writes exactly the requested definition to stdout, nothing else — composes
with anything that wants a WKT/PROJJSON string on a pipe or in a file, e.g.
`ogr2ogr -a_srs $(geoscribe EPSG:4326)` or `geoscribe EPSG:3857 --wkt2 >
crs.wkt`. Exit `1` with a message on stderr for an unknown code or a
requested dialect the CRS doesn't have.

### `--identify`: when there is no code to look up

An Esri-flavor Shapefile `.prj` carries no `AUTHORITY` node, so there is no
pair to look up and nothing for `geosetta --print-crs-code` to report.
`--identify` reads that WKT (from a file, or stdin) and identifies it by name,
validated against the WKT's own ellipsoid. Same output contract — the
definition alone on stdout — so it drops straight into a pipeline:

```
$ geoscribe --identify --projjson parcels.prj \
    | geosetta parcels.shp parcels.parquet --crs -
```

This is **weaker evidence** than a stated code (a name plus an ellipsoid), so
it declines rather than guessing, and where several real CRSes fit equally well
it writes **nothing** to stdout, lists them on stderr, and exits `2`:

```
$ geoscribe --identify utm.prj
geoscribe: 2 CRSes match this WKT's name and ellipsoid equally well;
re-run with the one you want (or --all to list them):
  EPSG:6339
  ESRI:102057

$ geoscribe --identify --all utm.prj    # the codes alone, for adjudication
EPSG:6339
ESRI:102057

$ geoscribe EPSG:6339 --wkt2             # then resolve the one you chose
```

The empty stdout is deliberate: `geosetta --crs -` hard-errors on an empty
override, so an ambiguous or failed identification fails the pipeline loudly
instead of quietly writing a mislabeled file. `--all` prints codes, not a
definition, so it is for reading — not for piping into `--crs`.

The CLI still never exposes the raw `resolve_by_name`, which validates nothing
at all — see `src/main.rs`'s module doc comment.

## Integration with Geosetta

[Geosetta](https://github.com/dxgeo/geosetta) is this crate's original
consumer. It used to link against this crate under an opt-in `crs-registry`
Cargo feature; **as of Geosetta 0.24.0 it does not depend on this crate at
all** — that feature was removed, and CRS resolution became run-time
composition over text instead:

```
# a CRS Geosetta can see the code for but not resolve
$ geoscribe "$(geosetta in.fgb --print-crs-code)" --projjson \
    | geosetta in.fgb out.parquet --crs -

# a CRS with no code at all (an Esri .prj)
$ geoscribe --identify --projjson parcels.prj \
    | geosetta parcels.shp parcels.parquet --crs -
```

Geosetta needed **zero changes** for the second form: its `--crs` flag already
accepted arbitrary WKT/PROJJSON text from a file or stdin and neither knows nor
cares what produced it. That is the payoff of its text-in design, and the
reason the identification work belongs entirely on this side of the boundary
(`plans/wkt-identify.org`). See Geosetta's `project.org`
§ "Recommended resolvers" for the other end of the pipe. How a second, non-CRS
capability (envelope computation) would be consumed — a revived Cargo feature,
more piping, or something else — is not yet decided; see
`plans/envelope-computation.org` § OPEN QUESTIONS.

## Status

**Built, not yet published under this name.** `v0.2.0` shipped on crates.io
under this crate's original name, `geosetta-crs-data`
([crates.io](https://crates.io/crates/geosetta-crs-data)). The crate has
since been renamed to its current name, `geoscribe`
([github.com/dxgeo/geoscribe](https://github.com/dxgeo/geoscribe)) — GitHub
rename done. Publishing `v0.4.0` (R6's public API plus `identify_from_wkt`) under
`geoscribe` to crates.io is next; once that's live, every `geosetta-crs-data` version
will be yanked there to point dependents at the new name.
The `GCR1` v2 blob (`src/registry.bin.zst`, 1.07 MB compressed) embeds
PROJJSON + WKT1 + WKT2:2019 for all 13,790 `proj.db` CRSes, generated by
`tools/gen_crs_registry.py`. See § Integration with Geosetta above for how
that's consumed downstream (R1, R2, R5).
Full design plan: [`plans/crs-registry.org`](plans/crs-registry.org); container format:
[`plans/registry-format.org`](plans/registry-format.org); public API plan (R6, done):
[`plans/public-api.org`](plans/public-api.org).

## Layout

```
Cargo.toml             crate manifest (no external dependencies)
src/lib.rs              public API: CrsRecord, resolve, resolve_by_name, all
src/registry.rs          GCR1 decode + lookup internals
src/zstd.rs              this crate's own zstd decoder (RFC 8878, from scratch)
src/identify.rs          identify_from_wkt: name + ellipsoid recovery, ambiguity-aware
src/wkt.rs               shallow WKT lexer (ported from geosetta, not shared)
src/json.rs              minimal read-only JSON reader for the registry's own PROJJSON
src/main.rs              the `geoscribe` CLI
src/generated.rs         generated: blob + sizes + versions
src/names.rs              generated: name -> (authority, code) index (20,760 entries)
src/registry.bin.zst      generated: the GCR1 v2 blob (1.07 MB, 13,790 CRSes)
tests/                  integration tests
  identify_esri.rs       the Esri corpus: equivalence, properties, projinfo oracles
  cli_identify.rs        --identify's stdout/stderr/exit-status contracts
  fixtures/              431 geographic + 2,274 projected real WKT1_ESRI exports
tools/                  build-time generators (need PROJ's projinfo + proj.db)
project.org             what this crate is: status, architecture, roadmap
handoff.org             orientation doc for picking up the CRS work
plans/                  design notes, one self-contained plan per file
  README.org             the plan index, ordered by dependency
  todo.org               live working items (what's open right now)
  crs-registry.org       the design plan (R1-R5)
  registry-format.org    the GCR1 container-format spec
  public-api.org         the public API plan (R6)
  wkt-identify.org       id-less WKT identification (implemented)
  envelope-computation.org  dimension-aware bbox computation (scoping only)
NOTICE                  third-party data attributions and terms
LICENSE                 MIT (covers the Rust code only)
LICENSES/                per-source license texts (Apache-2.0 for Esri)
```
