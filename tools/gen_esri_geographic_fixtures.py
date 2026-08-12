#!/usr/bin/env python3
"""Generate esri_geographic.tsv: real id-less Esri-flavor WKT1 for every
native ESRI geographic 2D CRS in proj.db, for R2's name-recovery bulk oracle
(geosetta/src/crs/registry.rs). Each line is `esri_code<TAB>wkt`.

Build-time only, mirrors tests/fixtures/gen_wkt1_crosswalk.py's oracle-fixture
role but for R2 (name recovery) instead of milestone C (structural crosswalk).
"""
import os
import sqlite3
import subprocess

PROJ_DB = os.environ.get("PROJ_DB", "/opt/homebrew/share/proj/proj.db")
OUT = "/Users/dan/Projects/geosetta/tests/fixtures/esri_geographic_wkt1.tsv"

db = sqlite3.connect(PROJ_DB)
rows = db.execute(
    "SELECT code FROM geodetic_crs WHERE auth_name='ESRI' AND type='geographic 2D' ORDER BY code"
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
