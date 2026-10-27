use super::*;
use crate::scenario::Harness;
use solidcraft_engine::geom::Vec3;

fn boxed() -> Harness {
    let mut h = Harness::new();
    h.app.run("solid.box", serde_json::json!({"length":40,"width":20,"height":10})).unwrap();
    h
}

/// Poll the background sheet build until it is no longer running.
fn wait_for_build(app: &mut SolveApp) {
    for _ in 0..4000 {
        poll_build(app);
        if !building(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!building(app), "the sheet build did not finish");
}

#[test]
fn entering_drawing_shows_only_its_tab_and_leaving_restores_the_design_camera() {
    let mut h = boxed();
    let camera = Camera { target: Vec3::new(3.0, 4.0, 5.0), yaw: 0.7, pitch: 0.4, distance: 321.0, ..Default::default() };
    h.app.cam = camera;
    h.app.ui.perspective = true;

    enter(&mut h.app, true).unwrap();
    assert!(active(&h.app));
    assert_eq!(h.app.ui.tab, "DRAWING");
    assert_eq!(crate::workspace::visible_tabs(&h.app), vec!["DRAWING"]);

    wait_for_build(&mut h.app);
    let sheet = h.app.drawing_workspace.view.sheet.as_ref().expect("the box's sheet was built");
    assert_eq!(sheet.scale, (2, 1), "the 40 x 20 x 10 box is drawn at 2:1");
    assert_eq!(sheet.views.len(), 3);
    assert!(h.app.drawing_workspace.view.error.is_none());

    enter(&mut h.app, false).unwrap();
    assert!(!active(&h.app));
    assert_eq!(h.app.ui.tab, "MODEL");
    assert_eq!(h.app.cam, camera, "the Design camera comes back exactly");
    assert!(h.app.ui.perspective);
}

/// A stepped shaft revolved about X: a turned part, so the sheet gets two views.
fn shafted() -> Harness {
    let mut h = Harness::new();
    let steps: [(&str, serde_json::Value); 4] = [
        ("sketch.create", serde_json::json!({"plane":"XY","name":"Profile"})),
        ("sketch.line", serde_json::json!({"points":[[0,0],[0,10],[30,10],[30,14],[80,14],[80,8],[120,8],[120,0]],"closed":true})),
        ("sketch.finish", serde_json::json!({})),
        ("solid.revolve", serde_json::json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})),
    ];
    for (command, params) in steps {
        h.app.run(command, params).unwrap();
    }
    h
}

#[test]
fn a_turned_part_is_built_with_two_views_and_the_panel_says_why() {
    let mut h = shafted();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    assert_eq!(view.choice, ViewChoice::Turned);
    let sheet = view.sheet.as_ref().expect("the shaft's sheet was built");
    assert_eq!(sheet.views.len(), 2, "a turned part gets its longitudinal and end view: {:?}", sheet.views);
    let names: Vec<&str> = sheet.views.iter().map(|v| view_name(v.view)).collect();
    assert_eq!(names, ["front", "right"]);
    assert!(sheet.prims.iter().any(|p| p.layer() == Layer::Center), "the axis is drawn");

    // The side panel states the choice in one caption line.
    let ctx = h.ctx.clone();
    let raw = egui::RawInput {
        // Tall enough that the whole side panel (14 title fields and the notes group) is
        // painted; the panel scrolls on a smaller window.
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 2600.0))),
        time: Some(4.0),
        focused: true,
        ..Default::default()
    };
    let app = &mut h.app;
    let mut out = ctx.run_ui(raw, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    out.textures_delta.clear();
    let mut texts = Vec::new();
    for clipped in &out.shapes {
        crate::test_support::each_shape(&clipped.shape, &mut |shape| {
            if let egui::Shape::Text(text) = shape {
                texts.push(text.galley.text().to_owned());
            }
        });
    }
    assert!(texts.iter().any(|text| text == "Views: front, right (turned part)"), "the caption is shown: {texts:?}");
}

#[test]
fn drawing_refuses_to_open_while_a_sketch_is_being_edited() {
    let mut h = boxed();
    h.app.run("sketch.create", serde_json::json!({"plane":"XY"})).unwrap();
    let error = enter(&mut h.app, true).unwrap_err();
    assert!(error.contains("before entering Drawing"), "{error}");
    assert!(!active(&h.app));
    assert_eq!(h.app.ui.tab, "SKETCH");
}

#[test]
fn switching_documents_leaves_drawing_as_it_leaves_render() {
    let mut h = boxed();
    crate::documents::new_tab(&mut h.app);
    crate::documents::switch(&mut h.app, 0);
    enter(&mut h.app, true).unwrap();
    assert!(active(&h.app));

    // Opening another design swaps this one out; the workspace leaves with it.
    crate::documents::new_tab(&mut h.app);
    assert!(!active(&h.app));
    assert_eq!(h.app.ui.tab, "MODEL");
    assert!(h.app.drawing_workspace.view.sheet.is_none(), "the new design starts with no sheet");
}

#[test]
fn a_finished_build_for_a_stale_design_revision_is_dropped() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    assert!(building(&h.app), "entering started the projection");
    // The design changes before the result is taken up.
    h.app.run("solid.sphere", serde_json::json!({"diameter":6})).unwrap();
    wait_for_build(&mut h.app);

    assert!(h.app.drawing_workspace.view.sheet.is_none(), "the stale sheet is not shown");
    assert!(h.app.drawing_workspace.view.error.is_none(), "a stale result is no error");
    assert_eq!(h.app.drawing_workspace.view.projections, 0);
    assert!(h.app.drawing_workspace.view.dirty, "the page asks for Update Views");
}

#[test]
fn a_title_edit_rebuilds_only_the_sheet_without_projecting_again() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let projections = h.app.drawing_workspace.view.projections;
    assert_eq!(projections, 1);
    let before: Vec<String> = h
        .app
        .drawing_workspace
        .view
        .sheet
        .as_ref()
        .unwrap()
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    h.app.drawing_workspace.view.title.benennung = "Bracket".into();
    title_changed(&mut h.app);

    let view = &h.app.drawing_workspace.view;
    assert_eq!(view.projections, projections, "a title edit never projects again");
    assert!(!view.dirty);
    assert!(view.error.is_none());
    let sheet = view.sheet.as_ref().unwrap();
    assert_eq!(sheet.title.benennung, "Bracket");
    assert!(sheet.prims.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "Bracket")), "the new name is lettered on the sheet");
    let after: Vec<String> = sheet
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_ne!(after, before, "the lettering changed");

    // A scale is a sheet-only change too.
    set_scale(&mut h.app, Scale::Fixed(1, 1));
    let view = &h.app.drawing_workspace.view;
    assert_eq!(view.projections, projections);
    assert_eq!(view.sheet.as_ref().unwrap().scale, (1, 1));

    // A scale that cannot fit is reported, without losing the last sheet or projecting again.
    set_scale(&mut h.app, Scale::Fixed(10_000, 1));
    let view = &h.app.drawing_workspace.view;
    assert!(view.error.is_some(), "a sheet that does not fit is an error");
    assert!(view.dirty);
    assert!(view.sheet.is_some());
    assert_eq!(view.projections, projections);
}

#[test]
fn control_channel_switches_to_drawing_and_reports_the_build() {
    let mut h = boxed();
    let response = h.call("ui.workspace", serde_json::json!({"workspace":"drawing"}));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["workspace"], "drawing");
    assert_eq!(h.app.ui.tab, "DRAWING");

    for _ in 0..400 {
        if h.call("ui.drawing.state", serde_json::json!({}))["result"]["building"] == false {
            break;
        }
        h.frames(2);
    }
    let state = h.call("ui.drawing.state", serde_json::json!({}))["result"].clone();
    assert_eq!(state["active"], true, "{state}");
    assert_eq!(state["building"], false, "{state}");
    assert_eq!(state["error"], Value::Null, "{state}");
    assert_eq!(state["views"], serde_json::json!(["front", "top", "right"]), "{state}");
    assert_eq!(state["view_count"], 3);
    assert_eq!(state["scale"], "auto");
    assert!(state["projections"].as_u64().unwrap_or(0) >= 1);

    let response = h.call("ui.workspace", serde_json::json!({"workspace":"design"}));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["workspace"], "design");
    assert!(!active(&h.app));
    assert_eq!(h.app.ui.tab, "MODEL");
}

#[test]
fn paper_millimetres_map_to_screen_with_y_up() {
    let map = PageMap { center: egui::pos2(100.0, 100.0), target_x: 0.0, target_y: 0.0, scale: 2.0 };
    assert_eq!(map.to(0.0, 0.0), egui::pos2(100.0, 100.0));
    assert_eq!(map.to(10.0, 0.0), egui::pos2(120.0, 100.0), "x to the right");
    assert_eq!(map.to(0.0, 10.0), egui::pos2(100.0, 80.0), "y up on paper is up on screen");
    let map = PageMap { target_x: 5.0, target_y: 2.0, ..map };
    assert_eq!(map.to(5.0, 2.0), egui::pos2(100.0, 100.0));
}

#[test]
fn sheet_prims_paint_their_pen_widths_dashes_and_rotated_text() {
    use solidcraft_engine::geom::Vec2;

    let ctx = crate::fonts::test_context();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.painter();
        let to = |x: f64, y: f64| egui::pos2(x as f32, y as f32);
        let visible = Prim::Line { a: Vec2::new(0.0, 0.0), b: Vec2::new(10.0, 0.0), layer: Layer::Visible };
        let hidden = Prim::Line { a: Vec2::new(0.0, 4.0), b: Vec2::new(10.0, 4.0), layer: Layer::Hidden };
        let frame = Prim::Line { a: Vec2::new(0.0, 8.0), b: Vec2::new(10.0, 8.0), layer: Layer::Frame };
        let text =
            Prim::Text { at: Vec2::new(5.0, 2.0), height: 3.0, angle_deg: 90.0, text: "Z".into(), anchor: TextAnchor::Center, layer: Layer::Text };
        for prim in [visible, hidden, frame, text] {
            paint_prim(painter, &prim, &to, 2.0);
        }
    });
    out.textures_delta.clear();
    let mut shapes = Vec::new();
    for clipped in &out.shapes {
        crate::test_support::each_shape(&clipped.shape, &mut |shape| shapes.push(shape.clone()));
    }

    let widths: Vec<f32> = shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::LineSegment { stroke, .. } => Some(stroke.width),
            _ => None,
        })
        .collect();
    // Visible 0.5 mm x 2 px/mm = 1 px; hidden 0.25 mm x 2 = 0.5 px, clamped to 1; frame 0.7 x 2
    // = 1.4 px; the dashed line is drawn as several segments at its pen width.
    assert!(widths.iter().any(|w| (*w - 1.4).abs() < 1e-5), "the frame keeps its 0.7 mm pen: {widths:?}");
    assert!(widths.iter().all(|w| *w >= 1.0), "no stroke is thinner than a pixel: {widths:?}");
    assert!(widths.iter().all(|w| (*w - 1.0).abs() < 1e-5 || (*w - 1.4).abs() < 1e-5), "{widths:?}");
    assert!(widths.len() > 3, "the hidden line is a run of dashes: {widths:?}");

    let text = shapes
        .iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == "Z" => Some(t),
            _ => None,
        })
        .expect("the rotated text is painted");
    assert!(text.angle.abs() > 1.0, "the 90 degree prim is rotated: {}", text.angle);
}

#[test]
fn the_window_paints_the_sheet_page_its_title_block_and_its_tab() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let ctx = h.ctx.clone();
    let raw = egui::RawInput {
        // Tall enough that the whole side panel (14 title fields and the notes group) is
        // painted; the panel scrolls on a smaller window.
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 2600.0))),
        time: Some(3.0),
        focused: true,
        ..Default::default()
    };
    let app = &mut h.app;
    let mut out = ctx.run_ui(raw, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    out.textures_delta.clear();
    let mut texts = Vec::new();
    let mut widths = Vec::new();
    for clipped in &out.shapes {
        crate::test_support::each_shape(&clipped.shape, &mut |shape| match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
            egui::Shape::LineSegment { stroke, .. } => widths.push(stroke.width),
            _ => {}
        });
    }
    for label in ["Benennung", "Zeichnungsnummer", "Werkstoff", "Erstellt durch", "Ausgabedatum", "Dokumentart"] {
        assert!(texts.iter().any(|text| text == label), "{label} is lettered on the sheet");
    }
    assert!(texts.iter().any(|text| text == "2:1"), "the box's scale is lettered: {texts:?}");
    assert!(widths.iter().all(|width| *width >= 1.0), "no hairline strokes: {widths:?}");
    assert!(crate::scenario::rect_of("drawing:paper").is_some(), "the paper is drawn");
    assert!(crate::scenario::rect_of("tab:DRAWING").is_some(), "the DRAWING tab is shown");
}

#[test]
fn the_ribbon_exports_the_sheet_as_pdf_and_dxf_through_the_save_dialog() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let doc = h.app.session.doc.clone();
    let undo = h.app.session.undo.len();
    let dirty = h.app.session.is_dirty();

    h.step(&serde_json::json!({"click":{"handle":"drawing:export_pdf"}})).unwrap();
    h.frames(4);
    let pdf = std::fs::read_dir(&h.dir)
        .unwrap()
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "pdf"))
        .expect("the PDF export wrote its file");
    let bytes = std::fs::read(pdf.path()).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.4"), "a real PDF: {:?}", &bytes[..8.min(bytes.len())]);

    h.frames(2);
    h.step(&serde_json::json!({"click":{"handle":"drawing:export_dxf"}})).unwrap();
    h.frames(4);
    let dxf = std::fs::read_dir(&h.dir)
        .unwrap()
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "dxf"))
        .expect("the DXF export wrote its file");
    let text = std::fs::read_to_string(dxf.path()).unwrap();
    assert!(text.contains("$INSUNITS"), "an AutoCAD DXF with millimetre units");
    assert!(text.contains("TITLE"), "the title block is on its layer");

    assert!(std::sync::Arc::ptr_eq(&doc, &h.app.session.doc), "exports do not change the design");
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(h.app.session.is_dirty(), dirty, "an export is not a design change");
}

#[test]
fn the_page_zooms_with_the_shared_navigation_without_touching_the_design_camera() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let design = h.app.cam;
    let before = h.app.drawing_workspace.view.cam.cam.distance;
    let rect = h.app.viewport.rect.expect("the page reports its viewport");
    let centre = rect.center();
    h.call("ui.scroll", serde_json::json!({"x":centre.x,"y":centre.y,"delta":-300}));
    h.frames(3);
    let after = h.app.drawing_workspace.view.cam.cam.distance;
    assert!((after - before).abs() > 1e-6, "the wheel moved the page camera: {before} -> {after}");
    assert_eq!(h.app.cam, design, "the Design camera stays exactly as it was");
}

/// The page's own free area: the layout Design's fit uses, from this frame.
fn free_area_of(h: &Harness) -> egui::Rect {
    let layout = h.app.viewport.layout.as_ref().expect("the page laid itself out");
    layout.drawable
}

#[test]
fn the_paper_is_fitted_into_the_free_area_beside_the_side_panel_and_floating_browser() {
    let mut h = boxed();
    h.app.ui.layout = crate::shell::LayoutMode::Floating;
    h.app.ui.show_browser = true;
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(4);

    let layout = h.app.viewport.layout.clone().expect("the page laid itself out");
    let area = layout.drawable;
    assert!(area.is_positive(), "{layout:?}");
    let paper = crate::scenario::rect_of("drawing:paper").expect("the paper is painted");
    assert!(area.contains_rect(paper), "paper {paper:?} is not inside the free area {area:?}");
    let off = paper.center() - area.center();
    assert!(off.length() <= 1.0, "paper centre {:?} is {off:?} from the free area's {:?}", paper.center(), area.center());

    // The panel and the floating Browser really are what the free area avoids.
    assert!(h.app.shell.browser.is_some(), "the floating Browser card is shown");
    assert!(h.widget("Benennung").is_some(), "the title-block side panel is shown");
    assert!(layout.drawable.right() < layout.viewport.right() - 16.0, "the side panel narrows the free area: {layout:?}");

    // The layout's own 16 px inset is the whole margin, so the paper keeps it on every side.
    let mut obstacles = crate::shell::covered_rects(&h.app);
    obstacles.extend([layout.axis_view, layout.navigation]);
    let free = crate::viewport_layout::free_rect(layout.viewport, &obstacles);
    let margin = 16.0 / h.ctx.pixels_per_point();
    for (side, paper_side, free_side, grows) in [
        ("left", paper.left(), free.left(), 1.0),
        ("right", paper.right(), free.right(), -1.0),
        ("top", paper.top(), free.top(), 1.0),
        ("bottom", paper.bottom(), free.bottom(), -1.0),
    ] {
        assert!(
            grows * (paper_side - free_side) >= margin - 0.5,
            "paper {side} {paper_side} leaves less than {margin} px inside the free rect {free:?}"
        );
    }
    // No card or panel that covers the page overlaps the paper.
    for covered in crate::shell::covered_rects(&h.app).into_iter().filter(|r| r.intersects(layout.viewport)) {
        assert!(!paper.intersects(covered), "paper {paper:?} lies under the covering rect {covered:?}");
    }

    // A window resize while the page is still fitted refits it.
    h.call("ui.resize", serde_json::json!({"width":1400,"height":900}));
    h.frames(3);
    let resized = free_area_of(&h);
    assert!(!same_rect(resized, area), "the window resize changed the free area: {area:?} -> {resized:?}");
    let paper = crate::scenario::rect_of("drawing:paper").expect("the paper is painted");
    assert!(resized.contains_rect(paper), "paper {paper:?} is not inside the resized free area {resized:?}");
    assert!((paper.center() - resized.center()).length() <= 1.0, "the resize refits the paper");

    // Hiding the floating Browser frees its space, and the still-fitted page follows it.
    assert!(h.app.shell.browser.is_some());
    h.call("ui.set", serde_json::json!({"showBrowser": false}));
    h.frames(4);
    assert!(h.app.shell.browser.is_none(), "the Browser card is hidden");
    let wider = free_area_of(&h);
    assert!(!same_rect(wider, resized), "the Browser's space changes the free area: {resized:?} -> {wider:?}");
    assert!(wider.area() > resized.area(), "the Browser's space joins the free area: {resized:?} -> {wider:?}");
    let paper = crate::scenario::rect_of("drawing:paper").expect("the paper is painted");
    assert!(wider.contains_rect(paper), "paper {paper:?} is not inside the wider free area {wider:?}");
    assert!((paper.center() - wider.center()).length() <= 1.0, "the panel change refits the paper");
}

#[test]
fn a_manual_pan_survives_a_resize_and_fit_returns_to_the_new_free_area() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(4);
    let fitted = h.app.drawing_workspace.view.cam.cam;
    let centre = h.app.viewport.rect.expect("the page reports its viewport").center();
    h.call("ui.drag", serde_json::json!({"x0":centre.x,"y0":centre.y,"x1":centre.x+60.0,"y1":centre.y+30.0,"button":"middle","steps":5}));
    let panned = h.app.drawing_workspace.view.cam.cam;
    assert_ne!(panned, fitted, "the middle drag pans the page");
    assert!(h.app.drawing_workspace.view.moved, "the pan is the user's, not a fit");

    h.call("ui.resize", serde_json::json!({"width":1500,"height":950}));
    h.frames(3);
    assert_eq!(h.app.drawing_workspace.view.cam.cam, panned, "a window resize keeps the manual view");

    let response = h.call("ui.view", serde_json::json!({"view":"fit"}));
    assert_eq!(response["result"]["fitted"], true, "{response}");
    h.frames(3);
    assert!(!h.app.drawing_workspace.view.moved, "Fit takes the page back");
    let area = free_area_of(&h);
    let paper = crate::scenario::rect_of("drawing:paper").expect("the paper is painted");
    assert!(area.contains_rect(paper), "paper {paper:?} is not inside the free area {area:?}");
    assert!((paper.center() - area.center()).length() <= 1.0, "Fit centres the paper in the new free area");
}

/// One path of a PDF content stream: its pen, dash pattern and painted segments.
#[derive(Debug)]
struct PdfPath {
    width: f64,
    segments: usize,
    filled: bool,
}

/// The path objects of the (uncompressed) content stream `write_pdf` writes, in sheet order.
fn pdf_paths(pdf: &[u8]) -> Vec<PdfPath> {
    let text = String::from_utf8_lossy(pdf);
    let start = text.find("stream\n").expect("the PDF has a content stream") + "stream\n".len();
    let end = start + text[start..].find("endstream").expect("the content stream ends");
    let mut paths = Vec::new();
    let mut current: Option<PdfPath> = None;
    let mut closed = false;
    for line in text[start..end].lines() {
        let line = line.trim();
        if let Some(width) = line.strip_suffix(" w") {
            current = Some(PdfPath { width: width.trim().parse().expect("a stroke width"), segments: 0, filled: false });
            closed = false;
        } else if line == "h" {
            closed = true;
        } else if line.ends_with(" l") {
            if let Some(path) = current.as_mut() {
                path.segments += 1;
            }
        } else if (line == "S" || line == "s" || line == "f")
            && let Some(mut path) = current.take()
        {
            if closed {
                path.segments += 1;
            }
            path.filled = line == "f";
            paths.push(path);
        }
    }
    paths
}

/// The segments a sheet primitive paints; `None` for lettering.
fn prim_segments(prim: &Prim) -> Option<usize> {
    match prim {
        Prim::Line { .. } => Some(1),
        Prim::Polyline { points, closed, .. } => {
            (!points.is_empty()).then(|| if points.len() == 1 || *closed { points.len() } else { points.len() - 1 })
        }
        // Curves are Bézier paths in the PDF: `m` + `c`, no straight segments.
        Prim::Arc { .. } | Prim::Circle { .. } | Prim::Ellipse { .. } => Some(0),
        Prim::Filled { .. } | Prim::Text { .. } => None,
    }
}

/// The written PDF's segments grouped by DXF layer, checked against the cached sheet's prims.
fn pdf_segments_per_layer(pdf: &[u8], sheet: &Sheet) -> std::collections::BTreeMap<String, usize> {
    let mut paths = pdf_paths(pdf);
    paths.retain(|path| !path.filled);
    let prims: Vec<&Prim> = sheet.prims.iter().filter(|prim| prim_segments(prim).is_some()).collect();
    assert_eq!(paths.len(), prims.len(), "one PDF path per line/polyline primitive of the cached sheet");
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (path, prim) in paths.iter().zip(&prims) {
        assert_eq!(path.width, prim.layer().width_mm(), "{:?} pen width", prim.layer());
        assert_eq!(Some(path.segments), prim_segments(prim), "{:?} segments", prim.layer());
        *counts.entry(prim.layer().dxf_name().to_string()).or_default() += path.segments;
    }
    counts
}

/// The cached sheet's segments grouped by DXF layer.
fn sheet_segments_per_layer(sheet: &Sheet) -> std::collections::BTreeMap<String, usize> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for prim in &sheet.prims {
        if let Some(segments) = prim_segments(prim) {
            *counts.entry(prim.layer().dxf_name().to_string()).or_default() += segments;
        }
    }
    counts
}

fn exported_file(dir: &std::path::Path, extension: &str) -> std::path::PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == extension))
        .unwrap_or_else(|| panic!("the {extension} export wrote its file"))
}

#[test]
fn exporting_a_fresh_sheet_writes_the_cached_sheet_without_projecting() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let sheet = h.app.drawing_workspace.view.sheet.clone().expect("a built sheet");
    let projections = h.app.drawing_workspace.view.projections;

    h.step(&serde_json::json!({"click":{"handle":"drawing:export_pdf"}})).unwrap();
    h.frames(4);

    assert_eq!(h.app.drawing_workspace.view.projections, projections, "a fresh export projects nothing again");
    assert!(h.app.drawing_workspace.pending_export.is_none(), "the export was written at once");
    let pdf = std::fs::read(exported_file(&h.dir, "pdf")).expect("the PDF file");
    assert!(pdf.starts_with(b"%PDF-1.4"));
    assert_eq!(pdf_segments_per_layer(&pdf, &sheet), sheet_segments_per_layer(&sheet), "the PDF is the cached sheet");
}

#[test]
fn exporting_a_stale_sheet_rebuilds_once_on_the_worker_and_then_writes_that_sheet() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let before = h.app.drawing_workspace.view.sheet.clone().expect("a built sheet");
    let projections = h.app.drawing_workspace.view.projections;
    // The design moves on: the cached sheet no longer shows it.
    h.app.run("solid.sphere", serde_json::json!({"diameter":6})).unwrap();

    let path = h.dir.join("stale.dxf");
    request_export(&mut h.app, path.to_string_lossy().into_owned(), ExportKind::Dxf);
    assert!(building(&h.app), "the stale export starts the background build");
    assert_eq!(h.app.drawing_workspace.view.projections, projections, "nothing was projected on this thread");
    assert!(!path.exists(), "nothing is written before the sheet is current");

    wait_for_build(&mut h.app);
    h.frames(2);
    assert_eq!(h.app.drawing_workspace.view.projections, projections + 1, "exactly one worker projection");
    assert!(h.app.drawing_workspace.pending_export.is_none(), "the waiting export was written");
    let sheet = h.app.drawing_workspace.view.sheet.as_ref().expect("the fresh sheet");
    let text = std::fs::read_to_string(&path).expect("the DXF file");
    assert_eq!(text, sheet_to_dxf(sheet).unwrap(), "the file is exactly the sheet on screen");
    assert_ne!(text, sheet_to_dxf(&before).unwrap(), "the written sheet includes the design change");

    // The same current sheet exports as PDF without another projection, and the written PDF's
    // segments per layer are the cached sheet's.
    let expected = sheet_segments_per_layer(h.app.drawing_workspace.view.sheet.as_ref().expect("the fresh sheet"));
    let pdf_path = h.dir.join("stale.pdf");
    request_export(&mut h.app, pdf_path.to_string_lossy().into_owned(), ExportKind::Pdf);
    assert!(!building(&h.app), "the current sheet is written at once");
    assert_eq!(h.app.drawing_workspace.view.projections, projections + 1, "a second export projects nothing");
    let pdf = std::fs::read(&pdf_path).expect("the PDF file");
    let sheet = h.app.drawing_workspace.view.sheet.as_ref().expect("the fresh sheet");
    assert_eq!(pdf_segments_per_layer(&pdf, sheet), expected, "the PDF is the cached sheet");
}

#[test]
fn the_dimensions_toggle_switches_between_automatic_and_none() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    assert!(h.app.drawing_workspace.view.auto_dimensions, "automatic dimensions are the default");
    let automatic = h.app.drawing_workspace.view.sheet.as_ref().unwrap();
    assert!(automatic.dimensions.len() >= 3, "the box gets its overall sizes: {:?}", automatic.dimensions);
    let state = h.call("ui.drawing.state", serde_json::json!({}))["result"].clone();
    assert_eq!(state["auto_dimensions"], true, "{state}");
    assert!(state["dimension_count"].as_u64().unwrap_or(0) >= 3, "{state}");

    h.app.drawing_workspace.view.auto_dimensions = false;
    start_build(&mut h.app);
    wait_for_build(&mut h.app);
    let bare = h.app.drawing_workspace.view.sheet.as_ref().unwrap();
    assert!(bare.dimensions.is_empty(), "Off removes every dimension");
    assert!(bare.notes.is_empty(), "Off removes the automatic notes");
    let state = h.call("ui.drawing.state", serde_json::json!({}))["result"].clone();
    assert_eq!(state["auto_dimensions"], false, "{state}");
    assert_eq!(state["dimension_count"], 0, "{state}");

    h.app.drawing_workspace.view.auto_dimensions = true;
    start_build(&mut h.app);
    wait_for_build(&mut h.app);
    assert!(!h.app.drawing_workspace.view.sheet.as_ref().unwrap().dimensions.is_empty());
}

#[test]
fn a_title_and_notes_edit_is_saved_with_the_design() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    {
        let view = &mut h.app.drawing_workspace.view;
        view.title.gezeichnet = "K. Beispielmann".into();
        view.title.firma = "Muster AG".into();
        view.notes.ra = "1,6".into();
        view.notes.extra_notes = vec!["Entgraten".into()];
    }
    title_changed(&mut h.app);
    // The sheet shows the new values before they are saved.
    let sheet = h.app.drawing_workspace.view.sheet.as_ref().unwrap();
    for expected in ["K. Beispielmann", "Muster AG", "Oberfl\u{00e4}che Ra 1,6", "Entgraten"] {
        assert!(sheet.prims.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == expected)), "{expected:?} is not on the sheet: {sheet:?}");
    }
    assert!(h.app.drawing_workspace.view.title_dirty);
    save_title(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    assert!(view.error.is_none(), "{:?}", view.error);
    assert!(!view.title_dirty, "the values were written");
    assert_eq!(h.app.session.doc.metadata.author(), Some("K. Beispielmann"));
    assert_eq!(h.app.session.doc.metadata.organization(), Some("Muster AG"));
    let stored = h.app.session.doc.metadata.get("app.solidcraft.drawing").and_then(|v| v.get("ra")).and_then(|v| v.as_str());
    assert_eq!(stored, Some("1,6"));
    // The metadata write is not a geometry change: the sheet on screen stays current.
    assert_eq!(view.revision, Some(h.app.session.revision));
    let state = control_state(&h.app);
    assert_eq!(state["title_dirty"], false);
    assert_eq!(state["notes_block"]["ra"], "1,6");
    assert_eq!(state["notes_block"]["extra_notes"], serde_json::json!(["Entgraten"]));
}

#[test]
fn the_drawing_workspace_publishes_the_documented_handle_names() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let names: Vec<String> = crate::scenario::handles("drawing:").into_iter().map(|(name, _)| name).collect();
    for wanted in [
        "drawing:paper",
        "drawing:scale",
        "drawing:update_views",
        "drawing:export_pdf",
        "drawing:export_dxf",
        "drawing:section",
        "drawing:section:full",
        "drawing:section:breakout",
        "drawing:section:auto",
        "drawing:section:none",
        "drawing:view:front",
        "drawing:view:top",
    ] {
        assert!(names.iter().any(|name| name == wanted), "{wanted} is a handle: {names:?}");
    }
    // The scale menu names every item `drawing:scale:<n>_<m>` (and `drawing:scale:auto`), as the
    // control-protocol docs say; the generic `dialog:list:` name stays beside it.
    h.step(&serde_json::json!({"click":{"handle":"drawing:scale"}})).unwrap();
    h.frames(3);
    let names: Vec<String> = crate::scenario::handles("drawing:scale").into_iter().map(|(name, _)| name).collect();
    assert!(names.iter().any(|name| name == "drawing:scale:auto"), "{names:?}");
    assert!(names.iter().any(|name| name == "drawing:scale:1_2"), "{names:?}");
    assert!(crate::scenario::handle_at("dialog:list:drawing_scale:1_2").is_some(), "the generic list name stays");
}

// ---------------------------------------------------------------- D9.2: the Section tool

/// The flywheel of the D9 tests (Ø120 x 35, Ø90 recess, Ø40 boss, Ø16 bore).
fn flywheel() -> Harness {
    let mut h = Harness::new();
    let steps: [(&str, serde_json::Value); 4] = [
        ("sketch.create", serde_json::json!({"plane":"XZ","name":"FlyProfile"})),
        ("sketch.line", serde_json::json!({"points":[[8,0],[60,0],[60,-20],[45,-20],[45,-10],[20,-10],[20,-35],[8,-35]],"closed":true})),
        ("sketch.finish", serde_json::json!({})),
        ("solid.revolve", serde_json::json!({"sketch":"FlyProfile","axis":"Z","angle":360,"body_name":"Flywheel"})),
    ];
    for (command, params) in steps {
        h.app.run(command, params).unwrap();
    }
    h
}

#[test]
fn a_screen_point_maps_to_the_end_views_millimetres_at_one_to_one_and_one_to_two() {
    use solidcraft_engine::drawing::ViewCentre;
    let mut h = flywheel();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(3);
    let sheet = h.app.drawing_workspace.view.sheet.clone().expect("the flywheel's sheet");
    assert_eq!(sheet.scale, (1, 2), "the flywheel is drawn at 1:2");
    // The end view is the sheet's view that is not the longitudinal one.
    let front = sheet.views.iter().find(|v| v.view == StandardView::Front).expect("front");
    let end = sheet.views.iter().find(|v| v.view == StandardView::Top).expect("the end view");
    let centre = h.app.drawing_workspace.view.projected.as_ref().unwrap().iter().find(|v| v.view == StandardView::Top).and_then(|v| {
        v.centre.iter().find_map(|c| match c {
            ViewCentre::Cross { c, .. } => Some(*c),
            _ => None,
        })
    });
    let centre = centre.expect("the end view's centre cross");
    let rect = h.app.viewport.rect.expect("the page");
    let map = PageMap::new(rect, h.app.drawing_workspace.view.cam.cam).expect("a page map");
    for (scale, sheet_scale) in [(1.0_f64, (1u32, 1u32)), (0.5, (1, 2))] {
        // The same known feature, placed with the view's origin at a different scale.
        let shift = solidcraft_engine::geom::Vec2::new(7.0, -3.0);
        let placed = super::PlacedView {
            view: end.view,
            origin: end.origin + shift,
            bbox: [end.bbox[0] + shift, end.bbox[1] + shift],
            centre_lines: end.centre_lines,
        };
        let _ = front;
        let at = view_to_screen(map, &placed, scale, centre);
        // A hand never lands exactly: a rounding offset is well inside 0.05 mm at both scales.
        let picked = at + egui::vec2(0.05, -0.05);
        let got = screen_to_view(map, &placed, scale, picked);
        assert!(got.dist(centre) <= 0.05, "at 1:{} the picked point maps to {got:?}, want {centre:?}", (1.0 / scale) as u32);
        // The placed view's scale really is what the mapping uses: the same screen offset is
        // twice as many view millimetres on a 1:2 sheet as on a 1:1 sheet.
        let offset = egui::vec2(20.0, -20.0);
        let at_one_to_one = view_to_screen(map, &placed, 1.0, centre);
        let at_zero = screen_to_view(map, &placed, 1.0, at_one_to_one);
        let at_offset = screen_to_view(map, &placed, 1.0, at_one_to_one + offset);
        let two_zero = screen_to_view(map, &placed, 0.5, at_one_to_one);
        let two_offset = screen_to_view(map, &placed, 0.5, at_one_to_one + offset);
        let one_to_one = at_offset - at_zero;
        let one_to_two = two_offset - two_zero;
        assert!(
            (one_to_two.x - 2.0 * one_to_one.x).abs() <= 1e-6 && (one_to_two.y - 2.0 * one_to_one.y).abs() <= 1e-6,
            "the sheet scale is part of the transform: {one_to_one:?} vs {one_to_two:?}"
        );
        let _ = sheet_scale;
    }
    // The sheet's own placed view and 1:2 scale round-trip too.
    let got = screen_to_view(map, end, sheet.scale_factor(), view_to_screen(map, end, sheet.scale_factor(), centre));
    assert!(got.dist(centre) <= 0.05, "the sheet's end view maps back to {got:?}, want {centre:?}");
    let _ = h;
}

#[test]
fn a_picked_cut_snaps_to_a_centre_line_a_circle_centre_and_horizontal() {
    use solidcraft_engine::drawing::ViewCentre;
    let centres = [
        ViewCentre::Cross { c: solidcraft_engine::geom::Vec2::new(10.0, 20.0), radius: 4.0 },
        ViewCentre::Line { a: solidcraft_engine::geom::Vec2::new(0.0, 0.0), b: solidcraft_engine::geom::Vec2::new(40.0, 0.0) },
    ];
    let point = |x: f64, y: f64| solidcraft_engine::geom::Vec2::new(x, y);
    // 2 mm on paper at 1:2 is 4 view mm; at 1:1 it is 2 view mm.
    assert_eq!(snap_to_centres(point(11.5, 6.0), &centres, 4.0), point(10.0, 6.0), "a vertical centre line");
    assert_eq!(snap_to_centres(point(7.0, 1.7), &centres, 2.0), point(7.0, 0.0), "a horizontal centre line");
    assert_eq!(snap_to_centres(point(9.1, 19.2), &centres, 2.0), point(10.0, 20.0), "a circle centre");
    assert_eq!(snap_to_centres(point(13.0, 23.0), &centres, 2.0), point(13.0, 23.0), "nothing near");
    // 3°: 1 in 19 still snaps, 1 in 10 does not.
    assert_eq!(snap_direction(point(0.0, 0.0), point(25.0, 1.0)), point(25.0, 0.0), "nearly horizontal");
    assert_eq!(snap_direction(point(0.0, 0.0), point(-1.0, 25.0)), point(0.0, 25.0), "nearly vertical");
    assert_eq!(snap_direction(point(0.0, 0.0), point(10.0, 1.0)), point(10.0, 1.0), "10° is not within 3°");
}

/// The acceptance shaft with a Ø8 × 20 blind end bore and a Ø6 cross hole 55 mm from the datum.
fn featured_shaft() -> Harness {
    let mut h = Harness::new();
    let steps: [(&str, serde_json::Value); 6] = [
        ("sketch.create", serde_json::json!({"plane":"XY","name":"Profile"})),
        ("sketch.line", serde_json::json!({"points":[[0,0],[0,10],[30,10],[30,14],[80,14],[80,8],[120,8],[120,0]],"closed":true})),
        ("sketch.finish", serde_json::json!({})),
        ("solid.revolve", serde_json::json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"})),
        (
            "solid.hole",
            serde_json::json!({"position":[120,0,0],"direction":[-1,0,0],"diameter":8,"depth":20,"type":"drilled","tip_angle":118,"targets":["Shaft"]}),
        ),
        ("solid.cylinder", serde_json::json!({"base":[55,0,-20],"axis":[0,0,1],"radius":3,"height":40,"operation":"cut","targets":["Shaft"]})),
    ];
    for (command, params) in steps {
        h.app.run(command, params).unwrap();
    }
    h
}

#[test]
fn the_first_edit_materialises_the_automatic_sections_before_adding_one() {
    let mut h = featured_shaft();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    let cuts = view.cuts.clone();
    assert_eq!(cuts.len(), 2, "the shaft's blind bore and cross hole are broken out: {cuts:?}");
    assert!(cuts.iter().all(|c| c.boundary.is_some()), "both are break-outs");
    let entries = materialise_auto(view);
    assert_eq!(entries.len(), 2, "the automatic sections become explicit entries: {entries:?}");
    assert_eq!(assigned_letters(&entries).unwrap(), vec!['A', 'B']);
    for (entry, cut) in entries.iter().zip(&cuts) {
        let SectionSpec::Breakout { around, view: breakout, .. } = entry else { panic!("a break-out: {entry:?}") };
        let boundary = cut.boundary.as_ref().expect("a window");
        assert_eq!(*breakout, cut.view);
        assert!((around[0] - boundary.centre.x).abs() <= 1e-9 && (around[1] - boundary.centre.y).abs() <= 1e-9);
        assert!((around[2] - boundary.rx.min(boundary.ry)).abs() <= 1e-9, "the smaller half axis");
    }
    // Writing them keeps the sections on the page.
    set_section_spec(&mut h.app, SectionMode::Explicit(entries.clone()));
    wait_for_build(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    assert_eq!(view.sections, SectionMode::Explicit(entries));
    let sheet = view.sheet.as_ref().expect("the rebuilt sheet");
    assert!(!sheet.sections.is_empty(), "the break-outs stay on the page");
    assert!(view.error.is_none(), "{:?}", view.error);
}

#[test]
fn deleting_a_section_removes_exactly_one_entry_and_keeps_the_other_letter() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let two = SectionMode::Explicit(vec![
        SectionSpec::Full {
            view: StandardView::Front,
            plane: solidcraft_engine::drawing::CuttingPlane { point: Vec3::new(20.0, 10.0, 5.0), normal: Vec3::Y },
            letter: Some('A'),
        },
        SectionSpec::Full {
            view: StandardView::Top,
            plane: solidcraft_engine::drawing::CuttingPlane { point: Vec3::new(20.0, 10.0, 5.0), normal: Vec3::Z },
            letter: Some('B'),
        },
    ]);
    set_section_spec(&mut h.app, two);
    wait_for_build(&mut h.app);
    let undo = h.app.session.undo.len();
    delete_section(&mut h.app, 'A');
    wait_for_build(&mut h.app);
    let SectionMode::Explicit(specs) = &h.app.drawing_workspace.view.sections else { panic!("explicit") };
    assert_eq!(specs.len(), 1, "exactly one entry went");
    assert_eq!(assigned_letters(specs).unwrap(), vec!['B'], "no re-lettering");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step for the delete");
    let stored = stored_sections(&h.app.session.doc).expect("stored");
    assert_eq!(
        assigned_letters(&match stored {
            SectionMode::Explicit(s) => s,
            _ => panic!("explicit"),
        })
        .unwrap(),
        vec!['B']
    );
    // Undo brings the deleted section back.
    h.app.run("edit.undo", serde_json::json!({})).unwrap();
    h.frames(3);
    let stored = stored_sections(&h.app.session.doc).expect("stored");
    assert_eq!(
        assigned_letters(&match stored {
            SectionMode::Explicit(s) => s,
            _ => panic!("explicit"),
        })
        .unwrap(),
        vec!['A', 'B']
    );
}

#[test]
fn escape_cancels_a_gesture_without_writing_the_spec() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let undo = h.app.session.undo.len();
    h.step(&serde_json::json!({"click": {"handle": "drawing:section:full"}})).unwrap();
    h.frames(2);
    assert!(h.app.drawing_workspace.view.section_tool, "the Section tool is on");
    // Pick the front view, then click at a second point: the gesture waits for its second point.
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front"}})).unwrap();
    h.frames(2);
    assert_eq!(h.app.drawing_workspace.view.picked_view, Some(StandardView::Front));
    let at = crate::scenario::handle_at("drawing:view:front").expect("the view's handle");
    h.call("ui.drag", serde_json::json!({"x0":at.x + 20.0,"y0":at.y,"x1":at.x + 20.0,"y1":at.y,"steps":1,"hold":5}));
    h.frames(2);
    assert!(h.app.drawing_workspace.view.gesture.is_some(), "the first click armed the gesture");
    h.step(&serde_json::json!({"key": "Escape"})).unwrap();
    h.frames(2);
    assert!(h.app.drawing_workspace.view.gesture.is_none(), "Escape cancelled the gesture");
    assert!(h.app.drawing_workspace.view.section_tool, "the tool stays on after the first Escape");
    assert_eq!(h.app.session.undo.len(), undo, "nothing was written");
    assert!(stored_sections(&h.app.session.doc).is_none(), "no stored spec");
    assert!(matches!(h.app.drawing_workspace.view.sections, SectionMode::Auto));
    // A second Escape leaves the tool.
    h.step(&serde_json::json!({"key": "Escape"})).unwrap();
    h.frames(2);
    assert!(!h.app.drawing_workspace.view.section_tool);
}

#[test]
fn a_section_edit_undone_while_away_rebuilds_the_page_on_return() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let mode = SectionMode::Explicit(vec![SectionSpec::Full {
        view: StandardView::Front,
        plane: solidcraft_engine::drawing::CuttingPlane { point: Vec3::new(20.0, 10.0, 5.0), normal: Vec3::Y },
        letter: Some('A'),
    }]);
    set_section_spec(&mut h.app, mode);
    wait_for_build(&mut h.app);
    assert_eq!(h.app.drawing_workspace.view.sections.letters(), vec!['A']);
    // Leave Drawing, undo the section edit in Design, and come back: the stored spec changed
    // without a geometry revision, so only the spec comparison can notice it.
    enter(&mut h.app, false).unwrap();
    h.app.run("edit.undo", serde_json::json!({})).unwrap();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    assert!(matches!(view.sections, SectionMode::Auto), "the undone spec is auto again: {:?}", view.sections);
    assert_eq!(view.sheet.as_ref().map_or(1, |s| s.sections.len()), 0, "the box has no stored section");
    assert!(view.error.is_none(), "{:?}", view.error);
}

#[test]
fn a_full_section_can_be_placed_with_two_clicks() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:section:full"}})).unwrap();
    h.frames(2);
    // Pick the front view, click its centre (the first point), then its top edge midpoint.
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front"}})).unwrap();
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front:center"}})).unwrap();
    h.frames(2);
    assert!(h.app.drawing_workspace.view.gesture.is_some(), "the first click armed the line");
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front:top"}})).unwrap();
    wait_for_build(&mut h.app);
    let view = &h.app.drawing_workspace.view;
    assert!(matches!(view.sections, SectionMode::Explicit(_)), "{:?}", view.sections);
    assert_eq!(view.sections.letters(), vec!['A']);
    let sheet = view.sheet.as_ref().expect("the rebuilt sheet");
    assert_eq!(sheet.sections.len(), 1);
    assert!(sheet.sections[0].full);
    assert_eq!(sheet.sections[0].label, Some('A'));
    assert!(!sheet.sections[0].hatch.is_empty(), "the section is hatched");
    assert!(view.error.is_none(), "{:?}", view.error);
}

#[test]
fn a_cut_no_view_looks_along_writes_nothing_and_keeps_the_tool_active() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    let undo = h.app.session.undo.len();
    h.step(&serde_json::json!({"click": {"handle": "drawing:section:full"}})).unwrap();
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front"}})).unwrap();
    h.frames(2);
    let at = crate::scenario::handle_at("drawing:view:front:center").expect("the view's centre");
    // A diagonal line in the front view cuts along a plane no standard view looks along.
    h.call("ui.drag", serde_json::json!({"x0":at.x,"y0":at.y,"x1":at.x+80.0,"y1":at.y+80.0,"steps":8}));
    h.frames(3);
    let view = &h.app.drawing_workspace.view;
    assert!(view.gesture.is_none(), "the failed gesture is over");
    assert!(view.section_tool, "the tool stays active");
    assert!(matches!(view.sections, SectionMode::Auto), "nothing was written: {:?}", view.sections);
    assert!(stored_sections(&h.app.session.doc).is_none());
    assert_eq!(h.app.session.undo.len(), undo);
    assert!(
        h.app.status.as_ref().is_some_and(|(text, _, _)| text.contains("no view looks along this cutting plane")),
        "the engine's error is in the status line: {:?}",
        h.app.status
    );
}

#[test]
fn a_title_edit_and_a_scale_change_keep_the_placed_section() {
    let mut h = boxed();
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    h.frames(2);
    // Place A-A by clicking, through the real tool.
    h.step(&serde_json::json!({"click": {"handle": "drawing:section:full"}})).unwrap();
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front"}})).unwrap();
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front:center"}})).unwrap();
    h.frames(2);
    h.step(&serde_json::json!({"click": {"handle": "drawing:view:front:top"}})).unwrap();
    wait_for_build(&mut h.app);
    let placed = stored_sections(&h.app.session.doc).expect("the placed A-A is stored");
    let undo = h.app.session.undo.len();

    // The side panel writes the whole title object when an editor loses focus.
    h.app.drawing_workspace.view.title.werkstoff = "C45".into();
    title_changed(&mut h.app);
    save_title(&mut h.app);
    assert_eq!(stored_sections(&h.app.session.doc), Some(placed.clone()), "the title edit kept the stored sections");
    assert!(h.app.session.undo.len() > undo, "the title edit is one undo step");

    // Changing the scale writes the title object again (the panel marks it dirty), and the
    // section must still be in the stored spec and on the page.
    set_scale(&mut h.app, Scale::Fixed(1, 1));
    save_title(&mut h.app);
    h.frames(3);
    wait_for_build(&mut h.app);
    assert_eq!(stored_sections(&h.app.session.doc), Some(placed.clone()), "the scale change kept the stored sections");
    let view = &h.app.drawing_workspace.view;
    assert!(view.error.is_none(), "{:?}", view.error);
    assert_eq!(view.sections, placed, "the editor still holds the placed spec");
    let sheet = view.sheet.as_ref().expect("the rebuilt sheet");
    assert_eq!(sheet.sections.len(), 1, "A-A is still on the page");
    assert_eq!(sheet.sections[0].label, Some('A'));
    assert!(!sheet.sections[0].hatch.is_empty(), "and still hatched");
    // The title value was stored as the panel sent it.
    let material = h.app.session.doc.metadata.get("app.solidcraft.drawing").and_then(|v| v.get("werkstoff"));
    assert_eq!(material, Some(&serde_json::json!("C45")));
}

#[test]
fn the_tangent_edges_rule_is_one_undo_step_and_the_page_uses_the_stored_value() {
    let mut h = boxed();
    assert_eq!(tangent_edges_of(&h.app), TangentEdges::Omit, "omit is the default");
    set_tangent_edges(&mut h.app, TangentEdges::Thin);
    assert_eq!(tangent_edges_of(&h.app), TangentEdges::Thin, "the ribbon's choice is stored");
    h.app.run("edit.undo", serde_json::json!({})).unwrap();
    assert_eq!(tangent_edges_of(&h.app), TangentEdges::Omit, "one undo restores the default");
    // The page's build reads the stored rule, so entering Drawing after setting it builds thin.
    set_tangent_edges(&mut h.app, TangentEdges::Thin);
    enter(&mut h.app, true).unwrap();
    wait_for_build(&mut h.app);
    assert!(h.app.drawing_workspace.view.error.is_none());
}
