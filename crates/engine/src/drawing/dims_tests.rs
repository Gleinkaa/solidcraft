#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::f64::consts::PI;

use serde_json::json;
use solidcraft_io::{PdfItem, text_width_mm};

use super::*;
use crate::Session;
use crate::drawing::centre::ViewCentre;
use crate::drawing::sheet::{NotesBlock, Sheet, TitleBlock, build_sheet_with, sheet_to_dxf, sheet_to_pdf};
use crate::drawing::{ProjectedView, Seg2mm, ViewOptions, project_views, standard_camera, view_name};

/// A plain rectangular view with the projected origin at (0, 0).
fn plain_view(view: StandardView, width: f64, height: f64) -> ProjectedView {
    ProjectedView {
        view,
        visible: vec![
            Seg2mm { a: Vec2::new(0.0, 0.0), b: Vec2::new(width, 0.0) },
            Seg2mm { a: Vec2::new(width, 0.0), b: Vec2::new(width, height) },
            Seg2mm { a: Vec2::new(width, height), b: Vec2::new(0.0, height) },
            Seg2mm { a: Vec2::new(0.0, height), b: Vec2::new(0.0, 0.0) },
        ],
        hidden: Vec::new(),
        visible_plain: vec![
            Seg2mm { a: Vec2::new(0.0, 0.0), b: Vec2::new(width, 0.0) },
            Seg2mm { a: Vec2::new(width, 0.0), b: Vec2::new(width, height) },
            Seg2mm { a: Vec2::new(width, height), b: Vec2::new(0.0, height) },
            Seg2mm { a: Vec2::new(0.0, height), b: Vec2::new(0.0, 0.0) },
        ],
        visible_curves: Vec::new(),
        hidden_plain: Vec::new(),
        hidden_curves: Vec::new(),
        tangent: Vec::new(),
        bbox: [Vec2::ZERO, Vec2::new(width, height)],
        camera: standard_camera(view),
        centre: Vec::new(),
        section: None,
        section_marks: Vec::new(),
    }
}

fn linear(p1: Vec2, p2: Vec2, angle_deg: f64, offset_mm: f64) -> Dimension {
    Dimension {
        view: StandardView::Front,
        geometry: DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm },
        text: None,
        suffix: None,
        text_shift_mm: 0.0,
    }
}

fn diameter(p1: Vec2, p2: Vec2, angle_deg: f64, offset_mm: f64, suffix: Option<&str>) -> Dimension {
    Dimension {
        view: StandardView::Front,
        geometry: DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm },
        text: None,
        suffix: suffix.map(str::to_string),
        text_shift_mm: 0.0,
    }
}

fn build(views: &[ProjectedView], scale: (u32, u32), annotations: &Annotations) -> Sheet {
    build_sheet_with(views, &TitleBlock::for_design("Dimension"), &NotesBlock::default(), Some(scale), annotations).unwrap()
}

fn placed(origin: Vec2, p: Vec2) -> Vec2 {
    origin + p
}

fn dim_lines(sheet: &Sheet, index: usize) -> Vec<(Vec2, Vec2)> {
    sheet
        .dimensions
        .get(index)
        .map(|d| {
            d.prims
                .iter()
                .filter_map(|p| match p {
                    Prim::Line { a, b, .. } => Some((*a, *b)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn dim_arrows(sheet: &Sheet, index: usize) -> Vec<Vec<Vec2>> {
    sheet
        .dimensions
        .get(index)
        .map(|d| {
            d.prims
                .iter()
                .filter_map(|p| match p {
                    Prim::Filled { points, .. } => Some(points.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn dim_text(sheet: &Sheet, index: usize) -> Option<(Vec2, f64, f64, String)> {
    sheet.dimensions.get(index)?.prims.iter().find_map(|p| match p {
        Prim::Text { at, height, angle_deg, text, .. } => Some((*at, *height, *angle_deg, text.clone())),
        _ => None,
    })
}

/// The apex, base midpoint, length and included angle of one arrowhead triangle.
fn arrow_geometry(points: &[Vec2]) -> (Vec2, Vec2, f64, f64) {
    let [apex, left, right] = [points[0], points[1], points[2]];
    let base = (left + right) * 0.5;
    let length = apex.dist(base);
    let half = left.dist(right) * 0.5;
    (apex, base, length, 2.0 * (half / length).atan().to_degrees())
}

fn segment_hits_box(a: Vec2, b: Vec2, corners: &[Vec2; 4], clear: f64) -> bool {
    // Transform into the box's (along, up) frame from its first corner.
    let along = (corners[1] - corners[0]).normalized().unwrap_or(Vec2::X);
    let up = along.perp();
    let origin = corners[0] - along * clear - up * clear;
    let local = |p: Vec2| Vec2::new((p - origin).dot(along), (p - origin).dot(up));
    let (lo, hi) = (Vec2::ZERO, Vec2::new(corners[1].dist(corners[0]) + 2.0 * clear, corners[3].dist(corners[0]) + 2.0 * clear));
    segment_hits_rect(local(a), local(b), lo, hi)
}

fn sheet_lines(sheet: &Sheet) -> Vec<(Vec2, Vec2)> {
    let mut out = Vec::new();
    for prim in &sheet.prims {
        match prim {
            Prim::Line { a, b, .. } => out.push((*a, *b)),
            Prim::Polyline { points, closed, .. } => {
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push((*a, *b));
                    }
                }
                if *closed && points.len() > 2 {
                    out.push((points[points.len() - 1], points[0]));
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
                if points.len() > 2 {
                    out.push((points[points.len() - 1], points[0]));
                }
            }
            Prim::Text { .. } => {}
        }
    }
    out
}

/// The cap-height box of a text whose baseline centre is `at`.
fn text_box(at: Vec2, width: f64, angle_deg: f64) -> [Vec2; 4] {
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    [at - u * (width * 0.5), at + u * (width * 0.5), at + u * (width * 0.5) + n * DIM_TEXT_HEIGHT_MM, at - u * (width * 0.5) + n * DIM_TEXT_HEIGHT_MM]
}

fn text_is_clear(sheet: &Sheet, at: Vec2, text: &str, angle_deg: f64) -> bool {
    let box_corners = text_box(at, text_width_mm(text, DIM_TEXT_HEIGHT_MM), angle_deg);
    // Arrowheads are filled polygons, not lines: a value may sit just above its own arrow.
    let mut lines = Vec::new();
    for prim in &sheet.prims {
        if !matches!(prim, Prim::Filled { .. }) {
            match prim {
                Prim::Line { a, b, .. } => lines.push((*a, *b)),
                Prim::Polyline { points, closed, .. } => {
                    for pair in points.windows(2) {
                        if let [a, b] = pair {
                            lines.push((*a, *b));
                        }
                    }
                    if *closed && points.len() > 2 {
                        lines.push((points[points.len() - 1], points[0]));
                    }
                }
                _ => {}
            }
        }
    }
    lines.iter().all(|(a, b)| !segment_hits_box(*a, *b, &box_corners, TEXT_CLEAR_MM))
}

#[test]
fn a_40_mm_linear_dimension_at_1_to_1_measures_the_iso_rules() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations { dimensions: vec![linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, -10.0)], notes: Vec::new() };
    let sheet = build(&views, (1, 1), &annotations);
    let origin = sheet.views[0].origin;
    let (a, b) = (placed(origin, Vec2::new(0.0, -10.0)), placed(origin, Vec2::new(40.0, -10.0)));
    let (p1, p2) = (placed(origin, Vec2::ZERO), placed(origin, Vec2::new(40.0, 0.0)));

    // Extension lines: from the feature point with no gap, 2 mm past the dimension line.
    let lines = dim_lines(&sheet, 0);
    let extension = |p: Vec2, foot: Vec2| {
        lines.iter().any(|(x, y)| {
            (x.dist(p) < 1e-9 && y.dist(foot + Vec2::new(0.0, -EXT_OVERSHOOT_MM)) < 1e-9)
                || (y.dist(p) < 1e-9 && x.dist(foot + Vec2::new(0.0, -EXT_OVERSHOOT_MM)) < 1e-9)
        })
    };
    assert!(extension(p1, a), "no extension line from p1 to 2 mm past the dimension line: {lines:?}");
    assert!(extension(p2, b), "no extension line from p2 to 2 mm past the dimension line: {lines:?}");
    assert!(
        lines.iter().any(|(x, y)| (x.dist(a) < 1e-9 && y.dist(b) < 1e-9) || (x.dist(b) < 1e-9 && y.dist(a) < 1e-9)),
        "no dimension line between the extension lines: {lines:?}"
    );

    // Closed filled arrowheads: apex on each extension line, 2.5 mm long, 30° included.
    let arrows = dim_arrows(&sheet, 0);
    assert_eq!(arrows.len(), 2, "a length has two arrowheads: {arrows:?}");
    let mut apexes = Vec::new();
    for points in &arrows {
        let (apex, _base, length, angle) = arrow_geometry(points);
        assert!((length - ARROW_LENGTH_MM).abs() <= 0.01, "arrowhead length {length} mm");
        assert!((angle - 30.0).abs() <= 0.1, "included angle {angle}°");
        apexes.push(apex);
    }
    assert!(apexes.iter().any(|p| p.dist(a) < 1e-9) && apexes.iter().any(|p| p.dist(b) < 1e-9), "apexes {apexes:?}");

    // Text "40", 3.5 mm, centred, baseline 1 mm above the dimension line.
    let (at, height, angle, text) = dim_text(&sheet, 0).expect("the dimension has text");
    assert_eq!((text.as_str(), height), ("40", DIM_TEXT_HEIGHT_MM));
    assert!(angle.abs() < 1e-9, "a horizontal dimension's text is horizontal");
    assert!((at.x - (a.x + b.x) * 0.5).abs() < 1e-9, "text is centred on the dimension line: {at:?}");
    assert!((at.y - (a.y + DIM_TEXT_GAP_MM)).abs() <= 0.01, "baseline {} is not 1 mm above {}", at.y, a.y);
    assert_eq!(sheet.dimensions[0].text_shift_mm, 0.0, "nothing crosses the text, so it stays centred");
}

#[test]
fn the_same_dimension_at_2_to_1_keeps_its_value_and_its_sheet_offset() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations { dimensions: vec![linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, -10.0)], notes: Vec::new() };
    let sheet = build(&views, (2, 1), &annotations);
    assert_eq!(sheet.scale, (2, 1));
    let origin = sheet.views[0].origin;
    let p1 = origin;
    let (a, b) = (origin + Vec2::new(0.0, -10.0), origin + Vec2::new(80.0, -10.0));
    assert!((b.x - a.x - 80.0).abs() < 1e-9, "the dimension line is 80 mm on paper");
    // The measurement is still the model value, and the offset is still 10 mm on the sheet.
    let (_, _, _, text) = dim_text(&sheet, 0).expect("the dimension has text");
    assert_eq!(text, "40");
    assert!((a.y - p1.y).abs() - 10.0 < 1e-9, "the offset is a sheet property");
    let (at, _, _, _) = dim_text(&sheet, 0).unwrap();
    assert!((at.y - (a.y + DIM_TEXT_GAP_MM)).abs() <= 0.01);
}

#[test]
fn a_vertical_dimension_reads_from_the_right() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations { dimensions: vec![linear(Vec2::ZERO, Vec2::new(0.0, 10.0), 90.0, 10.0)], notes: Vec::new() };
    let sheet = build(&views, (1, 1), &annotations);
    let (at, height, angle, text) = dim_text(&sheet, 0).expect("the dimension has text");
    assert_eq!((text.as_str(), height), ("10", DIM_TEXT_HEIGHT_MM));
    assert!((angle - 90.0).abs() < 1e-9, "a vertical dimension's text is turned 90°: {angle}");
    assert!(text_is_clear(&sheet, at, &text, angle), "the vertical text crosses a line");
}

#[test]
fn a_narrow_5_mm_dimension_puts_arrows_and_text_outside() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations { dimensions: vec![linear(Vec2::ZERO, Vec2::new(5.0, 0.0), 0.0, -10.0)], notes: Vec::new() };
    let sheet = build(&views, (1, 1), &annotations);
    let origin = sheet.views[0].origin;
    let a = origin + Vec2::new(0.0, -10.0);
    let b = origin + Vec2::new(5.0, -10.0);
    let arrows = dim_arrows(&sheet, 0);
    assert_eq!(arrows.len(), 2);
    for points in &arrows {
        let (apex, base, length, angle) = arrow_geometry(points);
        assert!((length - ARROW_LENGTH_MM).abs() <= 0.01);
        assert!((angle - 30.0).abs() <= 0.1);
        assert!(apex.dist(a) < 1e-9 || apex.dist(b) < 1e-9, "the apex is on an extension line");
        // The base is outside the span between the extension lines: the arrow points in.
        assert!(base.x < a.x + 1e-9 || base.x > b.x - 1e-9, "the arrowhead does not point in: {base:?}");
    }
    let (at, _, angle, text) = dim_text(&sheet, 0).unwrap();
    assert_eq!(text, "5");
    let half = (b.x - a.x) * 0.5;
    assert!((at.x - (a.x + b.x) * 0.5).abs() > half, "the text is inside the narrow span: {at:?}");
    assert!(text_is_clear(&sheet, at, &text, angle), "the narrow dimension's text overlaps a line");
    // Nothing overlaps: dimension line, arrows and text all keep clear astern of each other.
    let box_corners = text_box(at, text_width_mm(&text, DIM_TEXT_HEIGHT_MM), angle);
    for points in &arrows {
        for pair in points.windows(2) {
            if let [p, q] = pair {
                assert!(!segment_hits_box(*p, *q, &box_corners, 0.0), "an arrowhead touches the text");
            }
        }
    }
}

#[test]
fn text_that_crosses_a_centre_line_shifts_until_it_is_clear() {
    let mut view = plain_view(StandardView::Front, 40.0, 10.0);
    // A vertical centre line through the middle of the view.
    view.centre = vec![ViewCentre::Line { a: Vec2::new(20.0, -5.0), b: Vec2::new(20.0, 15.0) }];
    let views = [view];
    // Offset +5 puts the dimension line at the middle height, where the centre line crosses it.
    let annotations = Annotations { dimensions: vec![linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, 5.0)], notes: Vec::new() };
    let sheet = build(&views, (1, 1), &annotations);
    let (at, _, angle, text) = dim_text(&sheet, 0).expect("text");
    assert_eq!(text, "40");
    assert!(sheet.dimensions[0].text_shift_mm.abs() >= 4.0, "the text did not move off the centre line: {}", sheet.dimensions[0].text_shift_mm);
    assert!(text_is_clear(&sheet, at, &text, angle), "the shifted text still crosses a line");
    // The box is at least 0.5 mm from every line (DM-8a).
    let box_corners = text_box(at, text_width_mm(&text, DIM_TEXT_HEIGHT_MM), angle);
    for (a, b) in sheet_lines(&sheet) {
        assert!(!segment_hits_box(a, b, &box_corners, TEXT_CLEAR_MM), "a line is closer than 0.5 mm to the text box");
    }
}

#[test]
fn diameter_and_radius_use_their_symbols_and_a_decimal_comma() {
    assert_eq!(format_value(12.0), "12");
    assert_eq!(format_value(1.5), "1,5");
    assert_eq!(format_value(0.25), "0,25");
    assert_eq!(format_value(1.005), "1");
    assert_eq!(format_value(12.345), "12,35");

    let radius = Dimension {
        view: StandardView::Front,
        geometry: DimensionGeometry::Radius { centre: Vec2::new(20.0, 5.0), radius: 5.0, at_angle_deg: 45.0 },
        text: None,
        suffix: None,
        text_shift_mm: 0.0,
    };
    assert_eq!(radius.label(), "R5");
    let dia = diameter(Vec2::new(15.0, -5.0), Vec2::new(15.0, 5.0), 90.0, 0.0, Some("h6"));
    assert_eq!(dia.label(), "Ø10 h6");
    let small = diameter(Vec2::new(15.0, -0.75), Vec2::new(15.0, 0.75), 90.0, 0.0, None);
    assert_eq!(small.label(), "Ø1,5");

    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations { dimensions: vec![dia.clone(), radius], notes: Vec::new() };
    let sheet = build(&views, (1, 1), &annotations);
    let (_, _, _, text) = dim_text(&sheet, 0).expect("diameter text");
    assert!(text.starts_with('Ø'), "diameter text {text:?}");
    assert_eq!(text, "Ø10 h6");
    let (_, _, _, text) = dim_text(&sheet, 1).expect("radius text");
    assert!(text.starts_with('R'), "radius text {text:?}");
}

#[test]
fn the_band_grows_with_the_number_of_stacked_rows_and_the_stack_starts_at_ten() {
    assert_eq!(band_mm(0), DIM_BAND_MM);
    assert_eq!(band_mm(1), 15.0);
    assert_eq!(band_mm(2), 23.0);
    assert_eq!(band_mm(3), 31.0);

    let view = plain_view(StandardView::Front, 40.0, 10.0);
    let dims = vec![
        linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, 0.0),
        linear(Vec2::ZERO, Vec2::new(10.0, 0.0), 0.0, 0.0),
        linear(Vec2::ZERO, Vec2::new(20.0, 0.0), 0.0, 0.0),
    ];
    let stacked = stack(&view, DimSide::Below, &dims, 2.0);
    let measures: Vec<f64> = stacked.iter().map(|d| d.geometry.measured()).collect();
    assert_eq!(measures, [10.0, 20.0, 40.0], "shortest first");
    let offsets: Vec<f64> = stacked
        .iter()
        .map(|d| match d.geometry {
            DimensionGeometry::Linear { offset_mm, .. } => offset_mm,
            _ => panic!("a linear dimension"),
        })
        .collect();
    // The view's bottom outline is y = 0 at 2:1; p1 is also at y = 0.
    assert_eq!(offsets, [-10.0, -18.0, -26.0], "10 mm from the outline, 8 mm per further row");
    assert_eq!(side_rows(&stacked), [0, 3, 0, 0]);
}

#[test]
fn the_writers_carry_n_dimensions_and_n_anonymous_blocks() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let annotations = Annotations {
        dimensions: vec![linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, -10.0), linear(Vec2::ZERO, Vec2::new(10.0, 0.0), 0.0, -18.0)],
        notes: Vec::new(),
    };
    let sheet = build(&views, (1, 1), &annotations);
    let dxf = sheet_to_dxf(&sheet).unwrap();
    assert!(dxf.contains("SolidCraft ISO"), "the DIMSTYLE table names the style");
    assert!(dxf.contains("DIMSTYLE"));
    assert_eq!(dxf.matches("  0\nDIMENSION\n").count(), 2, "two DIMENSION entities");
    assert!(dxf.contains("*D1") && dxf.contains("*D2"), "each dimension has its anonymous block");

    let page = sheet_to_pdf(&sheet).unwrap();
    let filled = page.items.iter().filter(|item| matches!(item, PdfItem::Path { fill: true, .. })).count();
    assert_eq!(filled, 4, "N linear dimensions paint 2·N filled arrowheads");
}

#[test]
fn annotations_on_a_view_that_is_not_on_the_sheet_are_an_error() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let mut dim = linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, -10.0);
    dim.view = StandardView::Right;
    let annotations = Annotations { dimensions: vec![dim.clone()], notes: Vec::new() };
    assert!(build_sheet_with(&views, &TitleBlock::for_design("x"), &NotesBlock::default(), Some((1, 1)), &annotations).is_err());
    // A note on a missing view, a non-finite number and an over-long text are errors too.
    let note = Note { view: StandardView::Top, text: "x".into(), at: Vec2::ZERO, leader: vec![Vec2::ZERO], target: None };
    assert!(
        build_sheet_with(
            &views,
            &TitleBlock::for_design("x"),
            &NotesBlock::default(),
            Some((1, 1)),
            &Annotations { dimensions: Vec::new(), notes: vec![note] }
        )
        .is_err()
    );
    let mut bad = dim;
    if let DimensionGeometry::Linear { offset_mm, .. } = &mut bad.geometry {
        *offset_mm = f64::NAN;
    }
    assert!(
        build_sheet_with(
            &views,
            &TitleBlock::for_design("x"),
            &NotesBlock::default(),
            Some((1, 1)),
            &Annotations { dimensions: vec![bad], notes: Vec::new() }
        )
        .is_err()
    );
    let long = "x".repeat(MAX_TEXT_CHARS + 1);
    let mut note = Note { view: StandardView::Front, text: long, at: Vec2::ZERO, leader: vec![Vec2::ZERO], target: None };
    assert!(
        build_sheet_with(
            &views,
            &TitleBlock::for_design("x"),
            &NotesBlock::default(),
            Some((1, 1)),
            &Annotations { dimensions: Vec::new(), notes: vec![note.clone()] }
        )
        .is_err()
    );
    note.text = "a\nb".into();
    assert!(
        build_sheet_with(
            &views,
            &TitleBlock::for_design("x"),
            &NotesBlock::default(),
            Some((1, 1)),
            &Annotations { dimensions: Vec::new(), notes: vec![note] }
        )
        .is_err()
    );
}

#[test]
fn notes_draw_a_reference_line_a_leader_and_a_terminator() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let note = Note {
        view: StandardView::Front,
        text: "Bohrung Ø3 H7, durchgehend".into(),
        at: Vec2::new(2.0, 30.0),
        leader: vec![Vec2::new(38.0, 30.0), Vec2::new(30.0, 10.0)],
        target: None,
    };
    let annotations = Annotations { dimensions: Vec::new(), notes: vec![note] };
    let sheet = build(&views, (1, 1), &annotations);
    assert_eq!(sheet.notes.len(), 1);
    let drawn = &sheet.notes[0];
    let (a, b) = drawn.reference.expect("a reference line");
    assert!((a.y - b.y).abs() < 1e-9 && (b.x - a.x - text_width_mm(&drawn.text_string, DIM_TEXT_HEIGHT_MM)).abs() < 1e-9);
    assert!(!drawn.leader.is_empty(), "the leader is drawn");
    assert_eq!(drawn.terminator.len(), 3, "the target lands on the top edge, so it gets an arrowhead");
    let dxf = sheet_to_dxf(&sheet).unwrap();
    assert!(dxf.contains("LEADER"), "the note is a LEADER in the DXF");
    assert!(dxf.contains("Bohrung"), "the note's text is in the DXF");
    let page = sheet_to_pdf(&sheet).unwrap();
    assert!(page.items.iter().any(|item| matches!(item, PdfItem::Text { text, .. } if text == &drawn.text_string)));
}

#[test]
fn a_turned_part_relates_the_end_view_circle_to_the_front_view() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length": 40, "width": 20, "height": 10})).unwrap();
    let views = project_views(&s, &[StandardView::Front], &ViewOptions::default()).unwrap();
    let dim = Dimension {
        view: StandardView::Front,
        geometry: DimensionGeometry::DiameterOnCircle { centre: Vec2::new(20.0, 5.0), radius: 4.0, at_angle_deg: 30.0 },
        text: None,
        suffix: None,
        text_shift_mm: 0.0,
    };
    let sheet = build(&views, (1, 1), &Annotations { dimensions: vec![dim], notes: Vec::new() });
    let (at, _, angle, text) = dim_text(&sheet, 0).unwrap();
    assert_eq!(text, "Ø8");
    assert!(text_is_clear(&sheet, at, &text, angle));
    assert_eq!(view_name(sheet.views[0].view), "front");
}

#[test]
fn format_value_handles_negative_and_tiny_values() {
    assert_eq!(format_value(-12.0), "-12");
    assert_eq!(format_value(-1.5), "-1,5");
    assert_eq!(format_value(0.0), "0");
    assert_eq!(format_value(-0.001), "0");
    assert_eq!(format_value(f64::NAN), "0");
    assert_eq!(format_value(PI), "3,14");
}

#[test]
fn a_radial_dimension_has_one_arrowhead_and_one_solid_in_its_block() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let radius = Dimension {
        view: StandardView::Front,
        geometry: DimensionGeometry::Radius { centre: Vec2::new(20.0, 5.0), radius: 4.0, at_angle_deg: 200.0 },
        text: None,
        suffix: None,
        text_shift_mm: 0.0,
    };
    let sheet = build(&views, (1, 1), &Annotations { dimensions: vec![radius], notes: Vec::new() });
    assert_eq!(dim_arrows(&sheet, 0).len(), 1, "a radius has one arrow at the arc");
    let (_, _, _, text) = dim_text(&sheet, 0).unwrap();
    assert_eq!(text, "R4");
    let (at, _, angle, _) = dim_text(&sheet, 0).unwrap();
    assert!(text_is_clear(&sheet, at, &text, angle));
    let dxf = sheet_to_dxf(&sheet).unwrap();
    assert_eq!(dxf.matches("\nSOLID\n").count(), 1, "the radial block holds one filled arrowhead");
    assert_eq!(dxf.matches("  0\nDIMENSION\n").count(), 1);
}

#[test]
fn a_note_inside_a_surface_ends_in_a_one_millimetre_dot() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let note = Note {
        view: StandardView::Front,
        text: "Fläche fein".into(),
        at: Vec2::new(2.0, 25.0),
        leader: vec![Vec2::new(30.0, 25.0), Vec2::new(25.0, 7.0), Vec2::new(20.0, 5.0)],
        target: None,
    };
    let sheet = build(&views, (1, 1), &Annotations { dimensions: Vec::new(), notes: vec![note] });
    let drawn = &sheet.notes[0];
    assert_eq!(drawn.terminator.len(), 16, "the dot is a filled circle");
    let (cx, cy) = drawn.terminator.iter().fold((0.0, 0.0), |(x, y), p| (x + p.x, y + p.y));
    let (cx, cy) = (cx / 16.0, cy / 16.0);
    let radius = drawn.terminator[0].dist(Vec2::new(cx, cy));
    assert!((radius - 0.5).abs() < 1e-9, "the dot is 1 mm across, radius {radius}");
    let dxf = sheet_to_dxf(&sheet).unwrap();
    assert!(dxf.matches("\nSOLID\n").count() > 1, "the dot is a fan of SOLID triangles");
}

#[test]
fn an_explicit_text_shift_is_honoured_when_it_is_free() {
    let views = [plain_view(StandardView::Front, 40.0, 10.0)];
    let mut dim = linear(Vec2::ZERO, Vec2::new(40.0, 0.0), 0.0, -10.0);
    dim.text_shift_mm = 5.0;
    let sheet = build(&views, (1, 1), &Annotations { dimensions: vec![dim], notes: Vec::new() });
    assert_eq!(sheet.dimensions[0].text_shift_mm, 5.0);
}
