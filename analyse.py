#!/usr/bin/env python3
"""Measure one exported fixture sheet (PDF + DXF + drawing.sheet JSON) for the #860 DXF matrix.

Usage: analyse.py <dir> <part>   (reads <dir>/<part>.{pdf,dxf,sheet.json}; prints JSON)

Everything is read from the files: ezdxf for the DXF (header, layers, entities, DIMENSION defpoints and their
cached anonymous blocks, audit), PyMuPDF for the PDF page size and the stroked visible outline, the sheet JSON
for the scale and the view origins. Nothing here knows the expected values; those are in expected.json.
"""
import collections
import json
import math
import re
import sys
from pathlib import Path

import ezdxf
import ezdxf.bbox
import pymupdf

PT_PER_MM = 72.0 / 25.4


def decode(s: str) -> str:
    """AutoCAD \\U+XXXX escapes back to text."""
    return re.sub(r"\\U\+([0-9A-Fa-f]{4})", lambda m: chr(int(m.group(1), 16)), s or "")


def number_in(text: str):
    m = re.search(r"(\d+(?:[.,]\d+)?)", text)
    return float(m.group(1).replace(",", ".")) if m else None


def dxf_facts(path: Path, scale: float, origins: dict, boxes: dict) -> dict:
    doc = ezdxf.readfile(path)
    msp = doc.modelspace()
    out = {
        "acadver": doc.dxfversion,
        "insunits": doc.header.get("$INSUNITS"),
        "measurement": doc.header.get("$MEASUREMENT"),
        "lwdisplay": doc.header.get("$LWDISPLAY"),
        "modelspace_counts": dict(collections.Counter(e.dxftype() for e in msp)),
        "modelspace_total": len(msp),
    }
    layouts = {}
    for name in doc.layouts.names():
        if name == "Model":
            continue
        lay = doc.layouts.get(name)
        vps = [
            {"center": list(vp.dxf.center)[:2], "width": vp.dxf.width, "height": vp.dxf.height,
             "view_center": list(vp.dxf.view_center_point)[:2], "view_height": vp.dxf.view_height,
             "zoom_paper_per_model": vp.dxf.height / vp.dxf.view_height if vp.dxf.view_height else None}
            for vp in lay.query("VIEWPORT")
        ]
        layouts[name] = {"counts": dict(collections.Counter(e.dxftype() for e in lay)), "viewports": vps}
    out["layouts"] = layouts
    out["layers"] = [
        {"name": decode(l.dxf.name), "linetype": l.dxf.linetype, "lineweight_mm": (l.dxf.get("lineweight") or 0) / 100,
         "entities": sum(1 for e in msp if decode(e.dxf.layer) == decode(l.dxf.name))}
        for l in doc.layers
    ]
    out["entity_lineweights"] = sorted({e.dxf.get("lineweight", -1) for e in msp})
    # Visible-outline extents in model space (sheet mm) -> back to part mm.
    vis_names = {"0.50", "VISIBLE"}
    vis = [e for e in msp if decode(e.dxf.layer) in vis_names]
    ext = ezdxf.bbox.extents(vis)
    out["visible_extents_sheet_mm"] = [list(ext.extmin)[:2], list(ext.extmax)[:2]] if ext.has_data else None
    # True curves: back to part mm through the view origin and the scale.
    curves = []
    for e in msp.query("CIRCLE ARC ELLIPSE"):
        c = e.dxf.center
        inside = [v for v, b in boxes.items() if b[0][0] - 1 <= c.x <= b[1][0] + 1 and b[0][1] - 1 <= c.y <= b[1][1] + 1]
        view = inside[0] if inside else min(origins, key=lambda v: math.dist(origins[v], (c.x, c.y)))
        o = origins.get(view, (0.0, 0.0))
        item = {"type": e.dxftype(), "layer": decode(e.dxf.layer), "view": view,
                "centre_part_mm": [round((c.x - o[0]) / scale, 4), round((c.y - o[1]) / scale, 4)]}
        if e.dxftype() == "ELLIPSE":
            item["major_part_mm"] = round(e.dxf.major_axis.magnitude / scale, 4)
            item["ratio"] = round(e.dxf.ratio, 5)
        else:
            item["radius_part_mm"] = round(e.dxf.radius / scale, 4)
        if e.dxftype() == "ARC":
            item["angles_deg"] = [round(e.dxf.start_angle, 3), round(e.dxf.end_angle, 3)]
        curves.append(item)
    out["curves"] = curves
    # DIMENSIONs: displayed text (override or cached block), group 42, defpoint geometry, DIMLFAC.
    dims = []
    for e in msp.query("DIMENSION"):
        dimstyle = doc.dimstyles.get(e.dxf.dimstyle)
        dimlfac = e.override().get("dimlfac") or (dimstyle.dxf.get("dimlfac") if dimstyle else None) or 1.0
        block = doc.blocks.get(e.dxf.geometry) if e.dxf.hasattr("geometry") else None
        block_text = [decode(t.dxf.text if t.dxftype() == "TEXT" else t.text) for t in block.query("TEXT MTEXT")] if block else []
        kind = e.dimtype
        if kind in (0, 1):  # linear/rotated or aligned: distance of defpoint2/defpoint3 along the dimension direction
            p2, p3 = e.dxf.defpoint2, e.dxf.defpoint3
            ang = math.radians(e.dxf.get("angle", 0.0)) if kind == 0 else math.atan2(p3.y - p2.y, p3.x - p2.x)
            geom = abs((p3.x - p2.x) * math.cos(ang) + (p3.y - p2.y) * math.sin(ang))
        elif kind in (3, 4):  # diameter / radius: defpoint to defpoint4
            geom = math.dist(tuple(e.dxf.defpoint)[:2], tuple(e.dxf.defpoint4)[:2])
        else:
            geom = None
        override = decode(e.dxf.text)
        dims.append({
            "type": {0: "linear", 1: "aligned", 3: "diameter", 4: "radius"}.get(kind, str(kind)),
            "layer": decode(e.dxf.layer),
            "text_override": override,
            "block_text": block_text,
            "displayed": override.replace("<>", block_text[0] if block_text else "<>") if override else (block_text[0] if block_text else ""),
            "group42": round(e.dxf.get("actual_measurement", float("nan")), 4),
            "defpoint_sheet_mm": round(geom, 4) if geom is not None else None,
            "dimlfac": dimlfac,
            "regenerated_value": round(geom * dimlfac, 4) if geom is not None else None,
            "part_value_from_geometry": round(geom / scale, 4) if geom is not None else None,
        })
    out["dimensions"] = dims
    errors_before = None
    auditor = doc.audit()
    out["audit"] = {"errors": len(auditor.errors), "fixes": len(auditor.fixes),
                    "messages": [str(m.message) for m in auditor.errors][:10]}
    return out


def pdf_facts(path: Path, views: list) -> dict:
    doc = pymupdf.open(path)
    page = doc[0]
    w_mm, h_mm = page.rect.width / PT_PER_MM, page.rect.height / PT_PER_MM
    widths = collections.Counter()
    per_view = {}
    for d in page.get_drawings():
        lw = round((d.get("width") or 0) / PT_PER_MM, 3)
        widths[lw] += 1
        if abs(lw - 0.5) > 0.01:
            continue
        # The stroked extent: sample Bezier segments instead of trusting the control-point hull.
        pts = []
        for it in d["items"]:
            if it[0] == "l":
                pts += [it[1], it[2]]
            elif it[0] == "c":
                p0, p1, p2, p3 = it[1:5]
                for k in range(33):
                    t = k / 32
                    a, b, c_, e_ = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t ** 3
                    pts.append(pymupdf.Point(a * p0.x + b * p1.x + c_ * p2.x + e_ * p3.x, a * p0.y + b * p1.y + c_ * p2.y + e_ * p3.y))
            elif it[0] == "re":
                pts += [it[1].tl, it[1].br]
        r = pymupdf.Rect(min(p.x for p in pts), min(p.y for p in pts), max(p.x for p in pts), max(p.y for p in pts)) if pts else d["rect"]
        # PDF y runs down; sheet mm run up from the bottom-left.
        x0, x1 = r.x0 / PT_PER_MM, r.x1 / PT_PER_MM
        y0, y1 = h_mm - r.y1 / PT_PER_MM, h_mm - r.y0 / PT_PER_MM
        cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
        for v in views:
            (bx0, by0), (bx1, by1) = v["bbox_mm"]
            if bx0 - 1 <= cx <= bx1 + 1 and by0 - 1 <= cy <= by1 + 1:
                b = per_view.setdefault(v["view"], [x0, y0, x1, y1])
                per_view[v["view"]] = [min(b[0], x0), min(b[1], y0), max(b[2], x1), max(b[3], y1)]
    return {
        "page_mm": [round(w_mm, 3), round(h_mm, 3)],
        "stroke_widths_mm": dict(sorted(widths.items())),
        "visible_bbox_mm": {k: {"w": round(b[2] - b[0], 3), "h": round(b[3] - b[1], 3)} for k, b in per_view.items()},
    }


def main() -> int:
    folder, part = Path(sys.argv[1]), sys.argv[2]
    sheet = json.loads((folder / f"{part}.sheet.json").read_text())
    n, d = (int(x) for x in sheet["scale"].split(":"))
    scale = n / d
    origins = {v["view"]: tuple(v["origin_mm"]) for v in sheet["views"]}
    result = {
        "part": part,
        "scale": sheet["scale"],
        "paper": sheet["paper"],
        "arrangement": sheet["arrangement"],
        "view_choice": sheet["view_choice"],
        "views": [{"view": v["view"], "origin_mm": v["origin_mm"], "bbox_mm": v["bbox_mm"]} for v in sheet["views"]],
        "layers_sheet": sheet["layers"],
        "dxf": dxf_facts(folder / f"{part}.dxf", scale, origins, {v["view"]: v["bbox_mm"] for v in sheet["views"]}),
        "pdf": pdf_facts(folder / f"{part}.pdf", sheet["views"]),
    }
    print(json.dumps(result, indent=1, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
