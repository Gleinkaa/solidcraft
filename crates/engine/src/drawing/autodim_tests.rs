#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Measured tests for automatic dimensions and size notes.

use super::*;
use crate::Session;
use crate::drawing::dims::{Annotations, TEXT_CLEAR_MM};
use crate::drawing::sheet::{Layer, Prim, Sheet, TextAnchor, TitleBlock};
use crate::drawing::{NotesBlock, plan_sheet_auto};
use serde_json::json;
use solidcraft_io::text_width_mm;

/// A stepped shaft along X: Ø20 × 30, Ø30 × 60, Ø16 × 40 (130 mm overall).
fn stepped_shaft() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Profile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,10],[30,10],[30,15],[90,15],[90,8],[130,8],[130,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})).unwrap();
    s
}

fn shaft_with_blind_bore() -> Session {
    let mut s = stepped_shaft();
    s.execute(
        "solid.hole",
        &json!({"position":[130,0,0],"direction":[-1,0,0],"diameter":6,"depth":20,"type":"drilled","tip_angle":118,"targets":["Shaft"]}),
    )
    .unwrap();
    s
}

fn shaft_with_cross_hole() -> Session {
    let mut s = stepped_shaft();
    s.execute("solid.cylinder", &json!({"base":[125,0,-20],"axis":[0,0,1],"radius":1.5,"height":40,"operation":"cut","targets":["Shaft"]})).unwrap();
    s
}

/// An 80 × 40 × 10 plate with two Ø8 through holes at (15, 20) and (65, 20).
fn plate_with_two_holes() -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":80,"width":40,"height":10,"body_name":"Plate"})).unwrap();
    for x in [15.0, 65.0] {
        s.execute("solid.cylinder", &json!({"base":[x,20,-1],"axis":[0,0,1],"radius":4,"height":12,"operation":"cut","targets":["Plate"]})).unwrap();
    }
    s
}

/// Two bosses R20 and R12 around Ø16 and Ø10 through holes, 100 mm apart, joined by a web.
fn crank_like() -> Session {
    let mut s = Session::default();
    s.execute("solid.cylinder", &json!({"base":[0,0,0],"axis":[0,0,1],"radius":20,"height":10,"body_name":"Crank"})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[100,0,0],"axis":[0,0,1],"radius":12,"height":10,"operation":"join","targets":["Crank"]})).unwrap();
    s.execute("solid.box", &json!({"length":100,"width":24,"height":10,"corner":[0,-12,0],"operation":"join","targets":["Crank"]})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[0,0,-1],"axis":[0,0,1],"radius":8,"height":12,"operation":"cut","targets":["Crank"]})).unwrap();
    s.execute("solid.cylinder", &json!({"base":[100,0,-1],"axis":[0,0,1],"radius":5,"height":12,"operation":"cut","targets":["Crank"]})).unwrap();
    s
}

fn plan_of(s: &Session) -> (Sheet, Vec<AutoDimensionSource>) {
    let title = TitleBlock::for_design("Part");
    let (plan, sources) = plan_sheet_auto(s, None, 0.01, &title, &NotesBlock::default(), None, &Annotations::default()).unwrap();
    assert!(plan.sheet.unplaced.is_empty(), "unplaced: {:?}", plan.sheet.unplaced);
    (plan.sheet, sources)
}

/// The text of every dimension and note on the sheet.
fn texts(sheet: &Sheet) -> Vec<String> {
    let mut out: Vec<String> = sheet.dimensions.iter().map(|d| d.text.clone()).collect();
    out.extend(sheet.notes.iter().map(|n| n.text_string.clone()));
    out
}

fn texts_of(sheet: &Sheet, text: &str) -> usize {
    texts(sheet).iter().filter(|t| t.as_str() == text).count()
}

fn measured(sheet: &Sheet, text: &str) -> Option<f64> {
    sheet.dimensions.iter().find(|d| d.text == text).map(|d| d.measurement)
}

/// The sheet-mm baseline y of a dimension's text.
fn text_y(sheet: &Sheet, text: &str) -> Option<f64> {
    let dim = sheet.dimensions.iter().find(|d| d.text == text)?;
    dim.prims.iter().find_map(|p| match p {
        Prim::Text { at, .. } => Some(at.y),
        _ => None,
    })
}

fn text_box(at: Vec2, width: f64, angle_deg: f64, height: f64) -> [Vec2; 4] {
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    [at - u * (width * 0.5), at + u * (width * 0.5), at + u * (width * 0.5) + n * height, at - u * (width * 0.5) + n * height]
}

fn segment_hits_box(a: Vec2, b: Vec2, corners: &[Vec2; 4], clear: f64) -> bool {
    let along = (corners[1] - corners[0]).normalized().unwrap_or(Vec2::X);
    let up = along.perp();
    let origin = corners[0] - along * clear - up * clear;
    let local = |p: Vec2| Vec2::new((p - origin).dot(along), (p - origin).dot(up));
    let lo = Vec2::ZERO;
    let hi = Vec2::new(corners[1].dist(corners[0]) + 2.0 * clear, corners[3].dist(corners[0]) + 2.0 * clear);
    crate::drawing::dims::segment_hits_rect(local(a), local(b), lo, hi)
}

/// The brief's geometry check: no annotation text box is crossed by any sheet line, and every
/// box keeps the 0.5 mm clearance.
fn assert_texts_clear(sheet: &Sheet) {
    let mut lines = Vec::new();
    for prim in &sheet.prims {
        if !matches!(prim, Prim::Filled { .. }) {
            prim_lines(prim, &mut lines);
        }
    }
    for prim in &sheet.prims {
        let Prim::Text { at, height, angle_deg, text, anchor, layer } = prim else { continue };
        if *layer != Layer::Dimension {
            continue;
        }
        let width = text_width_mm(text, *height);
        let u = Vec2::from_angle(angle_deg.to_radians());
        let centre = match anchor {
            TextAnchor::Left => *at + u * (width * 0.5),
            TextAnchor::Center => *at,
            TextAnchor::Right => *at - u * (width * 0.5),
        };
        let corners = text_box(centre, width, *angle_deg, *height);
        for (a, b) in &lines {
            assert!(
                !segment_hits_box(*a, *b, &corners, TEXT_CLEAR_MM - 1e-6),
                "a line {a:?}..{b:?} crosses or comes closer than 0.5 mm to `{text}` at {at:?} angle {angle_deg} box {corners:?}"
            );
        }
    }
}

#[test]
fn a_stepped_shaft_gets_one_diameter_per_step_and_an_open_length_chain() {
    let (sheet, sources) = plan_of(&stepped_shaft());
    assert_eq!(texts_of(&sheet, "Ø20"), 1, "{:?}", texts(&sheet));
    assert_eq!(texts_of(&sheet, "Ø30"), 1, "{:?}", texts(&sheet));
    assert_eq!(texts_of(&sheet, "Ø16"), 1, "{:?}", texts(&sheet));
    for value in [20.0, 30.0, 16.0] {
        let text = format!("Ø{}", crate::drawing::format_value(value));
        let got = measured(&sheet, &text).unwrap_or_else(|| panic!("no {text}: {:?}", texts(&sheet)));
        assert!((got - value).abs() <= 1e-9, "{text} measured {got}");
    }
    // The chain: 30 and 60 on one row, overall 130 on the next, and no 40.
    for (text, value) in [("30", 30.0), ("60", 60.0), ("130", 130.0)] {
        let got = measured(&sheet, text).unwrap_or_else(|| panic!("no {text}: {:?}", texts(&sheet)));
        assert!((got - value).abs() <= 1e-9, "{text} measured {got}");
    }
    assert_eq!(texts_of(&sheet, "40"), 0, "the open chain end must not be dimensioned: {:?}", texts(&sheet));
    let y30 = text_y(&sheet, "30").unwrap();
    let y60 = text_y(&sheet, "60").unwrap();
    let y130 = text_y(&sheet, "130").unwrap();
    assert!((y30 - y60).abs() <= 1e-9, "the chain segments share one dimension line: {y30} vs {y60}");
    assert!(y130 < y30 - 7.0, "the overall length is on the next row, 8 mm further out: {y130} vs {y30}");
    let diameters: Vec<&AutoDimensionSource> = sources.iter().filter(|s| s.kind == "diameter").collect();
    assert_eq!(diameters.len(), 3, "{sources:?}");
    assert_texts_clear(&sheet);
}

#[test]
fn a_blind_bore_gets_its_size_and_cylindrical_depth_as_a_note() {
    let (sheet, _) = plan_of(&shaft_with_blind_bore());
    let notes: Vec<String> = sheet.notes.iter().map(|n| n.text_string.clone()).collect();
    assert!(notes.iter().any(|t| t == "Bohrung Ø6 x 20 tief"), "{notes:?}");
    assert_texts_clear(&sheet);
}

#[test]
fn a_cross_hole_gets_a_through_note_and_a_position_from_the_nearer_end() {
    let (sheet, _) = plan_of(&shaft_with_cross_hole());
    let notes: Vec<String> = sheet.notes.iter().map(|n| n.text_string.clone()).collect();
    assert!(notes.iter().any(|t| t == "Bohrung Ø3 durchgehend"), "{notes:?}");
    let five = measured(&sheet, "5").unwrap_or_else(|| panic!("no 5 position: {:?}", texts(&sheet)));
    assert!((five - 5.0).abs() <= 1e-9, "the position measured {five}");
    assert_texts_clear(&sheet);
}

#[test]
fn a_plate_gets_its_overall_sizes_once_and_grouped_equal_holes() {
    let (sheet, _) = plan_of(&plate_with_two_holes());
    for (text, value) in [("80", 80.0), ("40", 40.0), ("10", 10.0)] {
        assert_eq!(texts_of(&sheet, text), 1, "{text} must appear exactly once: {:?}", texts(&sheet));
        let got = measured(&sheet, text).unwrap();
        assert!((got - value).abs() <= 1e-9, "{text} measured {got}");
    }
    assert_eq!(texts_of(&sheet, "2x Ø8"), 1, "{:?}", texts(&sheet));
    let eight = measured(&sheet, "2x Ø8").unwrap();
    assert!((eight - 8.0).abs() <= 1e-9, "the group measures {eight}");
    for (text, value) in [("15", 15.0), ("65", 65.0), ("20", 20.0)] {
        assert_eq!(texts_of(&sheet, text), 1, "{text} must appear exactly once: {:?}", texts(&sheet));
        let got = measured(&sheet, text).unwrap();
        assert!((got - value).abs() <= 1e-9, "{text} measured {got}");
    }
    assert_texts_clear(&sheet);
}

#[test]
fn a_crank_gets_its_centre_distance_its_outer_radii_and_its_hole_diameters() {
    let (sheet, sources) = plan_of(&crank_like());
    assert!(
        sheet.notes.is_empty(),
        "the edge view already shows the through holes: {:?}",
        sheet.notes.iter().map(|n| &n.text_string).collect::<Vec<_>>()
    );
    let hundred = measured(&sheet, "100").unwrap_or_else(|| panic!("no centre distance: {:?}", texts(&sheet)));
    assert!((hundred - 100.0).abs() <= 1e-9, "the centre distance measured {hundred}");
    assert_eq!(texts_of(&sheet, "R20"), 1, "{:?}", texts(&sheet));
    assert_eq!(texts_of(&sheet, "R12"), 1, "{:?}", texts(&sheet));
    assert_eq!(texts_of(&sheet, "Ø16"), 1, "{:?}", texts(&sheet));
    assert_eq!(texts_of(&sheet, "Ø10"), 1, "{:?}", texts(&sheet));
    let sixteen = measured(&sheet, "Ø16").unwrap();
    let ten = measured(&sheet, "Ø10").unwrap();
    assert!((sixteen - 16.0).abs() <= 1e-9, "Ø16 measures {sixteen}");
    assert!((ten - 10.0).abs() <= 1e-9, "Ø10 measures {ten}");
    assert!(sources.iter().any(|s| s.kind == "radius" && (s.value - 20.0).abs() < 1e-9), "{sources:?}");
    assert_texts_clear(&sheet);
}

#[test]
fn no_dimensions_gives_exactly_the_previous_sheet() {
    let s = stepped_shaft();
    let title = TitleBlock::for_design("Part");
    let (auto, _) = plan_sheet_auto(&s, None, 0.01, &title, &NotesBlock::default(), None, &Annotations::default()).unwrap();
    let none = crate::drawing::plan_sheet_with(&s, None, 0.01, &title, &NotesBlock::default(), None, &Annotations::default()).unwrap();
    assert!(auto.sheet.dimensions.len() >= 4);
    assert!(none.sheet.dimensions.is_empty());
    assert!(none.sheet.notes.is_empty());
    assert_eq!(none.sheet.prims.len(), none.sheet.base_prims);
}

#[test]
fn small_holes_get_one_grouped_leader_note_and_no_diameter_dimension() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":20,"height":5,"body_name":"Plate"})).unwrap();
    for x in [10.0, 30.0] {
        s.execute("solid.cylinder", &json!({"base":[x,10,-1],"axis":[0,0,1],"radius":2.5,"height":7,"operation":"cut","targets":["Plate"]})).unwrap();
    }
    let (sheet, _) = plan_of(&s);
    let notes: Vec<String> = sheet.notes.iter().map(|n| n.text_string.clone()).collect();
    assert!(notes.iter().any(|t| t == "2x Bohrung Ø5 durchgehend"), "{notes:?}");
    assert!(
        !sheet.dimensions.iter().any(|d| d.text.contains("Ø5")),
        "a note value is not dimensioned again: {:?}",
        sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()
    );
    assert_texts_clear(&sheet);
}

// ---------------------------------------------------------------- D7.1 measuring tests

use crate::drawing::SheetPlan;

fn plan_with_auto(s: &Session) -> (SheetPlan, AutoPlan) {
    let title = TitleBlock::for_design("Part");
    let (plan, _) = plan_sheet_auto(s, None, 0.01, &title, &NotesBlock::default(), None, &Annotations::default()).unwrap();
    let selection = crate::drawing::ViewSelection { views: plan.projected.iter().map(|v| v.view).collect(), choice: plan.choice };
    let auto = super::plan(s, &selection, &plan.projected);
    (plan, auto)
}

fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.len2();
    if len2 <= 1e-18 {
        return p.dist(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.dist(a + d * t)
}

/// The distance from a sheet point to a view's drawn edges, placed on the sheet.
fn point_distance_to_edges(p: Vec2, view: &ProjectedView, origin: Vec2, scale: f64, include_hidden: bool) -> f64 {
    let mut best = f64::INFINITY;
    for seg in &view.visible {
        best = best.min(point_segment_distance(p, origin + seg.a * scale, origin + seg.b * scale));
    }
    if include_hidden {
        for seg in &view.hidden {
            best = best.min(point_segment_distance(p, origin + seg.a * scale, origin + seg.b * scale));
        }
    }
    best
}

fn placed_of(plan: &SheetPlan, view: StandardView) -> &crate::drawing::PlacedView {
    plan.sheet.views.iter().find(|v| v.view == view).expect("the view is placed")
}

/// D7.1 defect 1: every linear/diameter dimension's extension lines start on a drawn edge. D9.1:
/// a centre-distance dimension's extension lines start on the two arc centres, which is the
/// feature they measure.
fn assert_extension_lines_start_on_edges(plan: &SheetPlan, _auto: &AutoPlan, s: &Session) {
    let faces = crate::drawing::centre::surface_faces(s);
    let scale = plan.sheet.scale_factor();
    for dimension in &plan.sheet.dimensions {
        if !matches!(dimension.kind, crate::drawing::dims::DimensionKind::Rotated | crate::drawing::dims::DimensionKind::Diametric) {
            continue;
        }
        let Some(view) = plan.projected.iter().find(|v| v.view == dimension.view) else { continue };
        let placed = placed_of(plan, dimension.view);
        let allow_hidden = dimension.text.contains("tief");
        for p in [dimension.def_point2, dimension.def_point3] {
            let visible = point_distance_to_edges(p, view, placed.origin, scale, false);
            let both = point_distance_to_edges(p, view, placed.origin, scale, true);
            let on_centre = faces.iter().any(|face| {
                let origin = match face.surface {
                    solidcraft_kernel::FaceSurface::Cylinder { origin, .. } => Some(origin),
                    solidcraft_kernel::FaceSurface::Cone { apex, .. } => Some(apex),
                    solidcraft_kernel::FaceSurface::Torus { center, .. } => Some(center),
                    _ => None,
                };
                origin.is_some_and(|o| (placed.origin + project(view.view, o) * scale).dist(p) <= 0.05)
            });
            assert!(
                visible <= 0.05 || (allow_hidden && both <= 0.05) || on_centre,
                "{}: extension line starts at {p:?}, {visible:.3} mm from a visible edge ({both:.3} mm with hidden)",
                dimension.text
            );
        }
    }
}

/// D7.1 defect 2: a radius arrow lies on a visible edge and inside the face's angular range.
///
/// The drawn arrowhead is measured, not the planned angle: a radius leader must not flip about
/// the centre (a partial arc is not a full circle, D9.2 §0). The apex is the first point of the
/// dimension's filled arrowhead.
fn assert_radius_arrows(plan: &SheetPlan, auto: &AutoPlan, s: &Session) {
    let faces = crate::drawing::centre::surface_faces(s);
    let scale = plan.sheet.scale_factor();
    for planned in &auto.dims {
        let DimensionGeometry::Radius { centre, radius, at_angle_deg } = planned.dimension.geometry else { continue };
        let Some(view) = plan.projected.iter().find(|v| v.view == planned.dimension.view) else { continue };
        let placed = placed_of(plan, planned.dimension.view);
        let tip = centre + Vec2::from_angle(at_angle_deg.to_radians()) * radius;
        let tip_sheet = placed.origin + tip * scale;
        let drawn = plan
            .sheet
            .dimensions
            .iter()
            .find(|d| {
                d.view == planned.dimension.view
                    && d.kind == crate::drawing::dims::DimensionKind::Radial
                    && (d.text == planned.source.text || d.text.ends_with(&planned.source.text))
            })
            .unwrap_or_else(|| panic!("{}: no drawn radius dimension", planned.source.text));
        let apexes = arrow_apexes(drawn);
        assert_eq!(apexes.len(), 1, "{}: one arrowhead, got {apexes:?}", drawn.text);
        let Some(&apex) = apexes.first() else { continue };
        assert!(
            apex.dist(tip_sheet) <= 0.05,
            "{}: the drawn arrow apex {apex:?} is {:.3} mm from the planned point {tip_sheet:?}",
            drawn.text,
            apex.dist(tip_sheet)
        );
        let dist = point_distance_to_edges(apex, view, placed.origin, scale, false);
        assert!(dist <= 0.05, "{}: the arrow apex {apex:?} is {dist:.3} mm from a visible edge", drawn.text);
        let Some(face) = faces.iter().find(|f| f.face == planned.source.face) else { panic!("no face for {}", planned.source.text) };
        let from_centre = apex - (placed.origin + centre * scale);
        let angle = from_centre.y.atan2(from_centre.x).rem_euclid(2.0 * PI);
        let intervals = angular_intervals(face, centre, view.view);
        assert!(
            intervals.iter().any(|(start, end)| angle >= *start - 1e-6 && angle <= *end + 1e-6),
            "{}: angle {:.2}° is outside the face's range {intervals:?}",
            drawn.text,
            angle.to_degrees()
        );
    }
}

fn prim_lines(prim: &Prim, out: &mut Vec<(Vec2, Vec2)>) {
    match prim {
        Prim::Line { a, b, .. } => out.push((*a, *b)),
        Prim::Polyline { points, closed, .. } => {
            for pair in points.windows(2) {
                if let [a, b] = pair {
                    out.push((*a, *b));
                }
            }
            if *closed
                && points.len() > 2
                && let (Some(a), Some(b)) = (points.last(), points.first())
            {
                out.push((*a, *b));
            }
        }
        Prim::Arc { c, r, a0, a1, .. } => {
            let steps = (((a1 - a0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
            let point = |i: usize| *c + Vec2::from_angle(a0 + (a1 - a0) * i as f64 / steps as f64) * *r;
            for i in 0..steps {
                out.push((point(i), point(i + 1)));
            }
        }
        Prim::Circle { c, r, .. } => {
            let point = |i: usize| *c + Vec2::from_angle(std::f64::consts::TAU * i as f64 / 32.0) * *r;
            for i in 0..32 {
                out.push((point(i), point(i + 1)));
            }
        }
        Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
            let minor = major.perp() * *ratio;
            let steps = (((t1 - t0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
            let point = |i: usize| {
                let t = t0 + (t1 - t0) * i as f64 / steps as f64;
                *c + *major * t.cos() + minor * t.sin()
            };
            for i in 0..steps {
                out.push((point(i), point(i + 1)));
            }
        }
        Prim::Filled { points, .. } => {
            for pair in points.windows(2) {
                if let [a, b] = pair {
                    out.push((*a, *b));
                }
            }
        }
        Prim::Text { .. } => {}
    }
}

/// D7.1 defect 4: a leader crosses no dimension line, extension line, other leader or text box,
/// and leaves a visible outline only within the last 5 mm before its tip.
fn assert_leaders_clear(plan: &SheetPlan) {
    let sheet = &plan.sheet;
    let scale = plan.sheet.scale_factor();
    let mut text_boxes: Vec<[Vec2; 4]> = Vec::new();
    for prim in &sheet.prims {
        let Prim::Text { at, height, angle_deg, text, anchor, layer } = prim else { continue };
        if *layer != Layer::Dimension {
            continue;
        }
        let width = text_width_mm(text, *height);
        let u = Vec2::from_angle(angle_deg.to_radians());
        let centre = match anchor {
            TextAnchor::Left => *at + u * (width * 0.5),
            TextAnchor::Center => *at,
            TextAnchor::Right => *at - u * (width * 0.5),
        };
        text_boxes.push(text_box(centre, width, *angle_deg, *height));
    }
    let mut outline: Vec<(Vec2, Vec2)> = Vec::new();
    for prim in &sheet.prims {
        if matches!(prim.layer(), Layer::Visible | Layer::Hidden) {
            prim_lines(prim, &mut outline);
        }
    }
    for (index, note) in sheet.notes.iter().enumerate() {
        let mut obstacles: Vec<(Vec2, Vec2)> = Vec::new();
        for dimension in &sheet.dimensions {
            for prim in &dimension.prims {
                if prim.layer() == Layer::Dimension {
                    prim_lines(prim, &mut obstacles);
                }
            }
        }
        for (other_index, other) in sheet.notes.iter().enumerate() {
            if other_index == index {
                continue;
            }
            for prim in &other.prims {
                if prim.layer() == Layer::Dimension {
                    prim_lines(prim, &mut obstacles);
                }
            }
        }
        let Some((_, tip)) = note.leader.last().copied() else { continue };
        for (c, d) in &obstacles {
            if point_segment_distance(tip, *c, *d) < 0.3 && tip.dist(*c) > 0.6 && tip.dist(*d) > 0.6 {
                panic!("{}: tip {tip:?} lands on an annotation line {c:?}..{d:?}", note.text_string);
            }
        }
        let view = placed_of(plan, note.view);
        for (a, b) in &note.leader {
            for (c, d) in &obstacles {
                if let Some(t) = segment_crossing(*a, *b, *c, *d)
                    && t > 0.02
                    && t < 0.98
                {
                    panic!("{}: leader {a:?}..{b:?} crosses an annotation line {c:?}..{d:?}", note.text_string);
                }
            }
            for corners in &text_boxes {
                assert!(!segment_hits_box(*a, *b, corners, 0.0), "{}: leader {a:?}..{b:?} crosses a text box", note.text_string);
            }
            let length = a.dist(*b);
            for (c, d) in &outline {
                if let Some(t) = segment_crossing(*a, *b, *c, *d) {
                    let from_tip = length * (1.0 - t);
                    let at = *a + (*b - *a) * t;
                    let [lo, hi] = view.bbox;
                    let on_box = (at.x - lo.x).abs() <= 0.1 || (at.x - hi.x).abs() <= 0.1 || (at.y - lo.y).abs() <= 0.1 || (at.y - hi.y).abs() <= 0.1;
                    assert!(
                        from_tip <= 5.0 + 1e-9 || on_box,
                        "{}: leader {a:?}..{b:?} crosses the outline {c:?}..{d:?} {from_tip:.2} mm before its tip {tip:?} (scale {scale})",
                        note.text_string
                    );
                }
            }
        }
    }
}

fn assert_no_bare_notes(sheet: &Sheet) {
    for note in &sheet.notes {
        assert_ne!(note.text_string.trim(), "durchgehend", "a notes carries only its through state");
    }
}

/// Every D7.1 rule, on one sheet.
fn plan_and_check(s: &Session) -> (SheetPlan, AutoPlan) {
    let (plan, auto) = plan_with_auto(s);
    assert!(plan.sheet.unplaced.is_empty(), "unplaced: {:?}", plan.sheet.unplaced);
    assert!(plan.undimensioned.is_empty(), "undimensioned: {:?}", plan.undimensioned);
    assert!(auto.undimensioned().is_empty(), "undimensioned (plan): {:?}", auto.undimensioned());
    assert_extension_lines_start_on_edges(&plan, &auto, s);
    assert_radius_arrows(&plan, &auto, s);
    assert_leaders_clear(&plan);
    assert_no_bare_notes(&plan.sheet);
    (plan, auto)
}

#[test]
fn every_test_sheet_meets_the_d71_layers_rules() {
    for s in
        [stepped_shaft(), shaft_with_blind_bore(), shaft_with_cross_hole(), plate_with_two_holes(), crank_like(), flywheel_part(), keyway_shaft()]
    {
        plan_and_check(&s);
    }
}

#[test]
fn the_shafts_step_diameter_keeps_three_millimetres_from_the_cross_hole_axis() {
    let (plan, _) = plan_and_check(&shaft_with_cross_hole());
    let scale = plan.sheet.scale_factor();
    let dimension = plan
        .sheet
        .dimensions
        .iter()
        .find(|d| d.text == "Ø30")
        .unwrap_or_else(|| panic!("no Ø30: {:?}", plan.sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()));
    let view = plan.projected.iter().find(|v| v.view == StandardView::Front).expect("front view");
    let placed = placed_of(&plan, StandardView::Front);
    let axis = view
        .centre
        .iter()
        .find_map(|item| match *item {
            crate::drawing::ViewCentre::Line { a, b } if (a.x - b.x).abs() <= 1e-6 && a.y != b.y => {
                Some((a * scale + placed.origin, b * scale + placed.origin))
            }
            _ => None,
        })
        .expect("the cross hole's centre line");
    let distance = segment_distance(dimension.def_point, dimension.def_point3, axis.0, axis.1);
    assert!(distance >= 3.0, "Ø30 is {distance:.2} sheet mm from the cross-hole axis");
}

/// The flywheel fixture as a test part: Ø120 × 35, Ø90 recess 10 deep, Ø40 boss, Ø16 bore.
fn flywheel_part() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XZ","name":"FlyProfile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[8,0],[60,0],[60,-20],[45,-20],[45,-10],[20,-10],[20,-35],[8,-35]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"FlyProfile","axis":"Z","angle":360,"body_name":"Flywheel"})).unwrap();
    s
}

/// A Ø20 × 50 shaft with a 3 wide, 1,5 deep, 20 long keyway on top.
fn keyway_shaft() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Profile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,10],[50,10],[50,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})).unwrap();
    s.execute("solid.box", &json!({"length":20,"width":12,"height":3,"corner":[15,8.3868,-1.5],"operation":"cut","targets":["Shaft"]})).unwrap();
    s
}

#[test]
fn the_flywheel_recess_gets_its_diameter_and_depth() {
    let (plan, auto) = plan_and_check(&flywheel_part());
    assert!(
        plan.sheet.dimensions.iter().any(|d| d.text == "Ø90 x 10 tief"),
        "the recess dimension: {:?}",
        plan.sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()
    );
    assert!(auto.sources().iter().any(|s| (s.value - 90.0).abs() < 1e-6), "{:?}", auto.sources());
}

#[test]
fn a_keyway_gets_its_width_and_depth_note() {
    let (plan, _) = plan_and_check(&keyway_shaft());
    let notes: Vec<&str> = plan.sheet.notes.iter().map(|n| n.text_string.as_str()).collect();
    assert!(notes.contains(&"Keilnut 3 x 1,5 tief"), "{notes:?}");
    assert!(
        plan.sheet.dimensions.iter().any(|d| d.text == "20"),
        "the keyway length stays: {:?}",
        plan.sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()
    );
}

#[test]
fn a_blind_hole_states_its_depth_in_the_diameter_text_not_a_bare_note() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":80,"width":40,"height":10,"body_name":"Plate"})).unwrap();
    for x in [20.0, 60.0] {
        s.execute("solid.hole", &json!({"position":[x,20,10],"direction":[0,0,-1],"diameter":10,"depth":5,"type":"simple","targets":["Plate"]}))
            .unwrap();
    }
    let (plan, _) = plan_and_check(&s);
    assert!(
        plan.sheet.dimensions.iter().any(|d| d.text == "2x Ø10 x 5 tief"),
        "blind hole dimension: {:?}",
        plan.sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()
    );
    assert!(
        !plan.sheet.notes.iter().any(|n| n.text_string.trim().ends_with("tief") && !n.text_string.contains('Ø')),
        "no bare depth note: {:?}",
        plan.sheet.notes.iter().map(|n| &n.text_string).collect::<Vec<_>>()
    );
}

/// The four acceptance parts, when the fixture files are on the box. Run with
/// `SOLIDCRAFT_D71_FIXTURES=1`.
#[test]
fn acceptance_parts_meet_the_d71_rules() {
    if std::env::var_os("SOLIDCRAFT_D71_FIXTURES").is_none() {
        return;
    }
    let parts = [
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/shaft.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/crank.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/flywheel.solidcraft"),
        // The env-gated test plate (D9.1 §3): its R11,72 arc is the D9.2 radius-arrow case.
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/test_plate.step"),
    ];
    for path in parts {
        let mut s = Session::default();
        s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
        let (plan, auto) = plan_and_check(&s);
        let texts: Vec<String> = plan.sheet.dimensions.iter().map(|d| d.text.clone()).collect();
        let notes: Vec<String> = plan.sheet.notes.iter().map(|n| n.text_string.clone()).collect();
        if path.contains("flywheel") {
            assert!(texts.iter().any(|t| t == "Ø90 x 10 tief"), "{path}: {texts:?}");
        }
        if path.contains("shaft") {
            let dimension = plan.sheet.dimensions.iter().find(|d| d.text == "Ø28").expect("Ø28");
            let view = plan.projected.iter().find(|v| v.view == StandardView::Front).expect("front");
            let placed = placed_of(&plan, StandardView::Front);
            let scale = plan.sheet.scale_factor();
            let axis = view
                .centre
                .iter()
                .find_map(|item| match *item {
                    crate::drawing::ViewCentre::Line { a, b } if (a.x - b.x).abs() <= 1e-6 && a.y != b.y => {
                        Some((a * scale + placed.origin, b * scale + placed.origin))
                    }
                    _ => None,
                })
                .expect("cross-hole centre line");
            let distance = segment_distance(dimension.def_point, dimension.def_point3, axis.0, axis.1);
            assert!(distance >= 3.0, "{path}: Ø28 is {distance:.2} mm from the cross-hole axis");
        }
        if path.contains("crank") {
            assert!(plan.sheet.notes.is_empty(), "{path}: a through hole is already shown in the edge view: {notes:?}");
        }
        assert!(!auto.sources().is_empty(), "{path}: no dimensions");
    }
}

// ------------------------------------------------------------------- D9.1 measuring helpers

/// The length of `a`→`b` that runs within `tol` of the straight edge `c`→`d` and inside its own
/// extent: the D9.1 "collinear with an edge" measure in sheet millimetres. Short edge segments
/// (the chords of a tessellated curve) are ignored; a tangent line touches a curve, it does not
/// run along a straight edge.
fn collinear_overlap(a: Vec2, b: Vec2, c: Vec2, d: Vec2, tol: f64, min_edge: f64) -> f64 {
    let edge = d - c;
    let length = edge.len();
    if !(length.is_finite() && length >= min_edge) {
        return 0.0;
    }
    let u = edge * (1.0 / length);
    let n = u.perp();
    let dim = b - a;
    let dim_len = dim.len();
    if !(dim_len > 1e-9) {
        return 0.0;
    }
    // "Collinear" means within 5°: a line crossing an edge at a wider angle only touches it.
    if u.cross(dim * (1.0 / dim_len)).abs() > 0.0872 {
        return 0.0;
    }
    let (s0, s1) = ((a - c).dot(u), (b - c).dot(u));
    let (f0, f1) = ((a - c).dot(n), (b - c).dot(n));
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for (v0, v1, bound_lo, bound_hi) in [(s0, s1, -0.3, length + 0.3), (f0, f1, -tol, tol)] {
        let dv = v1 - v0;
        if dv.abs() < 1e-12 {
            if v0 < bound_lo || v0 > bound_hi {
                return 0.0;
            }
            continue;
        }
        let (ta, tb) = ((bound_lo - v0) / dv, (bound_hi - v0) / dv);
        let (ta, tb) = (ta.min(tb), ta.max(tb));
        lo = lo.max(ta);
        hi = hi.min(tb);
        if lo >= hi {
            return 0.0;
        }
    }
    ((hi - lo) * dim_len).max(0.0)
}

/// The drawn box of a text primitive, in sheet millimetres.
fn prim_text_box(prim: &Prim) -> Option<[Vec2; 4]> {
    let Prim::Text { at, height, angle_deg, text, anchor, .. } = prim else { return None };
    let width = text_width_mm(text, *height);
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    let centre = match anchor {
        TextAnchor::Left => *at + u * (width * 0.5),
        TextAnchor::Center => *at,
        TextAnchor::Right => *at - u * (width * 0.5),
    };
    Some([
        centre - u * (width * 0.5) - n * (*height * 0.5),
        centre + u * (width * 0.5) - n * (*height * 0.5),
        centre + u * (width * 0.5) + n * (*height * 0.5),
        centre - u * (width * 0.5) + n * (*height * 0.5),
    ])
}

/// The distance from `p` to a rotated box (0 inside).
fn point_box_distance(p: Vec2, corners: &[Vec2; 4]) -> f64 {
    if inside_polygon(p, corners) {
        return 0.0;
    }
    (0..4).map(|i| point_segment_distance(p, corners[i], corners[(i + 1) % 4])).fold(f64::INFINITY, f64::min)
}

/// The distance from a segment to a box (0 when they meet).
fn segment_box_distance(a: Vec2, b: Vec2, corners: &[Vec2; 4]) -> f64 {
    let mut best = point_box_distance(a, corners).min(point_box_distance(b, corners));
    for i in 0..4 {
        let (c, d) = (corners[i], corners[(i + 1) % 4]);
        best = best.min(segment_distance(a, b, c, d));
    }
    best
}

fn inside_polygon(p: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// The sheet's visible and hidden edges (the cut outline is drawn on `Visible`).
fn sheet_edges(sheet: &Sheet) -> Vec<(Vec2, Vec2)> {
    // Straight edges only: a chord of a drawn arc is not a straight drawing edge, and a
    // dimension line can only touch a curve. (`prim_lines` samples curves for other checks.)
    let mut out = Vec::new();
    for prim in &sheet.prims {
        if !matches!(prim.layer(), Layer::Visible | Layer::Hidden) {
            continue;
        }
        match prim {
            Prim::Line { a, b, .. } => out.push((*a, *b)),
            Prim::Polyline { points, closed, .. } => {
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push((*a, *b));
                    }
                }
                if *closed
                    && points.len() > 2
                    && let (Some(a), Some(b)) = (points.last(), points.first())
                {
                    out.push((*a, *b));
                }
            }
            _ => {}
        }
    }
    out
}

/// The line prims of one dimension's own drawing.
fn own_lines(dim: &crate::drawing::dims::DrawnDimension) -> Vec<(Vec2, Vec2)> {
    let mut out = Vec::new();
    for prim in &dim.prims {
        if let Prim::Line { .. } = prim {
            prim_lines(prim, &mut out);
        }
    }
    out
}

/// The apexes of a dimension's filled arrowheads.
fn arrow_apexes(dim: &crate::drawing::dims::DrawnDimension) -> Vec<Vec2> {
    dim.prims
        .iter()
        .filter_map(|prim| match prim {
            Prim::Filled { points, .. } if points.len() == 3 => points.first().copied(),
            _ => None,
        })
        .collect()
}

/// D9.1 item 1: no dimension or extension line runs along a drawn edge for more than 1 mm
/// (within 0.3 mm); an extension line may start on its feature.
fn assert_no_line_on_an_edge(sheet: &Sheet) {
    let edges = sheet_edges(sheet);
    for dim in &sheet.dimensions {
        for prim in &dim.prims {
            let Prim::Line { a, b, .. } = prim else { continue };
            let start = [dim.def_point2, dim.def_point3]
                .iter()
                .copied()
                .find(|p| point_segment_distance(*p, *a, *b) <= 1e-6 && (a.dist(*p) <= 1e-6 || b.dist(*p) <= 1e-6));
            let is_extension = start.is_some();
            for (c, d) in &edges {
                // An extension line starts on its feature: the edge it starts on (a shared
                // vertex, e.g. the extreme point of a tangent outline) is not an overlap.
                if let Some(start) = start
                    && (c.dist(start) <= 0.1 || d.dist(start) <= 0.1)
                {
                    continue;
                }
                let overlap = collinear_overlap(*a, *b, *c, *d, 0.3, 3.0);
                let allowed = if is_extension { 1.0 } else { 0.0 };
                assert!(
                    overlap <= 1.0 + allowed + 1e-6,
                    "{} on {:?} (all: {:?}): the {} line {a:?}..{b:?} runs {overlap:.2} mm along the edge {c:?}..{d:?}",
                    dim.text,
                    dim.view,
                    sheet.dimensions.iter().map(|x| (x.text.clone(), x.view, x.angle_deg)).collect::<Vec<_>>(),
                    if is_extension { "extension" } else { "dimension" }
                );
            }
        }
    }
}

/// D9.1 item 3: every linear and diameter dimension has exactly two arrowheads, each on an
/// extension line or a drawn edge.
fn assert_two_arrows(sheet: &Sheet) {
    for dim in &sheet.dimensions {
        if dim.kind == crate::drawing::dims::DimensionKind::Radial {
            continue;
        }
        let arrows = arrow_apexes(dim);
        assert_eq!(arrows.len(), 2, "{}: {:?}", dim.text, arrows);
        let edges = sheet_edges(sheet);
        let lines = own_lines(dim);
        for apex in arrows {
            let on_edge = edges.iter().any(|(c, d)| point_segment_distance(apex, *c, *d) <= 0.3);
            let own = lines.iter().filter(|(c, d)| point_segment_distance(apex, *c, *d) <= 0.3).count();
            assert!(on_edge || own >= 2, "{}: the arrow at {apex:?} is on no extension line or edge", dim.text);
        }
    }
}

/// D9.1 item 2: on the flywheel every diameter's arrows lie on its extension lines and every
/// value is between its arrows or within one text height of one.
fn assert_diameter_labels(sheet: &Sheet) {
    let edges = sheet_edges(sheet);
    for dim in &sheet.dimensions {
        if dim.kind != crate::drawing::dims::DimensionKind::Diametric {
            continue;
        }
        let arrows = arrow_apexes(dim);
        assert_eq!(arrows.len(), 2, "{}: {:?}", dim.text, arrows);
        let lines = own_lines(dim);
        for apex in &arrows {
            let on_edge = edges.iter().any(|(c, d)| point_segment_distance(*apex, *c, *d) <= 0.3);
            let own = lines.iter().filter(|(c, d)| point_segment_distance(*apex, *c, *d) <= 0.3).count();
            assert!(on_edge || own >= 2, "{}: the arrow at {apex:?} is on no extension line", dim.text);
        }
        let text = dim.prims.iter().find_map(|prim| match prim {
            Prim::Text { at, height, angle_deg, text, anchor, .. } => {
                let width = text_width_mm(text, *height);
                let u = Vec2::from_angle(angle_deg.to_radians());
                let centre = match anchor {
                    TextAnchor::Left => *at + u * (width * 0.5),
                    TextAnchor::Center => *at,
                    TextAnchor::Right => *at - u * (width * 0.5),
                };
                Some((centre, width, *angle_deg, *height))
            }
            _ => None,
        });
        let Some((centre, width, angle, height)) = text else { panic!("{}: no text", dim.text) };
        let corners = text_box(centre, width, angle, height);
        // With the value as the reference: is 0 (between) or is the nearest arrow within 3.5 mm?
        let u = Vec2::from_angle(angle.to_radians());
        let along: Vec<f64> = arrows.iter().map(|p| (*p - centre).dot(u)).collect();
        let lo = along.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = along.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let between = lo <= 0.0 && hi >= 0.0;
        let nearest = arrows.iter().map(|p| point_box_distance(*p, &corners)).fold(f64::INFINITY, f64::min);
        assert!(
            between || nearest <= 3.5 + 1e-6,
            "{} on {:?} at {:?}: the value is {nearest:.2} mm from its nearest arrow (arrows at {lo:.1}..{hi:.1} from it)",
            dim.text,
            dim.view,
            dim.text_midpoint
        );
    }
}

/// D9.1 item 5: a full section's axis and its end view's centre lie on one line in the
/// arrangement's alignment direction (0.5 mm).
fn assert_section_alignment(plan: &SheetPlan) {
    let Some(section) = plan.sheet.sections.iter().find(|s| s.full) else { return };
    let section_view = section.view;
    let Some(end) =
        plan.projected.iter().find(|v| v.view != section_view && v.centre.iter().any(|c| matches!(c, crate::drawing::ViewCentre::Cross { .. })))
    else {
        return;
    };
    let Some(longitudinal) = plan.projected.iter().find(|v| v.view == section_view) else { return };
    let Some(axis) = longitudinal.centre.iter().find_map(|c| match *c {
        crate::drawing::ViewCentre::Line { a, b } => Some((a + b) * 0.5),
        _ => None,
    }) else {
        return;
    };
    let Some(centre) = end.centre.iter().find_map(|c| match *c {
        crate::drawing::ViewCentre::Cross { c, .. } => Some(c),
        _ => None,
    }) else {
        return;
    };
    let scale = plan.sheet.scale_factor();
    let section_placed = placed_of(plan, section_view);
    let end_placed = placed_of(plan, end.view);
    let axis_sheet = section_placed.origin + axis * scale;
    let centre_sheet = end_placed.origin + centre * scale;
    let (a, b) = (section_placed.bbox, end_placed.bbox);
    let side_by_side = b[0].x > a[1].x + 0.5 || a[0].x > b[1].x + 0.5;
    let stacked = b[0].y > a[1].y + 0.5 || a[0].y > b[1].y + 0.5;
    let error = if side_by_side {
        (axis_sheet.y - centre_sheet.y).abs()
    } else if stacked {
        (axis_sheet.x - centre_sheet.x).abs()
    } else {
        (axis_sheet - centre_sheet).len()
    };
    assert!(error <= 0.5, "the section's axis {axis_sheet:?} and the end view's centre {centre_sheet:?} are {error:.2} mm apart");
}

/// D9.1 item 6: the `A–A` label keeps 2 mm from every leader and text box.
fn assert_section_label_clear(sheet: &Sheet) {
    for prim in &sheet.prims {
        let Prim::Text { text, height, .. } = prim else { continue };
        if !text.contains('\u{2013}') || height.abs() < 4.0 {
            continue;
        }
        let Some(corners) = prim_text_box(prim) else { continue };
        for note in &sheet.notes {
            for (a, b) in &note.leader {
                let d = segment_box_distance(*a, *b, &corners);
                assert!(d >= 2.0 - 1e-6, "the {text} label is {d:.2} mm from the leader of {}", note.text_string);
            }
            if let Some(other) = prim_text_box(&note.text) {
                let d = segment_box_distance(corners[0], corners[2], &other);
                assert!(d >= 2.0 - 1e-6, "the {text} label is {d:.2} mm from the note {}", note.text_string);
            }
        }
        for dim in &sheet.dimensions {
            for prim in &dim.prims {
                if let Some(other) = prim_text_box(prim) {
                    let d = segment_box_distance(corners[0], corners[2], &other);
                    assert!(d >= 2.0 - 1e-6, "the {text} label is {d:.2} mm from the value {}", dim.text);
                }
            }
        }
    }
}

#[test]
fn every_test_sheet_keeps_its_lines_off_edges_and_its_arrows_in_pairs() {
    for s in
        [stepped_shaft(), shaft_with_blind_bore(), shaft_with_cross_hole(), plate_with_two_holes(), crank_like(), flywheel_part(), keyway_shaft()]
    {
        let (plan, _) = plan_with_auto(&s);
        assert_no_line_on_an_edge(&plan.sheet);
        assert_two_arrows(&plan.sheet);
        assert_section_alignment(&plan);
        assert_section_label_clear(&plan.sheet);
    }
}

#[test]
fn a_disc_puts_its_outer_diameters_in_rows_and_its_inner_ones_on_the_end_view() {
    let (plan, _) = plan_with_auto(&flywheel_part());
    let sheet = &plan.sheet;
    let front = placed_of(&plan, StandardView::Front);
    let top = placed_of(&plan, StandardView::Top);
    let row = |text: &str| -> f64 {
        let dim = sheet
            .dimensions
            .iter()
            .find(|d| d.text == text)
            .unwrap_or_else(|| panic!("no {text}: {:?}", sheet.dimensions.iter().map(|d| &d.text).collect::<Vec<_>>()));
        assert_eq!(dim.view, StandardView::Front, "{text} reads on the section");
        let (a, _b) = own_lines(dim)
            .into_iter()
            .find(|(a, b)| (a.y - b.y).abs() <= 1e-6 && (a.dist(*b) - dim.measurement * sheet.scale_factor()).abs() <= 0.1)
            .unwrap_or_else(|| panic!("{text}: no horizontal dimension line"));
        a.y
    };
    let hub = row("Ø40");
    let outer = row("Ø120");
    let outline_top = front.bbox[1].y;
    assert!(hub - outline_top >= 7.0 - 1e-6, "Ø40 is {:.1} mm above the outline", hub - outline_top);
    assert!(outer - outline_top >= 7.0 - 1e-6, "Ø120 is {:.1} mm above the outline", outer - outline_top);
    assert!(outer - hub >= 7.0 - 1e-6, "the rows are {:.1} mm apart", outer - hub);
    assert!(hub < outer, "the smaller diameter takes the nearer row");
    // The inner diameters read on the end view, each on its own circle, and the section does not
    // repeat their values.
    let inner: Vec<(String, f64, StandardView)> = sheet.dimensions.iter().map(|d| (d.text.clone(), d.measurement, d.view)).collect();
    let on_end = |text: &str| inner.iter().any(|(t, _, v)| t.starts_with(text) && *v == StandardView::Top);
    assert!(on_end("Ø90"), "the recess reads on the end view: {inner:?}");
    assert!(on_end("Ø16"), "the bore reads on the end view: {inner:?}");
    for (value, name) in [(90.0_f64, "Ø90"), (16.0, "Ø16")] {
        assert!(!inner.iter().any(|(_, v, view)| *view == StandardView::Front && (v - value).abs() <= 1e-6), "{name} is not repeated in the section");
    }
    for dim in sheet.dimensions.iter().filter(|d| d.view == StandardView::Top) {
        let wanted = (dim.measurement / 2.0) * sheet.scale_factor();
        let centre = top.origin
            + project(StandardView::Top, crate::drawing::centre::turned_axis(&flywheel_part()).expect("axis").origin) * sheet.scale_factor();
        // The diameter's extension lines rise from the circle's waist points: one at either side.
        let xs: Vec<f64> =
            own_lines(dim).into_iter().filter(|(a, b)| (a.x - b.x).abs() <= 1e-6 && a.dist(*b) > 1.0).map(|(a, _)| a.x - centre.x).collect();
        assert!(
            xs.iter().any(|x| (*x - wanted).abs() <= 0.05) && xs.iter().any(|x| (*x + wanted).abs() <= 0.05),
            "{}: extension lines at {xs:?}, want ±{wanted:.2}",
            dim.text
        );
        let (a, b) = own_lines(dim)
            .into_iter()
            .filter(|(a, b)| (a.y - b.y).abs() <= 1e-6)
            .min_by(|(a, b), (c, d)| (((*a + *b) * 0.5) - centre).len().total_cmp(&((((*c + *d) * 0.5) - centre).len())))
            .unwrap_or_else(|| panic!("{}: no horizontal dimension line of a diameter", dim.text));
        assert!((a - b).len() <= 2.0 * wanted + 2.0 * crate::drawing::dims::ARROW_LENGTH_MM + 1e-6, "{}'s line spans a diameter", dim.text);
    }
    assert_diameter_labels(sheet);
}

#[test]
fn a_skewed_hole_gets_its_size_note_and_a_warning() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":20,"height":10,"body_name":"Plate"})).unwrap();
    let tilt = 3.0_f64.to_radians();
    s.execute(
        "solid.hole",
        &json!({"position":[-0.01,10,5],"direction":[tilt.cos(),tilt.sin(),0],"diameter":5,"depth":10,"type":"drilled","tip_angle":118,"targets":["Plate"]}),
    )
    .unwrap();
    let (plan, auto) = plan_with_auto(&s);
    let warnings = plan.warnings.clone();
    assert_eq!(warnings.len(), 1, "one skewed hole, one warning: {warnings:?}");
    assert_eq!(warnings[0].kind, "skew_axis");
    assert!((warnings[0].angle_deg - 3.0).abs() <= 0.2, "the angle is {:.2}°", warnings[0].angle_deg);
    assert!(auto.warnings().iter().any(|w| w.kind == "skew_axis"), "the plan reports it too");
    let notes: Vec<String> = plan.sheet.notes.iter().map(|n| n.text_string.clone()).collect();
    assert!(notes.iter().any(|n| n == "Bohrung Ø5 x 10 tief"), "the skewed hole keeps its note: {notes:?}");
    assert_eq!(plan.sheet.views.len(), 3, "no auxiliary view is added for the skew");
    assert!(plan.sheet.unplaced.is_empty(), "unplaced: {:?}", plan.sheet.unplaced);
}

/// The test plate fixture (Nik's own STEP part): two tangent arcs and their tangent lines,
/// extruded 10 mm, with a Ø5 × 10 hole 3° off the X axis. Run with `SOLIDCRAFT_D9_FIXTURES=1`.
#[test]
fn the_test_plate_gets_its_arcs_its_centre_distance_and_two_to_one() {
    if std::env::var_os("SOLIDCRAFT_D9_FIXTURES").is_none() {
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/test_plate.step");
    let mut s = Session::default();
    s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (plan, auto) = plan_with_auto(&s);
    let sheet = &plan.sheet;
    assert_eq!(sheet.scale, (2, 1), "the plate fits 2:1 in two views");
    assert_eq!(sheet.arrangement, crate::drawing::Arrangement::SideBySide, "2:1 with the plan beside the edge view");
    assert_eq!(sheet.views.len(), 2, "two views suffice: {:?}", sheet.views.iter().map(|v| v.view).collect::<Vec<_>>());
    let texts: Vec<String> = sheet.dimensions.iter().map(|d| d.text.clone()).collect();
    assert!(texts.iter().any(|t| t == "R11,72"), "the large arc gets its radius: {texts:?}");
    assert!(texts.iter().any(|t| t == "R2,5"), "the small arc gets its radius: {texts:?}");
    assert!(texts.iter().any(|t| t == "23,93"), "the centre distance: {texts:?}");
    for keep in ["24,67", "35,71", "10"] {
        assert!(texts.iter().any(|t| t == keep), "the overall size stays ({keep}): {texts:?}");
    }
    assert!(auto.undimensioned().is_empty(), "undimensioned: {:?}", auto.undimensioned());
    let warnings = plan.warnings.clone();
    assert_eq!(warnings.len(), 1, "the skewed hole is reported: {warnings:?}");
    assert!((warnings[0].angle_deg - 3.19).abs() <= 0.1);
    let notes: Vec<String> = sheet.notes.iter().map(|n| n.text_string.clone()).collect();
    assert!(notes.iter().any(|n| n == "Bohrung Ø5 x 10 tief"), "{notes:?}");
    assert!(sheet.unplaced.is_empty(), "unplaced: {:?}", sheet.unplaced);
    // The radius is a radius, not a diameter.
    let radius = sheet.dimensions.iter().find(|d| d.text == "R11,72").expect("R11,72");
    assert_eq!(radius.kind, crate::drawing::dims::DimensionKind::Radial);
    assert!((radius.measurement - 11.715).abs() <= 0.01);
    assert_extension_lines_start_on_edges(&plan, &auto, &s);
    assert_radius_arrows(&plan, &auto, &s);
    assert_leaders_clear(&plan);
    assert_no_line_on_an_edge(sheet);
    assert_two_arrows(sheet);
    assert_diameter_labels(sheet);
    assert_section_alignment(&plan);
    assert_section_label_clear(sheet);
}

/// D9.1 acceptance: all five parts, with the measured D9.1 rules. Run with
/// `SOLIDCRAFT_D9_FIXTURES=1`.
#[test]
fn acceptance_parts_meet_the_d91_rules() {
    if std::env::var_os("SOLIDCRAFT_D9_FIXTURES").is_none() {
        return;
    }
    let parts = [
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/shaft.solidcraft"), (1, 1)),
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/crank.solidcraft"), (1, 1)),
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/flywheel.solidcraft"), (1, 2)),
        (concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/test_plate.step"), (2, 1)),
    ];
    for (path, at_least) in parts {
        let mut s = Session::default();
        s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
        let (plan, auto) = plan_with_auto(&s);
        let sheet = &plan.sheet;
        assert!(sheet.scale_factor() >= f64::from(at_least.0) / f64::from(at_least.1) - 1e-9, "{path}: scale {}", sheet.scale_text());
        assert!(sheet.unplaced.is_empty(), "{path}: unplaced {:?}", sheet.unplaced);
        assert!(auto.undimensioned().is_empty(), "{path}: undimensioned {:?}", auto.undimensioned());
        assert_extension_lines_start_on_edges(&plan, &auto, &s);
        assert_radius_arrows(&plan, &auto, &s);
        assert_leaders_clear(&plan);
        assert_no_line_on_an_edge(sheet);
        assert_two_arrows(sheet);
        // The brief's diameter-label rule is about the disc's diameters (D9.1 item 2).
        if path.contains("flywheel") {
            assert_diameter_labels(sheet);
        }
        assert_section_alignment(&plan);
        assert_section_label_clear(sheet);
        if path.contains("flywheel") {
            let front = placed_of(&plan, StandardView::Front);
            let dims: Vec<&String> = sheet.dimensions.iter().filter(|d| d.view == StandardView::Front).map(|d| &d.text).collect();
            assert!(dims.iter().any(|t| t.as_str() == "Ø120"), "{dims:?}");
            assert!(dims.iter().any(|t| t.as_str() == "Ø40"), "{dims:?}");
            let top: Vec<&String> = sheet.dimensions.iter().filter(|d| d.view == StandardView::Top).map(|d| &d.text).collect();
            assert!(top.iter().any(|t| t.starts_with("Ø90")), "{top:?}");
            assert!(top.iter().any(|t| t.starts_with("Ø16")), "{top:?}");
            let _ = front;
        }
        if path.contains("test_plate.step") {
            assert_eq!(sheet.scale, (2, 1), "{path}");
            assert_eq!(sheet.views.len(), 2, "{path}: two views");
            assert!(plan.warnings.iter().any(|w| w.kind == "skew_axis"), "{path}: skew warning");
        }
    }
}
