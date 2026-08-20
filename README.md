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
  trusted-id lookup sits **identification**: recovering a CRS from a
  definition that states no code — an Esri-flavor Shapefile `.prj`, which
  carries no `AUTHORITY` node, or the PROJJSON a container format hands over
  — by its name, validated against that definition's own ellipsoid
  ([`plans/wkt-identify.org`](plans/wkt-identify.org),
  [`plans/projjson-identify.org`](plans/projjson-identify.org)).
Envelope (bounding-box) computation was briefly planned here as a second
capability. It was **retired on 2026-08-18 without being implemented** and moved
to `geosetta` (the fold) and `nazca` (transforming a bbox across CRSes) — see
[`plans/envelope-computation.org`](plans/envelope-computation.org) § STATUS.
`geosetta` already owned a public `Bbox` fold and already wrote GeoParquet's
`geo` bbox from it, so the "a value the consumer must derive" argument was
describing something the consumer already had.

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
  wrapper, and nothing on the horizon changes that — the three crates carry no
  dependency edges at all (see `../architecture.org`), so a pipe or a
  caller-side `impl` is how this crate is reached, by design rather than by
  default.
- The embedded CRS **data** specifically is a *derived* representation of
  PROJ/EPSG/Esri/IGN/IAU/NKG and is governed by those sources' terms — see
  [`NOTICE`](NOTICE). The Rust code throughout this crate is MIT (see
  [`LICENSE`](LICENSE)).

## Library usage

```toml
# Cargo.toml
[dependencies]
geoscribe = "0.5.0"
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

Or let the crate do the validating — identify a definition by name, checked
against its own ellipsoid. It never guesses, and where several real CRSes fit
equally well it never picks one:

```rust
use geoscribe::Identity;

match geoscribe::identify_from_wkt(&std::fs::read_to_string("parcels.prj")?) {
    Identity::Unique(rec) => println!("{}:{}", rec.authority, rec.code),
    Identity::Ambiguous(recs) => { /* several fit equally well — you choose */ }
    Identity::Unidentified => { /* nothing fits — decline, don't guess */ }
}
```

`identify_from_projjson` is the same thing for the other dialect. When you do
not know which you have — the usual case when the text arrived over a pipe —
`identify` sniffs it, uses an inline `id` if the definition states one, and
tells you which evidence it used:

```rust
use geoscribe::{Evidence, Identity};

let (identity, evidence) = geoscribe::identify(&text);
if let (Identity::Unique(rec), Evidence::ValidatedName) = (&identity, evidence) {
    eprintln!("{}:{} recovered by name, not stated", rec.authority, rec.code);
}
```

The full public surface is `resolve`, `resolve_by_name`, `identify`,
`identify_from_wkt`, `identify_from_projjson`, `all`, `CrsRecord`, `Identity`,
`Evidence`, `CRS_COUNT`, and `DATASET_VERSIONS` (see `src/lib.rs` doc comments,
`plans/public-api.org`, `plans/wkt-identify.org`, and
`plans/projjson-identify.org`).

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
`--identify` reads that definition (from a file, or stdin) and identifies it by
the strongest evidence it carries. Same output contract — the definition alone
on stdout — so it drops straight into a pipeline:

```
$ geoscribe --identify --projjson parcels.prj \
    | geosetta parcels.shp parcels.parquet --crs -
```

**WKT or PROJJSON, sniffed, not flagged.** The first non-whitespace `{` decides;
there is no input-dialect flag, because `--wkt`/`--wkt2`/`--projjson` already
mean the *output* dialect and two flag families sharing three names with
opposite meanings would be a footgun. That matters most for the formats that
bury their CRS in a container: GeoParquet records PROJJSON, so the definition
`geosetta --print-crs` prints out of one arrives here as JSON.

```
$ geosetta parcels.parquet --print-crs \
    | geoscribe --identify --projjson \
    | geosetta parcels.parquet parcels.fgb --crs -
```

**An inline `id` wins when the definition states one**, since a stated code is
stronger evidence than a recovered name — so the pipeline above is one
unconditional command whether or not the file's CRS has an id. Because you can
no longer tell from the mode alone which happened, `--identify` reports it on
one line of stderr (stdout is unchanged, so pipelines are unaffected):

```
$ geoscribe --identify --all parcels.prj
identified EPSG:4326 by name, validated against the definition's ellipsoid
EPSG:4326
```

Name recovery is **weaker evidence** than a stated code, so it declines rather
than guessing, and where several real CRSes fit equally well it writes
**nothing** to stdout, lists them on stderr, and exits `2`:

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
§ "Recommended resolvers" for the other end of the pipe. There is no plan to
revive a Cargo feature for this: `geosetta` and `nazca` both depend on nothing,
and reach this crate over a pipe or through a caller-supplied implementation
(see `../architecture.org`).

## Status

**Published under this name.** The crate began life as `geosetta-crs-data` and
was renamed to `geoscribe`
([github.com/dxgeo/geoscribe](https://github.com/dxgeo/geoscribe)); the rename
is complete on both ends — releases go out under the new name on
[crates.io](https://crates.io/crates/geoscribe), and every `geosetta-crs-data`
version is yanked, pointing dependents here.

The registry (R1–R5, R6a–R6d) and id-less identification in both dialects
(`identify`, `identify_from_wkt`, `identify_from_projjson`, `Evidence`) are
done; see [`plans/README.org`](plans/README.org) for the plan index and
[`plans/todo.org`](plans/todo.org) for what is open.
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
src/identify.rs          identify/_from_wkt/_from_projjson: name + ellipsoid recovery
src/wkt.rs               shallow WKT lexer (ported from geosetta, not shared)
src/json.rs              minimal read-only JSON reader for the registry's own PROJJSON
src/main.rs              the `geoscribe` CLI
src/generated.rs         generated: blob + sizes + versions
src/names.rs              generated: name -> (authority, code) index (20,760 entries)
src/registry.bin.zst      generated: the GCR1 v2 blob (1.07 MB, 13,790 CRSes)
tests/                  integration tests
  identify_esri.rs       the Esri corpus: equivalence, properties, projinfo oracles
  cli_identify.rs        --identify's stdout/stderr/exit-status contracts
  identify_projjson_oracle.rs  every embedded CRS, id stripped, fed back in
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
  projjson-identify.org  the same for PROJJSON, + the sniff (implemented)
  envelope-computation.org  dimension-aware bbox computation (RETIRED, moved out)
NOTICE                  third-party data attributions and terms
LICENSE                 MIT (covers the Rust code only)
LICENSES/                per-source license texts (Apache-2.0 for Esri)
```
