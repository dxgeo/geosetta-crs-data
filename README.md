# geosetta-crs-data

Embedded **CRS registry data** for [Geosetta](../geosetta): coordinate reference
system definitions (PROJJSON + WKT1) plus a name → (authority, code) index,
covering **all CRSes in PROJ's `proj.db`** — every authority (EPSG, ESRI, IGNF,
OGC, IAU_2015 planetary, NKG) and type.

This crate exists to **isolate third-party data and its terms** from the core
`geosetta` crate:

- The core `geosetta` crate stays **pure-MIT and dependency-free**. It pulls this
  crate in only under its (default-on, at R1) `crs-registry` feature.
- All decoding (zstd) and lookup logic live in `geosetta`, using its own
  from-scratch codecs — so this crate has **no dependencies** and holds only data
  plus thin accessors (`REGISTRY_BLOB_ZSTD`, `NAMES`, `DATASET_VERSIONS`).
- The embedded **data** is a *derived* representation of PROJ/EPSG/Esri/IGN/IAU/NKG
  and is governed by those sources' terms — see [`NOTICE`](NOTICE). The Rust code
  is MIT (see [`LICENSE`](LICENSE)).

## Status

**Skeleton.** The generated artifacts are not built yet; the accessors return
empty data. The generator (`tools/gen_crs_registry.py`, build-time only, needs
PROJ's `projinfo` + `proj.db`) and the milestones are described in the design
plan, [`crs-registry.org`](crs-registry.org).

Not under version control yet.

## Layout

```
Cargo.toml            crate manifest (no dependencies)
src/lib.rs            data accessors (skeleton)
tools/                build-time generators (skeleton)
crs-registry.org      the design plan
NOTICE                third-party data attributions and terms
LICENSE               MIT (covers the Rust code only)
LICENSES/             per-source license texts (e.g. Apache-2.0 for Esri)
```
