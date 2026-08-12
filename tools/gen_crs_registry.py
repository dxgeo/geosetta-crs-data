#!/usr/bin/env python3
"""Generate the embedded CRS registry (GCR1 format) for geosetta-crs-data.

Build-time only — needs PROJ's ``projinfo`` + ``proj.db`` and the ``zstd`` CLI.
GDAL/PROJ is a codegen tool here, never a runtime or link dependency: the emitted
artifacts ship as static data, decoded by ``geosetta``'s own ``compress/zstd.rs``.

Outputs (into ../src/, i.e. geosetta-crs-data/src/):
  * registry.bin.zst  — the GCR1 image (see ../registry-format.org), zstd-compressed
                        with the ``zstd`` CLI (plain single frame, no dictionary), so
                        geosetta's from-scratch decoder can read it.
  * generated.rs      — REGISTRY_BLOB_ZSTD / REGISTRY_BLOB_RAW_SIZE / CRS_COUNT /
                        DATASET_VERSIONS.
  * names.rs          — NAMES: (name, authority, code) index (official names per
                        authority + Esri aliases from alias_name); code is a STRING.

The image is fully self-describing (magic, format version, dataset versions, entry
count) so a blob built from a user's own proj.db is unambiguous — see
../registry-format.org § USER-BUILT BLOBS.

Mechanical by construction: it *reads* proj.db, it never embeds a hand-copied slice
of it (keeps the "ship code, not data" posture of the local-build path honest).
"""
from __future__ import annotations

import argparse
import concurrent.futures as futures
import datetime
import json
import os
import shutil
import sqlite3
import struct
import subprocess
import sys
from pathlib import Path

DEFAULT_PROJ_DB = os.environ.get("PROJ_DB", "/opt/homebrew/share/proj/proj.db")
HERE = Path(__file__).resolve().parent
SRC = HERE.parent / "src"

MAGIC = b"GCR1"
FORMAT_VERSION = 2
HEADER_SIZE = 64
RECORD_SIZE = 32

# PROJJSON keys that carry only usage/schema metadata, stripped for compactness
# (they never affect `projinfo --identify`). Removed recursively. "usages" (the
# plural array form, used when a CRS has multiple scope/area pairs) must be
# stripped as a whole key, not recursed into: stripping only its children's
# scope/area/bbox leaves `"usages":[{},{}]`, which projinfo's PROJJSON parser
# rejects outright ("missing children in usages child") — found by the R1
# reader's full-registry oracle.
STRIP_KEYS = {"$schema", "scope", "area", "bbox", "usage", "usages"}


# --- proj.db ---------------------------------------------------------------

def read_versions(db: sqlite3.Connection) -> list[tuple[str, str]]:
    """Dataset version stamps from proj.db's metadata table + a generation date."""
    rows = dict(db.execute("SELECT key, value FROM metadata").fetchall())
    wanted = [
        ("EPSG", "EPSG.VERSION"),
        ("PROJ", "PROJ.VERSION"),
        ("ESRI", "ESRI.VERSION"),
        ("IGNF", "IGNF.VERSION"),
        ("NKG", "NKG.VERSION"),
    ]
    out = [(name, rows[key]) for name, key in wanted if key in rows]
    major = rows.get("DATABASE.LAYOUT.VERSION.MAJOR", "?")
    minor = rows.get("DATABASE.LAYOUT.VERSION.MINOR", "?")
    out.append(("proj.db.layout", f"{major}.{minor}"))
    out.append(("generated", datetime.date.today().isoformat()))
    return out


def read_crs_list(db: sqlite3.Connection, limit: int | None) -> list[tuple[str, str]]:
    q = "SELECT auth_name, code FROM crs_view"
    if limit:
        q += f" LIMIT {int(limit)}"
    return [(a, str(c)) for a, c in db.execute(q).fetchall()]


def read_names(db: sqlite3.Connection) -> list[tuple[str, str, str]]:
    """(name, authority, code) — official CRS names + Esri aliases."""
    seen: set[tuple[str, str, str]] = set()
    for name, auth, code in db.execute(
        "SELECT name, auth_name, code FROM crs_view WHERE name IS NOT NULL AND name != ''"
    ):
        seen.add((name, auth, str(code)))
    crs_tables = ("geodetic_crs", "projected_crs", "vertical_crs", "compound_crs", "engineering_crs")
    ph = ",".join("?" * len(crs_tables))
    for alt, auth, code in db.execute(
        f"SELECT alt_name, auth_name, code FROM alias_name "
        f"WHERE source='ESRI' AND table_name IN ({ph})",
        crs_tables,
    ):
        seen.add((alt, auth, str(code)))
    return sorted(seen, key=lambda t: (t[0], t[1], t[2]))


# --- projinfo --------------------------------------------------------------

def strip_usage(obj):
    if isinstance(obj, dict):
        return {k: strip_usage(v) for k, v in obj.items() if k not in STRIP_KEYS}
    if isinstance(obj, list):
        return [strip_usage(v) for v in obj]
    return obj


# projinfo prints each requested format under its own `Header:` line. With
# --single-line the value is one line. A format that cannot be produced (e.g.
# WKT1 for a Geographic 3D CRS) leaves its section *empty* on stdout and prints
# the reason on stderr — so a section parser must NOT spill across headers into
# the next section (that once mis-stored "PROJJSON:" as the WKT1 value).
#
# --single-line is not a hard guarantee: a handful of EPSG AREA descriptions
# (found via R5's WKT2 bulk oracle — e.g. the "Gulf of Mexico ... Amery
# Terrace." area shared by EPSG:32075/32665) carry a literal embedded newline
# byte in the source data itself, which survives into WKT2's USAGE/AREA text
# even under --single-line and splits one logical section across two physical
# lines with *no* blank-line separator between them (a real separator between
# *different* sections always has one). So a section's content is every line
# from just after its header up to (but not including) either a blank line
# that follows already-captured content, or the next header — joined with no
# extra separator, which reconstructs the original text exactly minus the
# stray newline. Content-less sections (an unproducible format) still resolve
# to "" as before.
SECTION_HEADERS = {
    "WKT1:GDAL string:": "wkt",
    "WKT2:2019 string:": "wkt2",
    "PROJJSON:": "projjson",
}


def parse_sections(text: str) -> dict[str, str]:
    """Map each section to its (possibly newline-rejoined) content."""
    out: dict[str, list[str]] = {}
    cur: str | None = None
    for line in text.splitlines():
        matched = next((v for h, v in SECTION_HEADERS.items() if line.startswith(h)), None)
        if matched is not None:
            cur = matched
            out.setdefault(cur, [])
            continue
        if cur is None:
            continue
        if line.strip() == "":
            if out[cur]:
                cur = None  # blank line after content = section boundary
            continue  # leading blank line before any content: skip, keep waiting
        out[cur].append(line)
    return {k: "".join(v) for k, v in out.items()}


def fetch_def(
    auth: str, code: str, proj_db: str
) -> tuple[str, str, bytes, bytes | None, bytes | None] | None:
    """Return (auth, code, projjson_bytes, wkt1_bytes_or_None, wkt2_bytes_or_None), or None on failure."""
    env = dict(os.environ, PROJ_DB=proj_db)
    key = f"{auth}:{code}"
    r = subprocess.run(
        ["projinfo", "-o", "PROJJSON,WKT1_GDAL,WKT2_2019", "--single-line", key],
        capture_output=True, text=True, env=env,
    )
    secs = parse_sections(r.stdout)
    pj_line = secs.get("projjson", "")
    if not pj_line:
        return None  # no PROJJSON — cannot embed (should be zero cases)
    try:
        compact = json.dumps(strip_usage(json.loads(pj_line)), separators=(",", ":"), ensure_ascii=False)
    except json.JSONDecodeError:
        return None
    wkt_line = secs.get("wkt", "")
    wkt = None
    if wkt_line and not wkt_line.startswith("Error when exporting"):
        wkt = wkt_line.encode("utf-8")
    wkt2_line = secs.get("wkt2", "")
    wkt2 = None
    if wkt2_line and not wkt2_line.startswith("Error when exporting"):
        wkt2 = wkt2_line.encode("utf-8")
    return (auth, code, compact.encode("utf-8"), wkt, wkt2)


# --- GCR1 image ------------------------------------------------------------

def build_image(entries, versions) -> bytes:
    """entries: list of (auth, code, projjson_bytes, wkt_or_None, wkt2_or_None). Returns the image."""
    authorities = sorted({a for a, *_ in entries})
    auth_id = {a: i for i, a in enumerate(authorities)}

    # Sort by (auth_id, code bytes) — the order the reader's binary search assumes.
    entries = sorted(entries, key=lambda e: (auth_id[e[0]], e[1].encode("utf-8")))

    keys = bytearray()
    payload = bytearray()
    index = bytearray()
    any_wkt = False
    any_wkt2 = False

    for auth, code, pj, wkt, wkt2 in entries:
        code_b = code.encode("utf-8")
        key_off = len(keys)
        keys += code_b
        proj_off = len(payload)
        payload += pj
        if wkt is not None:
            any_wkt = True
            wkt_off = len(payload)
            payload += wkt
            has_wkt, wkt_len = 1, len(wkt)
        else:
            wkt_off, has_wkt, wkt_len = 0, 0, 0
        if wkt2 is not None:
            any_wkt2 = True
            wkt2_off = len(payload)
            payload += wkt2
            has_wkt2, wkt2_len = 1, len(wkt2)
        else:
            wkt2_off, has_wkt2, wkt2_len = 0, 0, 0
        index += struct.pack(
            "<BBBBIIIIIII",
            auth_id[auth], len(code_b), has_wkt, has_wkt2,
            key_off, proj_off, len(pj), wkt_off, wkt_len, wkt2_off, wkt2_len,
        )

    auth_table = bytearray()
    for a in authorities:
        ab = a.encode("utf-8")
        auth_table += struct.pack("<B", len(ab)) + ab

    versions_sec = bytearray(struct.pack("<H", len(versions)))
    for src, ver in versions:
        for s in (src, ver):
            sb = s.encode("utf-8")
            versions_sec += struct.pack("<H", len(sb)) + sb

    authorities_off = HEADER_SIZE
    index_off = authorities_off + len(auth_table)
    keys_off = index_off + len(index)
    payload_off = keys_off + len(keys)
    versions_off = payload_off + len(payload)
    image_len = versions_off + len(versions_sec)

    flags = (1 if any_wkt else 0) | (2 if any_wkt2 else 0)
    header = struct.pack(
        "<4sHHIHHIIIIII",
        MAGIC, FORMAT_VERSION, flags, len(entries), len(authorities), 0,
        authorities_off, index_off, keys_off, payload_off, versions_off, image_len,
    )
    header += b"\x00" * (HEADER_SIZE - len(header))
    assert len(header) == HEADER_SIZE, len(header)

    return bytes(header + auth_table + index + keys + payload + versions_sec)


def self_check(image: bytes, entries) -> None:
    """Parse the image back and confirm structure + a couple of lookups."""
    (magic, ver, _flags, n, auth_count, _r,
     auth_off, idx_off, keys_off, pay_off, ver_off, image_len) = struct.unpack_from("<4sHHIHHIIIIII", image, 0)
    assert magic == MAGIC, magic
    assert ver == FORMAT_VERSION
    assert n == len(entries), (n, len(entries))
    assert image_len == len(image), (image_len, len(image))

    authorities = []
    p = auth_off
    for _ in range(auth_count):
        ln = image[p]; p += 1
        authorities.append(image[p:p + ln].decode()); p += ln
    assert p == idx_off

    prev = None
    for i in range(n):
        aid, klen, has_wkt, has_wkt2, koff, poff, plen, woff, wlen, w2off, w2len = struct.unpack_from(
            "<BBBBIIIIIII", image, idx_off + i * RECORD_SIZE)
        code = image[keys_off + koff: keys_off + koff + klen]
        key = (aid, bytes(code))
        assert prev is None or prev <= key, "index not sorted"
        prev = key
        assert plen > 0, "empty PROJJSON"
        assert pay_off + poff + plen <= ver_off
        if has_wkt:
            assert pay_off + woff + wlen <= ver_off
        if has_wkt2:
            assert pay_off + w2off + w2len <= ver_off


# --- Rust emission ---------------------------------------------------------

def rust_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def write_generated(raw_size: int, crs_count: int, versions) -> None:
    lines = [
        "//! GENERATED by tools/gen_crs_registry.py — do not edit by hand.",
        "//!",
        "//! The GCR1 registry blob + stamped sizes/versions. See ../registry-format.org.",
        "",
        'pub(super) const REGISTRY_BLOB_ZSTD: &[u8] = include_bytes!("registry.bin.zst");',
        f"pub(super) const REGISTRY_BLOB_RAW_SIZE: usize = {raw_size};",
        f"pub(super) const CRS_COUNT: usize = {crs_count};",
        "pub(super) static DATASET_VERSIONS: &[(&str, &str)] = &[",
    ]
    for src, ver in versions:
        lines.append(f"    ({rust_str(src)}, {rust_str(ver)}),")
    lines += ["];", ""]
    (SRC / "generated.rs").write_text("\n".join(lines), encoding="utf-8")


def write_names(names) -> None:
    lines = [
        "//! GENERATED by tools/gen_crs_registry.py — do not edit by hand.",
        "//!",
        "//! Name → (authority, code) index; code is a string. Sorted by name for a",
        "//! future binary search (name→code recovery, milestone R2).",
        "",
        "pub(super) static NAMES: &[(&str, &str, &str)] = &[",
    ]
    for name, auth, code in names:
        lines.append(f"    ({rust_str(name)}, {rust_str(auth)}, {rust_str(code)}),")
    lines += ["];", ""]
    (SRC / "names.rs").write_text("\n".join(lines), encoding="utf-8")


# --- driver ----------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--proj-db", default=DEFAULT_PROJ_DB)
    ap.add_argument("--limit", type=int, default=None, help="only the first N CRSes (smoke test)")
    ap.add_argument("--jobs", type=int, default=8, help="parallel projinfo workers")
    ap.add_argument("--keep-raw", action="store_true", help="keep the uncompressed image next to the .zst")
    args = ap.parse_args()

    if not Path(args.proj_db).exists():
        print(f"proj.db not found: {args.proj_db}", file=sys.stderr)
        return 1
    for tool in ("projinfo", "zstd"):
        if shutil.which(tool) is None:
            print(f"required tool not on PATH: {tool}", file=sys.stderr)
            return 1

    db = sqlite3.connect(f"file:{args.proj_db}?mode=ro", uri=True)
    versions = read_versions(db)
    crs_list = read_crs_list(db, args.limit)
    names = read_names(db)
    db.close()
    print(f"proj.db: {args.proj_db}", file=sys.stderr)
    print(f"versions: {versions}", file=sys.stderr)
    print(f"CRSes to fetch: {len(crs_list)}  |  name index: {len(names)}", file=sys.stderr)

    entries = []
    skipped = []
    no_wkt = 0
    no_wkt2 = 0
    no_wkt_no_wkt2 = []
    done = 0
    with futures.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = {ex.submit(fetch_def, a, c, args.proj_db): (a, c) for a, c in crs_list}
        for fut in futures.as_completed(futs):
            a, c = futs[fut]
            res = fut.result()
            done += 1
            if done % 1000 == 0:
                print(f"  ...{done}/{len(crs_list)}", file=sys.stderr)
            if res is None:
                skipped.append(f"{a}:{c}")
                continue
            if res[3] is None:
                no_wkt += 1
            if res[4] is None:
                no_wkt2 += 1
                if res[3] is None:
                    no_wkt_no_wkt2.append(f"{a}:{c}")
            entries.append(res)

    print(
        f"embedded: {len(entries)}  |  no WKT1: {no_wkt}  |  no WKT2: {no_wkt2}  |  "
        f"no WKT1 and no WKT2: {len(no_wkt_no_wkt2)}  |  skipped (no PROJJSON): {len(skipped)}",
        file=sys.stderr,
    )
    if skipped:
        print("  skipped: " + ", ".join(skipped[:20]) + (" ..." if len(skipped) > 20 else ""),
              file=sys.stderr)
    if no_wkt_no_wkt2:
        # R5 exists to close exactly this gap; a nonzero count here means some
        # CRS has neither faithful form (only bare PROJJSON) — worth knowing.
        print("  no WKT1 or WKT2 (PROJJSON-only): " + ", ".join(no_wkt_no_wkt2[:20])
              + (" ..." if len(no_wkt_no_wkt2) > 20 else ""), file=sys.stderr)

    image = build_image(entries, versions)
    self_check(image, entries)
    print(f"image: {len(image):,} bytes raw", file=sys.stderr)

    raw_path = SRC / "registry.bin"
    zst_path = SRC / "registry.bin.zst"
    raw_path.write_bytes(image)
    subprocess.run(["zstd", "-19", "-q", "-f", "-o", str(zst_path), str(raw_path)], check=True)
    comp = zst_path.stat().st_size
    if not args.keep_raw:
        raw_path.unlink()
    print(f"compressed: {comp:,} bytes ({comp / len(image) * 100:.1f}%) -> {zst_path}", file=sys.stderr)

    write_generated(len(image), len(entries), versions)
    write_names(names)
    print("wrote generated.rs + names.rs", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
