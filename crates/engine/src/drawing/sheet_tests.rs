#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use serde_json::json;
use solidcraft_render::StandardView;

use super::*;
use crate::Session;
use crate::drawing::{ViewOptions, project_views, rohmass};

fn boxed(length: f64, width: f64, height: f64) -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length": length, "width": width, "height": height, "body_name": "Plate"})).unwrap();
    s
}

fn projected(s: &Session, views: &[StandardView]) -> Vec<ProjectedView> {
    project_views(s, views, &ViewOptions::default()).unwrap()
}

fn placed(sheet: &Sheet, view: StandardView) -> &PlacedView {
    sheet.views.iter().find(|v| v.view == view).unwrap_or_else(|| panic!("{view:?} is not placed"))
}

fn span(b: [Vec2; 2]) -> Vec2 {
    b[1] - b[0]
}

fn inside_frame(sheet: &Sheet, b: [Vec2; 2]) -> bool {
    let [f0, f1] = sheet.frame_rect();
    b[0].x >= f0.x - 1e-9 && b[0].y >= f0.y - 1e-9 && b[1].x <= f1.x + 1e-9 && b[1].y <= f1.y + 1e-9
}

fn clear_of_title_block(sheet: &Sheet, b: [Vec2; 2]) -> bool {
    let [t0, t1] = sheet.title_block_rect();
    !(b[1].x > t0.x && b[0].x < t1.x && b[1].y > t0.y && b[0].y < t1.y)
}

fn frame_polyline(sheet: &Sheet) -> Vec<Vec2> {
    sheet
        .prims
        .iter()
        .find_map(|p| match p {
            Prim::Polyline { points, closed: true, layer: Layer::Frame } => Some(points.clone()),
            _ => None,
        })
        .expect("the sheet draws its frame as a closed polyline")
}

fn text_values(sheet: &Sheet) -> Vec<String> {
    sheet
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_40_20_10_box_is_laid_out_in_first_angle_at_2_to_1() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    assert_eq!(sheet.scale, (2, 1), "the largest ISO 5455 scale that fits");
    assert_eq!(sheet.paper, Paper::A4);
    assert_eq!(sheet.views.len(), 3);

    let front = placed(&sheet, StandardView::Front);
    let top = placed(&sheet, StandardView::Top);
    let right = placed(&sheet, StandardView::Right);
    // Method 1: the view from above is drawn below the front view, the view from the right to
    // its left; front and top share x, front and right share y.
    assert!(top.origin.y < front.origin.y, "top {top:?} is not below front {front:?}");
    assert!(right.origin.x < front.origin.x, "right {right:?} is not left of front {front:?}");
    assert!((front.origin.x - top.origin.x).abs() < 1e-6, "front and top must share x");
    assert!((front.origin.y - right.origin.y).abs() < 1e-6, "front and right must share y");

    // 40 × 20 × 10 mm at 2:1: front 80 × 20, top 80 × 40, right 40 × 20 on paper.
    for (view, expected) in [(front, Vec2::new(80.0, 20.0)), (top, Vec2::new(80.0, 40.0)), (right, Vec2::new(40.0, 20.0))] {
        let d = span(view.bbox);
        assert!((d.x - expected.x).abs() < 1e-9 && (d.y - expected.y).abs() < 1e-9, "{:?} spans {d:?}, expected {expected:?}", view.view);
    }
    assert!((front.bbox[0].y - top.bbox[1].y - VIEW_GAP_MM).abs() < 1e-9, "the gap below the front view is 15 mm");
    assert!((front.bbox[0].x - right.bbox[1].x - VIEW_GAP_MM).abs() < 1e-9, "the gap left of the front view is 15 mm");

    // The frame is exactly ISO 5457: 20 mm left, 10 mm elsewhere.
    let [f0, f1] = sheet.frame_rect();
    assert_eq!(f0, Vec2::new(20.0, 10.0));
    assert_eq!(f1, Vec2::new(287.0, 200.0));
    let frame = frame_polyline(&sheet);
    assert_eq!(frame, vec![Vec2::new(20.0, 10.0), Vec2::new(287.0, 10.0), Vec2::new(287.0, 200.0), Vec2::new(20.0, 200.0)]);

    // The title block is 180 mm wide in the bottom-right corner, touching the frame.
    let [t0, t1] = sheet.title_block_rect();
    assert!((t1.x - 287.0).abs() < 1e-12 && (t0.y - 10.0).abs() < 1e-12, "{t0:?} {t1:?}");
    assert!((t1.x - t0.x - TITLE_BLOCK_WIDTH_MM).abs() < 1e-12);

    // Every view lies in the drawing area with the 10 mm margin and clear of the title block.
    for v in &sheet.views {
        assert!(inside_frame(&sheet, v.bbox), "{v:?}");
        assert!(v.bbox[0].x >= 20.0 + DIM_BAND_MM - 1e-9 && v.bbox[0].y >= 10.0 + DIM_BAND_MM - 1e-9, "{v:?}");
        assert!(clear_of_title_block(&sheet, v.bbox), "{v:?} overlaps the title block");
    }
    let union = sheet.views.iter().fold([Vec2::new(f64::INFINITY, f64::INFINITY), Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY)], |acc, v| {
        [Vec2::new(acc[0].x.min(v.bbox[0].x), acc[0].y.min(v.bbox[0].y)), Vec2::new(acc[1].x.max(v.bbox[1].x), acc[1].y.max(v.bbox[1].y))]
    });
    assert!(
        union[0].y >= sheet.title_block_rect()[1].y + DIM_BAND_MM - 1e-9,
        "the arrangement keeps its dimension band above the title block: {union:?}"
    );
}

#[test]
fn a_2000_mm_bar_gets_a_reduction_scale_and_still_fits() {
    let s = boxed(2000.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Bar"), &NotesBlock::default(), None).unwrap();
    let factor = f64::from(sheet.scale.0) / f64::from(sheet.scale.1);
    assert!(factor <= 0.1 + 1e-12, "expected a reduction of 1:10 or smaller, got {}:{}", sheet.scale.0, sheet.scale.1);
    assert_eq!(sheet.scale, (1, 10));
    let front = placed(&sheet, StandardView::Front);
    assert!((span(front.bbox).x - 2000.0 * factor).abs() < 1e-9, "2000 mm at 1:10 is 200 mm on paper");
    for v in &sheet.views {
        assert!(inside_frame(&sheet, v.bbox), "{v:?}");
        assert!(clear_of_title_block(&sheet, v.bbox), "{v:?} overlaps the title block");
        assert!(v.bbox[0].x >= 20.0 + DIM_BAND_MM - 1e-9 && v.bbox[0].y >= 10.0 + DIM_BAND_MM - 1e-9, "{v:?}");
    }
}

#[test]
fn an_explicit_scale_that_does_not_fit_is_an_error_and_never_panics() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let title = TitleBlock::for_design("Plate");
    for scale in [(10, 1), (5, 1), (0, 1), (1, 0), (100_001, 1)] {
        assert!(build_sheet(&views, &title, &NotesBlock::default(), Some(scale)).is_err(), "accepted {scale:?}");
    }
    let sheet = build_sheet(&views, &title, &NotesBlock::default(), Some((1, 1))).unwrap();
    assert_eq!(sheet.scale, (1, 1));
    assert_eq!(sheet.scale_text(), "1:1");
    // No views is still a sheet: frame and title block, at the neutral 1:1.
    let empty = build_sheet(&[], &title, &NotesBlock::default(), None).unwrap();
    assert_eq!(empty.scale, (1, 1));
    assert!(empty.views.is_empty());
    assert_eq!(frame_polyline(&empty).len(), 4);
}

#[test]
fn duplicate_non_finite_and_too_many_views_are_errors() {
    let s = boxed(40.0, 20.0, 10.0);
    let mut views = projected(&s, &[StandardView::Front]);
    let title = TitleBlock::for_design("Plate");
    assert!(build_sheet(&views, &title, &NotesBlock::default(), None).is_ok());
    let duplicate = vec![views[0].clone(), views[0].clone()];
    assert!(build_sheet(&duplicate, &title, &NotesBlock::default(), None).is_err());
    views[0].bbox[1] = Vec2::new(f64::NAN, 10.0);
    assert!(build_sheet(&views, &title, &NotesBlock::default(), None).is_err());
    let seven: Vec<ProjectedView> = [
        StandardView::Front,
        StandardView::Top,
        StandardView::Right,
        StandardView::Left,
        StandardView::Bottom,
        StandardView::Back,
        StandardView::Iso,
    ]
    .iter()
    .map(|view| ProjectedView {
        view: *view,
        visible: Vec::new(),
        hidden: Vec::new(),
        visible_plain: Vec::new(),
        visible_curves: Vec::new(),
        hidden_plain: Vec::new(),
        hidden_curves: Vec::new(),
        tangent: Vec::new(),
        bbox: [Vec2::ZERO, Vec2::new(10.0, 10.0)],
        camera: crate::drawing::standard_camera(*view),
        centre: Vec::new(),
        section: None,
        section_marks: Vec::new(),
    })
    .collect();
    assert!(build_sheet(&seven, &title, &NotesBlock::default(), None).is_err());
    assert_eq!(parse_scale("1:2"), Some((1, 2)));
    assert_eq!(parse_scale(" 2 : 1 "), Some((2, 1)));
    for bad in ["", "1", "1:", ":2", "0:1", "1:0", "1:2:3", "-1:2", "1e2:1", "100001:1"] {
        assert!(parse_scale(bad).is_none(), "accepted {bad:?}");
    }
}

#[test]
fn the_title_block_names_every_iso_7200_field_and_fills_the_defaults() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let title = TitleBlock::for_design("Plate");
    let sheet = build_sheet(&views, &title, &NotesBlock::default(), None).unwrap();
    let texts = text_values(&sheet);
    for label in [
        "Benennung",
        "Zeichnungsnummer",
        "Ma\u{00df}stab",
        "Werkstoff",
        "Allgemeintoleranz",
        "Erstellt durch",
        "Ausgabedatum",
        "Dokumentart",
        "Blatt",
        "Projektionsmethode",
    ] {
        assert!(texts.iter().any(|t| t == label), "missing title-block label {label:?}: {texts:?}");
    }
    assert!(texts.iter().any(|t| t == "Plate"));
    assert!(texts.iter().any(|t| t == "2:1"));
    assert!(texts.iter().any(|t| t == "ISO 2768-mK"));
    assert!(texts.iter().any(|t| t == "Fertigungszeichnung"));
    assert!(texts.iter().any(|t| t == "1/1"));
    assert!(texts.iter().any(|t| t == "SolidCraft"));
    // The default date is dd.mm.yyyy (the checker's pattern), and the material has a letter.
    let date = &sheet.title.datum;
    let parts: Vec<&str> = date.split('.').collect();
    assert_eq!(parts.len(), 3, "{date}");
    assert!(parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())), "{date}");
    assert!((2..=4).contains(&parts[2].len()), "{date}");
    assert!(sheet.title.werkstoff.chars().any(|c| c.is_ascii_alphanumeric()));
    // Every title-block text is at least the 2.5 mm A4 minimum and inside the block.
    let [t0, t1] = sheet.title_block_rect();
    for prim in &sheet.prims {
        if let Prim::Text { at, height, text, .. } = prim
            && at.x >= t0.x - 1e-9
            && at.y >= t0.y - 1e-9
            && at.y <= t1.y
            && text != "Projektionsmethode"
        {
            assert!(*height >= 2.5 - 1e-9, "{text:?} is only {height} mm high");
        }
    }
    // The first-angle symbol is geometry: the truncated cone as a polyline and two true circles.
    let cone = sheet.prims.iter().any(|p| matches!(p, Prim::Polyline { layer: Layer::TitleBlockInner, .. }));
    let circles = sheet.prims.iter().filter(|p| matches!(p, Prim::Circle { layer: Layer::TitleBlockInner, .. })).count();
    assert!(cone && circles >= 2, "the projection symbol needs the cone and two circles: cone {cone}, {circles} circles");
}

/// The title block's cells in sheet mm, from the table the drawing uses.
fn title_cells(sheet: &Sheet) -> Vec<[Vec2; 2]> {
    let [t0, _t1] = sheet.title_block_rect();
    title_block_cells().into_iter().map(|cell| [cell.rect[0] + t0, cell.rect[1] + t0]).collect()
}

/// The box of a left/centre/right anchored text primitive, from the PDF writer's metrics.
fn text_box(at: Vec2, height: f64, text: &str, anchor: TextAnchor) -> [Vec2; 2] {
    let width = solidcraft_io::text_width_mm(text, height);
    let x = match anchor {
        TextAnchor::Left => at.x,
        TextAnchor::Center => at.x - width / 2.0,
        TextAnchor::Right => at.x - width,
    };
    [Vec2::new(x, at.y), Vec2::new(x + width, at.y + height)]
}

/// Every text prim of the title block with the cell that holds it. Each box stays inside its
/// cell with at least 0.5 mm clearance (D8), and no two boxes overlap (TB-6): a value never sits
/// on its label.
fn title_block_boxes(sheet: &Sheet) -> Vec<(String, f64, [Vec2; 2], [Vec2; 2])> {
    let cells = title_cells(sheet);
    let [t0, t1] = sheet.title_block_rect();
    let mut out = Vec::new();
    for prim in &sheet.prims {
        let Prim::Text { at, height, angle_deg, text, anchor, .. } = prim else { continue };
        if at.x < t0.x || at.x > t1.x || at.y < t0.y || at.y > t1.y {
            continue;
        }
        assert_eq!(*angle_deg, 0.0, "title-block text {text:?} is rotated");
        let b = text_box(*at, *height, text, *anchor);
        let cell = cells.iter().find(|c| b[0].x >= c[0].x - 1e-9 && b[1].x <= c[1].x + 1e-9 && at.y >= c[0].y && at.y <= c[1].y);
        let Some(cell) = cell else { panic!("the text {text:?} at {at:?} is not in a title-block cell") };
        out.push((text.clone(), *height, b, *cell));
    }
    const CLEAR: f64 = 0.5;
    for (text, height, b, cell) in &out {
        assert!(
            b[0].x >= cell[0].x + CLEAR - 1e-9
                && b[1].x <= cell[1].x - CLEAR + 1e-9
                && b[0].y >= cell[0].y + CLEAR - 1e-9
                && b[1].y <= cell[1].y - CLEAR + 1e-9,
            "the text {text:?} ({b:?} at {height} mm) is closer than 0.5 mm to its cell {cell:?}"
        );
    }
    for (i, (ta, _, a, _)) in out.iter().enumerate() {
        for (tb, _, b, _) in out.iter().skip(i + 1) {
            let (dx, dy) = ((a[1].x.min(b[1].x) - a[0].x.max(b[0].x)), (a[1].y.min(b[1].y) - a[0].y.max(b[0].y)));
            assert!(dx <= 0.0 || dy <= 0.0, "the texts {ta:?} and {tb:?} overlap by {dx} × {dy} mm");
        }
    }
    out
}

#[test]
fn the_title_block_is_180_mm_wide_and_every_text_box_stays_in_its_cell() {
    // A long numbered name and a wide Dokumentart: the values that used to overflow.
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let mut title = TitleBlock::for_design("7-10.1_Countershaft_Pulley_Shaft");
    title.dokumentart = "Fertigungszeichnung".to_string();
    title.werkstoff = "ISO 2768-mK".to_string();
    title.allgemeintoleranz = "ISO 2768-mK".to_string();
    title.firma = "Muster GmbH".to_string();
    title.geprueft = "K. Beispielmann".to_string();
    let sheet = build_sheet(&views, &title, &NotesBlock::default(), None).unwrap();
    let [t0, t1] = sheet.title_block_rect();
    assert!((t1.x - t0.x - TITLE_BLOCK_WIDTH_MM).abs() < 1e-12, "the block is 180 mm wide: {t0:?} {t1:?}");
    assert!((t1.y - t0.y - TITLE_BLOCK_HEIGHT_MM).abs() < 1e-12);
    let boxes = title_block_boxes(&sheet);
    assert!(boxes.len() >= 20, "{boxes:?}");
    for (text, height, b, cell) in &boxes {
        let width = solidcraft_io::text_width_mm(text, *height);
        assert!((b[1].x - b[0].x - width).abs() < 1e-9, "{text:?} measures {width} mm, its box is {b:?}");
        assert!((cell[1].x - cell[0].x) >= 20.0 - 1e-9, "a title-block cell is too narrow: {cell:?}");
    }
}

#[test]
fn a_numbered_name_gives_a_cleaned_benennung_and_the_drawing_number() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let title = TitleBlock::for_design("7-10.1_Countershaft_Pulley_Shaft");
    assert_eq!(title.benennung, "Countershaft Pulley Shaft");
    assert_eq!(title.zeichnungsnummer, "7-10.1");
    let sheet = build_sheet(&views, &title, &NotesBlock::default(), None).unwrap();
    let boxes = title_block_boxes(&sheet);
    let value = |name: &str| boxes.iter().find(|(text, ..)| text == name).unwrap_or_else(|| panic!("no {name:?} value: {boxes:?}")).clone();
    let (name, height, b, cell) = value("Countershaft Pulley Shaft");
    assert!((height - 5.0).abs() < 1e-12, "the Benennung takes 5 mm (7 mm does not fit), not {height}: {name:?}");
    assert!(b[1].x - b[0].x <= cell[1].x - cell[0].x - 1.0, "{b:?} in {cell:?}");
    let (number, height, _, _) = value("7-10.1");
    assert!((height - 5.0).abs() < 1e-12, "the short drawing number takes 5 mm, not {height}: {number:?}");
    assert!(!name.contains('_') && !name.contains('\u{2026}'), "the cleaned name is not shortened: {name}");
}

#[test]
fn a_200_character_name_is_shortened_at_2_5_mm_and_never_leaves_its_cell() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let mut title = TitleBlock::for_design("Plate");
    title.benennung = "W".repeat(200);
    title.zeichnungsnummer = "8".repeat(200);
    let sheet = build_sheet(&views, &title, &NotesBlock::default(), None).unwrap();
    let boxes = title_block_boxes(&sheet);
    for prefix in ['W', '8'] {
        let (text, height, b, cell) = boxes
            .iter()
            .find(|(text, ..)| text.starts_with(prefix) && text.ends_with('\u{2026}'))
            .unwrap_or_else(|| panic!("no shortened value starting with {prefix:?}: {boxes:?}"))
            .clone();
        assert!((height - 2.5).abs() < 1e-12, "a shortened value stays at 2.5 mm, not {height}");
        let width = solidcraft_io::text_width_mm(&text, height);
        assert!(width <= cell[1].x - cell[0].x - 1.0 + 1e-9, "{width} mm in {cell:?}");
        assert!(b[1].x <= cell[1].x - 0.5 + 1e-9, "{b:?} crosses {cell:?}");
    }
}

#[test]
fn the_dxf_carries_units_layers_and_one_entity_per_primitive() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    let dxf = sheet_to_dxf(&sheet).unwrap();
    let p = pairs(&dxf);
    assert!(p.len() > 100);
    let at = p.iter().position(|(c, v)| (*c, v.as_str()) == (9, "$INSUNITS")).expect("$INSUNITS");
    assert_eq!(p.get(at + 1).map(|(c, v)| (*c, v.as_str())), Some((70, "4")), "millimetres");
    let entities = dxf_entities(&dxf);
    assert_eq!(entities.len(), sheet.prims.len(), "one DXF entity per primitive");
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, fields) in &entities {
        let layer = fields.iter().find(|(c, _)| *c == 8).map_or("", |(_, v)| v.as_str()).to_string();
        *counts.entry(layer).or_default() += 1;
    }
    assert_eq!(counts.get("FRAME"), Some(&5), "frame + four centring marks: {counts:?}");
    assert!(counts.get("VISIBLE").copied().unwrap_or(0) > 0, "{counts:?}");
    assert_eq!(counts.get("HIDDEN").copied().unwrap_or(0), 0, "a box has no hidden edges: {counts:?}");
    assert!(counts.get("THIN").copied().unwrap_or(0) > 0, "projection-symbol centre lines: {counts:?}");
    assert!(counts.get("TITLE").copied().unwrap_or(0) > 0, "{counts:?}");
    assert!(counts.get("TEXT").copied().unwrap_or(0) > 0, "{counts:?}");
    // Every layer is defined in the LAYER table, and no entity is on an undefined layer.
    let defined = layer_names(&dxf);
    for (name, _) in [("FRAME", 0), ("VISIBLE", 0), ("HIDDEN", 0), ("THIN", 0), ("TITLE", 0), ("TEXT", 0)] {
        assert!(defined.iter().any(|d| d == name), "{name} is not defined: {defined:?}");
    }
    for (kind, fields) in &entities {
        let layer = fields.iter().find(|(c, _)| *c == 8).map_or("", |(_, v)| v.as_str());
        assert!(defined.iter().any(|d| d == layer), "{kind} on undefined layer {layer:?}");
    }
    // The frame polyline is on 0.70 and its points are exactly the ISO 5457 rectangle.
    let frame = entities.iter().find_map(|(kind, fields)| {
        let layer = fields.iter().find(|(c, _)| *c == 8).map(|(_, v)| v.as_str());
        (kind.as_str() == "LWPOLYLINE" && layer == Some("FRAME")).then_some(fields)
    });
    let frame = frame.expect("frame LWPOLYLINE");
    let xs: Vec<f64> = frame.iter().filter(|(c, _)| *c == 10).map(|(_, v)| v.parse().unwrap()).collect();
    let ys: Vec<f64> = frame.iter().filter(|(c, _)| *c == 20).map(|(_, v)| v.parse().unwrap()).collect();
    assert!(xs.iter().any(|x| (*x - 20.0).abs() < 1e-9) && xs.iter().any(|x| (*x - 287.0).abs() < 1e-9), "{xs:?}");
    assert!(ys.iter().any(|y| (*y - 10.0).abs() < 1e-9) && ys.iter().any(|y| (*y - 200.0).abs() < 1e-9), "{ys:?}");
}

/// Group-code pairs of a DXF text.
fn pairs(dxf: &str) -> Vec<(i32, String)> {
    let lines: Vec<&str> = dxf.lines().collect();
    lines.chunks(2).map(|c| (c.first().and_then(|l| l.trim().parse().ok()).unwrap_or(-1), c.get(1).unwrap_or(&"").trim().to_string())).collect()
}

/// The ENTITIES section, each entity as (type, its group codes after the type).
fn dxf_entities(dxf: &str) -> Vec<(String, Vec<(i32, String)>)> {
    let p = pairs(dxf);
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "ENTITIES")).expect("ENTITIES");
    let mut out: Vec<(String, Vec<(i32, String)>)> = Vec::new();
    for (c, v) in &p[start + 1..] {
        match (*c, v.as_str()) {
            (0, "ENDSEC") => break,
            (0, kind) => out.push((kind.to_string(), Vec::new())),
            _ => {
                if let Some(last) = out.last_mut() {
                    last.1.push((*c, v.clone()));
                }
            }
        }
    }
    // The sheet furniture lives in the paper-space block, not in ENTITIES: either directly or,
    // as here, behind one block reference. Its VIEWPORT is a layout helper, not a primitive.
    for (kind, fields) in block_entities(&p, "*Paper_Space") {
        if kind == "VIEWPORT" {
            continue;
        }
        if kind == "INSERT" {
            if let Some(name) = fields.iter().find(|(c, _)| *c == 2).map(|(_, v)| v.clone()) {
                out.extend(block_entities(&p, &name));
            }
        } else {
            out.push((kind, fields));
        }
    }
    out
}

/// The entities of one named block of the BLOCKS section, as (kind, group codes).
fn block_entities(p: &[(i32, String)], name: &str) -> Vec<(String, Vec<(i32, String)>)> {
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "BLOCKS")).expect("BLOCKS");
    let mut out: Vec<(String, Vec<(i32, String)>)> = Vec::new();
    let mut block: Option<String> = None;
    let mut entity: Option<(String, Vec<(i32, String)>)> = None;
    for (c, v) in &p[start + 1..] {
        if *c == 0 && v == "ENDSEC" {
            break;
        }
        match (*c, v.as_str()) {
            (0, "BLOCK") => {
                block = Some(String::new());
            }
            (0, "ENDBLK") => {
                if block.as_deref() == Some(name)
                    && let Some(e) = entity.take()
                {
                    out.push(e);
                }
                block = None;
            }
            (0, kind) if block.as_deref() == Some(name) => {
                if let Some(e) = entity.take() {
                    out.push(e);
                }
                entity = Some((kind.to_string(), Vec::new()));
            }
            (2, value) if block.as_deref() == Some("") => block = Some(value.to_string()),
            _ => {
                if let Some((_, fields)) = entity.as_mut() {
                    fields.push((*c, v.clone()));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- PDF writer (D2c)

/// One path parsed from an uncompressed PDF content stream.
#[derive(Debug, Clone, PartialEq)]
struct PdfPath {
    points: Vec<[f64; 2]>,
    closed: bool,
    width: f64,
    dash: Vec<f64>,
}

impl PdfPath {
    /// The line segments the path paints, polylines counted per segment.
    fn segments(&self) -> usize {
        match self.points.len() {
            0 => 0,
            1 => 1, // a zero-length segment, painted as a dot
            n if self.closed => n,
            n => n - 1,
        }
    }
}

/// The bytes of object `number`'s stream, using the object's own `/Length`.
fn object_stream(pdf: &[u8], number: usize) -> Vec<u8> {
    let marker = format!("{number} 0 obj\n");
    let start = pdf.windows(marker.len()).position(|w| w == marker.as_bytes()).expect("object");
    let body = &pdf[start..];
    let length_at = body.windows(8).position(|w| w == b"/Length ").expect("length") + 8;
    let length_end = body[length_at..].iter().position(|b| *b == b' ').expect("length end") + length_at;
    let length: usize = std::str::from_utf8(&body[length_at..length_end]).unwrap().parse().unwrap();
    let data = body.windows(7).position(|w| w == b"stream\n").expect("stream") + 7;
    assert_eq!(&body[data + length..data + length + 9], b"endstream");
    body[data..data + length].to_vec()
}

fn pdf_content(pdf: &[u8]) -> String {
    String::from_utf8_lossy(&object_stream(pdf, 4)).into_owned()
}

/// Parse the path objects of the writer's line-per-operator content stream. Text blocks are
/// skipped as a whole, so their strings cannot be mistaken for operators.
fn pdf_paths(content: &str) -> Vec<PdfPath> {
    let mut out = Vec::new();
    let mut current: Option<PdfPath> = None;
    let (mut width, mut dash) = (0.0, Vec::new());
    let mut in_text = false;
    for line in content.lines().map(str::trim) {
        if line == "BT" {
            in_text = true;
            continue;
        }
        if line == "ET" {
            in_text = false;
            continue;
        }
        if in_text || line.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&operator) = tokens.last() else { continue };
        match operator {
            "w" if tokens.len() == 2 => width = tokens[0].parse().unwrap(),
            "d" if tokens.len() >= 3 && tokens[0] == "[" => {
                let inner = &tokens[1..tokens.len() - 2];
                dash = if inner.len() == 1 && inner[0] == "]" { Vec::new() } else { inner.iter().map(|t| t.parse().unwrap()).collect() };
            }
            "m" if tokens.len() == 3 => {
                current = Some(PdfPath {
                    points: vec![[tokens[0].parse().unwrap(), tokens[1].parse().unwrap()]],
                    closed: false,
                    width,
                    dash: dash.clone(),
                });
            }
            "l" if tokens.len() == 3 => {
                if let Some(path) = current.as_mut() {
                    path.points.push([tokens[0].parse().unwrap(), tokens[1].parse().unwrap()]);
                }
            }
            "h" => {
                if let Some(path) = current.as_mut() {
                    path.closed = true;
                }
            }
            "S" | "s" | "f" => {
                if let Some(path) = current.take() {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
    out
}

/// The length a primitive paints; `None` for text.
fn prim_length(prim: &Prim) -> Option<f64> {
    match prim {
        Prim::Line { a, b, .. } => Some(a.dist(*b)),
        Prim::Polyline { points, closed, .. } => (!points.is_empty()).then(|| path_length(points, *closed)),
        Prim::Arc { r, a0, a1, .. } => Some(r * (a1 - a0).abs()),
        Prim::Circle { r, .. } => Some(TAU * r),
        Prim::Ellipse { major, ratio, t0, t1, .. } => {
            let (a, b) = (major.len(), major.len() * ratio);
            let h = if a + b > 0.0 { ((a - b) / (a + b)).powi(2) } else { 0.0 };
            Some(std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt())) * ((t1 - t0).abs() / TAU))
        }
        Prim::Filled { .. } | Prim::Text { .. } => None,
    }
}

/// The segments a primitive paints as a path; `None` for text.
fn prim_segments(prim: &Prim) -> Option<usize> {
    match prim {
        Prim::Line { .. } => Some(1),
        Prim::Polyline { points, closed, .. } => {
            (!points.is_empty()).then(|| if points.len() == 1 || *closed { points.len() } else { points.len() - 1 })
        }
        // One analytic entity is one segment on both sides (the PDF path parser sees its `m`).
        Prim::Arc { .. } | Prim::Circle { .. } | Prim::Ellipse { .. } => Some(1),
        Prim::Filled { .. } | Prim::Text { .. } => None,
    }
}

#[test]
fn the_pdf_of_the_box_measures_true_scale_and_matches_the_dxf_per_layer() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    assert_eq!(sheet.scale, (2, 1));

    let page = sheet_to_pdf(&sheet).unwrap();
    assert_eq!((page.width_mm, page.height_mm), (297.0, 210.0), "page = A4 landscape paper");
    assert_eq!(page.items.len(), sheet.prims.len(), "one PDF item per primitive");
    let pdf = solidcraft_io::write_pdf(&page).unwrap();
    let file = String::from_utf8_lossy(&pdf);
    assert!(pdf.starts_with(b"%PDF-1.4\n%"));
    assert!(file.contains("/MediaBox [0 0 841.89 595.28]"), "297 × 210 mm in points");
    let content = pdf_content(&pdf);
    let paths = pdf_paths(&content);

    // Every path carries its layer's pen: width, dash and segment count, in sheet order.
    let expected: Vec<(Layer, usize)> = sheet.prims.iter().filter_map(|p| prim_segments(p).map(|n| (p.layer(), n))).collect();
    assert_eq!(paths.len(), expected.len(), "one PDF path per line/polyline primitive");
    let lengths: Vec<f64> = sheet.prims.iter().filter_map(prim_length).collect();
    for ((path, (layer, segments)), length) in paths.iter().zip(&expected).zip(&lengths) {
        assert_eq!(path.segments(), *segments, "{layer:?}");
        assert_eq!(path.width, layer.pen().width_mm, "{layer:?} width");
        assert_eq!(path.dash, pdf_dash(layer.pen().line_kind, *length), "{layer:?} dash");
    }

    // The frame is the D2a rectangle, stroked 0.7 mm: 20 mm from the left and 10 mm elsewhere.
    let frame = &paths[0];
    assert!(frame.closed, "the frame is a closed polyline");
    assert_eq!(frame.width, 0.7);
    let xs: Vec<f64> = frame.points.iter().map(|p| p[0]).collect();
    let ys: Vec<f64> = frame.points.iter().map(|p| p[1]).collect();
    let (min_x, max_x) = (xs.iter().copied().fold(f64::INFINITY, f64::min), xs.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    let (min_y, max_y) = (ys.iter().copied().fold(f64::INFINITY, f64::min), ys.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    assert!((min_x - 20.0).abs() < 1e-9 && (max_x - 287.0).abs() < 1e-9, "frame x {min_x}..{max_x}");
    assert!((min_y - 10.0).abs() < 1e-9 && (max_y - 200.0).abs() < 1e-9, "frame y {min_y}..{max_y}");

    // The front view's visible outline spans 40 mm of model at 2:1 = 80 mm in the content stream.
    let front = placed(&sheet, StandardView::Front);
    let inside = |p: &[f64; 2]| {
        p[0] >= front.bbox[0].x - 1e-9 && p[0] <= front.bbox[1].x + 1e-9 && p[1] >= front.bbox[0].y - 1e-9 && p[1] <= front.bbox[1].y + 1e-9
    };
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lo_y, mut hi_y) = (f64::INFINITY, f64::NEG_INFINITY);
    for (path, (layer, _)) in paths.iter().zip(&expected) {
        if *layer == Layer::Visible && path.points.iter().all(inside) {
            for p in &path.points {
                lo = lo.min(p[0]);
                hi = hi.max(p[0]);
                lo_y = lo_y.min(p[1]);
                hi_y = hi_y.max(p[1]);
            }
        }
    }
    let scale = 2.0;
    assert!((hi - lo - 40.0 * scale).abs() <= 0.01, "front outline width {} mm, expected 80", hi - lo);
    assert!((hi_y - lo_y - 10.0 * scale).abs() <= 0.01, "front outline height {} mm, expected 20", hi_y - lo_y);

    // Same number of line segments per layer as the DXF of the same sheet.
    let dxf = sheet_to_dxf(&sheet).unwrap();
    let entities = dxf_entities(&dxf);
    assert_eq!(entities.len(), sheet.prims.len(), "one DXF entity per primitive");
    let mut dxf_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, fields) in &entities {
        let layer = fields.iter().find(|(c, _)| *c == 8).map_or("", |(_, v)| v.as_str()).to_string();
        let segments = match kind.as_str() {
            "LINE" => 1,
            "LWPOLYLINE" => fields.iter().filter(|(c, _)| *c == 10).count().saturating_sub(1),
            "ARC" | "CIRCLE" | "ELLIPSE" => 1,
            _ => 0,
        };
        *dxf_counts.entry(layer).or_default() += segments;
    }
    // Lettering is not a line segment on either side.
    dxf_counts.retain(|_, segments| *segments > 0);
    let mut pdf_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (path, (layer, _)) in paths.iter().zip(&expected) {
        *pdf_counts.entry(layer.dxf_name().to_string()).or_default() += path.segments();
    }
    assert_eq!(pdf_counts, dxf_counts, "PDF and DXF segment counts per layer");
    // A box: the frame (4 + 4 centring marks), 12 visible edges (3 views × 4), no hidden edges.
    assert_eq!(pdf_counts.get("FRAME"), Some(&8));
    assert_eq!(pdf_counts.get("VISIBLE"), Some(&12));
    assert!(!pdf_counts.contains_key("HIDDEN"));
    assert!(pdf_counts.get("TITLE").copied().unwrap_or(0) > 0);
    assert!(pdf_counts.get("THIN").copied().unwrap_or(0) > 0);
}

#[test]
fn a_non_finite_or_empty_sheet_primitive_is_an_error_not_a_panic() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let mut sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    let line = sheet.prims.iter().position(|p| matches!(p, Prim::Line { .. })).unwrap();
    if let Prim::Line { a, .. } = &mut sheet.prims[line] {
        a.x = f64::NAN;
    }
    assert!(sheet_to_pdf(&sheet).is_err(), "a NaN point must be refused");
    let mut sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    let text = sheet.prims.iter().position(|p| matches!(p, Prim::Text { .. })).unwrap();
    if let Prim::Text { height, .. } = &mut sheet.prims[text] {
        *height = 0.0;
    }
    assert!(sheet_to_pdf(&sheet).is_err(), "a zero text height must be refused");
    // An empty polyline is dropped, exactly as the DXF writer drops it.
    let mut sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    sheet.prims.push(Prim::Polyline { points: Vec::new(), closed: true, layer: Layer::Thin });
    let before = sheet.prims.len();
    let page = sheet_to_pdf(&sheet).unwrap();
    assert_eq!(page.items.len(), before - 1);
}

#[test]
fn the_pdf_text_follows_the_prim_anchors_heights_and_angles() {
    use solidcraft_io::{PdfItem, TextAnchor as PdfTextAnchor};

    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    let page = sheet_to_pdf(&sheet).unwrap();
    let prims: Vec<&Prim> = sheet.prims.iter().filter(|p| matches!(p, Prim::Text { .. })).collect();
    let items: Vec<&PdfItem> = page.items.iter().filter(|i| matches!(i, PdfItem::Text { .. })).collect();
    assert_eq!(prims.len(), items.len(), "one PDF text item per text primitive");
    assert!(prims.len() >= 29, "{prims:?}");
    for (prim, item) in prims.iter().zip(&items) {
        let Prim::Text { at, height, angle_deg, text, anchor, .. } = prim else { continue };
        let PdfItem::Text { at: pdf_at, height_mm, angle_deg: pdf_angle, text: pdf_text, anchor: pdf_anchor, bold } = item else {
            continue;
        };
        assert_eq!(*pdf_at, [at.x, at.y], "{text:?}");
        assert_eq!(*height_mm, *height, "{text:?}");
        assert_eq!(pdf_angle, angle_deg, "{text:?}");
        assert_eq!(pdf_text, text);
        assert!(!bold, "the sheet uses no bold lettering");
        let expected = match anchor {
            TextAnchor::Left => PdfTextAnchor::BottomLeft,
            TextAnchor::Center => PdfTextAnchor::BottomCenter,
            TextAnchor::Right => PdfTextAnchor::BottomRight,
        };
        assert_eq!(*pdf_anchor, expected, "{text:?}");
    }
}

/// AutoCAD's `\U+XXXX` escape (AC1015 has no encoding flag) back to the letters the PDF draws.
fn decode_dxf_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("\\U+") {
        out.push_str(&rest[..at]);
        let code = rest.get(at + 3..at + 7).and_then(|hex| u32::from_str_radix(hex, 16).ok());
        match (code, char::from_u32(code.unwrap_or(0))) {
            (Some(value), Some(c)) if value >= 0x20 => out.push(c),
            _ => out.push('\u{fffd}'),
        }
        rest = rest.get(at + 7..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

#[test]
fn every_dxf_text_aligns_on_the_pdf_anchor() {
    use solidcraft_io::{PdfItem, TextAnchor as PdfTextAnchor};

    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let notes = NotesBlock { extra_notes: vec!["Bohrung \u{00d8}6 durchgehend".to_string()], ..NotesBlock::default() };
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &notes, None).unwrap();
    let dxf = sheet_to_dxf(&sheet).unwrap();
    let page = sheet_to_pdf(&sheet).unwrap();

    // Every DXF TEXT: its string, its alignment kind (group 72) and its alignment point — group
    // 10 for left text, group 11 for centred and right text.
    let number = |fields: &[(i32, String)], code: i32| fields.iter().find(|(c, _)| *c == code).and_then(|(_, v)| v.parse::<f64>().ok());
    let mut dxf_texts: Vec<(String, f64, Vec2)> = Vec::new();
    for (kind, fields) in dxf_entities(&dxf) {
        if kind != "TEXT" {
            continue;
        }
        let Some(text) = fields.iter().find(|(c, _)| *c == 1).map(|(_, v)| decode_dxf_escapes(v)) else { continue };
        let halign = number(&fields, 72).unwrap_or(0.0);
        let point_code = if halign == 0.0 { 10 } else { 11 };
        let point = Vec2::new(number(&fields, point_code).unwrap_or(f64::NAN), number(&fields, point_code + 10).unwrap_or(f64::NAN));
        dxf_texts.push((text, halign, point));
    }
    let pdf_texts: Vec<(&str, PdfTextAnchor, Vec2)> = page
        .items
        .iter()
        .filter_map(|item| match item {
            PdfItem::Text { at, text, anchor, .. } => Some((text.as_str(), *anchor, Vec2::new(at[0], at[1]))),
            _ => None,
        })
        .collect();
    assert_eq!(dxf_texts.len(), pdf_texts.len(), "one DXF TEXT per PDF text item: {} vs {}", dxf_texts.len(), pdf_texts.len());
    // Match each DXF text with the PDF item of the same string and anchor. Within 0.05 mm the
    // alignment point is the PDF writer's text origin; a viewer's font width cannot move it.
    let mut used = vec![false; pdf_texts.len()];
    for (text, halign, point) in &dxf_texts {
        let want_halign = |anchor: PdfTextAnchor| match anchor {
            PdfTextAnchor::BottomLeft | PdfTextAnchor::MiddleLeft | PdfTextAnchor::TopLeft => 0.0,
            PdfTextAnchor::BottomCenter | PdfTextAnchor::Center | PdfTextAnchor::TopCenter => 1.0,
            PdfTextAnchor::BottomRight | PdfTextAnchor::MiddleRight | PdfTextAnchor::TopRight => 2.0,
        };
        let found = pdf_texts
            .iter()
            .enumerate()
            .position(|(i, (pdf_text, anchor, at))| !used[i] && *pdf_text == text && want_halign(*anchor) == *halign && at.dist(*point) <= 0.05);
        let Some(i) = found else {
            panic!("{text:?} (halign {halign}) at {point:?} has no PDF anchor within 0.05 mm: {pdf_texts:?}");
        };
        used[i] = true;
    }
}

/// The LAYER table's 370 values (hundredths of a millimetre), by decoded layer name.
fn layer_lineweights(dxf: &str) -> BTreeMap<String, i16> {
    let p = pairs(dxf);
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "LAYER")).expect("LAYER table");
    let mut out = BTreeMap::new();
    let mut name: Option<String> = None;
    for (c, v) in &p[start + 1..] {
        match (*c, v.as_str()) {
            (0, "ENDTAB") => break,
            (0, "LAYER") => name = None,
            (2, decoded) => name = Some(decode_dxf_escapes(decoded)),
            (370, value) => {
                if let (Some(name), Ok(value)) = (name.take(), value.parse::<i16>()) {
                    out.insert(name, value);
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn the_dxf_layer_lineweights_are_the_pdf_pen_widths() {
    use solidcraft_io::PdfItem;

    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let sheet = build_sheet(&views, &TitleBlock::for_design("Plate"), &NotesBlock::default(), None).unwrap();
    let dxf = sheet_to_dxf(&sheet).unwrap();
    let page = sheet_to_pdf(&sheet).unwrap();
    assert_eq!(sheet.prims.len(), page.items.len(), "one PDF item per primitive");

    // The width the PDF writer strokes each layer's prims with, taken from the PDF items
    // themselves (text is filled, not stroked: width 0).
    let mut widths: BTreeMap<String, f64> = BTreeMap::new();
    for (prim, item) in sheet.prims.iter().zip(&page.items) {
        let width = match item {
            PdfItem::Path { width_mm, .. } => *width_mm,
            PdfItem::EllipseArc { width_mm, .. } => *width_mm,
            PdfItem::Text { .. } => 0.0,
        };
        assert!((width - prim.layer().width_mm()).abs() < 1e-12, "{:?}: PDF {width} mm, pen {} mm", prim.layer(), prim.layer().width_mm());
        let name = prim.layer().dxf_name().to_string();
        let entry = widths.entry(name).or_insert(width);
        assert!((*entry - width).abs() < 1e-12, "one width per layer");
    }
    // The DXF layer names the same width, in hundredths of a millimetre. Lettering's zero-width
    // pen becomes the thin 0.25 mm, the width every ISO sheet would print it in.
    let lineweights = layer_lineweights(&dxf);
    let pens: BTreeMap<&str, f64> = PENS.iter().map(|(_, pen)| (pen.dxf_name, pen.width_mm)).collect();
    for (name, hundredths) in &lineweights {
        assert!(solidcraft_io::DxfLineweight::STANDARD.contains(hundredths), "layer {name}: {hundredths} is not a standard lineweight");
        // What the PDF says for the layer where it has geometry, else the layer's own pen (a
        // section layer of an unsectioned sheet still carries its width), else the thin width.
        let pdf = widths.get(name).copied();
        let expected = match pdf {
            Some(width) => {
                if width > 0.0 {
                    width * 100.0
                } else {
                    25.0
                }
            }
            None => match pens.get(name.as_str()) {
                Some(width) if *width > 0.0 => width * 100.0,
                _ => 25.0,
            },
        };
        assert!((f64::from(*hundredths) - expected).abs() <= 0.5, "layer {name}: DXF {hundredths} vs PDF/pen {expected} hundredths");
    }
}

/// The names in the LAYER table.
fn layer_names(dxf: &str) -> Vec<String> {
    let p = pairs(dxf);
    let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "LAYER")).expect("LAYER table");
    let mut names = Vec::new();
    let mut record = false;
    for (c, v) in &p[start + 1..] {
        match (*c, v.as_str()) {
            (0, "ENDTAB") => break,
            (0, "LAYER") => record = true,
            (2, name) if record => {
                names.push(name.to_string());
                record = false;
            }
            _ => {}
        }
    }
    names
}

// ------------------------------------------------------------------ D8: title block and notes

#[test]
fn design_names_clean_underscores_and_split_the_drawing_number() {
    assert_eq!(design_names("7-10.1_Countershaft_Pulley_Shaft"), ("Countershaft Pulley Shaft".to_string(), "7-10.1".to_string()));
    assert_eq!(design_names("7-10.1-Countershaft"), ("Countershaft".to_string(), "7-10.1".to_string()));
    assert_eq!(design_names("2026-10-24_Part"), ("Part".to_string(), "2026-10-24".to_string()));
    // No leading drawing-number token: the cleaned name is both the Benennung and the number.
    assert_eq!(design_names("shaft"), ("shaft".to_string(), "shaft".to_string()));
    assert_eq!(design_names("AB 12-3 Welle"), ("AB 12-3 Welle".to_string(), "AB 12-3 Welle".to_string()));
    assert_eq!(design_names("my_part"), ("my part".to_string(), "my part".to_string()));
    assert_eq!(design_names(""), ("Untitled".to_string(), "Untitled".to_string()));
}

#[test]
fn the_design_properties_win_over_the_cleaned_name() {
    let mut s = boxed(40.0, 20.0, 10.0);
    s.doc_mut().name = "Plate".to_string();
    let defaults = TitleBlock::for_document(s.doc.as_ref(), None);
    assert_eq!(defaults.benennung, "Plate");
    assert_eq!(defaults.zeichnungsnummer, "Plate");
    assert_eq!(defaults.werkstoff, "n. b.", "a box has no material");
    s.execute(
        "document.metadata.set",
        &json!({"values": {
            "description": "Ventilated brake disc", "part_number": "BR-77", "author": "Jane Doe",
            "organization": "ACME", "revision": "B"
        }}),
    )
    .unwrap();
    let title = TitleBlock::for_document(s.doc.as_ref(), None);
    assert_eq!(title.benennung, "Ventilated brake disc");
    assert_eq!(title.zeichnungsnummer, "BR-77");
    assert_eq!(title.gezeichnet, "Jane Doe");
    assert_eq!(title.firma, "ACME");
    assert_eq!(title.aend, "B");
    // Only one of them set: the other still comes from the name.
    s.execute("document.metadata.remove", &json!({"keys": ["description"]})).unwrap();
    s.execute("document.metadata.set", &json!({"values": {"part_number": "7-10.1"}})).unwrap();
    s.doc_mut().name = "7-10.1_Countershaft_Pulley_Shaft".to_string();
    let title = TitleBlock::for_document(s.doc.as_ref(), None);
    assert_eq!(title.benennung, "Countershaft Pulley Shaft");
    assert_eq!(title.zeichnungsnummer, "7-10.1");
}

#[test]
fn the_material_of_the_body_reaches_the_title_block() {
    let mut s = boxed(40.0, 20.0, 10.0);
    s.execute("material.assign", &json!({"bodies": ["Plate"], "material": "Steel"})).unwrap();
    let title = TitleBlock::for_document(s.doc.as_ref(), None);
    assert_eq!(title.werkstoff, "Steel");
    s.execute("material.assign", &json!({"bodies": ["Plate"], "material": "Default"})).unwrap();
    assert_eq!(TitleBlock::for_document(s.doc.as_ref(), None).werkstoff, "n. b.");
}

/// A turned part about X: Ø30 over 50 mm, then Ø20 over 80 mm, 130 mm overall.
fn stepped_shaft() -> Session {
    let mut s = Session::default();
    s.execute("sketch.create", &json!({"plane":"XY","name":"Profile"})).unwrap();
    s.execute("sketch.line", &json!({"points":[[0,0],[0,15],[50,15],[50,10],[130,10],[130,0]],"closed":true})).unwrap();
    s.execute("sketch.finish", &json!({})).unwrap();
    s.execute("solid.revolve", &json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})).unwrap();
    s
}

#[test]
fn the_raw_size_comes_from_the_exact_geometry() {
    let shaft = stepped_shaft();
    assert_eq!(rohmass(&shaft), Some("\u{00d8}30 x 130".to_string()), "a turned part states its largest diameter and its length");
    let plate = boxed(80.0, 40.0, 10.0);
    assert_eq!(rohmass(&plate), Some("80 x 40 x 10".to_string()), "a flat part states L x W x H, largest first");
    let odd = boxed(80.04, 40.0, 9.96);
    assert_eq!(rohmass(&odd), Some("80,1 x 40 x 10".to_string()), "each value rounds up to 0.1 mm with a decimal comma");
    assert_eq!(rohmass(&Session::default()), None, "nothing visible, no raw size");
    // The title block takes the computed value as its default and an override wins.
    assert_eq!(TitleBlock::for_document(plate.doc.as_ref(), rohmass(&plate)).rohmass, "80 x 40 x 10");
}

#[test]
fn the_stored_drawing_values_come_back_over_the_design_defaults() {
    let mut s = boxed(40.0, 20.0, 10.0);
    s.execute(
        "document.metadata.set",
        &json!({"values": {
            "description": "Welle", "part_number": "7-10.1", "author": "K. Beispielmann",
            "organization": "Muster AG", "revision": "B",
            "app.solidcraft.drawing": {
                "werkstoff": "S235JR", "geprueft": "A. Pruefer", "geprueft_datum": "23.10.2026",
                "rohmass": "\u{00d8}28 x 120", "dokumentart": "Fertigungszeichnung",
                "allgemeintoleranz": "ISO 2768-m", "ra": "1,6", "edge_note": "",
                "extra_notes": ["A", "B"], "aend_datum": "23.10.2026"
            }
        }}),
    )
    .unwrap();
    let title = stored_title(s.doc.as_ref(), rohmass(&s));
    assert_eq!(title.benennung, "Welle");
    assert_eq!(title.zeichnungsnummer, "7-10.1");
    assert_eq!(title.gezeichnet, "K. Beispielmann");
    assert_eq!(title.firma, "Muster AG");
    assert_eq!(title.aend, "B");
    assert_eq!(title.werkstoff, "S235JR");
    assert_eq!(title.geprueft, "A. Pruefer");
    assert_eq!(title.geprueft_datum, "23.10.2026");
    assert_eq!(title.rohmass, "\u{00d8}28 x 120");
    assert_eq!(title.allgemeintoleranz, "ISO 2768-m");
    assert_eq!(title.aend_datum, "23.10.2026");
    let notes = stored_notes(s.doc.as_ref());
    assert_eq!(notes.ra, "1,6");
    assert_eq!(notes.edge_note, "", "a deliberately empty edge note stays empty");
    assert_eq!(notes.extra_notes, ["A", "B"]);
    assert!(notes.enabled);
}

#[test]
fn the_general_tolerance_line_names_the_k_source_only_for_an_iso_2768_class() {
    assert_eq!(general_tolerance_note("ISO 2768-mK"), "Allgemeintoleranzen ISO 2768-m, K nach EN ISO 22081:2021");
    assert_eq!(general_tolerance_note("ISO 2768-m"), "Allgemeintoleranzen ISO 2768-m");
    assert_eq!(general_tolerance_note("ISO 2768-f"), "Allgemeintoleranzen ISO 2768-f");
    assert_eq!(general_tolerance_note("DIN 7168-m"), "DIN 7168-m", "not an ISO 2768 class: written as given");
    assert_eq!(general_tolerance_note(""), "");
}

/// All thin lines above the title block: the notes block's surface symbol.
fn surface_symbol_lines(sheet: &Sheet) -> Vec<(Vec2, Vec2)> {
    let [_, tb1] = sheet.title_block_rect();
    sheet
        .prims
        .iter()
        .filter_map(|prim| match prim {
            Prim::Line { a, b, layer: Layer::Thin } if a.y > tb1.y && b.y > tb1.y => Some((*a, *b)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_notes_block_states_the_defaults_and_draws_the_iso_21920_symbol() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let title = TitleBlock::for_design("Plate");
    let notes = NotesBlock::default();
    let sheet = build_sheet(&views, &title, &notes, None).unwrap();
    let texts = text_values(&sheet);
    for expected in [
        "Allgemeintoleranzen ISO 2768-m, K nach EN ISO 22081:2021",
        "Projektionsmethode 1",
        "Werkst\u{00fc}ckkanten ISO 13715: -0,2",
        "Oberfl\u{00e4}che Ra 3,2",
        "Ra 3,2",
    ] {
        assert!(texts.iter().any(|text| text == expected), "missing note {expected:?}: {texts:?}");
    }
    // The block sits left-aligned on the title block and its height is excluded from the drawing.
    let [tb0, tb1] = sheet.title_block_rect();
    let [n0, n1] = sheet.notes_block_rect();
    assert!((n0.x - tb0.x).abs() < 1e-12 && (n1.x - tb1.x).abs() < 1e-12, "{n0:?} {n1:?}");
    assert!((n0.y - tb1.y).abs() < 1e-12, "directly above the title block: {n0:?} {tb1:?}");
    assert!(n1.y - n0.y > 0.0);
    for view in &sheet.views {
        assert!(
            !(view.bbox[1].x > n0.x && view.bbox[0].x < n1.x && view.bbox[1].y > n0.y && view.bbox[0].y < n1.y),
            "{view:?} crosses the notes block"
        );
    }
    // EN ISO 21920-1: H1 = 1.4 h = 4.9 and H2 = 2 H1 = 9.8 at 60° to the surface: the short leg
    // is 5.657 mm long at 120°, the long leg 11.314 mm at 60°; the removal bar is horizontal at
    // height H1 above the vertex. Each symbol appears twice (the removal symbol and the
    // bracketed basic symbol).
    let mut short = 0;
    let mut long = 0;
    let mut bars = 0;
    for (a, b) in surface_symbol_lines(&sheet) {
        let d = b - a;
        let length = d.len();
        let angle = d.y.atan2(d.x).to_degrees();
        if (length - 5.657).abs() < 0.01 && (angle.abs() - 120.0).abs() < 0.1 {
            short += 1;
        } else if (length - 11.314).abs() < 0.01 && (angle - 60.0).abs() < 0.1 {
            long += 1;
        } else if d.y.abs() < 1e-9 && length > 10.0 {
            bars += 1;
        }
    }
    assert!(short >= 2, "the 60° short legs (H1 = 4.9) are missing: {short}");
    assert!(long >= 2, "the 60° long legs (H2 = 9.8) are missing: {long}");
    assert!(bars >= 1, "the material-removal bar is missing");
}

#[test]
fn empty_note_lines_are_left_out_without_a_gap() {
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front]);
    let title = TitleBlock::for_design("Plate");
    let notes = NotesBlock {
        enabled: true,
        ra: "1,6".to_string(),
        edge_note: String::new(),
        extra_notes: vec!["Fase 1 x 45\u{00b0}".to_string(), String::new(), "Entgraten".to_string()],
    };
    let sheet = build_sheet(&views, &title, &notes, None).unwrap();
    let texts = text_values(&sheet);
    assert!(!texts.iter().any(|text| text.contains("Werkst\u{00fc}ckkanten")), "the empty edge note is left out: {texts:?}");
    assert!(texts.iter().any(|text| text == "Oberfl\u{00e4}che Ra 1,6"));
    assert!(texts.iter().any(|text| text == "Fase 1 x 45\u{00b0}"));
    assert!(texts.iter().any(|text| text == "Entgraten"));
    // The lines below the surface line keep one 5.25 mm pitch across the empty line.
    let baselines: Vec<f64> = sheet
        .prims
        .iter()
        .filter_map(|prim| match prim {
            Prim::Text { at, text, .. } if text == "Fase 1 x 45\u{00b0}" || text == "Entgraten" => Some(at.y),
            _ => None,
        })
        .collect();
    assert_eq!(baselines.len(), 2, "{texts:?}");
    assert!((baselines[0] - baselines[1] - NOTES_LINE_PITCH_MM).abs() < 1e-9, "{baselines:?}");
    // Off means no block at all.
    let off = build_sheet(&views, &title, &NotesBlock { enabled: false, ..NotesBlock::default() }, None).unwrap();
    assert!((off.notes_block_rect()[0].y - off.notes_block_rect()[1].y).abs() < 1e-12);
}

#[test]
fn the_notes_block_is_excluded_from_the_drawing_area() {
    // A part tall enough to need the room: with the block it takes a smaller scale (or fails),
    // without it it fits larger.
    let s = boxed(40.0, 20.0, 10.0);
    let views = projected(&s, &[StandardView::Front, StandardView::Top, StandardView::Right]);
    let title = TitleBlock::for_design("Plate");
    let with = build_sheet(&views, &title, &NotesBlock::default(), None).unwrap();
    let without = build_sheet(&views, &title, &NotesBlock { enabled: false, ..NotesBlock::default() }, None).unwrap();
    assert!(without.scale.0 * with.scale.1 >= with.scale.0 * without.scale.1, "{:?} vs {:?}", with.scale, without.scale);
    for view in &with.views {
        assert!(clear_of_title_block(&with, view.bbox));
        assert!(clear_of_notes_block(&with, view.bbox), "{view:?} crosses the notes block");
    }
}

fn clear_of_notes_block(sheet: &Sheet, b: [Vec2; 2]) -> bool {
    let [n0, n1] = sheet.notes_block_rect();
    !(b[1].x > n0.x && b[0].x < n1.x && b[1].y > n0.y && b[0].y < n1.y)
}

/// The four acceptance parts: no view, dimension, leader or note text may cross the notes block
/// (D8). Run with `SOLIDCRAFT_D8_FIXTURES=1`; the fixtures live outside the repository.
#[test]
fn the_four_acceptance_sheets_keep_the_notes_block_clear() {
    if std::env::var_os("SOLIDCRAFT_D8_FIXTURES").is_none() {
        return;
    }
    let parts = [
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/shaft.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/crank.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/flywheel.solidcraft"),
    ];
    for path in parts {
        let mut s = Session::default();
        s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
        let title = stored_title(s.doc.as_ref(), rohmass(&s));
        let notes = stored_notes(s.doc.as_ref());
        let (plan, _) =
            crate::drawing::plan_sheet_auto(&s, None, 0.01, &title, &notes, None, &Annotations::default()).unwrap_or_else(|e| panic!("{path}: {e}"));
        let sheet = &plan.sheet;
        let [n0, n1] = sheet.notes_block_rect();
        assert!(n1.y - n0.y > 20.0, "{path}: the notes block is missing");
        for view in &sheet.views {
            assert!(
                !(view.bbox[1].x > n0.x && view.bbox[0].x < n1.x && view.bbox[1].y > n0.y && view.bbox[0].y < n1.y),
                "{path}: the {:?} view crosses the notes block",
                view.view
            );
        }
        for prim in &sheet.prims[sheet.base_prims..] {
            match prim {
                Prim::Line { a, b, .. } => {
                    assert!(!crate::drawing::dims::segment_hits_rect(*a, *b, n0, n1), "{path}: a line crosses the notes block: {a:?}→{b:?}")
                }
                Prim::Polyline { points, .. } => {
                    for pair in points.windows(2) {
                        if let [a, b] = pair {
                            assert!(!crate::drawing::dims::segment_hits_rect(*a, *b, n0, n1), "{path}: a polyline crosses the notes block");
                        }
                    }
                }
                Prim::Arc { c, r, a0, a1, .. } => {
                    let steps = (((a1 - a0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
                    for i in 0..steps {
                        let point = |j: usize| *c + Vec2::from_angle(a0 + (a1 - a0) * j as f64 / steps as f64) * *r;
                        assert!(!crate::drawing::dims::segment_hits_rect(point(i), point(i + 1), n0, n1), "{path}: an arc crosses the notes block");
                    }
                }
                Prim::Circle { c, r, .. } => {
                    let point = |j: usize| *c + Vec2::from_angle(TAU * j as f64 / 32.0) * *r;
                    for i in 0..32 {
                        assert!(!crate::drawing::dims::segment_hits_rect(point(i), point(i + 1), n0, n1), "{path}: a circle crosses the notes block");
                    }
                }
                Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
                    let minor = major.perp() * *ratio;
                    let steps = (((t1 - t0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
                    let point = |j: usize| {
                        let t = t0 + (t1 - t0) * j as f64 / steps as f64;
                        *c + *major * t.cos() + minor * t.sin()
                    };
                    for i in 0..steps {
                        assert!(
                            !crate::drawing::dims::segment_hits_rect(point(i), point(i + 1), n0, n1),
                            "{path}: an ellipse crosses the notes block"
                        );
                    }
                }
                Prim::Text { at, height, text, .. } => {
                    let box_hi = Vec2::new(at.x + solidcraft_io::text_width_mm(text, *height), at.y + *height);
                    assert!(!(box_hi.x > n0.x && at.x < n1.x && box_hi.y > n0.y && at.y < n1.y), "{path}: the text {text:?} crosses the notes block");
                }
                Prim::Filled { .. } => {}
            }
        }
        // The block's own lines are present on the sheet.
        let texts = text_values(sheet);
        assert!(texts.iter().any(|t| t == "Projektionsmethode 1"), "{path}: {texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("Oberfl\u{00e4}che Ra ")), "{path}: {texts:?}");
    }
}

// --------------------------------------------------------------- D10: analytic curves (D10)

/// The distance from `p` to a segment `a`→`b`.
fn point_segment(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.len2();
    if !(len2 > 0.0) {
        return p.dist(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.dist(a + d * t)
}

fn distance_to_segments(p: Vec2, segments: &[crate::drawing::Seg2mm]) -> f64 {
    segments.iter().map(|s| point_segment(p, s.a, s.b)).fold(f64::INFINITY, f64::min)
}

/// A 40 × 20 × 6 plate with a Ø10 through hole along Y, and a second plate in front of the
/// hole's left half.
fn plate_with_a_covered_hole() -> Session {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":6,"height":20,"center":[0,0,0],"body_name":"Plate"})).unwrap();
    s.execute("solid.hole", &json!({"position":[0,-3,0],"direction":[0,1,0],"diameter":10,"through":true,"targets":["Plate"]})).unwrap();
    s.execute("solid.box", &json!({"length":20,"width":6,"height":20,"center":[-10,-10,0],"body_name":"Cover"})).unwrap();
    s
}

fn projected_curves(view: &ProjectedView) -> Vec<(crate::drawing::Curve2mm, bool)> {
    view.visible_curves.iter().map(|c| (*c, false)).chain(view.hidden_curves.iter().map(|c| (*c, true))).collect()
}

#[test]
fn a_hidden_split_keeps_the_circle_as_visible_and_hidden_arcs() {
    let s = plate_with_a_covered_hole();
    let views = crate::drawing::project_views(
        &s,
        &[StandardView::Front],
        &ViewOptions { tolerance: 0.01, hidden: true, tangent: crate::drawing::TangentEdges::Omit, curves: true },
    )
    .unwrap();
    let view = &views[0];
    let mut visible_sweep = 0.0;
    let mut hidden_sweep = 0.0;
    let mut visible_x: Vec<f64> = Vec::new();
    let mut hidden_x: Vec<f64> = Vec::new();
    for (curve, hidden) in projected_curves(view) {
        let (c, r, start, end) = match curve {
            crate::drawing::Curve2mm::Arc { c, r, a0, a1 } => (c, r, a0, a1),
            crate::drawing::Curve2mm::Ellipse { .. } => continue,
        };
        if (r - 5.0).abs() > 1e-6 || c.dist(Vec2::ZERO) > 0.05 {
            continue;
        }
        let middle = (start + end) * 0.5;
        let x = c.x + r * middle.cos();
        if hidden {
            hidden_sweep += end - start;
            hidden_x.push(x);
        } else {
            visible_sweep += end - start;
            visible_x.push(x);
        }
    }
    assert!(visible_sweep > 0.2 && hidden_sweep > 0.2, "the hole's circle splits: visible {visible_sweep}, hidden {hidden_sweep}");
    assert!(
        visible_sweep + hidden_sweep > TAU - 0.2,
        "the two arcs cover the circle (a few degrees at the split may be slivers): {visible_sweep} + {hidden_sweep}"
    );
    // The cover hides one side: every visible run is on the other side of the circle.
    assert!(visible_x.iter().all(|x| x.signum() == visible_x[0].signum()), "visible on one side: {visible_x:?}");
    assert!(hidden_x.iter().all(|x| x.signum() == -visible_x[0].signum()), "hidden on the other: {hidden_x:?} vs {visible_x:?}");
    // The sheet and the DXF keep the split: ARC on 0.50 and on HIDDEN.
    let sheet = build_sheet(&views, &TitleBlock::for_design("Covered hole"), &NotesBlock::default(), Some((1, 1))).unwrap();
    assert!(sheet.prims.iter().any(|p| matches!(p, Prim::Arc { layer: Layer::Visible, .. })));
    assert!(sheet.prims.iter().any(|p| matches!(p, Prim::Arc { layer: Layer::Hidden, .. })));
    let dxf = sheet_to_dxf(&sheet).unwrap();
    let kinds = dxf_entities(&dxf);
    let layer_of = |kind: &str, layer: &str| kinds.iter().any(|(k, f)| k == kind && f.iter().any(|(c, v)| *c == 8 && v == layer));
    assert!(layer_of("ARC", "VISIBLE"), "a visible ARC");
    assert!(layer_of("ARC", "HIDDEN"), "a hidden ARC");
    // No polyline stands in for the hole's circle: the only ones left are the frame's four
    // corners and a freehand boundary.
    for (kind, fields) in &kinds {
        if kind == "LWPOLYLINE" {
            let points = fields.iter().filter(|(c, _)| *c == 10).count();
            assert!(points <= 8, "a {points}-point polyline is not a true arc");
        }
    }
}

#[test]
fn a_circle_seen_obliquely_is_an_ellipse_with_its_cosine_ratio() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":6,"height":20,"center":[0,0,0],"body_name":"Plate"})).unwrap();
    s.execute("solid.hole", &json!({"position":[0,-3,0],"direction":[0,1,0],"diameter":10,"through":true,"targets":["Plate"]})).unwrap();
    // Turn the plate 60° about X: the hole's axis now meets the front view at 60°, so its circle
    // projects to an ellipse whose minor/major is cos 60° = 0.5.
    s.execute("solid.move", &json!({"bodies":["Plate"],"axis":[1,0,0],"angle":60})).unwrap();
    let views = crate::drawing::project_views(
        &s,
        &[StandardView::Front],
        &ViewOptions { tolerance: 0.01, hidden: true, tangent: crate::drawing::TangentEdges::Omit, curves: true },
    )
    .unwrap();
    let ellipses: Vec<(f64, f64)> = projected_curves(&views[0])
        .into_iter()
        .filter_map(|(c, _)| match c {
            crate::drawing::Curve2mm::Ellipse { major, ratio, .. } if (major.len() - 5.0).abs() < 0.05 => Some((ratio, major.len())),
            _ => None,
        })
        .collect();
    assert!(!ellipses.is_empty(), "the oblique circle is an ELLIPSE: {:?}", views[0].visible_curves);
    assert!(ellipses.iter().all(|(ratio, _)| (ratio - 0.5).abs() < 0.01), "{ellipses:?}");
}

#[test]
fn a_full_section_keeps_the_hole_as_an_arc() {
    let mut s = plate_with_a_covered_hole();
    // Remove the cover: the section is only about the plate.
    s.execute("solid.remove", &json!({"bodies":["Cover"]})).unwrap();
    let view = StandardView::Front;
    let cut = crate::drawing::ViewCut {
        view,
        plane: crate::drawing::CuttingPlane { point: solidcraft_geom::Vec3::ZERO, normal: solidcraft_geom::Vec3::new(0.0, -1.0, 0.0) },
        boundary: None,
        label: Some('A'),
        from: None,
    };
    let views = crate::drawing::project_views_cut(
        &s,
        &[view],
        &ViewOptions { tolerance: 0.01, hidden: true, tangent: crate::drawing::TangentEdges::Omit, curves: true },
        &[cut],
    )
    .unwrap();
    let arcs: Vec<f64> = projected_curves(&views[0])
        .into_iter()
        .filter_map(|(c, _)| match c {
            crate::drawing::Curve2mm::Arc { c, r, a0, a1 } if (r - 5.0).abs() < 1e-6 && c.dist(Vec2::ZERO) < 0.1 => Some(a1 - a0),
            _ => None,
        })
        .collect();
    assert!(!arcs.is_empty(), "the cut leaves the hole's far mouth as an arc: {:?}", Views(&views));
    assert!(arcs.iter().sum::<f64>() >= std::f64::consts::PI - 1e-3, "at least half the circle: {arcs:?}");
}

/// A tiny Debug wrapper so a test failure prints the view's curves without dumping everything.
struct Views<'a>(&'a [ProjectedView]);
impl std::fmt::Debug for Views<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().map(|v| (v.view, &v.visible_curves, &v.hidden_curves)).collect::<Vec<_>>().fmt(f)
    }
}

#[test]
fn tangent_edges_are_omitted_by_default_and_thin_on_request() {
    let mut s = Session::default();
    s.execute("solid.box", &json!({"length":40,"width":20,"height":10,"center":[0,0,0],"body_name":"Plate"})).unwrap();
    s.execute("solid.fillet", &json!({"edges":[[20,10,0]],"radius":4,"body":"Plate"})).unwrap();
    // The measuring test: edges whose two faces are G1, with their stations in world mm.
    let mut tangent_edges: Vec<Vec<solidcraft_geom::Vec3>> = Vec::new();
    for body in s.world_state().bodies.iter().filter(|b| s.body_visible(&b.name)) {
        let (_, placement) = body.mesh_in_place();
        let report = solidcraft_kernel::seam_diagnostics(
            &body.body,
            &[],
            5,
            solidcraft_kernel::ContinuityTolerances { gap_mm: 0.001, angle_deg: 0.5, curvature_per_mm: 0.01 },
        )
        .unwrap();
        for seam in report.seams {
            if matches!(seam.verdict, solidcraft_kernel::SeamVerdict::Smooth | solidcraft_kernel::SeamVerdict::CurvatureJump) {
                tangent_edges.push(seam.points.iter().map(|p| solidcraft_doc::apply_point(&placement, *p)).collect());
            }
        }
    }
    assert!(!tangent_edges.is_empty(), "a filleted box has G1 seams");
    let options = |tangent| ViewOptions { tolerance: 0.01, hidden: true, tangent, curves: true };
    // The right view sees the x = 20 face; the fillet seam on it is a light edge inside the face.
    let omit = crate::drawing::project_views(&s, &[StandardView::Right], &options(crate::drawing::TangentEdges::Omit)).unwrap();
    let thin = crate::drawing::project_views(&s, &[StandardView::Right], &options(crate::drawing::TangentEdges::Thin)).unwrap();
    let draw = crate::drawing::project_views(&s, &[StandardView::Right], &options(crate::drawing::TangentEdges::Draw)).unwrap();
    // Project the seams into that view.
    let camera = crate::drawing::standard_camera(StandardView::Right);
    let (right, up, _) = camera.basis();
    let project = |p: solidcraft_geom::Vec3| Vec2::new((p - camera.target).dot(right), (p - camera.target).dot(up));
    let seam_segments: Vec<(Vec2, Vec2)> =
        tangent_edges.iter().flat_map(|points| points.windows(2).filter_map(|w| Some((project(*w.first()?), project(*w.get(1)?))))).collect();
    assert!(!seam_segments.is_empty());
    // By default no thick visible edge lies on a tangent seam.
    assert!(!thin[0].tangent.is_empty(), "thin mode puts the seams on their own layer");
    for (a, b) in &seam_segments {
        let mid = (*a + *b) * 0.5;
        // A seam that projects on the box's own silhouette cannot be told apart from it; the
        // interior one is the measuring case (here the seam across the x = 20 face).
        if mid.x.abs() < 9.5 {
            assert!(distance_to_segments(mid, &omit[0].visible) > 0.05, "a tangent seam is drawn thick by default: {mid:?}");
            assert!(distance_to_segments(mid, &thin[0].tangent) <= 0.05, "the seam is a light edge with thin: {mid:?}");
        }
        assert!(distance_to_segments(mid, &draw[0].visible) <= 0.05, "draw mode keeps it visible");
    }
    assert!(draw[0].visible.len() > omit[0].visible.len(), "the tangent seams are extra visible edges in draw mode");
}

#[test]
fn cylinder_silhouettes_are_not_tangent_edges_and_stay_visible() {
    let mut s = Session::default();
    s.execute("solid.cylinder", &json!({"radius":10,"height":30,"base":[0,0,-15],"body_name":"Boss"})).unwrap();
    let views = crate::drawing::project_views(
        &s,
        &[StandardView::Front],
        &ViewOptions { tolerance: 0.01, hidden: true, tangent: crate::drawing::TangentEdges::Omit, curves: true },
    )
    .unwrap();
    let view = &views[0];
    assert!(view.tangent.is_empty(), "a plain cylinder has no tangent seam: {:?}", view.tangent);
    // The two vertical silhouette lines of the cylinder remain visible and are 30 mm long.
    let vertical: Vec<&crate::drawing::Seg2mm> =
        view.visible.iter().filter(|s| (s.a.x - s.b.x).abs() < 1e-6 && (s.a.y - s.b.y).abs() > 25.0).collect();
    assert_eq!(vertical.len(), 2, "the cylinder's two silhouettes stay: {:?}", view.visible);
    assert!(vertical.iter().all(|s| (s.a.x.abs() - 10.0).abs() < 0.05), "{vertical:?}");
}

/// The five acceptance parts, when the fixture files are on the box: every analytic curve stays
/// within the chord tolerance of the polyline geometry it replaces, the sheet entity count
/// drops (the shaft by an order of magnitude), and no tangent seam is drawn as a thick edge.
/// Run with `SOLIDCRAFT_D9_FIXTURES=1 SOLIDCRAFT_D71_FIXTURES=1`.
#[test]
fn acceptance_parts_meet_the_d10_rules() {
    if std::env::var_os("SOLIDCRAFT_D9_FIXTURES").is_none() || std::env::var_os("SOLIDCRAFT_D71_FIXTURES").is_none() {
        return;
    }
    let parts = [
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/shaft.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/crank.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/flywheel.solidcraft"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/drawing/test_plate.step"),
    ];
    for path in parts {
        let mut s = Session::default();
        s.execute("doc.open", &json!({"path": path})).unwrap_or_else(|e| panic!("{path}: {e}"));
        let title = stored_title(s.doc.as_ref(), rohmass(&s));
        let notes = stored_notes(s.doc.as_ref());
        let (plan, _) = crate::drawing::plan_sheet_auto_sections_tangent(
            &s,
            None,
            0.01,
            &title,
            &notes,
            None,
            &Annotations::default(),
            &crate::drawing::SectionMode::Auto,
            crate::drawing::TangentEdges::Omit,
        )
        .unwrap_or_else(|e| panic!("{path}: {e}"));
        let views: Vec<StandardView> = plan.projected.iter().map(|v| v.view).collect();
        let options = |tangent| ViewOptions { tolerance: 0.01, hidden: true, tangent, curves: false };
        let old_projected = crate::drawing::project_views_cut(&s, &views, &options(crate::drawing::TangentEdges::Omit), &plan.cuts)
            .unwrap_or_else(|e| panic!("{path}: old polylines: {e}"));
        let (old_plan, _) =
            crate::drawing::plan_sheet_auto_reuse(&s, plan.choice, &views, &old_projected, &title, &notes, None, &Annotations::default())
                .unwrap_or_else(|e| panic!("{path}: old sheet: {e}"));
        let new_prims = plan.sheet.prims.len();
        let old_prims = old_plan.sheet.prims.len();
        println!(
            "D10 acceptance {}: {old_prims} polyline prims -> {new_prims} analytic prims (x{:.1})",
            path.rsplit('/').next().unwrap_or(path),
            old_prims as f64 / new_prims.max(1) as f64
        );
        assert!(new_prims > 0, "{path}: no prims");
        if path.contains("shaft") {
            // The polyline build here already has the collinear cleanup; the pre-D10 DXF of the
            // same part had 4623 entities against 313 now (a factor of 14.8, D10).
            assert!(old_prims >= 4 * new_prims, "{path}: {old_prims} -> {new_prims} is too small a drop");
        }
        // Every new curve sampled at 64 points is within 0.01 mm of the old polyline geometry.
        let mut worst: f64 = 0.0;
        for (new, old) in plan.projected.iter().zip(&old_projected) {
            let old_segments: Vec<crate::drawing::Seg2mm> = old.visible.iter().chain(&old.hidden).copied().collect();
            for list in [&new.visible_curves, &new.hidden_curves] {
                for curve in list {
                    for p in curve.sample(64) {
                        worst = worst.max(distance_to_segments(p, &old_segments));
                    }
                }
            }
        }
        println!("D10 acceptance {}: worst analytic-curve distance to the old polyline {worst:.6} mm", path.rsplit('/').next().unwrap_or(path));
        assert!(worst <= 0.012, "{path}: an analytic point is {worst} mm from the old polyline");
        // The measuring tangent test: no thick visible edge of the default sheet lies on a G1
        // seam that the thin mode puts on its own layer.
        let thin = crate::drawing::project_views_cut(
            &s,
            &views,
            &ViewOptions { tolerance: 0.01, hidden: true, tangent: crate::drawing::TangentEdges::Thin, curves: true },
            &plan.cuts,
        )
        .unwrap_or_else(|e| panic!("{path}: thin: {e}"));
        let scale = plan.sheet.scale_factor();
        let visible_segments: Vec<(Vec2, Vec2)> = plan
            .sheet
            .prims
            .iter()
            .filter(|p| p.layer() == Layer::Visible)
            .flat_map(|prim| match prim {
                Prim::Line { a, b, .. } => vec![(*a, *b)],
                Prim::Arc { c, r, a0, a1, .. } => {
                    let steps = (((a1 - a0).abs() / 2.0_f64.to_radians()).ceil() as usize).clamp(2, 720);
                    let point = |i: usize| *c + Vec2::from_angle(a0 + (a1 - a0) * i as f64 / steps as f64) * *r;
                    (0..steps).map(|i| (point(i), point(i + 1))).collect()
                }
                Prim::Circle { c, r, .. } => {
                    let point = |i: usize| *c + Vec2::from_angle(TAU * i as f64 / 180.0) * *r;
                    (0..180).map(|i| (point(i), point(i + 1))).collect()
                }
                Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
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
            .collect();
        // The measuring test: B-rep edges whose two faces meet at G1, found on the body itself.
        let mut g1_edges = 0usize;
        for body in s.world_state().bodies.iter().filter(|b| s.body_visible(&b.name)) {
            let Ok(report) = solidcraft_kernel::seam_diagnostics(
                &body.body,
                &[],
                5,
                solidcraft_kernel::ContinuityTolerances { gap_mm: 0.001, angle_deg: 0.5, curvature_per_mm: 0.01 },
            ) else {
                continue;
            };
            g1_edges += report
                .seams
                .iter()
                .filter(|seam| matches!(seam.verdict, solidcraft_kernel::SeamVerdict::Smooth | solidcraft_kernel::SeamVerdict::CurvatureJump))
                .count();
        }
        let mut tangent_count = 0usize;
        let mut drawn_thick = 0usize;
        for (view, projected) in plan.projected.iter().zip(&thin) {
            let Some(placed) = plan.sheet.views.iter().find(|p| p.view == view.view) else { continue };
            for seg in &projected.tangent {
                tangent_count += 1;
                let (a, b) = (placed.origin + seg.a * scale, placed.origin + seg.b * scale);
                let mid = (a + b) * 0.5;
                if visible_segments.iter().any(|(c, d)| point_segment(mid, *c, *d) <= 0.05) {
                    drawn_thick += 1;
                }
            }
        }
        println!(
            "D10 acceptance {}: {g1_edges} G1 edges measured, {tangent_count} tangent seams visible, {drawn_thick} drawn thick by default",
            path.rsplit('/').next().unwrap_or(path)
        );
        assert_eq!(drawn_thick, 0, "{path}: {drawn_thick} tangent seams are visible thick edges");
        if path.contains("crank") || path.contains("test_plate.step") {
            assert!(g1_edges > 0, "{path}: the measuring test finds no G1 edge");
        }
    }
}
