//! Embedded CRS registry data for Geosetta.
//!
//! CRS definitions (PROJJSON + WKT1) derived at build time from PROJ's `proj.db`,
//! plus a name → (authority, code) index. This crate carries **only data and thin
//! accessors** — decompression (zstd) and lookup live in `geosetta`, which uses
//! its own from-scratch codecs. Keeping the data (and its third-party terms) in a
//! separate crate lets the core `geosetta` crate stay pure-MIT and dependency-free.
//!
//! The embedded data is governed by the terms in `NOTICE` (PROJ, EPSG/IOGP, Esri,
//! IGN France, IAU, NKG). It is a *derived* representation, not the official
//! datasets.
//!
//! The blob's in-memory layout (`GCR1`) is specified in `registry-format.org`; the
//! `geosetta` reader decodes it. This crate only exposes the raw bytes and a few
//! stamped constants. Everything below is a re-export of generated data
//! (`generated.rs`, `names.rs`) produced by `tools/gen_crs_registry.py`; the
//! accessors return empty data until the generator has run.

#![forbid(unsafe_code)]

mod generated;
mod names;

/// The zstd-compressed `GCR1` registry blob (`(authority, code) → {PROJJSON,
/// WKT1}`). Decoded by the consumer (`geosetta`) with its own zstd decoder, which
/// needs the decompressed length — see [`REGISTRY_BLOB_RAW_SIZE`].
///
/// Empty until the generator runs.
pub const REGISTRY_BLOB_ZSTD: &[u8] = generated::REGISTRY_BLOB_ZSTD;

/// Decompressed length of the `GCR1` image inside [`REGISTRY_BLOB_ZSTD`]. Required
/// because `geosetta`'s zstd decoder is told the output size up front (it ignores
/// the frame's own content-size field). `0` until generated.
pub const REGISTRY_BLOB_RAW_SIZE: usize = generated::REGISTRY_BLOB_RAW_SIZE;

/// Name → (authority, code) index: every authority's official CRS names plus Esri
/// aliases, for id-less inputs (e.g. shapefile `.prj`). The code is a **string**
/// (IGNF/OGC/PROJ/NKG use alphanumeric codes such as `CRS84`, `LAMB93`), so this
/// is `(name, authority, code)`. Populated by the generator; consumed by name→code
/// recovery (milestone R2).
pub static NAMES: &[(&str, &str, &str)] = names::NAMES;

/// Dataset versions this build was generated from (stamped by the generator, read
/// from `proj.db`'s `metadata` table). Empty until generated.
pub static DATASET_VERSIONS: &[(&str, &str)] = generated::DATASET_VERSIONS;

/// Number of CRS definitions embedded (0 until generated).
pub const CRS_COUNT: usize = generated::CRS_COUNT;
