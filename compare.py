#!/usr/bin/env python3
"""Compare analyse.py output with the hand-derived expected.json. Usage: compare.py <measure dir>; prints JSON."""
import json, math, sys
from pathlib import Path

exp = json.loads(Path(__file__).with_name("expected.json").read_text())
out = {}
for part, e in exp.items():
    if part.startswith("_"):
        continue
    r = json.loads((Path(sys.argv[1]) / f"{part}.json").read_text())
    scale = eval(r["scale"].replace(":", "/"))
    curves = [c for c in r["dxf"]["curves"] if "radius_part_mm" in c]
    used, worst_r, worst_c, worst_a, missing = set(), 0.0, 0.0, 0.0, []
    for view, x, y, rad, *rest in e["curves"]:
        best = None
        for i, c in enumerate(curves):
            if i in used or c["view"] != view:
                continue
            d = math.dist(c["centre_part_mm"], (x, y)) + abs(c["radius_part_mm"] - rad)
            if d < 0.05 and (best is None or d < best[0]):
                best = (d, i)
        if best is None:
            missing.append([view, x, y, rad]); continue
        c = curves[best[1]]; used.add(best[1])
        worst_r = max(worst_r, abs(c["radius_part_mm"] - rad))
        worst_c = max(worst_c, math.dist(c["centre_part_mm"], (x, y)))
        if len(rest) == 3 and c.get("angles_deg"):
            worst_a = max(worst_a, max(abs((c["angles_deg"][0] - rest[1] + 180) % 360 - 180), abs((c["angles_deg"][1] - rest[2] + 180) % 360 - 180)))
    extra = [curves[i] for i in range(len(curves)) if i not in used]
    view, w, h = e["main"]
    pdf = r["pdf"]["visible_bbox_mm"].get(view, {})
    dims = r["dxf"]["dimensions"]
    out[part] = {
        "scale": r["scale"], "expected_curves": len(e["curves"]), "matched": len(e["curves"]) - len(missing), "missing": missing,
        "max_radius_dev_mm": round(worst_r, 4), "max_centre_dev_mm": round(worst_c, 4), "max_angle_dev_deg": round(worst_a, 4),
        "extra_curves": [{k: c[k] for k in ("type", "layer", "view", "centre_part_mm", "radius_part_mm", "angles_deg") if k in c} for c in extra],
        "pdf_main_view_mm": pdf, "expected_main_view_mm": {"w": w * scale, "h": h * scale},
        "dims_displayed_eq_group42": all(abs((n := __import__("re").search(r"(\d+(?:[.,]\d+)?)", __import__("re").sub(r"^\d+x\s*", "", d["displayed"]))) and float(n.group(1).replace(",", ".")) - d["group42"]) < 1e-6 for d in dims),
        "dims_geometry_over_scale_eq_group42": all(abs(d["part_value_from_geometry"] - d["group42"]) < 1e-3 for d in dims if d["part_value_from_geometry"] is not None),
        "dims_regenerated_eq_group42": all(abs(d["regenerated_value"] - d["group42"]) < 1e-3 for d in dims if d["regenerated_value"] is not None),
        "dims_empty_text": sum(1 for d in dims if not d["text_override"]),
        "dims_total": len(dims),
    }
print(json.dumps(out, indent=1, ensure_ascii=False))
