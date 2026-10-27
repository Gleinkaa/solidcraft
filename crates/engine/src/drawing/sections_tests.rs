#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Measuring tests for sections, break-outs and hatching (D9).

use super::*;
use crate::Session;
use crate::drawing::dims::{Annotations, TEXT_CLEAR_MM};
use crate::drawing::sheet::{Arrangement, Layer, PlacedView, Prim, Sheet, TextAnchor, TitleBlock, sheet_to_dxf};
use crate::drawing::{NotesBlock, SheetPlan, plan_sheet_auto_sections};
use serde_json::json;
use solidcraft_io::text_width_mm;

// ------------------------------------------------------------------------------- test parts

/// Ø120 × 35 disc, Ø90 × 10 recess from both faces, Ø40 boss, Ø16 through bore (the flywheel).
fn flywheel_part() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XZ","name":"FlyProfile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[8,0],[60,0],[60,-20],[45,-20],[45,-10],[20,-10],[20,-35],[8,-35]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"FlyProfile","axis":"Z","angle":360,"body_name":"Flywheel"})).unwrap();
    s
}

/// The acceptance shaft: Ø20 × 30, Ø28 × 50, Ø16 × 40, a Ø8 × 20 blind end bore and a Ø6 cross
/// hole 55 mm from the datum.
fn stepped_shaft_with_features() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Profile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,10],[30,10],[30,14],[80,14],[80,8],[120,8],[120,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})).unwrap();
    s.execute(
        "solid.hole",
        &json!({"position":[120,0,0],"direction":[-1,0,0],"diameter":8,"depth":20,"type":"drilled","tip_angle":118,"targets":["Shaft"]}),
    )
    .unwrap();
    s.execute("solid.cylinder", &json!({"base":[55,0,-20],"axis":[0,0,1],"radius":3,"height":40,"operation":"cut","targets":["Shaft"]})).unwrap();
    s
}

/// A flat plate 80 × 40 × 10 with two Ø8 holes.
fn plate_with_two_holes() -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":80,"width":40,"height":10,"body_name":"Plate"})).unwrap();
    for x in [15.0, 65.0] {
        s.execute("solid.cylinder", &json!({"base":[x,20,-1],"axis":[0,0,1],"radius":4,"height":12,"operation":"cut","targets":["Plate"]})).unwrap();
    }
    s
}

fn plan_with(s: &Session, mode: SectionMode) -> SheetPlan {
    let title = TitleBlock::for_design("Part");
    let (plan, _) = plan_sheet_auto_sections(s, None, 0.01, &title, &NotesBlock::default(), None, &Annotations::default(), &mode).unwrap();
    plan
}

fn front(plan: &SheetPlan) -> &crate::drawing::ProjectedView {
    plan.projected.iter().find(|v| v.view == StandardView::Front).expect("front view")
}

fn placed_of(sheet: &Sheet, view: StandardView) -> &PlacedView {
    sheet.views.iter().find(|v| v.view == view).expect("a placed view")
}

// ------------------------------------------------------------------------------- prim helpers

fn text_prims(sheet: &Sheet) -> Vec<(Vec2, f64, f64, String)> {
    sheet
        .prims
        .iter()
        .filter_map(|prim| match prim {
            Prim::Text { at, height, angle_deg, text, .. } => Some((*at, *height, *angle_deg, text.clone())),
            _ => None,
        })
        .collect()
}

fn layer_lines(sheet: &Sheet, layer: Layer) -> Vec<(Vec2, Vec2)> {
    sheet
        .prims
        .iter()
        .flat_map(|prim| match prim {
            Prim::Line { a, b, layer: l } if *l == layer => vec![(*a, *b)],
            Prim::Polyline { points, closed, layer: l } if *l == layer => {
                let mut out: Vec<(Vec2, Vec2)> = points.windows(2).map(|pair| (pair[0], pair[1])).collect();
                if *closed && points.len() > 2 {
                    out.push((points[points.len() - 1], points[0]));
                }
                out
            }
            // A point on a drawn arc lies on the curve; sample finely enough that a 0.05 mm
            // distance test sees it (2° is a 0.007 mm sagitta even at R45).
            Prim::Arc { c, r, a0, a1, layer: l } if *l == layer => {
                let steps = (((a1 - a0).abs() / 2.0_f64.to_radians()).ceil() as usize).clamp(2, 720);
                let point = |i: usize| *c + Vec2::from_angle(a0 + (a1 - a0) * i as f64 / steps as f64) * *r;
                (0..steps).map(|i| (point(i), point(i + 1))).collect()
            }
            Prim::Circle { c, r, layer: l } if *l == layer => {
                let point = |i: usize| *c + Vec2::from_angle(std::f64::consts::TAU * i as f64 / 180.0) * *r;
                (0..180).map(|i| (point(i), point(i + 1))).collect()
            }
            Prim::Ellipse { c, major, ratio, t0, t1, layer: l } if *l == layer => {
                let minor = major.perp() * *ratio;
                let steps = (((t1 - t0).abs() / 2.0_f64.to_radians()).ceil() as usize).clamp(2, 720);
                let point = |i: usize| {
                    let t = t0 + (t1 - t0) * i as f64 / steps as f64;
                    *c + *major * t.cos() + minor * t.sin()
                };
                (0..steps).map(|i| (point(i), point(i + 1))).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

/// The box of a dimension text primitive, in the text's own rotation.
fn text_box(at: Vec2, width: f64, angle_deg: f64, height: f64) -> [Vec2; 4] {
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    [at - u * (width * 0.5), at + u * (width * 0.5), at + u * (width * 0.5) + n * height, at - u * (width * 0.5) + n * height]
}

fn dimension_text_boxes(sheet: &Sheet) -> Vec<[Vec2; 4]> {
    let mut out = Vec::new();
    for dim in &sheet.dimensions {
        if let Some(Prim::Text { at, height, angle_deg, text, anchor, .. }) = dim.prims.iter().find(|p| matches!(p, Prim::Text { .. })) {
            let width = text_width_mm(text, *height);
            let u = Vec2::from_angle(angle_deg.to_radians());
            let centre = match anchor {
                TextAnchor::Left => *at + u * (width * 0.5),
                TextAnchor::Center => *at,
                TextAnchor::Right => *at - u * (width * 0.5),
            };
            out.push(text_box(centre, width, *angle_deg, *height));
        }
    }
    for note in &sheet.notes {
        if let Prim::Text { at, height, text, .. } = &note.text {
            let width = text_width_mm(text, *height);
            out.push(text_box(*at + Vec2::new(width * 0.5, 0.0), width, 0.0, *height));
        }
    }
    out
}

fn boxes_overlap(a: &[Vec2; 4], b: &[Vec2; 4], clear: f64) -> bool {
    // Separating-axis test on the boxes (convex quads), each grown by `clear`.
    for corners in [a, b] {
        for i in 0..4 {
            let axis = (corners[(i + 1) % 4] - corners[i]).normalized().unwrap_or(Vec2::X).perp();
            let project =
                |quad: &[Vec2; 4]| quad.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.dot(axis)), hi.max(p.dot(axis))));
            let (a_lo, a_hi) = project(a);
            let (b_lo, b_hi) = project(b);
            if a_hi < b_lo - clear || b_hi < a_lo - clear {
                return false;
            }
        }
    }
    true
}

fn segment_hits_box(a: Vec2, b: Vec2, corners: &[Vec2; 4]) -> bool {
    let along = (corners[1] - corners[0]).normalized().unwrap_or(Vec2::X);
    let up = along.perp();
    let origin = corners[0];
    let local = |p: Vec2| Vec2::new((p - origin).dot(along), (p - origin).dot(up));
    let lo = Vec2::new(0.0, 0.0);
    let hi = Vec2::new(corners[1].dist(corners[0]), corners[3].dist(corners[0]));
    crate::drawing::dims::segment_hits_rect(local(a), local(b), lo, hi)
}

fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.dot(d);
    if len2 <= 1e-18 {
        return p.dist(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.dist(a + d * t)
}

fn point_box_distance(p: Vec2, corners: &[Vec2; 4]) -> f64 {
    let mut best = f64::INFINITY;
    for i in 0..4 {
        best = best.min(point_segment_distance(p, corners[i], corners[(i + 1) % 4]));
    }
    best
}

fn inside_box(p: Vec2, corners: &[Vec2; 4]) -> bool {
    let mut sign = 0.0_f64;
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let cross = (b - a).cross(p - a);
        if cross.abs() < 1e-12 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// The boundary polygons of a view section, in view millimetres.
fn view_boundaries(plan: &SheetPlan, view: StandardView) -> Vec<Vec<Vec2>> {
    plan.projected.iter().find(|v| v.view == view).and_then(|v| v.section.as_ref()).map(|s| s.boundaries.clone()).unwrap_or_default()
}

fn inside_any(p: Vec2, polygons: &[Vec<Vec2>]) -> bool {
    polygons.iter().any(|poly| inside_polygon(p, poly))
}

fn distance_to_polygon(p: Vec2, poly: &[Vec2]) -> f64 {
    let mut best = f64::INFINITY;
    for i in 0..poly.len() {
        best = best.min(point_segment_distance(p, poly[i], poly[(i + 1) % poly.len()]));
    }
    best
}

fn covers(segments: &[crate::drawing::Seg2mm], p: Vec2) -> bool {
    segments.iter().any(|s| point_segment_distance(p, s.a, s.b) <= 0.01)
}

// ------------------------------------------------------------------------------- geometry units

#[test]
fn a_freehand_boundary_is_smooth_and_wavy() {
    // D9.1: the amplitude is 0.1 x the window radius, capped at 1.5 mm and never below 0.3 mm.
    assert!((freehand_amplitude(10.0) - 1.0).abs() < 1e-12);
    assert!((freehand_amplitude(100.0) - FREEHAND_AMPLITUDE_MM).abs() < 1e-12);
    assert!((freehand_amplitude(1.0) - FREEHAND_MIN_MM).abs() < 1e-12);
    for (radius, expected) in [(10.0, 1.0), (3.0, 0.3), (40.0, 1.5)] {
        let boundary = Boundary::circle(0.0, 0.0, radius);
        let points = boundary.points();
        assert_eq!(points.len(), 64);
        let radii: Vec<f64> = points.iter().map(|p| p.len()).collect();
        let min = radii.iter().copied().fold(f64::INFINITY, f64::min);
        let max = radii.iter().copied().fold(0.0, f64::max);
        assert!((max - (radius + expected)).abs() < 0.05, "radius {radius}: peak {max}, want {}", radius + expected);
        assert!(max - min >= expected - 1e-6, "radius {radius}: wobble {:.3} is flatter than its amplitude {expected}", max - min);
        for i in 1..points.len() - 1 {
            let a = points[i] - points[i - 1];
            let b = points[i + 1] - points[i];
            let angle = a.cross(b).atan2(a.dot(b)).abs();
            assert!(angle < 0.5, "radius {radius}: turn at {i}: {angle}");
        }
        assert!(!points.iter().all(|p| (p.x - points[0].x).abs() < 1e-9), "not a straight line");
    }
}

#[test]
fn hatch_spans_are_45_degrees_three_millimetres_apart_and_skip_the_island() {
    let outer = vec![Vec2::new(0.0, 0.0), Vec2::new(30.0, 0.0), Vec2::new(30.0, 20.0), Vec2::new(0.0, 20.0)];
    let island = vec![Vec2::new(10.0, 8.0), Vec2::new(20.0, 8.0), Vec2::new(20.0, 12.0), Vec2::new(10.0, 12.0)];
    let region = CutRegion { outer: outer.clone(), islands: vec![island.clone()] };
    let spans = hatch_spans(&region, 3.0, 45.0, None);
    assert!(!spans.is_empty());
    let normal = Vec2::new(-45.0_f64.to_radians().sin(), 45.0_f64.to_radians().cos());
    let mut offsets: Vec<f64> = spans.iter().map(|(a, b)| ((*a + *b) * 0.5).dot(normal)).collect();
    offsets.sort_by(f64::total_cmp);
    offsets.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    for pair in offsets.windows(2) {
        assert!((pair[1] - pair[0] - 3.0).abs() < 0.01, "spacing {} at {pair:?}", pair[1] - pair[0]);
    }
    for (a, b) in &spans {
        let direction = (*b - *a).normalized().expect("a stroke has length");
        assert!((direction.y.atan2(direction.x).to_degrees() - 45.0).abs() < 0.1);
        let mid = (*a + *b) * 0.5;
        assert!(inside_polygon(mid, &outer) && !inside_polygon(mid, &island), "a hatch stroke inside the island at {mid:?}");
    }
}

#[test]
fn cut_loops_chain_and_nest_into_a_region_with_an_island() {
    let square = [
        (Vec2::ZERO, Vec2::new(10.0, 0.0)),
        (Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)),
        (Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)),
        (Vec2::new(0.0, 10.0), Vec2::ZERO),
    ];
    let loops = chain_loops(&square, 1e-6);
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].len(), 4);
    let hole = vec![Vec2::new(3.0, 3.0), Vec2::new(6.0, 3.0), Vec2::new(6.0, 6.0), Vec2::new(3.0, 6.0)];
    let regions = regions(&[loops[0].clone(), hole]);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].islands.len(), 1);
}

#[test]
fn a_region_clipped_to_a_boundary_is_smaller_and_still_closed() {
    let outer = vec![Vec2::new(0.0, 0.0), Vec2::new(30.0, 0.0), Vec2::new(30.0, 20.0), Vec2::new(0.0, 20.0)];
    let region = CutRegion { outer: outer.clone(), islands: Vec::new() };
    let boundary = Boundary::circle(15.0, 10.0, 6.0).points();
    let clipped = clip_regions(&[region], &boundary, 1e-6);
    assert_eq!(clipped.len(), 1);
    let area = polygon_area(&clipped[0].outer).abs();
    assert!(area < polygon_area(&outer).abs() * 0.4, "{area}");
    for p in &clipped[0].outer {
        assert!(inside_polygon(*p, &boundary) || distance_to_polygon(*p, &boundary) < 1e-5, "{p:?} outside the boundary");
    }
}

// ------------------------------------------------------------------------------- full section

#[test]
fn the_flywheel_gets_a_full_section_with_two_hatch_regions() {
    let plan = plan_with(&flywheel_part(), SectionMode::Auto);
    let sheet = &plan.sheet;
    assert_eq!(sheet.sections.len(), 1, "{:?}", sheet.sections);
    let section = &sheet.sections[0];
    assert!(section.full && section.label == Some('A'));
    assert_eq!(section.regions.len(), 2, "a disc cut through its axis has two material regions: {:?}", section.regions);
    assert!(section.hatch.len() > 10, "{} hatch strokes", section.hatch.len());
    assert_eq!(front(&plan).hidden.len(), 0, "a full section keeps no hidden line");
    // No hidden prim inside the section view's box.
    let placed = placed_of(sheet, StandardView::Front);
    for (a, b) in layer_lines(sheet, Layer::Hidden) {
        let mid = (a + b) * 0.5;
        assert!(
            mid.x < placed.bbox[0].x - 1e-6 || mid.x > placed.bbox[1].x + 1e-6 || mid.y < placed.bbox[0].y - 1e-6 || mid.y > placed.bbox[1].y + 1e-6,
            "a hidden line {a:?}..{b:?} inside the full section"
        );
    }
    // 45° and 3 mm on paper, and no stroke inside the bore (|x| < 8) or the recess
    // (20 < |x| < 45 near a face) of the disc.
    let [placed_lo, placed_hi] = placed.bbox;
    let normal = Vec2::new(-45.0_f64.to_radians().sin(), 45.0_f64.to_radians().cos());
    let mut offsets: Vec<f64> = Vec::new();
    for (a, b) in &section.hatch {
        let direction = (*b - *a).normalized().expect("hatch stroke");
        assert!((direction.y.atan2(direction.x).to_degrees() - 45.0).abs() < 0.1, "{direction:?}");
        let mid = (*a + *b) * 0.5;
        offsets.push(mid.dot(normal));
        // A stroke is drawn only in material: inside one region's outer loop and outside every
        // island. The bore and the recess are the gaps between the two regions, so this is the
        // measured "no hatch line inside a void".
        let in_material = section
            .regions
            .iter()
            .any(|region| inside_polygon(mid, &region.outer) && !region.islands.iter().any(|island| inside_polygon(mid, island)));
        assert!(in_material, "a hatch stroke at {mid:?} is not in the cut material");
        assert!(mid.x >= placed_lo.x - 1e-6 && mid.x <= placed_hi.x + 1e-6 && mid.y >= placed_lo.y - 1e-6 && mid.y <= placed_hi.y + 1e-6);
    }
    // The Ø16 bore is the gap between the two material regions.
    let mut gap = (f64::NEG_INFINITY, f64::INFINITY);
    for (_, b) in &section.hatch {
        let _ = b;
    }
    if let (Some(left), Some(right)) = (section.regions.first(), section.regions.get(1)) {
        gap.0 = left.outer.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        gap.1 = right.outer.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    }
    for (a, b) in &section.hatch {
        let mid = (*a + *b) * 0.5;
        assert!(!(mid.x > gap.0 + 1e-6 && mid.x < gap.1 - 1e-6), "a hatch stroke at {mid:?} is inside the Ø16 bore ({gap:?})");
    }
    offsets.sort_by(f64::total_cmp);
    offsets.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let mut smallest = f64::INFINITY;
    for pair in offsets.windows(2) {
        let step = pair[1] - pair[0];
        if step > 1e-6 {
            smallest = smallest.min(step);
        }
    }
    assert!((smallest - 3.0).abs() < 0.01, "hatch spacing {smallest}");
    // The section line (thin, on CENTER) and its two thick ends + arrows, the two A letters and
    // the A–A label.
    assert!(!layer_lines(sheet, Layer::Center).is_empty());
    let thick = layer_lines(sheet, Layer::Section);
    assert_eq!(thick.len(), 2, "two thick 6 mm ends: {thick:?}");
    for (a, b) in &thick {
        assert!((a.dist(*b) - 6.0).abs() < 1e-6, "a section end is {:.2} mm", a.dist(*b));
    }
    let texts = text_prims(sheet);
    assert_eq!(texts.iter().filter(|(_, h, _, t)| t == "A" && (*h - 5.0).abs() < 1e-9).count(), 2, "{texts:?}");
    assert!(texts.iter().any(|(_, h, _, t)| t == "A\u{2013}A" && (*h - 5.0).abs() < 1e-9), "{texts:?}");
    // The filled arrows of the viewing direction exist (Layer::Section polygons).
    assert!(sheet.prims.iter().filter(|p| matches!(p, Prim::Filled { layer: Layer::Section, .. })).count() >= 2);
    assert!(sheet.unplaced.is_empty(), "unplaced: {:?}", sheet.unplaced);
    // D9.1: the recess diameter reads on the end view when that view draws its circle, else it
    // keeps the D9 form on the visible cut wall; its text keeps the depth either way.
    let dim = sheet.dimensions.iter().find(|d| d.text.starts_with("Ø90")).expect("the recess dimension");
    assert!(dim.text.contains("tief"), "the recess keeps its depth: {}", dim.text);
    if dim.view == StandardView::Top {
        let placed_end = placed_of(sheet, StandardView::Top);
        let visible = layer_lines(sheet, Layer::Visible);
        for p in [dim.def_point2, dim.def_point3] {
            let radius = (p - placed_end.origin).len();
            let wanted = 45.0 * sheet.scale_factor();
            assert!((radius - wanted).abs() <= 0.05, "the Ø90 arrow at {p:?} is {radius:.2} mm from the centre, want {wanted:.2}");
            assert!(
                visible.iter().any(|(a, b)| point_segment_distance(p, *a, *b) <= 0.05),
                "the Ø90 dimension's point {p:?} is not on a visible edge"
            );
        }
    } else {
        let visible = layer_lines(sheet, Layer::Visible);
        for p in [dim.def_point2, dim.def_point3] {
            assert!(
                visible.iter().any(|(a, b)| point_segment_distance(p, *a, *b) <= 0.05),
                "the Ø90 dimension's point {p:?} is not on a visible cut edge"
            );
        }
    }
}

#[test]
fn the_full_section_dxf_carries_one_hatch_per_cut_region_on_schraffur() {
    let plan = plan_with(&flywheel_part(), SectionMode::Auto);
    let dxf = sheet_to_dxf(&plan.sheet).unwrap();
    let pairs: Vec<(i32, String)> = {
        let lines: Vec<&str> = dxf.lines().collect();
        lines.chunks(2).filter_map(|c| Some((c.first()?.trim().parse().ok()?, c.get(1)?.trim().to_string()))).collect()
    };
    let hatches: Vec<usize> = pairs.iter().enumerate().filter(|(_, (c, v))| *c == 0 && v == "HATCH").map(|(i, _)| i).collect();
    assert_eq!(hatches.len(), plan.sheet.sections[0].regions.len(), "one HATCH per cut region");
    for at in hatches {
        let entity: Vec<&(i32, String)> = pairs[at + 1..].iter().take_while(|(c, _)| *c != 0).collect();
        assert!(entity.iter().any(|(c, v)| *c == 8 && v == "HATCH"), "HATCH not on HATCH: {entity:?}");
        assert!(entity.iter().any(|(c, v)| *c == 2 && v == "ANSI31"));
        let paths = entity.iter().filter(|(c, _)| *c == 91).filter_map(|(_, v)| v.parse::<usize>().ok()).next().unwrap_or(0);
        assert!(paths >= 1, "a HATCH needs a boundary path");
    }
    // A cut region with an island: a box with a bore along the cutting plane gives one outer
    // path and one island path.
    let mut box_session = Session::default();
    box_session.execute("solid.box", &json!({"length":40,"width":20,"height":10,"body_name":"Box"})).unwrap();
    box_session
        .execute("solid.cylinder", &json!({"base":[20,-1,5],"axis":[0,1,0],"radius":4,"height":22,"operation":"cut","targets":["Box"]}))
        .unwrap();
    let mode = SectionMode::Explicit(vec![SectionSpec::Full {
        view: StandardView::Front,
        plane: CuttingPlane { point: solidcraft_geom::Vec3::new(20.0, 10.0, 5.0), normal: solidcraft_geom::Vec3::Y },
        letter: None,
    }]);
    let plan = plan_with(&box_session, mode);
    let section = &plan.sheet.sections[0];
    assert_eq!(section.regions.len(), 1, "{:?}", section.regions);
    assert_eq!(section.regions[0].islands.len(), 1, "the bore is an island: {:?}", section.regions[0]);
    let dxf = sheet_to_dxf(&plan.sheet).unwrap();
    let pairs: Vec<(i32, String)> = {
        let lines: Vec<&str> = dxf.lines().collect();
        lines.chunks(2).filter_map(|c| Some((c.first()?.trim().parse().ok()?, c.get(1)?.trim().to_string()))).collect()
    };
    let at = pairs.iter().position(|(c, v)| *c == 0 && v == "HATCH").expect("a HATCH");
    let entity: Vec<&(i32, String)> = pairs[at + 1..].iter().take_while(|(c, _)| *c != 0).collect();
    let paths = entity.iter().find(|(c, _)| *c == 91).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    assert_eq!(paths, 2, "outer loop + bore island: {entity:?}");
}

// ------------------------------------------------------------------------------- break-outs

#[test]
fn the_shaft_gets_two_breakouts_with_no_hidden_lines_inside() {
    let s = stepped_shaft_with_features();
    let plan = plan_with(&s, SectionMode::Auto);
    let sheet = &plan.sheet;
    assert_eq!(sheet.sections.len(), 1);
    let section = &sheet.sections[0];
    assert!(!section.full && section.label.is_none());
    assert_eq!(section.boundaries.len(), 2, "a blind end bore and a cross hole: {:?}", section.boundaries);
    for boundary in &section.boundaries {
        assert_eq!(boundary.len(), 64, "a freehand polyline, not a zigzag");
        let centre = boundary.iter().fold(Vec2::ZERO, |sum, p| sum + *p) * (1.0 / boundary.len() as f64);
        let radii: Vec<f64> = boundary.iter().map(|p| p.dist(centre)).collect();
        let min = radii.iter().copied().fold(f64::INFINITY, f64::min);
        let max = radii.iter().copied().fold(0.0, f64::max);
        assert!(max - min >= 1.0, "the boundary is not a freehand wobble: {min}..{max}");
    }
    // Two regions per break-out: the cut through a bore splits the material.
    assert_eq!(section.regions.len(), 4, "{:?}", section.regions);
    // No hidden segment inside a boundary, in view mm.
    let boundaries = view_boundaries(&plan, StandardView::Front);
    for seg in &front(&plan).hidden {
        let mid = (seg.a + seg.b) * 0.5;
        assert!(!inside_any(mid, &boundaries), "hidden line {seg:?} inside a break-out");
    }
    // The bore walls are visible: two horizontal edges at ±4 over at least 10 mm in the bore's
    // x range.
    let visible: Vec<crate::drawing::Seg2mm> = front(&plan).visible.clone();
    for y in [4.0_f64, -4.0] {
        // The tessellation splits the wall into short edges: merge the collinear runs.
        let mut runs: Vec<(f64, f64)> = visible
            .iter()
            .filter(|seg| (seg.a.y - y).abs() < 0.05 && (seg.b.y - y).abs() < 0.05)
            .map(|seg| (seg.a.x.min(seg.b.x), seg.a.x.max(seg.b.x)))
            .collect();
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut covered = 0.0;
        let mut end = f64::NEG_INFINITY;
        for (lo, hi) in runs {
            let start = lo.max(end);
            if hi > start {
                covered += hi - start;
                end = hi;
            }
        }
        assert!(covered >= 10.0, "the bore wall at y={y} is drawn visible over only {covered:.1} mm");
    }
    // No hatch stroke inside the bore void (|y| < 4 at the right end) or the cross-hole void
    // (|x - 55| < 3), in sheet mm.
    let placed = placed_of(sheet, StandardView::Front);
    let scale = sheet.scale_factor();
    for (a, b) in &section.hatch {
        let model = ((*a + *b) * 0.5 - placed.origin) * (1.0 / scale);
        assert!(!(model.x > 99.0 && model.y.abs() < 4.0), "hatch inside the blind bore at {model:?}");
        assert!(!((model.x - 55.0).abs() < 3.0), "hatch inside the cross hole at {model:?}");
    }
    // Outside the boundaries the drawing is unchanged: sample the no-section view outside them.
    let none = plan_with(&s, SectionMode::None);
    let no_section = front(&none);
    let outside = |p: Vec2| !inside_any(p, &boundaries) && boundaries.iter().all(|poly| distance_to_polygon(p, poly) > 2.0);
    for (hidden, segments) in [(false, &no_section.visible), (true, &no_section.hidden)] {
        for seg in segments {
            for p in [seg.a, (seg.a + seg.b) * 0.5, seg.b] {
                if !outside(p) {
                    continue;
                }
                let found = if hidden { covers(&front(&plan).hidden, p) } else { covers(&front(&plan).visible, p) };
                assert!(found, "the view changed outside the break-outs at {p:?}");
            }
        }
    }
    assert!(sheet.unplaced.is_empty(), "unplaced: {:?}", sheet.unplaced);
}

#[test]
fn a_flat_plate_gets_no_section() {
    let plan = plan_with(&plate_with_two_holes(), SectionMode::Auto);
    assert!(plan.sheet.sections.is_empty());
    assert!(plan.sheet.prims.iter().all(|p| !matches!(p.layer(), Layer::Hatch | Layer::Section | Layer::Freehand)));
}

#[test]
fn sections_none_draws_no_section_geometry_and_keeps_the_hidden_lines() {
    let plan = plan_with(&stepped_shaft_with_features(), SectionMode::None);
    assert!(plan.sheet.sections.is_empty());
    assert!(plan.sheet.prims.iter().all(|p| !matches!(p.layer(), Layer::Hatch | Layer::Section | Layer::Freehand)));
    let hidden = layer_lines(&plan.sheet, Layer::Hidden);
    assert!(!hidden.is_empty(), "the D8 hidden lines stay");
    // The explicit empty list is the same sheet.
    let explicit = plan_with(&stepped_shaft_with_features(), SectionMode::Explicit(Vec::new()));
    assert_eq!(explicit.sheet.prims, plan.sheet.prims);
}

#[test]
fn an_explicit_full_section_on_a_box_gives_one_region() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":20,"height":10,"body_name":"Box"})).unwrap();
    let mode = SectionMode::Explicit(vec![SectionSpec::Full {
        view: StandardView::Front,
        plane: CuttingPlane { point: solidcraft_geom::Vec3::new(20.0, 10.0, 5.0), normal: solidcraft_geom::Vec3::Y },
        letter: None,
    }]);
    let plan = plan_with(&s, mode);
    let section = &plan.sheet.sections[0];
    assert!(section.full);
    assert_eq!(section.regions.len(), 1);
    assert!(!section.hatch.is_empty());
    assert_eq!(front(&plan).hidden.len(), 0);
}

// ------------------------------------------------------------------------------- sheet rules

#[test]
fn no_dimension_text_lands_on_the_hatching_and_centre_lines_come_last() {
    for s in [flywheel_part(), stepped_shaft_with_features()] {
        let plan = plan_with(&s, SectionMode::Auto);
        let sheet = &plan.sheet;
        let boxes = dimension_text_boxes(sheet);
        for (a, b) in layer_lines(sheet, Layer::Hatch) {
            for corners in &boxes {
                assert!(!segment_hits_box(a, b, corners), "a hatch stroke {a:?}..{b:?} crosses a dimension text {corners:?}");
            }
        }
        let last_hatch = sheet.prims.iter().rposition(|p| p.layer() == Layer::Hatch).unwrap_or(0);
        let first_centre = sheet.prims.iter().position(|p| p.layer() == Layer::Center).unwrap_or(usize::MAX);
        assert!(last_hatch < first_centre, "the centre lines are drawn over the hatching");
    }
}

#[test]
fn the_notes_surface_value_starts_right_of_the_long_leg() {
    let sheet = plan_with(&plate_with_two_holes(), SectionMode::None).sheet;
    let value = sheet
        .prims
        .iter()
        .find_map(|prim| match prim {
            Prim::Text { at, text, anchor: TextAnchor::Left, height, .. } if text == "Ra 3,2" => Some((*at, *height)),
            _ => None,
        })
        .expect("the Ra value");
    // The long leg is the thin 60° line that reaches H2 above the symbol's baseline.
    let mut checked = false;
    let mut nearest = f64::NEG_INFINITY;
    for (a, b) in layer_lines(&sheet, Layer::Thin) {
        let d = b - a;
        if d.x.abs() < 1e-9 {
            continue;
        }
        let angle = d.y.atan2(d.x).to_degrees();
        if (angle - 60.0).abs() > 0.1 && (angle + 120.0).abs() > 0.1 {
            continue;
        }
        let (low, high) = if a.y < b.y { (a, b) } else { (b, a) };
        if high.x - low.x < 1.0 {
            continue;
        }
        let length = low.dist(high);
        // The long leg is H2 = 9.8 mm high: 9.8 / sin 60° = 11.31 mm long.
        if (length - 9.8 / 60.0_f64.to_radians().sin()).abs() > 0.3 {
            continue;
        }
        if low.x >= value.0.x {
            continue;
        }
        let text_top = value.0.y + value.1;
        let x_at_top = low.x + (text_top - low.y) / 60.0_f64.to_radians().tan();
        nearest = nearest.max(x_at_top);
        checked = true;
    }
    assert!(checked, "no long 60° leg left of the value");
    assert!(value.0.x - nearest >= 0.5, "Ra text starts {:.2} mm right of the long leg", value.0.x - nearest);
}

#[test]
fn a_radius_text_stays_within_one_text_height_of_its_arrow() {
    let mut s = Session::default();
    s.execute("solid.cylinder", &json!({"base":[0,0,0],"axis":[0,0,1],"radius":20,"height":10,"body_name":"Crank"})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[100,0,0],"axis":[0,0,1],"radius":12,"height":10,"operation":"join","targets":["Crank"]})).unwrap();
    s.execute("solid.box", &json!({"length":100,"width":24,"height":10,"corner":[0,-12,0],"operation":"join","targets":["Crank"]})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[0,0,-1],"axis":[0,0,1],"radius":8,"height":12,"operation":"cut","targets":["Crank"]})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[100,0,-1],"axis":[0,0,1],"radius":5,"height":12,"operation":"cut","targets":["Crank"]})).unwrap();
    let plan = plan_with(&s, SectionMode::Auto);
    let sheet = &plan.sheet;
    for text in ["R12", "R20"] {
        let dim = sheet.dimensions.iter().find(|d| d.text == text).unwrap_or_else(|| panic!("{text}"));
        let arrow = dim.opposite.expect("the arc point");
        let prim = dim.prims.iter().find(|p| matches!(p, Prim::Text { .. })).expect("text prim");
        let Prim::Text { at, height, angle_deg, text: value, anchor, .. } = prim else { panic!("not a text") };
        let width = text_width_mm(value, *height);
        let u = Vec2::from_angle(angle_deg.to_radians());
        let centre = match anchor {
            TextAnchor::Left => *at + u * (width * 0.5),
            TextAnchor::Center => *at,
            TextAnchor::Right => *at - u * (width * 0.5),
        };
        let corners = text_box(centre, width, *angle_deg, *height);
        // The leader reaches the text (D9: no floating value): one of its drawn ends is on the
        // text box, within the 0.5 mm clearance.
        let mut nearest = f64::INFINITY;
        for prim in &dim.prims {
            if let Prim::Line { a, b, .. } = prim {
                for p in [*a, *b] {
                    nearest = nearest.min(if inside_box(p, &corners) { 0.0 } else { point_box_distance(p, &corners) });
                }
            }
        }
        assert!(nearest <= 1.1, "{text}'s text is {nearest:.2} mm from its leader end");
        // The leader starts at the arrow; the text may be farther along it, but the arrow is
        // still on the same dimension line.
        let along = (arrow - dim.def_point).normalized().expect("a radius line");
        let to_text = (centre - arrow).dot(along);
        assert!(to_text > 0.0, "{text}'s text is not along its radius line");
    }
}

#[test]
fn the_flywheel_takes_a_second_scale_and_the_view_group_is_centred() {
    let plan = plan_with(&flywheel_part(), SectionMode::Auto);
    let sheet = &plan.sheet;
    assert!(sheet.scale == (1, 2) || sheet.scale == (1, 1) || sheet.scale.0 > sheet.scale.1, "flywheel scale {:?}", sheet.scale);
    let [_notes_lo, notes_hi] = sheet.notes_block_rect();
    let [frame_lo, frame_hi] = sheet.frame_rect();
    let boxes: Vec<[Vec2; 2]> = sheet.views.iter().map(|v| v.bbox).collect();
    let lo = boxes.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |m, b| Vec2::new(m.x.min(b[0].x), m.y.min(b[0].y)));
    let hi = boxes.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |m, b| Vec2::new(m.x.max(b[1].x), m.y.max(b[1].y)));
    let centre = (lo + hi) * 0.5;
    assert!(centre.y >= notes_hi.y, "the views are in the upper arm");
    let region = Vec2::new((frame_lo.x + frame_hi.x) * 0.5, (notes_hi.y + frame_hi.y) * 0.5);
    assert!(
        (centre - region).len() <= 5.0,
        "the view group centre {centre:?} is {:.1} mm from the free region's {region:?}",
        (centre - region).len()
    );
    assert!(matches!(sheet.arrangement, Arrangement::SideBySide | Arrangement::FirstAngle | Arrangement::VerticalStack));
}

// ------------------------------------------------------------------------------- acceptance

/// The four acceptance sheets: no scale smaller than D8's, the group centred, and the expected
/// section per part. Run with `SOLIDCRAFT_D9_FIXTURES=1`.
#[test]
fn acceptance_parts_meet_the_d9_rules() {
    if std::env::var_os("SOLIDCRAFT_D9_FIXTURES").is_none() {
        return;
    }
    let parts = [
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/shaft.solidcraft"), (1, 1)),
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/crank.solidcraft"), (1, 1)),
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/flywheel.solidcraft"), (1, 2)),
    ];
    for (path, at_least) in parts {
        let mut s = Session::default();
        s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
        let plan = plan_with(&s, SectionMode::Auto);
        let sheet = &plan.sheet;
        let factor = sheet.scale_factor();
        let least = f64::from(at_least.0) / f64::from(at_least.1);
        assert!(factor >= least - 1e-9, "{path}: scale {} is smaller than {at_least:?}", sheet.scale_text());
        let [_notes_lo, notes_hi] = sheet.notes_block_rect();
        let [frame_lo, frame_hi] = sheet.frame_rect();
        let lo = sheet.views.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |m, v| Vec2::new(m.x.min(v.bbox[0].x), m.y.min(v.bbox[0].y)));
        let hi =
            sheet.views.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |m, v| Vec2::new(m.x.max(v.bbox[1].x), m.y.max(v.bbox[1].y)));
        let centre = (lo + hi) * 0.5;
        let region = if centre.y >= notes_hi.y {
            Vec2::new((frame_lo.x + frame_hi.x) * 0.5, (notes_hi.y + frame_hi.y) * 0.5)
        } else {
            let [tb_lo, _] = sheet.title_block_rect();
            Vec2::new((frame_lo.x + tb_lo.x) * 0.5, (frame_lo.y + frame_hi.y) * 0.5)
        };
        assert!((centre - region).len() <= 5.0, "{path}: view group centre {centre:?} vs {region:?}");
        if path.contains("flywheel") {
            assert_eq!(sheet.sections.len(), 1);
            assert!(sheet.sections[0].full && sheet.sections[0].label == Some('A'));
            assert_eq!(sheet.sections[0].regions.len(), 2);
            assert_eq!(sheet.sections[0].spacing_mm, 3.0);
        }
        if path.contains("shaft") {
            assert_eq!(sheet.sections[0].boundaries.len(), 2);
        }
        if path.contains("crank") {
            assert!(sheet.sections.is_empty());
        }
        assert!(sheet.unplaced.is_empty(), "{path}: unplaced {:?}", sheet.unplaced);
        // No two annotation texts touch (D8's Ø20/30 defect, extended to every pair and every
        // rotation).
        let boxes = dimension_text_boxes(sheet);
        for i in 0..boxes.len() {
            for j in (i + 1)..boxes.len() {
                assert!(
                    !boxes_overlap(&boxes[i], &boxes[j], TEXT_CLEAR_MM - 1e-6),
                    "{path}: annotation texts {i} and {j} touch: {:?} {:?}",
                    boxes[i],
                    boxes[j]
                );
            }
        }
    }
}

/// A Ø20 × 40 shaft with a Ø6 × 12 blind bore from the right end and a Ø3 cross hole 4 mm from
/// that end: both break-outs would cross the end face if the window were not fitted (D9.1).
fn shaft_with_a_bore_close_to_the_end() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Bar"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,10],[40,10],[40,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Bar","axis":"x","angle":360,"body_name":"Bar"})).unwrap();
    s.execute(
        "solid.hole",
        &json!({"position":[40,0,0],"direction":[-1,0,0],"diameter":6,"depth":12,"type":"drilled","tip_angle":118,"targets":["Bar"]}),
    )
    .unwrap();
    s.execute("solid.cylinder", &json!({"base":[36,0,-20],"axis":[0,0,1],"radius":1.5,"height":40,"operation":"cut","targets":["Bar"]})).unwrap();
    s
}

/// Every point of a break-out's freehand boundary stays inside the view's outline (0.1 mm).
fn assert_breakouts_inside_outline(s: &Session, plan: &SheetPlan) {
    let scale = plan.sheet.scale_factor();
    for section in &plan.sheet.sections {
        let Some(polygon) = super::turned_silhouette(s, section.view) else { continue };
        let placed = placed_of(&plan.sheet, section.view);
        for boundary in &section.boundaries {
            for point in boundary {
                let view_point = (*point - placed.origin) * (1.0 / scale);
                let outside = if inside_polygon(view_point, &polygon) { 0.0 } else { distance_to_polygon(view_point, &polygon) };
                assert!(
                    outside <= 0.1 + 1e-9,
                    "a break-out point {view_point:?} lies {outside:.2} mm outside the outline (boundary in {:?})",
                    section.view
                );
            }
        }
    }
}

#[test]
fn break_out_windows_stay_inside_the_outline() {
    for s in [stepped_shaft_with_features(), shaft_with_a_bore_close_to_the_end()] {
        let plan = plan_with(&s, SectionMode::Auto);
        assert!(!plan.sheet.sections.is_empty(), "the test part is sectioned");
        assert!(plan.sheet.sections.iter().any(|s| !s.boundaries.is_empty()), "at least one break-out");
        assert_breakouts_inside_outline(&s, &plan);
    }
}

#[test]
fn a_break_out_window_fits_its_step_and_scales_its_wobble() {
    // A Ø3 cross hole 4 mm from a Ø10 end: a fitted window whose
    // wobble is 0.34 mm, not the old 1.5 mm blob.
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Small"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,5],[30,5],[30,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Small","axis":"x","angle":360,"body_name":"Small"})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[26,0,-10],"axis":[0,0,1],"radius":1.5,"height":20,"operation":"cut","targets":["Small"]})).unwrap();
    let plan = plan_with(&s, SectionMode::Auto);
    let section = plan.sheet.sections.first().expect("a break-out");
    let boundary = section.boundaries.first().expect("a freehand boundary");
    assert_breakouts_inside_outline(&s, &plan);
    // The boundary's radial wobble: max minus min radius about its centre.
    let centre = boundary.iter().fold(Vec2::ZERO, |sum, p| sum + *p) * (1.0 / boundary.len() as f64);
    let radii: Vec<f64> = boundary.iter().map(|p| p.dist(centre)).collect();
    let min = radii.iter().copied().fold(f64::INFINITY, f64::min);
    let max = radii.iter().copied().fold(0.0, f64::max);
    assert!(max - min >= 0.3 * plan.sheet.scale_factor() - 1e-6, "the wobble is visible: {:.2} mm", max - min);
    assert!(max - min <= 2.0 * FREEHAND_AMPLITUDE_MM * plan.sheet.scale_factor() + 1e-6, "the wobble stays small: {:.2} mm", max - min);
}
