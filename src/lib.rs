//! Embedded CRS registry: an authoritative (authority, code) lookup.
//!
//! CRS definitions (PROJJSON + WKT1 + WKT2:2019) derived at build time from PROJ's
//! `proj.db`, plus a name → (authority, code) index. Decompression (zstd) and
//! `GCR1` lookup live *here* (R6, see `plans/public-api.org`), behind [`resolve`],
//! [`resolve_by_name`], and [`all`] — nothing outside this crate needs to parse
//! the wire format directly. [`identify_from_wkt`] sits one level up: it
//! recovers a CRS from an *id-less* WKT by name, validated against that WKT's
//! own ellipsoid, and reports ambiguity rather than picking
//! (`plans/wkt-identify.org`). `geosetta` was this crate's only consumer when it
//! owned decoding itself; a second one ([nazca](https://github.com/dxgeo/nazca))
//! is why that moved here instead.
//!
//! The embedded data is governed by the terms in `NOTICE` (PROJ, EPSG/IOGP, Esri,
//! IGN France, IAU, NKG). It is a *derived* representation, not the official
//! datasets.
//!
//! The blob's in-memory layout (`GCR1`) is specified in `plans/registry-format.org`;
//! `registry.rs` decodes it. Everything below is built on generated data
//! (`generated.rs`, `names.rs`) produced by `tools/gen_crs_registry.py`; the
//! accessors return empty/`None` results until the generator has run.

#![forbid(unsafe_code)]

mod generated;
mod identify;
mod json;
mod names;
mod registry;
mod wkt;
mod zstd;

pub(crate) use generated::{REGISTRY_BLOB_RAW_SIZE, REGISTRY_BLOB_ZSTD};
pub(crate) use names::NAMES;

pub use identify::{identify_from_wkt, Identity};

/// A resolved CRS definition, in every authoritative form the registry
/// stores for it. `wkt`/`wkt2` are `None` where PROJ can't express the CRS
/// in that dialect (~4% of entries have no WKT1 — see the generator's
/// `no_wkt`/`no_wkt2` counters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrsRecord {
    pub authority: &'static str,
    pub code: &'static str,
    pub projjson: &'static str,
    pub wkt: Option<&'static str>,
    pub wkt2: Option<&'static str>,
}

/// Resolve by `(authority, code)` — e.g. `("EPSG", "4326")`. Binary searches
/// the decoded index; decompresses the blob once on first call. `None` if
/// the generator hasn't run or the pair isn't in the registry.
pub fn resolve(authority: &str, code: &str) -> Option<CrsRecord> {
    registry::resolve(authority, code)
}

/// Name -> `(authority, code)` candidates: every authority's official name
/// plus Esri aliases (`NAMES`), for a given catalog name. Multiple
/// authorities can share a name, so this returns every candidate rather than
/// picking one — weaker evidence than an inline id, so callers that need the
/// trust distinction `plans/crs-registry.org`'s § Validation draws (inline id
/// trusted outright; name/param match validated before snapping) implement
/// that policy themselves on top of this. This crate does one honest lookup,
/// not a validation policy.
pub fn resolve_by_name(name: &str) -> impl Iterator<Item = (&'static str, &'static str)> {
    registry::resolve_by_name(name)
}

/// Every record — bulk/oracle consumers (crosswalk generators, identify
/// oracles). Materializes the whole decoded index; not for hot paths.
pub fn all() -> impl Iterator<Item = CrsRecord> {
    registry::all()
}

/// Dataset versions this build was generated from (stamped by the generator, read
/// from `proj.db`'s `metadata` table). Empty until generated.
pub static DATASET_VERSIONS: &[(&str, &str)] = generated::DATASET_VERSIONS;

/// Number of CRS definitions embedded (0 until generated).
pub const CRS_COUNT: usize = generated::CRS_COUNT;
