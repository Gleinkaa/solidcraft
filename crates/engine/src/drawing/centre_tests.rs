#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Measured tests for centre lines, centre marks and the automatic view choice.

use serde_json::json;
use solidcraft_io::PdfItem;
use solidcraft_render::StandardView;

use super::*;
use crate::Session;
use crate::drawing::sheet::{CENTRE_OVERSHOOT_MM, Layer, Prim, Sheet, TitleBlock, sheet_to_dxf, sheet_to_pdf};
use crate::drawing::{NotesBlock, ViewChoice, plan_sheet};

/// The fixture's stepped shaft: a revolved profile about X, 120 mm long and up to Ø28.
fn stepped_shaft() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Profile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,10],[30,10],[30,14],[80,14],[80,8],[120,8],[120,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})).unwrap();
    s
}

/// A 100 × 40 × 8 flat plate with two Ø10 through holes on y = 20, at x = 20 and x = 80.
fn plate_with_two_holes() -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":100,"width":40,"height":8,"body_name":"Plate"})).unwrap();
    for x in [20.0, 80.0] {
        s.execute("solid.cylinder", &json!({"radius":5,"height":20,"base":[x,20,-6],"operation":"cut","targets":["Plate"]})).unwrap();
    }
    s
}

fn boxed(length: f64, width: f64, height: f64) -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":length,"width":width,"height":height,"body_name":"Box"})).unwrap();
    s
}

fn plan_of(s: &Session) -> crate::drawing::SheetPlan {
    plan_sheet(s, None, 0.01, &TitleBlock::for_design("Part"), &NotesBlock::default(), None).unwrap()
}

fn placed(sheet: &Sheet, view: StandardView) -> crate::drawing::PlacedView {
    sheet.views.iter().find(|v| v.view == view).cloned().unwrap_or_else(|| panic!("{view:?} is not placed: {:?}", sheet.views))
}

fn scale_of(sheet: &Sheet) -> f64 {
    f64::from(sheet.scale.0) / f64::from(sheet.scale.1)
}

/// The sheet's centre lines as `(a, b)` pairs.
fn centre_lines(sheet: &Sheet) -> Vec<(Vec2, Vec2)> {
    sheet
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Line { a, b, layer: Layer::Center } => Some((*a, *b)),
            _ => None,
        })
        .collect()
}

fn horizontal(a: Vec2, b: Vec2) -> bool {
    (a.y - b.y).abs() < 1e-9
}

fn vertical(a: Vec2, b: Vec2) -> bool {
    (a.x - b.x).abs() < 1e-9
}

fn near(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() <= 0.01, "{what}: {a} is not {b} ± 0.01");
}

#[test]
fn a_stepped_shaft_is_turned_with_a_centre_line_on_its_axis_and_an_end_cross() {
    let s = stepped_shaft();
    let plan = plan_of(&s);
    assert_eq!(plan.choice, ViewChoice::Turned);
    assert_eq!(plan.sheet.views.len(), 2, "a turned part gets two views: {:?}", plan.sheet.views);
    let front = placed(&plan.sheet, StandardView::Front);
    let end = placed(&plan.sheet, StandardView::Right);
    let scale = scale_of(&plan.sheet);
    let lines = centre_lines(&plan.sheet);

    // The front view's centre line lies on the axis (the projected world origin's y) and reaches
    // 3 mm past both end faces.
    let axis = lines
        .iter()
        .find(|(a, b)| horizontal(*a, *b) && (a.y - front.origin.y).abs() < 1e-6 && a.x.min(b.x) <= front.bbox[0].x)
        .unwrap_or_else(|| panic!("no horizontal centre line on the axis: {lines:?}"));
    assert!((axis.0.y - front.origin.y).abs() < 1e-6, "the line is collinear with the axis");
    near(axis.0.x.min(axis.1.x), front.bbox[0].x - CENTRE_OVERSHOOT_MM, "the line starts past the left end face");
    near(axis.0.x.max(axis.1.x), front.bbox[1].x + CENTRE_OVERSHOOT_MM, "the line ends past the right end face");

    // The end view's centre cross is centred on the axis and its arms reach 3 mm past the largest
    // circle (Ø28: radius 14).
    let arm = 14.0 * scale + CENTRE_OVERSHOOT_MM;
    let centre = end.origin;
    let across = lines
        .iter()
        .find(|(a, b)| horizontal(*a, *b) && (a.y - centre.y).abs() < 1e-9 && (a.x.min(b.x) - (centre.x - arm)).abs() < 0.01)
        .unwrap_or_else(|| panic!("no horizontal cross arm at {centre:?}: {lines:?}"));
    near(across.0.x.min(across.1.x), centre.x - arm, "the cross arm reaches past the circle");
    near(across.0.x.max(across.1.x), centre.x + arm, "the cross arm reaches past the circle");
    let up = lines
        .iter()
        .find(|(a, b)| vertical(*a, *b) && (a.x - centre.x).abs() < 1e-9 && (a.y.min(b.y) - (centre.y - arm)).abs() < 0.01)
        .unwrap_or_else(|| panic!("no vertical cross arm at {centre:?}: {lines:?}"));
    near(up.0.y.min(up.1.y), centre.y - arm, "the cross arm reaches past the circle");
    near(up.0.y.max(up.1.y), centre.y + arm, "the cross arm reaches past the circle");

    // The cross is reported per view.
    assert_eq!(front.centre_lines, 1, "the front view's one axis line");
    assert_eq!(end.centre_lines, 2, "the end view's cross: two arms");
}

#[test]
fn a_flat_plate_with_two_holes_gets_a_cross_per_hole_and_one_line_per_hole_in_the_edge_view() {
    let s = plate_with_two_holes();
    let plan = plan_of(&s);
    assert_eq!(plan.choice, ViewChoice::Flat, "100 × 40 × 8 is a flat part");
    assert_eq!(plan.sheet.views.len(), 2, "{:?}", plan.sheet.views);
    let plan_view = placed(&plan.sheet, StandardView::Top);
    let edge = placed(&plan.sheet, StandardView::Front);
    let scale = scale_of(&plan.sheet);
    let lines = centre_lines(&plan.sheet);
    let arm = 5.0 * scale + CENTRE_OVERSHOOT_MM;
    // The two views share the drawing's x (first angle), so look only inside one view's box.
    let inside = |view: &crate::drawing::PlacedView, (a, b): (Vec2, Vec2)| {
        let lo = view.bbox[0] - Vec2::new(CENTRE_OVERSHOOT_MM + 1.0, CENTRE_OVERSHOOT_MM + 1.0);
        let hi = view.bbox[1] + Vec2::new(CENTRE_OVERSHOOT_MM + 1.0, CENTRE_OVERSHOOT_MM + 1.0);
        a.x >= lo.x && a.x <= hi.x && b.x >= lo.x && b.x <= hi.x && a.y >= lo.y && a.y <= hi.y && b.y >= lo.y && b.y <= hi.y
    };
    let plan_lines: Vec<(Vec2, Vec2)> = lines.iter().copied().filter(|line| inside(&plan_view, *line)).collect();
    let edge_lines: Vec<(Vec2, Vec2)> = lines.iter().copied().filter(|line| inside(&edge, *line)).collect();

    // The plan view (looking along the plate's normal, Z) carries a cross per hole; the two arms
    // along the common hole line may share one line.
    for (x, y) in [(20.0, 20.0), (80.0, 20.0)] {
        let at = plan_view.origin + Vec2::new(x * scale, y * scale);
        let up = plan_lines
            .iter()
            .find(|(a, b)| vertical(*a, *b) && (a.x - at.x).abs() < 1e-9 && a.y.min(b.y) <= at.y - arm + 1e-9)
            .unwrap_or_else(|| panic!("no vertical cross arm through the hole at {x}: {plan_lines:?}"));
        near(up.0.y.min(up.1.y), at.y - arm, "the vertical arm reaches past the hole");
        near(up.0.y.max(up.1.y), at.y + arm, "the vertical arm reaches past the hole");
        assert!(
            plan_lines
                .iter()
                .any(|(a, b)| horizontal(*a, *b) && (a.y - at.y).abs() < 1e-9 && a.x.min(b.x) <= at.x + 1e-9 && a.x.max(b.x) >= at.x - 1e-9),
            "no horizontal cross arm through the hole at {x}: {plan_lines:?}"
        );
    }

    // The edge view (looking along Y) has one centre line per hole, perpendicular to the plate
    // faces, reaching 3 mm past them.
    let faces = Vec2::new(edge.bbox[0].y, edge.bbox[1].y);
    for x in [20.0, 80.0] {
        let at_x = edge.origin.x + x * scale;
        let line = edge_lines
            .iter()
            .find(|(a, b)| vertical(*a, *b) && (a.x - at_x).abs() < 1e-9)
            .unwrap_or_else(|| panic!("no centre line through the hole at x = {x}: {edge_lines:?}"));
        near(line.0.y.min(line.1.y), faces.x - CENTRE_OVERSHOOT_MM, "the line starts past the near face");
        near(line.0.y.max(line.1.y), faces.y + CENTRE_OVERSHOOT_MM, "the line ends past the far face");
    }
    // Two holes, two lines in the edge view; the plan view's two crosses share their common arm.
    assert_eq!(edge.centre_lines, 2, "one centre line per hole in the edge view");
    assert_eq!(plan_view.centre_lines, 3, "two crosses sharing one arm");
}

#[test]
fn a_fillet_below_half_a_turn_gets_no_centre_line() {
    let mut s = boxed(40.0, 20.0, 10.0);
    s.execute("solid.fillet", &json!({"edges":[[20,0,10]],"radius":2,"body":"Box"})).unwrap();
    let faces = s.execute("model.faces", &json!({"body":"Box"})).unwrap();
    let cylinders = faces["faces"].as_array().unwrap().iter().filter(|f| f.get("cylinder").is_some()).count();
    assert_eq!(cylinders, 1, "the R2 fillet is one quarter cylinder: {faces}");
    assert!(axes(&s).is_empty(), "a quarter cylinder is below half a turn: {:?}", axes(&s));
    let plan = plan_of(&s);
    assert!(!plan.sheet.prims.iter().any(|p| p.layer() == Layer::Center), "no centre line for the fillet");
}

#[test]
fn a_box_keeps_three_views_and_a_cube_drops_the_repeated_ones() {
    let box_plan = plan_of(&boxed(40.0, 20.0, 10.0));
    assert_eq!(box_plan.choice, ViewChoice::General);
    let views: Vec<StandardView> = box_plan.sheet.views.iter().map(|v| v.view).collect();
    assert_eq!(views, [StandardView::Front, StandardView::Top, StandardView::Right]);
    assert!(box_plan.sheet.prims.iter().all(|p| p.layer() != Layer::Center), "a box has no round faces");

    let cube_plan = plan_of(&boxed(10.0, 10.0, 10.0));
    assert_eq!(cube_plan.choice, ViewChoice::General);
    assert!(cube_plan.sheet.views.len() < 3, "a cube repeats its views: {:?}", cube_plan.sheet.views);
}

#[test]
fn an_explicit_views_parameter_is_used_unchanged() {
    let s = plate_with_two_holes();
    let title = TitleBlock::for_design("Plate");
    let plan = plan_sheet(&s, Some(&[StandardView::Front, StandardView::Left]), 0.01, &title, &NotesBlock::default(), None).unwrap();
    assert_eq!(plan.choice, ViewChoice::Explicit);
    let views: Vec<StandardView> = plan.sheet.views.iter().map(|v| v.view).collect();
    assert_eq!(views, [StandardView::Front, StandardView::Left]);
}

// ------------------------------------------------------------------ writers

fn dxf_pairs(dxf: &str) -> Vec<(i32, String)> {
    let lines: Vec<&str> = dxf.lines().collect();
    lines.chunks(2).map(|c| (c.first().and_then(|l| l.trim().parse().ok()).unwrap_or(-1), c.get(1).unwrap_or(&"").trim().to_string())).collect()
}

/// The records of one symbol table, each as its group codes after the record type.
fn table_records(p: &[(i32, String)], table: &str) -> Vec<Vec<(i32, String)>> {
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, table)).expect("table");
    let mut out: Vec<Vec<(i32, String)>> = Vec::new();
    for (c, v) in &p[start + 1..] {
        match (*c, v.as_str()) {
            (0, "ENDTAB") => break,
            (0, _) => out.push(Vec::new()),
            _ => {
                if let Some(record) = out.last_mut() {
                    record.push((*c, v.clone()));
                }
            }
        }
    }
    out
}

fn field(record: &[(i32, String)], code: i32) -> &str {
    record.iter().find(|(c, _)| *c == code).map_or("", |(_, v)| v.as_str())
}

#[test]
fn the_dxf_defines_the_center_linetype_and_puts_every_centre_line_on_the_center_layer() {
    let plan = plan_of(&stepped_shaft());
    let dxf = sheet_to_dxf(&plan.sheet).unwrap();
    let p = dxf_pairs(&dxf);

    // A real CENTER linetype with the ISO 128-2 type 04 elements at d = 0.25 mm.
    let types = table_records(&p, "LTYPE");
    let center = types.iter().find(|r| field(r, 2) == "CENTER").expect("the CENTER linetype is defined");
    let elements: Vec<f64> = center.iter().filter(|(c, _)| *c == 49).map(|(_, v)| v.parse().unwrap()).collect();
    assert_eq!(elements, [6.0, -0.75, 0.0, -0.75], "long dash 24·d, gap 3·d, dot 0.5·d, gap 3·d");
    let total: f64 = center.iter().find(|(c, _)| *c == 40).map(|(_, v)| v.parse().unwrap()).unwrap();
    assert!((total - 7.5).abs() < 1e-9, "the pattern is 7.5 mm long: {total}");

    // The layer the centre lines are on names that linetype.
    let layers = table_records(&p, "LAYER");
    let center_layer = layers.iter().find(|r| field(r, 2) == "CENTER").expect("the CENTER layer is defined");
    assert_eq!(field(center_layer, 6), "CENTER");

    // Every centre-line primitive becomes a LINE on CENTER, and nothing else does.
    let centre_prims = plan.sheet.prims.iter().filter(|prim| prim.layer() == Layer::Center).count();
    assert!(centre_prims >= 3, "the shaft has an axis line and a cross: {centre_prims}");
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "ENTITIES")).expect("ENTITIES");
    let entities = p[start + 1..].iter().filter(|(c, _)| *c == 0).count();
    assert!(entities >= centre_prims);
    let on_center_layer = p[start + 1..]
        .windows(2)
        .filter(|pair| pair[0].0 == 8 && pair[0].1 == "CENTER" && (pair[1].0 == 0 || pair[1].0 == 100 || pair[1].0 == 370))
        .count();
    // Each entity carries its layer code (8) before its first geometry code.
    let mut layer_of_entity = Vec::new();
    let mut current = String::new();
    for (c, v) in &p[start + 1..] {
        if *c == 8 {
            current = v.clone();
        } else if *c == 100 && v == "AcDbLine" {
            layer_of_entity.push(current.clone());
        }
    }
    assert_eq!(layer_of_entity.iter().filter(|l| *l == "CENTER").count(), centre_prims, "{layer_of_entity:?}");
    assert!(on_center_layer > 0, "entities on CENTER are written");
}

/// The length a primitive paints.
fn prim_length(prim: &Prim) -> Option<f64> {
    match prim {
        Prim::Line { a, b, .. } => Some(a.dist(*b)),
        Prim::Polyline { points, closed, .. } => {
            if points.is_empty() {
                return None;
            }
            let mut length: f64 = points.windows(2).map(|pair| pair[0].dist(pair[1])).sum();
            if *closed && let (Some(first), Some(last)) = (points.first(), points.last()) {
                length += last.dist(*first);
            }
            Some(length)
        }
        Prim::Arc { r, a0, a1, .. } => Some(r * (a1 - a0).abs()),
        Prim::Circle { r, .. } => Some(std::f64::consts::TAU * r),
        Prim::Ellipse { major, ratio, t0, t1, .. } => {
            let (a, b) = (major.len(), major.len() * ratio);
            let h = if a + b > 0.0 { ((a - b) / (a + b)).powi(2) } else { 0.0 };
            Some(std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt())) * ((t1 - t0).abs() / std::f64::consts::TAU))
        }
        Prim::Filled { .. } | Prim::Text { .. } => None,
    }
}

#[test]
fn the_pdf_draws_centre_lines_with_the_long_dash_dot_array_and_a_quarter_millimetre_pen() {
    let plan = plan_of(&stepped_shaft());
    let page = sheet_to_pdf(&plan.sheet).unwrap();
    let stroked: Vec<&Prim> = plan.sheet.prims.iter().filter(|p| prim_length(p).is_some()).collect();
    let paths: Vec<&PdfItem> = page.items.iter().filter(|item| matches!(item, PdfItem::Path { .. } | PdfItem::EllipseArc { .. })).collect();
    assert_eq!(stroked.len(), paths.len(), "one PDF item per stroked primitive");
    let mut centre = 0;
    for (prim, item) in stroked.iter().zip(&paths) {
        if prim.layer() != Layer::Center {
            continue;
        }
        centre += 1;
        let PdfItem::Path { width_mm, dash_mm, .. } = item else { panic!("a centre line is a path: {item:?}") };
        assert!((width_mm - 0.25).abs() < 1e-12, "the centre pen is 0.25 mm, not {width_mm}");
        let length = prim_length(prim).unwrap();
        assert_eq!(dash_mm.len(), 4, "long dash, gap, dot, gap: {dash_mm:?}");
        assert!((dash_mm[1] - 0.75).abs() < 1e-12 && (dash_mm[2] - 0.125).abs() < 1e-12 && (dash_mm[3] - 0.75).abs() < 1e-12);
        assert!(dash_mm[0] >= 6.0 - 1e-9, "the first dash is a long one, not a dot: {dash_mm:?}");
        // The line begins and ends on a long dash: the painted length is whole periods plus the
        // final dash.
        let period: f64 = dash_mm.iter().sum();
        let remainder = length % period;
        assert!(
            remainder <= 1e-6 || (remainder - dash_mm[0]).abs() <= 1e-6,
            "the line ends mid-dot: length {length}, period {period}, remainder {remainder}"
        );
    }
    assert!(centre >= 3, "the shaft's centre lines are on the page: {centre}");
}
