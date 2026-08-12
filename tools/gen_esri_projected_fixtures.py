#!/usr/bin/env python3
"""Generate esri_projected_wkt1.tsv: real id-less Esri-flavor WKT1 for every
native ESRI projected CRS in proj.db, for R2's projected name-recovery bulk
oracle (geosetta/src/crs/registry.rs). Each line is `esri_code<TAB>wkt`.

Build-time only; projected counterpart to gen_esri_geographic_fixtures.py.
"""
import os
import sqlite3
import subprocess

PROJ_DB = os.environ.get("PROJ_DB", "/opt/homebrew/share/proj/proj.db")
OUT = "/Users/dan/Projects/geosetta/tests/fixtures/esri_projected_wkt1.tsv"

db = sqlite3.connect(PROJ_DB)
rows = db.execute(
    "SELECT code FROM projected_crs WHERE auth_name='ESRI' ORDER BY code"
).fetchall()

out = []
for (code,) in rows:
    r = subprocess.run(
        ["projinfo", f"ESRI:{code}", "-o", "WKT1_ESRI", "--single-line", "-q"],
        capture_output=True, text=True,
    )
    wkt = r.stdout.strip()
    if wkt and not wkt.startswith("Error"):
        out.append(f"{code}\t{wkt}")

with open(OUT, "w") as f:
    f.write("\n".join(out) + "\n")
print(f"{len(out)}/{len(rows)} fixtures -> {OUT}")
