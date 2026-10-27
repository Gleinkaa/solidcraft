use super::*;
use solidcraft_geom::Vec2;
use solidcraft_render::StandardView;

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap()
}

fn view<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["views"].as_array().and_then(|all| all.iter().find(|v| v["view"] == name)).unwrap()
}

fn bbox(view: &Value) -> (Vec2, Vec2) {
    let b = view["bbox"].as_array().unwrap();
    let point = |i: usize| -> Vec2 {
        let p = b[i].as_array().unwrap();
        Vec2::new(p[0].as_f64().unwrap(), p[1].as_f64().unwrap())
    };
    (point(0), point(1))
}

fn size(view: &Value) -> Vec2 {
    let (lo, hi) = bbox(view);
    hi - lo
}

#[test]
fn drawing_views_measure_a_40_20_10_box_in_front_top_and_right() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let result = run(&mut s, "drawing.views", json!({}));
    let all = result["views"].as_array().unwrap();
    assert_eq!(all.len(), 3);
    let names: Vec<&str> = all.iter().filter_map(|v| v["view"].as_str()).collect();
    assert_eq!(names, ["front", "top", "right"]);
    for (name, expected) in [("front", Vec2::new(40., 10.)), ("top", Vec2::new(40., 20.)), ("right", Vec2::new(20., 10.))] {
        let v = view(&result, name);
        let d = size(v);
        assert!((d.x - expected.x).abs() < 1e-6 && (d.y - expected.y).abs() < 1e-6, "{name} spans {d:?}, expected {expected:?}");
        let (lo, hi) = bbox(v);
        // The camera looks at the world origin: a box from (0,0,0) starts at (0,0) in every view.
        assert!(lo.dist(Vec2::ZERO) < 1e-6, "{name} min {lo:?}");
        assert!((hi - expected).len() < 1e-6, "{name} max {hi:?}");
        assert!(v["hidden"].as_array().unwrap().is_empty(), "{name}: a box's rear edges lie under its front ones");
        assert!(!v["visible"].as_array().unwrap().is_empty(), "{name}");
        let camera = &v["camera"];
        assert_eq!(camera["target"], json!([0.0, 0.0, 0.0]), "{name}");
        assert_eq!(camera["fov"], 0.0, "{name} is orthographic");
    }
    assert_eq!(result["view_choice"], "general", "a plain box keeps front, top and right");
    assert_eq!(result["stats"]["views"], 3);
    assert_eq!(result["stats"]["hidden"], true);
    assert!(result["stats"]["visible_segments"].as_u64().unwrap() > 0);
}

#[test]
fn drawing_views_show_a_through_hole_as_hidden_lines_in_the_side_view() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    run(&mut s, "solid.cylinder", json!({"radius":4,"height":12,"base":[20,10,-1],"operation":"cut","targets":["Plate"]}));
    let result = run(&mut s, "drawing.views", json!({"views":["right"]}));
    let right = view(&result, "right");
    let hidden = right["hidden"].as_array().unwrap();
    assert!(!hidden.is_empty(), "the bore behind the side face is hidden: {result}");
    assert!(!right["visible"].as_array().unwrap().is_empty());
    let d = size(right);
    assert!((d.x - 20.).abs() < 1e-6 && (d.y - 10.).abs() < 1e-6, "{d:?}");
    let (lo, hi) = bbox(right);
    for segment in hidden {
        let segment = segment.as_array().unwrap();
        let values: Vec<f64> = segment.iter().filter_map(Value::as_f64).collect();
        assert_eq!(values.len(), 4, "{segment:?}");
        assert!(values[0] >= lo.x - 1e-6 && values[2] >= lo.x - 1e-6 && values[1] >= lo.y - 1e-6 && values[3] >= lo.y - 1e-6);
        assert!(values[0] <= hi.x + 1e-6 && values[2] <= hi.x + 1e-6 && values[1] <= hi.y + 1e-6 && values[3] <= hi.y + 1e-6);
    }
    let omitted = run(&mut s, "drawing.views", json!({"views":["right"],"hidden":false}));
    let right = view(&omitted, "right");
    assert!(right["hidden"].as_array().unwrap().is_empty());
    let d = size(right);
    assert!((d.x - 20.).abs() < 1e-6 && (d.y - 10.).abs() < 1e-6, "the bbox still covers hidden geometry: {d:?}");
}

#[test]
fn drawing_views_leaves_the_document_and_undo_stack_unchanged() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let spec = crate::cmd::find_command("drawing.views").unwrap();
    assert!(!spec.undoable);
    let doc = s.doc.clone();
    let (undo, redo, revision) = (s.undo.len(), s.redo.len(), s.revision);
    let result = run(&mut s, "drawing.views", json!({"views":["front","top","right","back","bottom","left"]}));
    assert_eq!(result["views"].as_array().unwrap().len(), 6);
    assert!(std::sync::Arc::ptr_eq(&doc, &s.doc));
    assert_eq!(s.undo.len(), undo);
    assert_eq!(s.redo.len(), redo);
    assert_eq!(s.revision, revision);
}

#[test]
fn drawing_views_rejects_unknown_views_and_bad_options_without_panicking() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    for p in [
        json!({"views":["side"]}),
        json!({"views":["front","iso"]}),
        json!({"views":"front"}),
        json!({"views":[1]}),
        json!({"views":[null]}),
        json!({"views":[["front"]]}),
        json!({"views":[true]}),
        json!({"tolerance":-0.001}),
        json!({"tolerance":0}),
        json!({"tolerance":1.5}),
        json!({"tolerance":"0.01"}),
        json!({"tolerance":null}),
        json!({"tolerance":1e308}),
        json!({"hidden":"yes"}),
        json!({"hidden":1}),
        json!({"hidden":null}),
    ] {
        assert!(s.execute(CMD, &p).is_err(), "accepted {p}");
    }
    let many = vec!["front"; 65];
    assert!(s.execute(CMD, &json!({"views": many})).is_err());
    // Refused calls leave the command usable; an explicitly empty list is not an error.
    let empty = run(&mut s, "drawing.views", json!({"views":[]}));
    assert!(empty["views"].as_array().unwrap().is_empty());
    assert!(run(&mut s, "drawing.views", json!({"tolerance":0.0001})).get("views").is_some());

    let mut blank = Session::default();
    assert!(blank.execute(CMD, &json!({})).is_err(), "there is nothing visible to project");
}

#[test]
fn drawing_views_projects_any_standard_view_through_the_public_seam() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let opts = crate::drawing::ViewOptions::default();
    let projected = crate::drawing::project_views(&s, &[StandardView::Front, StandardView::Iso], &opts).unwrap();
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].camera, crate::drawing::standard_camera(StandardView::Front));
    assert_eq!(projected[0].camera.target, solidcraft_geom::Vec3::ZERO);
    assert!(projected[0].hidden.is_empty());
    assert!(!projected[1].visible.is_empty(), "the iso view still draws the box");
    assert_eq!(crate::drawing::view_name(StandardView::Bottom), "bottom");
    assert!(
        crate::drawing::project_views(
            &s,
            &[StandardView::Front],
            &crate::drawing::ViewOptions { tolerance: 0.0, hidden: true, tangent: crate::drawing::TangentEdges::Draw, curves: true }
        )
        .is_err()
    );
}

#[test]
fn drawing_views_include_shown_sketch_profiles_on_the_view() {
    let mut s = Session::default();
    run(&mut s, "sketch.create", json!({"plane":"XY"}));
    run(&mut s, "sketch.rectangle.two_point", json!({"p0":[0,0],"p1":[60,40]}));
    let result = run(&mut s, "drawing.views", json!({"views":["top"]}));
    let top = view(&result, "top");
    assert_eq!(top["visible"].as_array().unwrap().len(), 4);
    assert!(top["hidden"].as_array().unwrap().is_empty());
    let (lo, hi) = bbox(top);
    assert!(lo.dist(Vec2::ZERO) < 1e-9, "{lo:?}");
    assert!((hi - Vec2::new(60., 40.)).len() < 1e-9, "{hi:?}");
}

/// A stepped shaft revolved about X, 120 mm long, up to Ø28.
fn shaft(s: &mut Session) {
    run(s, "sketch.create", json!({"plane":"XY","name":"Profile"}));
    run(s, "sketch.line", json!({"points":[[0,0],[0,10],[30,10],[30,14],[80,14],[80,8],[120,8],[120,0]],"closed":true}));
    run(s, "sketch.finish", json!({}));
    run(s, "solid.revolve", json!({"sketch":"Profile","axis":"x","angle":360,"body_name":"Shaft"}));
}

#[test]
fn drawing_sheet_chooses_a_turned_parts_views_and_reports_its_centre_lines() {
    let mut s = Session::default();
    shaft(&mut s);
    let result = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(result["view_choice"], "turned");
    let views = result["views"].as_array().unwrap();
    assert_eq!(views.len(), 2, "a turned part gets two views: {result}");
    assert_eq!((views[0]["view"].as_str(), views[1]["view"].as_str()), (Some("front"), Some("right")));
    assert!(views[0]["centre_lines"].as_u64().unwrap() >= 1, "the axis is a centre line: {result}");
    assert!(views[1]["centre_lines"].as_u64().unwrap() >= 2, "the end view carries a cross: {result}");
    assert!(result["layers"]["CENTER"].as_u64().unwrap() >= 3, "{result}");

    // "auto" is the same as leaving the parameter out.
    let auto = run(&mut s, "drawing.sheet", json!({"views": "auto"}));
    assert_eq!(auto["view_choice"], "turned");
    assert_eq!(auto["views"].as_array().unwrap().len(), 2);
    // An explicit list is used as given, and says so.
    let explicit = run(&mut s, "drawing.sheet", json!({"views":["front","top","right"]}));
    assert_eq!(explicit["view_choice"], "explicit");
    assert_eq!(explicit["views"].as_array().unwrap().len(), 3);
    // The projection command reports the same choice.
    let projected = run(&mut s, "drawing.views", json!({}));
    assert_eq!(projected["view_choice"], "turned");
    assert_eq!(projected["views"].as_array().unwrap().len(), 2);
    assert!(projected["views"][1]["hidden"].as_array().is_some_and(|h| !h.is_empty()), "the end view has hidden edges");
    // `hidden: false` still drops them (the choice itself is made with them).
    let visible_only = run(&mut s, "drawing.views", json!({"hidden": false}));
    assert!(visible_only["views"].as_array().unwrap().iter().all(|v| v["hidden"].as_array().is_some_and(Vec::is_empty)));
    assert_eq!(visible_only["view_choice"], "turned");
}

fn temp_path(tag: &str, extension: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("solidcraft-drawing-{tag}-{}-{nanos}.{extension}", std::process::id()))
}

fn temp_dxf(tag: &str) -> std::path::PathBuf {
    temp_path(tag, "dxf")
}

#[test]
fn drawing_export_writes_the_dxf_and_leaves_the_document_and_undo_unchanged() {
    let path = temp_dxf("export");
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let spec = crate::cmd::find_command("drawing.export").unwrap();
    assert!(!spec.undoable, "drawing.export must not create an undo step");
    let doc = s.doc.clone();
    let (undo, redo, revision) = (s.undo.len(), s.redo.len(), s.revision);
    let result = run(&mut s, "drawing.export", json!({"path": path.to_string_lossy()}));
    assert_eq!(result["path"], path.to_string_lossy().as_ref());
    assert_eq!(result["format"], "dxf");
    assert_eq!(result["scale"], "2:1");
    assert_eq!(result["views"].as_array().unwrap().len(), 3);
    assert!(result["entities"].as_u64().unwrap() > 40, "{result}");
    for view in result["views"].as_array().unwrap() {
        assert!(view["view"].is_string());
        assert_eq!(view["origin_mm"].as_array().unwrap().len(), 2);
        assert_eq!(view["bbox_mm"].as_array().unwrap().len(), 2);
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("$INSUNITS"));
    assert!(text.contains("HIDDEN"));
    assert!(text.contains("FRAME"));
    assert!(std::sync::Arc::ptr_eq(&doc, &s.doc), "the document changed");
    assert_eq!(s.undo.len(), undo);
    assert_eq!(s.redo.len(), redo);
    assert_eq!(s.revision, revision);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn drawing_export_honours_an_explicit_scale_and_rejects_one_that_does_not_fit() {
    let path = temp_dxf("scale");
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let too_large = s.execute(EXPORT, &json!({"path": path.to_string_lossy(), "scale": "10:1"})).unwrap_err().to_string();
    assert!(too_large.contains("does not fit"), "{too_large}");
    assert!(!path.exists(), "a refused scale must not leave a file");
    let one_to_one = run(&mut s, "drawing.export", json!({"path": path.to_string_lossy(), "scale": "1:1"}));
    assert_eq!(one_to_one["scale"], "1:1");
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn drawing_export_writes_the_pdf_and_infers_the_format_from_the_extension() {
    let path = temp_path("pdf", "pdf");
    let path_s = path.to_string_lossy().to_string();
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let doc = s.doc.clone();
    let (undo, redo, revision) = (s.undo.len(), s.redo.len(), s.revision);
    // No `format`: the .pdf extension selects the PDF writer and the JSON is the DXF shape.
    let result = run(&mut s, "drawing.export", json!({"path": path_s, "title": {"benennung": "Plate"}}));
    assert_eq!(result["path"], path_s.as_str());
    assert_eq!(result["format"], "pdf");
    assert_eq!(result["scale"], "2:1");
    assert_eq!(result["views"].as_array().unwrap().len(), 3);
    assert!(result["entities"].as_u64().unwrap() > 40, "{result}");
    assert!(result["views"][0]["origin_mm"].is_array());
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.4\n%"));
    let file = String::from_utf8_lossy(&bytes);
    assert!(file.contains("/MediaBox [0 0 841.89 595.28]"), "A4 landscape in points");
    assert!(file.contains("/Title (Plate \\(2:1\\))"), "{file}");
    assert!(file.contains("0.7 w") && file.contains("0.5 w") && file.contains("0.25 w"));
    assert!(std::sync::Arc::ptr_eq(&doc, &s.doc), "the document changed");
    assert_eq!((s.undo.len(), s.redo.len(), s.revision), (undo, redo, revision));

    // An explicit format must agree with the extension.
    std::fs::remove_file(&path).unwrap();
    assert!(!path.exists());
    let explicit = run(&mut s, "drawing.export", json!({"path": path_s, "format": "pdf"}));
    assert_eq!(explicit["format"], "pdf");
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    let mismatch = s.execute(EXPORT, &json!({"path": path_s, "format": "dxf"})).unwrap_err().to_string();
    assert!(mismatch.contains("does not match"), "{mismatch}");
    assert!(!path.exists(), "a refused format must not write anything");
    let as_dxf = temp_dxf("pdf");
    let as_dxf_s = as_dxf.to_string_lossy().to_string();
    assert!(s.execute(EXPORT, &json!({"path": as_dxf_s, "format": "pdf"})).is_err());
    assert!(!as_dxf.exists());
    // An unknown or missing extension is an error, and nothing is written.
    for bad_path in ["no-extension", "drawing.step", "drawing.svg", "drawing.pdf.txt"] {
        assert!(s.execute(EXPORT, &json!({"path": bad_path})).is_err(), "accepted {bad_path}");
    }
}

#[test]
fn drawing_sheet_returns_scale_and_placements_without_writing_or_changing_the_document() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let spec = crate::cmd::find_command("drawing.sheet").unwrap();
    assert!(!spec.undoable);
    let doc = s.doc.clone();
    let (undo, redo, revision) = (s.undo.len(), s.redo.len(), s.revision);
    let result = run(&mut s, "drawing.sheet", json!({"scale":"2:1","title":{"benennung":"Plate","zeichnungsnummer":"P-01","werkstoff":"S235JR"}}));
    assert_eq!(result["paper"], "A4");
    assert_eq!(result["size_mm"], json!([297.0, 210.0]));
    assert_eq!(result["scale"], "2:1");
    assert_eq!(result["title"]["benennung"], "Plate");
    assert_eq!(result["title"]["zeichnungsnummer"], "P-01");
    assert_eq!(result["title"]["werkstoff"], "S235JR");
    assert!(result["prims"].as_u64().unwrap() > 40, "{result}");
    assert_eq!(result["views"].as_array().unwrap().len(), 3);
    assert_eq!(result["frame_mm"], json!([[20.0, 10.0], [287.0, 200.0]]));
    assert_eq!(result["title_block_mm"], json!([[107.0, 10.0], [287.0, 59.0]]));
    let names: Vec<&str> = result["views"].as_array().unwrap().iter().filter_map(|v| v["view"].as_str()).collect();
    assert_eq!(names, ["front", "top", "right"]);
    assert!(std::sync::Arc::ptr_eq(&doc, &s.doc));
    assert_eq!(s.undo.len(), undo);
    assert_eq!(s.redo.len(), redo);
    assert_eq!(s.revision, revision);
    // `drawing.sheet` writes nothing, so a path is not one of its parameters.
    assert!(s.execute("drawing.sheet", &json!({"path": temp_dxf("sheet").to_string_lossy()})).is_err());
}

#[test]
fn drawing_sheet_accepts_dimensions_and_notes_and_reports_them() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let dim = json!({
        "view": "front", "type": "linear", "p1": [0, 0], "p2": [40, 0], "angle": 0, "offset": -10
    });
    let dia = json!({
        "view": "front", "type": "diameter", "p1": [15, -5], "p2": [15, 5], "angle": 90, "offset": 0, "suffix": "h6"
    });
    let note = json!({
        "view": "front", "text": "Bohrung Ø3 H7, durchgehend", "at": [5, 30],
        "leader": [[38, 30], [30, 10]]
    });
    let result = run(&mut s, "drawing.sheet", json!({"dimensions": [dim, dia], "notes": [note]}));
    assert_eq!(result["dimensions"], 2);
    assert_eq!(result["notes"], 1);
    assert!(result["layers"]["DIMENSION"].as_u64().unwrap_or(0) > 0, "{result}");
    let dims = result["annotations"]["dimensions"].as_array().unwrap();
    assert_eq!(dims[0]["text"], "40");
    assert!((dims[0]["measured"].as_f64().unwrap() - 40.0).abs() < 1e-9);
    assert_eq!(dims[1]["text"], "Ø10 h6");
    let notes = result["annotations"]["notes"].as_array().unwrap();
    assert_eq!(notes[0]["text"], "Bohrung Ø3 H7, durchgehend");
    assert!(!notes[0]["leader"].as_array().unwrap().is_empty());
}

#[test]
fn drawing_sheet_takes_sections_auto_none_and_an_explicit_list() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    // Auto: a plain box is neither turned nor hollow; no section.
    let auto = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(auto["section_mode"], "auto");
    assert_eq!(auto["sections"].as_array().unwrap().len(), 0);
    let none = run(&mut s, "drawing.sheet", json!({"sections": "none"}));
    assert_eq!(none["section_mode"], "none");
    // An explicit full section through the box's middle: one cut region with hatching.
    let full =
        run(&mut s, "drawing.sheet", json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [20, 10, 5], "normal": [0, 1, 0]}}]}));
    assert_eq!(full["section_mode"], "explicit");
    assert_eq!(full["sections"][0]["type"], "full");
    assert_eq!(full["sections"][0]["label"], "A");
    assert_eq!(full["sections"][0]["regions"], 1);
    assert!(full["sections"][0]["hatch_lines"].as_u64().unwrap_or(0) > 0, "{full}");
    // A break-out around the middle of the front view.
    let breakout = run(&mut s, "drawing.sheet", json!({"sections": [{"type": "breakout", "view": "front", "around": [20, 5, 6]}]}));
    assert_eq!(breakout["sections"][0]["type"], "breakout");
    assert_eq!(breakout["sections"][0]["label"], serde_json::Value::Null);
    assert!(breakout["sections"][0]["regions"].as_u64().unwrap_or(0) >= 1, "{breakout}");
    // Hostile shapes are refused and the command stays usable.
    for bad in [
        json!({"sections": "x"}),
        json!({"sections": 5}),
        json!({"sections": {}}),
        json!({"sections": [{"type": "full", "view": "front"}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0], "normal": [0, 1, 0]}}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0, 0], "normal": [0, 0, 0]}}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0, 0], "normal": [1, 0, 0]}}]}),
        json!({"sections": [{"type": "nope", "view": "front"}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, -1]}]}),
        json!({"sections": [{"type": "breakout", "view": "nope", "around": [0, 0, 1]}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "x": 1}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0, 0], "normal": [0, 1, 0]}}, {"type": "breakout", "view": "front", "around": [0, 0, 1]}]}),
    ] {
        assert!(s.execute("drawing.sheet", &bad).is_err(), "accepted {bad}");
    }
    assert!(s.execute("drawing.sheet", &json!({})).is_ok(), "the command stays usable");
}

#[test]
fn drawing_export_writes_real_dimension_entities_and_leaders() {
    let path = temp_dxf("dims");
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let result = run(
        &mut s,
        "drawing.export",
        json!({
            "path": path.to_string_lossy(),
            "dimensions": [
                {"view": "front", "type": "linear", "p1": [0, 0], "p2": [40, 0], "angle": 0, "offset": -10},
                {"view": "front", "type": "diameter", "p1": [15, -5], "p2": [15, 5], "angle": 90, "offset": 0, "suffix": "h6"}
            ],
            "notes": [{"view": "front", "text": "Bohrung Ø3 H7", "at": [5, 30], "leader": [[38, 30], [30, 10]]}]
        }),
    );
    assert_eq!(result["dimensions"], 2);
    assert_eq!(result["notes"], 1);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("SolidCraft ISO"), "the DIMSTYLE table is missing");
    assert_eq!(text.matches("  0\nDIMENSION\n").count(), 2, "two real DIMENSION entities");
    assert!(text.contains("*D1") && text.contains("*D2"), "anonymous dimension blocks");
    assert!(text.contains("LEADER"), "the note is a LEADER");
    assert!(text.contains("  8\nDIMENSION\n"), "dimensions and notes are on layer DIMENSION");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn drawing_export_rejects_hostile_parameters_without_panicking_or_writing() {
    let path = temp_dxf("hostile");
    let path_s = path.to_string_lossy().to_string();
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let long = "x".repeat(201);
    for p in [
        json!({}),
        json!({"path": ""}),
        json!({"path": "no-extension"}),
        json!({"path": "not-a-drawing.step"}),
        json!({"path": 123}),
        json!({"path": null}),
        json!({"path": path_s, "format": "svg"}),
        json!({"path": path_s, "format": "pdf"}),
        json!({"path": path_s, "format": 1}),
        json!({"path": path_s, "format": ""}),
        json!({"path": path_s, "scale": "2"}),
        json!({"path": path_s, "scale": "1-2"}),
        json!({"path": path_s, "scale": "0:1"}),
        json!({"path": path_s, "scale": "1:0"}),
        json!({"path": path_s, "scale": "1:2:3"}),
        json!({"path": path_s, "scale": "100001:1"}),
        json!({"path": path_s, "scale": 2}),
        json!({"path": path_s, "scale": true}),
        json!({"path": path_s, "scale": ""}),
        json!({"path": path_s, "title": "Plate"}),
        json!({"path": path_s, "title": 5}),
        json!({"path": path_s, "title": []}),
        json!({"path": path_s, "title": {"unknown": "x"}}),
        json!({"path": path_s, "title": {"benennung": 5}}),
        json!({"path": path_s, "title": {"benennung": ""}}),
        json!({"path": path_s, "title": {"benennung": "a\nb"}}),
        json!({"path": path_s, "title": {"benennung": long}}),
        json!({"path": path_s, "title": {"datum": "2026-10-23"}}),
        json!({"path": path_s, "title": {"blatt": "2"}}),
        json!({"path": path_s, "title": {"blatt": "2/1"}}),
        json!({"path": path_s, "title": {"werkstoff": "---"}}),
        json!({"path": path_s, "views": ["side"]}),
        json!({"path": path_s, "views": ["front", "front"]}),
        json!({"path": path_s, "views": ["front", "iso"]}),
        json!({"path": path_s, "views": "front"}),
        json!({"path": path_s, "views": [1]}),
        json!({"path": path_s, "views": ["front", "top", "right", "left", "bottom", "back", "front"]}),
        json!({"path": path_s, "tolerance": -1}),
        json!({"path": path_s, "tolerance": 0}),
        json!({"path": path_s, "tolerance": 2}),
        json!({"path": path_s, "tolerance": "0.01"}),
        json!({"path": path_s, "tolerance": null}),
        json!({"path": path_s, "dimensions": "x"}),
        json!({"path": path_s, "dimensions": {}}),
        json!({"path": path_s, "dimensions": [1]}),
        json!({"path": path_s, "dimensions": [{}]}),
        json!({"path": path_s, "dimensions": [{"view": "side", "type": "linear", "p1": [0, 0], "p2": [1, 0]}]}),
        json!({"path": path_s, "dimensions": [{"view": "bottom", "type": "linear", "p1": [0, 0], "p2": [1, 0]}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "nope", "p1": [0, 0], "p2": [1, 0]}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0], "p2": [1, 0]}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0], "angle": 1e308}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0], "offset": 1e308}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0], "text": long}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0], "text": "a\nb"}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0], "unknown": 1}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "radius", "centre": [0, 0], "radius": 0}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "radius", "centre": [0, 0], "radius": -2}]}),
        json!({"path": path_s, "dimensions": [{"view": "front", "type": "radius", "centre": [0, 0]}]}),
        json!({"path": path_s, "notes": "x"}),
        json!({"path": path_s, "notes": [1]}),
        json!({"path": path_s, "notes": [{}]}),
        json!({"path": path_s, "notes": [{"view": "side", "text": "x", "at": [0, 0], "leader": [[0, 0]]}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": "", "at": [0, 0], "leader": [[0, 0]]}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": long, "at": [0, 0], "leader": [[0, 0]]}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": "x", "at": [0], "leader": [[0, 0]]}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": "x", "at": [0, 0], "leader": []}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": "x", "at": [0, 0], "leader": [[0, 0]], "target": [1e308, 0]}]}),
        json!({"path": path_s, "notes": [{"view": "front", "text": "x", "at": [0, 0], "leader": [[0, 0]], "unknown": 2}]}),
    ] {
        assert!(s.execute(EXPORT, &p).is_err(), "accepted {p}");
        assert!(!path.exists(), "wrote a file for {p}");
    }
    // More than 500 dimensions or notes is refused before any file is written.
    let many_dims: Vec<Value> = (0..501).map(|_| json!({"view": "front", "type": "linear", "p1": [0, 0], "p2": [1, 0]})).collect();
    let many_notes: Vec<Value> = (0..501).map(|_| json!({"view": "front", "text": "x", "at": [0, 0], "leader": [[0, 0]]})).collect();
    assert!(s.execute(EXPORT, &json!({"path": path_s, "dimensions": many_dims})).is_err());
    assert!(s.execute(EXPORT, &json!({"path": path_s, "notes": many_notes})).is_err());
    assert!(!path.exists());
    for views in [json!("AUTO"), json!("Auto"), json!(" auto ")] {
        assert!(s.execute(EXPORT, &json!({"path": path_s, "views": views})).is_ok(), "auto is accepted as {views}");
        std::fs::remove_file(&path).ok();
    }
    let mut empty = Session::default();
    assert!(empty.execute(EXPORT, &json!({"path": path_s})).is_err(), "a design with nothing visible has no sheet");
    assert!(!path.exists());
    // Refused calls leave the commands usable.
    let ok = run(&mut s, "drawing.export", json!({"path": path_s, "title": {"benennung": "Plate", "datum": "23.10.2026"}}));
    assert_eq!(ok["scale"], "2:1");
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert!(sheet["prims"].as_u64().unwrap() > 0);
}

#[test]
fn automatic_dimensions_are_the_default_and_none_gives_the_bare_sheet() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let automatic = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(automatic["dimension_mode"], "auto");
    assert!(automatic["dimensions"].as_u64().unwrap_or(0) >= 3, "{automatic}");
    let sources = automatic["auto_dimensions"].as_array().unwrap();
    assert!(!sources.is_empty(), "{automatic}");
    assert!(sources.iter().all(|s| s["face"].is_u64() && s["value"].is_number() && s["text"].is_string()), "{automatic}");
    assert_eq!(automatic["unplaced"], json!([]));
    assert_eq!(automatic["undimensioned"], json!([]), "the box's features are all referenced");

    let none = run(&mut s, "drawing.sheet", json!({"dimensions":"none"}));
    assert_eq!(none["dimension_mode"], "none");
    assert_eq!(none["dimensions"], 0);
    assert_eq!(none["notes"], 0);
    assert_eq!(none["auto_dimensions"], json!([]));
    assert_eq!(none["undimensioned"], json!([]));

    let explicit =
        run(&mut s, "drawing.sheet", json!({"dimensions":[{"view":"front","type":"linear","p1":[0,0],"p2":[40,0],"angle":0,"offset":-12}]}));
    assert_eq!(explicit["dimension_mode"], "explicit");
    assert_eq!(explicit["dimensions"], 1);
    assert_eq!(explicit["auto_dimensions"], json!([]));
    assert_eq!(explicit["undimensioned"], json!([]));
    assert_eq!(explicit["annotations"]["dimensions"][0]["text"], "40");
    for bad in [json!("x"), json!(1), json!({})] {
        assert!(s.execute("drawing.sheet", &json!({"dimensions": bad})).is_err());
    }
}

#[test]
fn drawing_sheet_takes_the_new_title_fields_and_the_notes_block() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let result = run(
        &mut s,
        "drawing.sheet",
        json!({
            "title": {
                "gezeichnet": "K. Beispielmann", "datum": "23.10.2026",
                "geprueft": "A. Pruefer", "geprueft_datum": "24.10.2026",
                "aend": "B", "aend_datum": "24.10.2026", "firma": "Muster AG", "rohmass": "\u{00d8}28 x 120"
            },
            "notes_block": {"ra": "1,6", "edge_note": "Kanten gebrochen", "extra_notes": ["Fase 1 x 45\u{00b0}"]}
        }),
    );
    assert_eq!(result["title"]["gezeichnet"], "K. Beispielmann");
    assert_eq!(result["title"]["gezeichnet_datum"], "23.10.2026");
    assert_eq!(result["title"]["geprueft"], "A. Pruefer");
    assert_eq!(result["title"]["geprueft_datum"], "24.10.2026");
    assert_eq!(result["title"]["aend"], "B");
    assert_eq!(result["title"]["aend_datum"], "24.10.2026");
    assert_eq!(result["title"]["firma"], "Muster AG");
    assert_eq!(result["title"]["rohmass"], "\u{00d8}28 x 120");
    assert_eq!(result["notes_block"]["ra"], "1,6");
    assert_eq!(result["notes_block"]["edge_note"], "Kanten gebrochen");
    let lines: Vec<&str> = result["notes_block"]["lines"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(lines.contains(&"Oberfl\u{00e4}che Ra 1,6"), "{lines:?}");
    assert!(lines.contains(&"Kanten gebrochen"), "{lines:?}");
    assert!(lines.contains(&"Fase 1 x 45\u{00b0}"), "{lines:?}");
    assert!(lines.contains(&"Projektionsmethode 1"), "{lines:?}");
    // The notes block sits above the title block and is not empty.
    let tb = result["title_block_mm"].as_array().unwrap();
    let nb = result["notes_block_mm"].as_array().unwrap();
    assert_eq!(nb[0][0], tb[0][0], "left-aligned with the title block: {nb:?} {tb:?}");
    assert_eq!(nb[0][1], tb[1][1], "directly above the title block: {nb:?} {tb:?}");
    assert!(nb[1][1].as_f64().unwrap() > nb[0][1].as_f64().unwrap());
    // The defaults come from the design: the raw size is computed unless overridden.
    let auto = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(auto["title"]["rohmass"], "40 x 20 x 10");
    assert_eq!(auto["title"]["gezeichnet"], "SolidCraft");
    assert_eq!(auto["title"]["geprueft"], "", "an empty approved-by is stored empty and drawn as a dash");
    assert_eq!(auto["notes_block"]["ra"], "3,2");
}

#[test]
fn drawing_title_set_persists_through_save_and_reopen_and_one_undo_removes_it() {
    let dir = std::env::temp_dir().join(format!("solidcraft-drawing-title-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("7-10.1_Countershaft_Pulley_Shaft.solidcraft");
    let path_s = path.to_string_lossy().to_string();
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    s.doc_mut().name = "7-10.1_Countershaft_Pulley_Shaft".to_string();
    s.mark_saved();
    let undo = s.undo.len();
    let saved = run(
        &mut s,
        "drawing.title.set",
        json!({
            "benennung": "Countershaft Pulley Shaft",
            "zeichnungsnummer": "7-10.1",
            "gezeichnet": "K. Beispielmann", "firma": "Muster AG", "aend": "B",
            "geprueft": "A. Pruefer", "geprueft_datum": "24.10.2026",
            "rohmass": "\u{00d8}28 x 120", "blatt": "1/1",
            "notes_block": {"ra": "1,6", "edge_note": "Kanten gebrochen", "extra_notes": ["Fase 1 x 45\u{00b0}"]}
        }),
    );
    let changed: Vec<&str> = saved["changed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for key in ["author", "organization", "revision", "app.solidcraft.drawing"] {
        assert!(changed.contains(&key), "{key} is not stored: {changed:?}");
    }
    assert!(!changed.contains(&"description"), "a value equal to the computed default is not stored: {changed:?}");
    assert!(!changed.contains(&"part_number"), "the drawing-number token is already the default: {changed:?}");
    assert_eq!(s.undo.len(), undo + 1, "one undo step, through document.metadata.set");
    assert!(s.is_dirty());
    let metadata = run(&mut s, "document.metadata", json!({}));
    assert_eq!(metadata["metadata"]["author"], "K. Beispielmann");
    assert_eq!(metadata["metadata"]["app.solidcraft.drawing"]["ra"], "1,6");
    assert_eq!(metadata["metadata"]["app.solidcraft.drawing"]["extra_notes"], json!(["Fase 1 x 45\u{00b0}"]));

    // The sheet command reads them without any parameter.
    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["title"]["gezeichnet"], "K. Beispielmann");
    assert_eq!(sheet["title"]["firma"], "Muster AG");
    assert_eq!(sheet["title"]["aend"], "B");
    assert_eq!(sheet["title"]["rohmass"], "\u{00d8}28 x 120");
    assert_eq!(sheet["notes_block"]["ra"], "1,6");
    let lines: Vec<&str> = sheet["notes_block"]["lines"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(lines.contains(&"Oberfl\u{00e4}che Ra 1,6"), "{lines:?}");

    // Save, reopen, and the same values come back from the design.
    run(&mut s, "file.save_as", json!({"path": path_s, "format": "container"}));
    let mut reopened = Session::default();
    run(&mut reopened, "doc.open", json!({"path": path_s}));
    let sheet = run(&mut reopened, "drawing.sheet", json!({}));
    assert_eq!(sheet["title"]["benennung"], "Countershaft Pulley Shaft");
    assert_eq!(sheet["title"]["zeichnungsnummer"], "7-10.1");
    assert_eq!(sheet["title"]["gezeichnet"], "K. Beispielmann");
    assert_eq!(sheet["title"]["geprueft"], "A. Pruefer");
    assert_eq!(sheet["title"]["firma"], "Muster AG");
    assert_eq!(sheet["title"]["aend"], "B");
    assert_eq!(sheet["title"]["rohmass"], "\u{00d8}28 x 120");
    assert_eq!(sheet["notes_block"]["ra"], "1,6");
    assert_eq!(sheet["notes_block"]["edge_note"], "Kanten gebrochen");
    assert_eq!(sheet["notes_block"]["extra_notes"], json!(["Fase 1 x 45\u{00b0}"]));

    // One undo takes the whole change back (the nested metadata command's step).
    run(&mut s, "edit.undo", json!({}));
    assert!(s.doc.metadata.author().is_none());
    assert!(s.doc.metadata.get("app.solidcraft.drawing").is_none());
    assert!(s.is_dirty(), "the undo removes the change that was saved");
    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["title"]["gezeichnet"], "SolidCraft");
    assert_eq!(sheet["notes_block"]["ra"], "3,2");
    run(&mut s, "edit.redo", json!({}));
    assert_eq!(s.doc.metadata.author(), Some("K. Beispielmann"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drawing_title_set_stores_nothing_for_the_computed_defaults() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let undo = s.undo.len();
    let result = run(
        &mut s,
        "drawing.title.set",
        json!({
            "benennung": "Untitled", "zeichnungsnummer": "Untitled", "gezeichnet": "SolidCraft",
            "firma": "", "aend": "", "rohmass": "40 x 20 x 10", "werkstoff": "n. b.",
            "notes_block": {"ra": "3,2", "edge_note": "Werkst\u{00fc}ckkanten ISO 13715: -0,2", "extra_notes": []}
        }),
    );
    assert_eq!(result["changed"], json!([]));
    assert_eq!(s.undo.len(), undo, "no change, no undo step");
    assert!(s.doc.metadata.is_empty(), "{:?}", s.doc.metadata);
}

#[test]
fn drawing_title_set_rejects_hostile_parameters_without_panicking() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let long = "x".repeat(201);
    let undo = s.undo.len();
    for p in [
        json!([]),
        json!({"unknown": "x"}),
        json!({"benennung": 5}),
        json!({"benennung": ""}),
        json!({"benennung": long}),
        json!({"benennung": "a\nb"}),
        json!({"datum": "2026-10-23"}),
        json!({"gezeichnet_datum": "32.13.2026"}),
        json!({"geprueft_datum": "x"}),
        json!({"aend_datum": 5}),
        json!({"blatt": "2"}),
        json!({"blatt": "2/1"}),
        json!({"werkstoff": "---"}),
        json!({"rohmass": "a\tb"}),
        json!({"notes_block": "x"}),
        json!({"notes_block": 5}),
        json!({"notes_block": {"unknown": 1}}),
        json!({"notes_block": {"enabled": "yes"}}),
        json!({"notes_block": {"ra": "abc"}}),
        json!({"notes_block": {"ra": -1}}),
        json!({"notes_block": {"ra": 1.0e300}}),
        json!({"notes_block": {"edge_note": "a\nb"}}),
        json!({"notes_block": {"extra_notes": "x"}}),
        json!({"notes_block": {"extra_notes": [1]}}),
        json!({"notes_block": {"extra_notes": ["a", "b", "c", "d", "e", "f"]}}),
        json!({"notes_block": {"extra_notes": [long]}}),
    ] {
        assert!(s.execute("drawing.title.set", &p).is_err(), "accepted {p}");
        assert_eq!(s.undo.len(), undo, "{p} left an undo step");
    }
    // A notes_block with a wrong shape is refused by the sheet commands too.
    for p in [
        json!({"notes_block": "x"}),
        json!({"notes_block": {"ra": "abc"}}),
        json!({"notes_block": {"extra_notes": ["a", "b", "c", "d", "e", "f"]}}),
        json!({"title": {"gezeichnet": 5}}),
        json!({"title": {"geprueft_datum": "x"}}),
        json!({"title": {"firma": "a\nb"}}),
        json!({"title": {"rohmass": "a\tb"}}),
    ] {
        assert!(s.execute("drawing.sheet", &p).is_err(), "drawing.sheet accepted {p}");
    }
    // Refused calls leave the commands usable.
    assert!(run(&mut s, "drawing.title.set", json!({"gezeichnet": "Jane"})).get("changed").is_some());
    assert_eq!(s.doc.metadata.author(), Some("Jane"));
}

// ------------------------------------------------------------------- D9.2 stored sections

/// The flywheel of the D9 tests: Ø120 x 35 with a Ø90 recess 10 deep, a Ø40 boss and a Ø16 bore.
fn flywheel() -> Session {
    let mut s = Session::default();
    run(&mut s, "sketch.create", json!({"plane":"XZ","name":"FlyProfile"}));
    run(&mut s, "sketch.line", json!({"points":[[8,0],[60,0],[60,-20],[45,-20],[45,-10],[20,-10],[20,-35],[8,-35]],"closed":true}));
    run(&mut s, "sketch.finish", json!({}));
    run(&mut s, "solid.revolve", json!({"sketch":"FlyProfile","axis":"Z","angle":360,"body_name":"Flywheel"}));
    s
}

fn stored_sections(s: &Session) -> Option<Value> {
    s.doc.metadata.get(crate::drawing::DRAWING_METADATA).and_then(|v| v.get("sections")).cloned()
}

/// The whole stored `app.solidcraft.drawing` object, as the tests compare it across writes.
fn stored_drawing(s: &Session) -> Option<Value> {
    s.doc.metadata.get(crate::drawing::DRAWING_METADATA).cloned()
}

/// An explicit full section A–A through the front view's middle (D9.2's grammar).
fn full_section_a() -> SectionMode {
    use crate::drawing::{CuttingPlane, SectionSpec};
    use solidcraft_geom::Vec3;
    SectionMode::Explicit(vec![SectionSpec::Full {
        view: StandardView::Front,
        plane: CuttingPlane { point: Vec3::new(20.0, 10.0, 5.0), normal: Vec3::Y },
        letter: Some('A'),
    }])
}

#[test]
fn drawing_sections_set_persists_through_save_and_reopen_and_one_undo_removes_it() {
    let dir = std::env::temp_dir().join(format!("solidcraft-drawing-sections-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sections.solidcraft");
    let path_s = path.to_string_lossy().to_string();
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    s.mark_saved();
    let undo = s.undo.len();
    let saved = run(
        &mut s,
        "drawing.sections.set",
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [20, 10, 5], "normal": [0, 1, 0]}, "letter": "A"}]}),
    );
    assert_eq!(saved["section_mode"], "explicit");
    assert_eq!(saved["letters"], json!(["A"]));
    assert_eq!(saved["sections"][0]["letter"], "A");
    assert_eq!(saved["changed"], json!(["app.solidcraft.drawing"]));
    assert_eq!(s.undo.len(), undo + 1, "one undo step, through document.metadata.set");
    assert!(s.is_dirty());

    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["section_mode"], "explicit");
    assert_eq!(sheet["section_source"], "stored");
    assert_eq!(sheet["sections"][0]["label"], "A");
    assert_eq!(sheet["sections"][0]["type"], "full");
    assert_eq!(sheet["sections"][0]["plane"]["normal"], json!([0.0, 1.0, 0.0]));

    // A parameter overrides the stored spec for that call and does not write it.
    let none = run(&mut s, "drawing.sheet", json!({"sections": "none"}));
    assert_eq!(none["section_source"], "param");
    assert_eq!(none["section_mode"], "none");
    assert_eq!(none["sections"], json!([]));
    assert!(stored_sections(&s).is_some(), "the call's parameter was not stored");

    // The stored spec goes with the design.
    run(&mut s, "file.save_as", json!({"path": path_s, "format": "container"}));
    let mut reopened = Session::default();
    run(&mut reopened, "doc.open", json!({"path": path_s}));
    let sheet = run(&mut reopened, "drawing.sheet", json!({}));
    assert_eq!(sheet["section_source"], "stored");
    assert_eq!(sheet["sections"][0]["label"], "A");
    assert_eq!(sheet["section_mode"], "explicit");
    // A CLI-style export on the reopened file uses the same stored spec, without a parameter.
    let exported = dir.join("reopened.dxf");
    let exported = run(&mut reopened, "drawing.export", json!({"path": exported.to_string_lossy()}));
    assert_eq!(exported["section_source"], "stored");
    assert_eq!(exported["sections"][0]["label"], "A");
    assert_eq!(exported["section_mode"], "explicit");

    // One undo restores the previous spec (none) and the page follows it.
    run(&mut s, "edit.undo", json!({}));
    assert!(stored_sections(&s).is_none());
    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["section_source"], "default");
    assert_eq!(sheet["section_mode"], "auto", "the box has no automatic section");
    assert_eq!(sheet["sections"], json!([]));
    run(&mut s, "edit.redo", json!({}));
    assert!(stored_sections(&s).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drawing_export_without_a_sections_param_uses_the_stored_spec() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    run(&mut s, "drawing.sections.set", json!({"sections": "none"}));
    let stored = stored_sections(&s);
    assert_eq!(stored, Some(json!("none")));
    let path = std::env::temp_dir().join(format!("solidcraft-drawing-sections-export-{}.dxf", std::process::id()));
    let path_s = path.to_string_lossy().to_string();
    let exported = run(&mut s, "drawing.export", json!({"path": path_s}));
    assert_eq!(exported["section_source"], "stored");
    assert_eq!(exported["section_mode"], "none");
    assert_eq!(exported["sections"], json!([]));
    assert_eq!(stored_sections(&s), stored, "an export never writes the spec");
    // An explicit param wins without being written.
    let explicit = run(
        &mut s,
        "drawing.export",
        json!({"path": path_s, "sections": [{"type": "full", "view": "front", "plane": {"point": [20, 10, 5], "normal": [0, 1, 0]}}]}),
    );
    assert_eq!(explicit["section_source"], "param");
    assert_eq!(explicit["section_mode"], "explicit");
    assert_eq!(explicit["sections"][0]["label"], "A");
    assert_eq!(stored_sections(&s), stored, "the override is not stored");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_full_section_from_the_end_views_centre_line_derives_the_auto_plane_and_sheet() {
    let mut s = flywheel();
    let auto = run(&mut s, "drawing.sheet", json!({}));
    let auto_section = auto["sections"][0].clone();
    assert_eq!(auto_section["label"], "A");
    let plane_point = solidcraft_geom::Vec3::new(
        auto_section["plane"]["point"][0].as_f64().unwrap(),
        auto_section["plane"]["point"][1].as_f64().unwrap(),
        auto_section["plane"]["point"][2].as_f64().unwrap(),
    );
    let plane_normal = solidcraft_geom::Vec3::new(
        auto_section["plane"]["normal"][0].as_f64().unwrap(),
        auto_section["plane"]["normal"][1].as_f64().unwrap(),
        auto_section["plane"]["normal"][2].as_f64().unwrap(),
    );
    let plane = crate::drawing::CuttingPlane { point: plane_point, normal: plane_normal };
    // The sheet's other view is the end view the section line is picked in (the flywheel's
    // axis is Z, so that is the top view).
    let names: Vec<&str> = auto["views"].as_array().unwrap().iter().filter_map(|v| v["view"].as_str()).collect();
    let sectioned = auto_section["view"].as_str().unwrap();
    let end_view = names.iter().find(|name| **name != sectioned).copied().expect("a second view");
    let end_view = crate::drawing::sections::named_view(end_view).expect("a standard view");
    let views = [crate::drawing::sections::named_view(sectioned).unwrap(), end_view];
    // The line the end view shows: the plane's trace across it, long enough to span the view.
    let projected = crate::drawing::project_views(&s, &views, &crate::drawing::ViewOptions::default()).unwrap();
    let end = projected.iter().find(|v| v.view == end_view).expect("the end view");
    let mark = crate::drawing::section_mark(plane, end, 'A').expect("the section trace");
    let a = mark.centre - mark.dir * 40.0;
    let b = mark.centre + mark.dir * 40.0;
    let from = json!({"type": "full", "from": {"view": crate::drawing::view_name(end_view), "line": [[a.x, a.y], [b.x, b.y]]}});
    let derived = run(&mut s, "drawing.sheet", json!({"sections": [from]}));
    let section = &derived["sections"][0];
    assert_eq!(derived["section_mode"], "explicit");
    assert_eq!(derived["section_source"], "param");
    assert_eq!(section["label"], "A");
    assert_eq!(section["view"], auto_section["view"], "the sectioned view is the one looking along the cut");
    let normal = solidcraft_geom::Vec3::new(
        section["plane"]["normal"][0].as_f64().unwrap(),
        section["plane"]["normal"][1].as_f64().unwrap(),
        section["plane"]["normal"][2].as_f64().unwrap(),
    );
    assert!(normal.dot(plane_normal) > 0.9999, "the derived normal {normal:?} is the auto one {plane_normal:?}");
    let derived_point = solidcraft_geom::Vec3::new(
        section["plane"]["point"][0].as_f64().unwrap(),
        section["plane"]["point"][1].as_f64().unwrap(),
        section["plane"]["point"][2].as_f64().unwrap(),
    );
    assert!((derived_point - plane_point).dot(plane_normal).abs() <= 1e-6, "the derived plane is offset from the auto plane");
    assert_eq!(section["from"]["view"], crate::drawing::view_name(end_view), "the picked line is reported");
    assert_eq!(section["regions"], auto_section["regions"]);
    assert_eq!(section["hatch_lines"], auto_section["hatch_lines"]);
    assert_eq!(derived["prims"], auto["prims"], "the derived sheet is the automatic one");
}

#[test]
fn a_cutting_line_no_view_looks_along_is_the_documented_error() {
    let mut s = flywheel();
    // A horizontal line in the front view cuts along Z; the sheet shows front (Y) and right (X).
    let bad = json!({"views": ["front", "right"], "sections": [{"type": "full", "from": {"view": "front", "line": [[-20, 0], [20, 0]]}}]});
    let error = s.execute("drawing.sheet", &bad).unwrap_err().to_string();
    assert!(error.contains("no view looks along this cutting plane"), "{error}");
    // The picked view itself has to be on the sheet.
    let off_sheet = json!({"views": ["front", "top"], "sections": [{"type": "full", "from": {"view": "right", "line": [[-20, 0], [20, 0]]}}]});
    let error = s.execute("drawing.sheet", &off_sheet).unwrap_err().to_string();
    assert!(error.contains("a section's line is in view `right`, which is not on the sheet"), "{error}");
}

#[test]
fn a_sections_letter_stays_with_its_section_when_another_is_deleted() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let two = json!({"views": ["front", "top"], "sections": [
        {"type": "full", "view": "front", "plane": {"point": [20, 10, 5], "normal": [0, 1, 0]}, "letter": "A"},
        {"type": "full", "view": "top", "plane": {"point": [20, 10, 5], "normal": [0, 0, 1]}, "letter": "B"}
    ]});
    let both = run(&mut s, "drawing.sheet", two.clone());
    let labels: Vec<&str> = both["sections"].as_array().unwrap().iter().filter_map(|s| s["label"].as_str()).collect();
    assert_eq!(labels, ["A", "B"]);
    // Deleting A (the UI writes the remaining entry with its stored letter) keeps B's letter.
    let one = json!({"views": ["front", "top"], "sections": [
        {"type": "full", "view": "top", "plane": {"point": [20, 10, 5], "normal": [0, 0, 1]}, "letter": "B"}
    ]});
    let remaining = run(&mut s, "drawing.sheet", one.clone());
    assert_eq!(remaining["sections"][0]["label"], "B", "no re-lettering on delete");
    // Two entries may not share a letter.
    let duplicate = json!({"views": ["front", "top"], "sections": [
        {"type": "full", "view": "front", "plane": {"point": [20, 10, 5], "normal": [0, 1, 0]}, "letter": "A"},
        {"type": "full", "view": "top", "plane": {"point": [20, 10, 5], "normal": [0, 0, 1]}, "letter": "A"}
    ]});
    let error = s.execute("drawing.sheet", &duplicate).unwrap_err().to_string();
    assert!(error.contains("used twice"), "{error}");
}

#[test]
fn drawing_sections_set_rejects_hostile_parameters_and_writes_nothing() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10}));
    let undo = s.undo.len();
    for p in [
        json!({}),
        json!({"sections": null}),
        json!({"sections": 5}),
        json!({"sections": {}}),
        json!({"sections": "x"}),
        json!({"sections": "auto", "extra": 1}),
        json!({"sections": [1]}),
        json!({"sections": [{}]}),
        json!({"sections": [{"type": "nope", "view": "front"}]}),
        json!({"sections": [{"type": "full", "view": "front"}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0, 0], "normal": [0, 1, 0]}, "from": {"view": "right", "line": [[0, 0], [0, 1]]}}]}),
        json!({"sections": [{"type": "full", "view": "front", "plane": {"point": [0, 0, 0], "normal": [0, 1, 0]}, "around": [0, 0, 1]}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "plane": {"point": [0, 0, 0], "normal": [0, 1, 0]}}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "from": {"view": "right", "line": [[0, 0], [0, 1]]}}]}),
        json!({"sections": [{"type": "full", "from": {"view": "right", "line": [[0, 0], [0, 1]]}, "view": "front"}]}),
        json!({"sections": [{"type": "full", "from": {"view": "right", "line": [[0, 0]]}}]}),
        json!({"sections": [{"type": "full", "from": {"view": "right", "line": [[0, 0], [0, 0]]}}]}),
        json!({"sections": [{"type": "full", "from": {"view": "right", "line": [[0, 0], [1e308, 0]]}}]}),
        json!({"sections": [{"type": "full", "from": {"view": "nope", "line": [[0, 0], [0, 1]]}}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 0]}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, -1]}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0]}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "letter": "AB"}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "letter": 7}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "unknown": 1}]}),
        json!({"sections": [{"type": "breakout", "view": "front", "around": [0, 0, 1], "letter": "A"}, {"type": "breakout", "view": "top", "around": [0, 0, 1], "letter": "A"}]}),
    ] {
        assert!(s.execute("drawing.sections.set", &p).is_err(), "accepted {p}");
    }
    assert_eq!(s.undo.len(), undo, "a refused value leaves the design as it was");
    assert!(stored_sections(&s).is_none());
    // Setting what is already stored changes nothing (no undo step).
    let good = json!({"sections": "none"});
    run(&mut s, "drawing.sections.set", good.clone());
    let after = s.undo.len();
    let again = run(&mut s, "drawing.sections.set", good.clone());
    assert_eq!(again["changed"], json!([]));
    assert_eq!(s.undo.len(), after);
    // "auto" removes the stored value.
    let auto = run(&mut s, "drawing.sections.set", json!({"sections": "auto"}));
    assert_eq!(auto["sections"], "auto");
    assert_eq!(auto["section_mode"], "auto");
    assert!(stored_sections(&s).is_none());
}

#[test]
fn the_tangent_edge_rule_round_trips_and_the_sheet_uses_it() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"center":[0,0,0],"body_name":"Plate"}));
    run(&mut s, "solid.fillet", json!({"edges":[[20,10,0]],"radius":4,"body":"Plate"}));
    // Setting the choice is one undo step and is reflected by drawing.sheet / drawing.export.
    let undo = s.undo.len();
    let stored = run(&mut s, "drawing.tangent_edges.set", json!({"tangent_edges":"thin"}));
    assert_eq!((stored["tangent_edges"].as_str(), stored["source"].as_str()), (Some("thin"), Some("stored")));
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin));
    let sheet = run(&mut s, "drawing.sheet", json!({"views":["right"],"sections":"none","dimensions":"none"}));
    assert_eq!((sheet["tangent_edges"].as_str(), sheet["tangent_source"].as_str()), (Some("thin"), Some("stored")));
    // An explicit parameter overrides the stored value for that call and writes nothing.
    let omitted = run(&mut s, "drawing.sheet", json!({"views":["right"],"sections":"none","dimensions":"none","tangent_edges":"omit"}));
    assert_eq!((omitted["tangent_edges"].as_str(), omitted["tangent_source"].as_str()), (Some("omit"), Some("param")));
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin));
    // The exported DXF carries thin light edges on the TANGENT layer only with `thin`.
    let path = std::env::temp_dir().join(format!("sc_d10_tangent_{}.dxf", std::process::id()));
    let path_text = path.to_string_lossy().to_string();
    run(&mut s, "drawing.export", json!({"path":path_text,"views":["right"],"sections":"none","dimensions":"none","tangent_edges":"thin"}));
    let thin = std::fs::read_to_string(&path).unwrap();
    assert!(thin.matches("  8\nTANGENT\n").count() >= 1, "a thin tangent edge is written on TANGENT");
    run(&mut s, "drawing.export", json!({"path":path_text,"views":["right"],"sections":"none","dimensions":"none","tangent_edges":"omit"}));
    let omit = std::fs::read_to_string(&path).unwrap();
    assert!(!omit.contains("  8\nTANGENT\n"), "no tangent entity by default");
    let _ = std::fs::remove_file(&path);
    // A title save keeps the stored drawing options.
    run(&mut s, "drawing.sections.set", json!({"sections":"none"}));
    run(&mut s, "drawing.title.set", json!({"geprueft":"Nik"}));
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin), "the title save keeps the tangent rule");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(SectionMode::None), "and the sections");
    // One undo goes back to the default (the rule is one metadata write).
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin), "undo removed the title, not the rule");
    // Invalid values are refused and leave the design as it was.
    let undo = s.undo.len();
    for bad in [json!({"tangent_edges":"draw"}), json!({"tangent_edges":1}), json!({}), json!({"tangent_edges":"thin","other":1})] {
        assert!(s.execute("drawing.tangent_edges.set", &bad).is_err(), "accepted {bad}");
    }
    assert_eq!(s.undo.len(), undo);
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin));
}

// ------------------------------------------------------------------- D9.3 title edits keep the other drawing keys

#[test]
fn a_title_edit_keeps_the_stored_sections_and_the_export_uses_them() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let mode = full_section_a();
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    let stored = stored_drawing(&s).expect("the stored drawing object");
    let sections_before = run(&mut s, "drawing.sheet", json!({}))["sections"].clone();
    assert_eq!(sections_before[0]["label"], "A", "the explicit section is on the sheet: {sections_before}");

    let undo = s.undo.len();
    let saved = run(&mut s, "drawing.title.set", json!({"werkstoff": "C45"}));
    assert_eq!(saved["title"]["werkstoff"], "C45");
    assert_eq!(s.undo.len(), undo + 1, "the title write is one undo step");
    let merged = stored_drawing(&s).expect("still stored");
    assert_eq!(merged["sections"], stored["sections"], "the title edit kept the stored sections exactly");
    assert_eq!(merged["werkstoff"], "C45", "the title value itself was stored");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()), "the stored A-A is unchanged");

    // `drawing.export` without a `sections` parameter reads the stored spec, not the default.
    let path = std::env::temp_dir().join(format!("sc_d93_title_keeps_sections_{}.dxf", std::process::id()));
    let path_text = path.to_string_lossy().to_string();
    let exported = run(&mut s, "drawing.export", json!({"path": path_text}));
    assert_eq!(exported["section_source"], "stored");
    assert_eq!(exported["sections"], sections_before, "the exported sections are the ones from before the title edit");
    assert_eq!(exported["title"]["werkstoff"], "C45", "the title edit itself reached the sheet");
    let dxf = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(dxf.contains("SECTION"), "the exported DXF carries the section line");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn resetting_the_title_to_its_defaults_keeps_the_stored_sections() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let mode = full_section_a();
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    let sections_only = stored_drawing(&s).expect("the stored drawing object");
    run(&mut s, "drawing.title.set", json!({"werkstoff": "C45", "geprueft": "Nik"}));
    assert_ne!(stored_drawing(&s).expect("stored"), sections_only, "the title values were stored");

    // Every field back to its computed default: the title keys are removed again, the sections
    // stay. (The UI writes the whole object on each edit, so this is the reset path.)
    let reset = run(
        &mut s,
        "drawing.title.set",
        json!({
            "benennung": "Untitled", "zeichnungsnummer": "Untitled", "gezeichnet": "SolidCraft",
            "firma": "", "aend": "", "rohmass": "40 x 20 x 10", "werkstoff": "n. b.", "geprueft": "",
            "notes_block": {"enabled": true, "ra": "3,2", "edge_note": "Werkst\u{00fc}ckkanten ISO 13715: -0,2", "extra_notes": []},
        }),
    );
    assert_eq!(reset["changed"], json!(["app.solidcraft.drawing"]), "only the drawing object went from the title value back to the default");
    assert_eq!(stored_drawing(&s).expect("still stored"), sections_only, "a reset to the defaults did not erase the sections");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()));

    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["section_source"], "stored");
    assert_eq!(sheet["sections"][0]["label"], "A");
    assert_eq!(sheet["title"]["werkstoff"], "n. b.", "the title really is the computed default again");
}

#[test]
fn a_section_write_keeps_the_stored_title_values() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    run(&mut s, "drawing.title.set", json!({"werkstoff": "C45", "geprueft": "Nik"}));
    let title_only = stored_drawing(&s).expect("the stored drawing object");
    assert_eq!(title_only["werkstoff"], "C45");

    let mode = full_section_a();
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    let merged = stored_drawing(&s).expect("still stored");
    assert_eq!(merged["werkstoff"], "C45", "the section write kept the material: {merged}");
    assert_eq!(merged["geprueft"], "Nik", "and the approved-by: {merged}");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()));

    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["title"]["werkstoff"], "C45", "the title still reaches the sheet");
    assert_eq!(sheet["section_source"], "stored");
    assert_eq!(sheet["sections"][0]["label"], "A", "and so does the section");
}

#[test]
fn the_tangent_rule_and_the_sections_survive_each_others_writes() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let mode = full_section_a();
    run(&mut s, "drawing.tangent_edges.set", json!({"tangent_edges": "thin"}));
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin), "the section write kept the tangent rule");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()));

    // Removing the sections leaves the tangent rule; removing the rule leaves the sections.
    run(&mut s, "drawing.sections.set", json!({"sections": "none"}));
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin));
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(SectionMode::None));
    run(&mut s, "drawing.tangent_edges.set", json!({"tangent_edges": "omit"}));
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), None, "omit removes only its own key");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(SectionMode::None), "the sections key stayed");

    // The other direction: a stored section survives choosing the tangent rule.
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    run(&mut s, "drawing.tangent_edges.set", json!({"tangent_edges": "thin"}));
    let sheet = run(&mut s, "drawing.sheet", json!({}));
    assert_eq!(sheet["tangent_edges"], "thin");
    assert_eq!(sheet["tangent_source"], "stored");
    assert_eq!(sheet["section_source"], "stored");
    assert_eq!(sheet["sections"][0]["label"], "A");
}

#[test]
fn undoing_a_title_edit_restores_the_exact_drawing_object() {
    let mut s = Session::default();
    run(&mut s, "solid.box", json!({"length":40,"width":20,"height":10,"body_name":"Plate"}));
    let mode = full_section_a();
    run(&mut s, "drawing.sections.set", json!({"sections": crate::drawing::section_mode_value(&mode)}));
    run(&mut s, "drawing.tangent_edges.set", json!({"tangent_edges": "thin"}));
    let before = s.doc.metadata.clone();
    let object_before = stored_drawing(&s).expect("the stored drawing object");

    let undo = s.undo.len();
    run(&mut s, "drawing.title.set", json!({"werkstoff": "C45"}));
    assert_eq!(s.undo.len(), undo + 1);
    assert_ne!(stored_drawing(&s).expect("stored"), object_before);
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()), "the title edit itself kept the sections");
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin), "and the tangent rule");

    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.doc.metadata, before, "undo restores the exact metadata, drawing object included");
    assert_eq!(stored_drawing(&s).expect("stored"), object_before);
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode.clone()), "the sections survived edit and undo");
    assert_eq!(crate::drawing::stored_tangent_edges(&s.doc), Some(TangentEdges::Thin), "and so did the tangent rule");

    run(&mut s, "edit.redo", json!({}));
    assert_eq!(stored_drawing(&s).expect("stored")["werkstoff"], "C45");
    assert_eq!(crate::drawing::stored_sections(&s.doc), Some(mode));
}
