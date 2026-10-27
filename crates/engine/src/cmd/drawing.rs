//! Drawing commands: `drawing.views` (the projected standard views, see `crate::drawing`),
//! `drawing.sheet` (the laid-out A4 sheet as JSON), `drawing.export` (the same sheet as DXF or
//! PDF) and `drawing.title.set` (save the title-block and notes values with the design). The
//! first three are read-only: they never change the document or the undo history. The title
//! command writes through `document.metadata.set`, so its change is one undo step.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use solidcraft_geom::Vec2;
use solidcraft_render::StandardView;

use super::CommandSpec;
use crate::drawing::dims::{MAX_DIMENSIONS, MAX_NOTES, MAX_TEXT_CHARS, MAX_VALUE_MM};
use crate::drawing::{
    Annotations, AutoDimensionSource, DEFAULT_EDGE_NOTE, DEFAULT_RA, Dimension, DimensionGeometry, MAX_EXTRA_NOTES, MAX_NOTE_CHARS, MAX_TOLERANCE,
    MIN_TOLERANCE, Note, NotesBlock, SectionMode, Sheet, SheetPlan, TangentEdges, TitleBlock, Undimensioned, ViewChoice, ViewOptions, format_value,
    notes_block_lines, parse_scale, plan_sheet_auto_sections_tangent, plan_sheet_with_sections_tangent, plan_views_with, project_views, rohmass,
    sheet_to_dxf, sheet_to_pdf, view_name,
};
use crate::params::{bad, str_};
use crate::{EngineError, Result, Session};

const CMD: &str = "drawing.views";
const SHEET: &str = "drawing.sheet";
const EXPORT: &str = "drawing.export";
const TITLE_SET: &str = "drawing.title.set";
const SECTIONS_SET: &str = "drawing.sections.set";
const TANGENT_SET: &str = "drawing.tangent_edges.set";
const MAX_VIEWS: usize = 64;
const MAX_SHEET_VIEWS: usize = 6;

/// The `app.solidcraft.drawing` keys `drawing.title.set` owns: it rewrites (or removes) exactly
/// these and keeps every other key in the object, such as the stored sections or the
/// tangent-edge rule.
const OWNED_TITLE_KEYS: [&str; 13] = [
    "werkstoff",
    "dokumentart",
    "allgemeintoleranz",
    "datum",
    "aend_datum",
    "blatt",
    "ra",
    "edge_note",
    "rohmass",
    "geprueft",
    "geprueft_datum",
    "extra_notes",
    "enabled",
];

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new(CMD, "Project Standard Views", views)
        .icon("sheet")
        .noundo()
        .params(
            "views?: [front|top|right|back|bottom|left] or \"auto\" (default \"auto\": the views the sheet would choose); tolerance?: 0.0001..1 mm (curve chord tolerance, default 0.01); hidden?: true|false (include occluded segments, default true) → view_choice: explicit|turned|flat|general, views: [{view, visible: [[x1,y1,x2,y2]…], hidden: [...], bbox: [[min x,min y],[max x,max y]], camera, centre}], stats. mm; view-local x right / y up with the origin at the projected world origin; visible bodies and shown sketch profile curves; no document change, no undo step",
        ),
    CommandSpec::new(SHEET, "Drawing Sheet", sheet)
        .icon("sheet")
        .noundo()
        .params(
            "views?: [front|top|right|back|bottom|left] or \"auto\" (default \"auto\": a turned part gets its longitudinal and end view, a flat part its plan and one edge view, anything else front, top and right less repeated views); tolerance?: 0.0001..1 mm (default 0.01); scale?: an ISO 5455 scale such as \"1:2\" (default: the largest whose views, dimension bands and the notes block fit); title?: {benennung, zeichnungsnummer, werkstoff, allgemeintoleranz, gezeichnet (created by), datum (created-by date, dd.mm.yyyy), geprueft (approved by; empty: –), geprueft_datum, aend (revision index; – for the first issue), aend_datum, firma, rohmass (raw size), dokumentart, blatt (n/m)}; defaults come from the design (properties description, part_number, author, organization, revision; the body's material; the raw size from the B-rep) and the values drawing.title.set saved; notes_block?: {enabled (default true), ra (default \"3,2\"), edge_note (default \"Werkstückkanten ISO 13715: -0,2\"), extra_notes: [up to 5 lines]} — the notes above the title block, whose height the drawing area excludes; dimensions?: \"auto\" (default: read from the B-rep: turned parts get one diameter per step, chained lengths and the overall length, bores and cross holes get size notes; flat parts overall sizes, grouped hole diameters, positions and radii) | \"none\" | [{view, type: linear|diameter|radius|diameter_on_circle, p1, p2, angle?, offset?, centre?, radius?, at_angle?, text?, suffix?, text_shift?}] in the view's own mm (offset and text_shift in sheet mm); notes?: [{view, text, at (sheet mm from the view origin), leader: [[x,y]…] (view mm), target?}]; sections?: \"auto\" (default when the design stores none: a solid turned part gets a local break-out per internal feature, a hollow one a full axial section A–A; flat and general parts none) | \"none\" | [{type: \"full\", view, plane: {point: [x,y,z], normal: [x,y,z] (facing the view)}} | {type: \"full\", from: {view, line: [[x1,y1],[x2,y2]]}} | {type: \"breakout\", view, around: [x, y, r]}, each entry may carry letter A–Z] — without the parameter the design’s stored sections apply, and an explicit parameter overrides them for this call without writing them; tangent_edges?: \"omit\" (default: tangent (G1) seams are left out) | \"thin\" (thin light edges on the TANGENT layer) — without the parameter the design’s stored choice applies, and an explicit parameter overrides it for this call → paper, size_mm, scale, arrangement, view_choice, dimension_mode, section_mode, section_source: param|stored|default, sections: [{view, type, label, regions, hatch_lines, spacing_mm, angle_deg, plane, from}], title, notes_block: {enabled, ra, edge_note, extra_notes, lines}, views: [{view, origin_mm, bbox_mm, centre_lines}], frame_mm, title_block_mm, notes_block_mm, dimensions, notes, auto_dimensions: [{view, face, kind, value, text}] (the B-rep source of each automatic dimension), unplaced: [texts that could not be placed without crossing a line], undimensioned: [{face, kind, value}] (features the completeness pass found but could not reference), warnings: [{kind: \"skew_axis\", face, angle_deg}] (holes whose axis is more than 0.5° off a principal axis: they still get their size note, no auxiliary view is drawn), annotations, prims, layers. A4 landscape sheet in sheet mm, first-angle (ISO method 1), centre lines on CENTER, dimensions and notes on DIMENSION; no document change, no undo step",
        ),
    CommandSpec::new(EXPORT, "Export Drawing", export)
        .icon("dxf")
        .noundo()
        .params(
            "path: file to write, ending in .dxf or .pdf; format?: dxf|pdf (default: from the path's extension); views?, tolerance?, scale?, title?, notes_block?, dimensions?, notes?, sections?: as drawing.sheet (dimensions default to \"auto\", sections to the design’s stored spec); tangent_edges?: as drawing.sheet → writes an AutoCAD 2000 DXF in mm on the layers FRAME, VISIBLE, HIDDEN, THIN, CENTER (CENTER linetype), DIMENSION, TITLE, TEXT, HATCH, SECTION and FREEHAND, with real DIMENSION entities (DIMSTYLE SolidCraft ISO, anonymous *D blocks), LEADER notes and real HATCH entities on HATCH (ANSI31 at the sheet's 3 mm or 2 mm spacing), or the same sheet as a true-scale PDF page; returns {path, format, scale, arrangement, view_choice, dimension_mode, section_mode, section_source, sections, title, notes_block, views: [{view, origin_mm, bbox_mm, centre_lines}], dimensions, notes, auto_dimensions, unplaced, undimensioned, warnings, entities}; no document change, no undo step",
        ),
    CommandSpec::new(TITLE_SET, "Save Drawing Title", title_set)
        .icon("settings")
        .noundo()
        .params(
            "benennung?, zeichnungsnummer?, werkstoff?, allgemeintoleranz?, gezeichnet?, datum?, geprueft?, geprueft_datum?, aend?, aend_datum?, firma?, rohmass?, dokumentart?, blatt?, notes_block?: as drawing.sheet → stores the title and notes values in the design's metadata through document.metadata.set (one undo step; the design becomes unsaved). The well-known keys description, part_number, author, organization and revision carry Benennung, Zeichnungsnummer, Gezeichnet, Firma and Änd.; the rest lives under app.solidcraft.drawing. A value equal to the design's computed default is not stored. Returns title, notes_block, changed and metadata; no second persistence path",
        ),
    CommandSpec::new(SECTIONS_SET, "Save Drawing Sections", sections_set)
        .icon("section")
        .noundo()
        .params(
            "sections: \"auto\" | \"none\" | [{type: \"full\", view, plane: {point: [x,y,z], normal: [x,y,z]}} | {type: \"full\", from: {view, line: [[x1,y1],[x2,y2]]}} | {type: \"breakout\", view, around: [x, y, r]}], each entry may carry letter (A–Z) → validates the value exactly as drawing.sheet does and stores it under app.solidcraft.drawing as `sections` through document.metadata.set (one undo step). \"auto\" removes the stored value. drawing.sheet and drawing.export without a `sections` parameter then use it, and an explicit parameter overrides it for that call without writing it. Returns sections (the stored value), section_mode, letters, changed and metadata",
        ),
    CommandSpec::new(TANGENT_SET, "Save Drawing Tangent Edges", tangent_set)
        .icon("edge_display")
        .noundo()
        .params(
            "tangent_edges: \"omit\" | \"thin\" → stores how tangent (G1) edges are drawn under app.solidcraft.drawing as `tangent_edges` through document.metadata.set (one undo step). \"omit\" is the ISO default and removes the stored value. drawing.sheet and drawing.export without a `tangent_edges` parameter then use the stored rule, and an explicit parameter overrides it for that call without writing it. Returns tangent_edges (the stored value, \"omit\" when none), source: stored|default, changed and metadata",
        ),
];

/// The views a command asks for: `views` omitted or `"auto"`, or an explicit list.
#[derive(Clone, Debug, PartialEq)]
enum ViewsParam {
    Auto,
    Explicit(Vec<StandardView>),
}

impl ViewsParam {
    fn as_deref(&self) -> Option<&[StandardView]> {
        match self {
            ViewsParam::Auto => None,
            ViewsParam::Explicit(views) => Some(views),
        }
    }
}

/// The six views a drawing sheet places, by name; `iso` is left to the seam.
fn named(name: &str) -> Option<StandardView> {
    Some(match name.to_ascii_lowercase().as_str() {
        "front" => StandardView::Front,
        "top" => StandardView::Top,
        "right" => StandardView::Right,
        "back" => StandardView::Back,
        "bottom" => StandardView::Bottom,
        "left" => StandardView::Left,
        _ => return None,
    })
}

fn requested_views(p: &Value, cmd: &str, max: usize) -> Result<ViewsParam> {
    let Some(value) = p.get("views") else { return Ok(ViewsParam::Auto) };
    if value.as_str().is_some_and(|text| text.trim().eq_ignore_ascii_case("auto")) {
        return Ok(ViewsParam::Auto);
    }
    let Some(list) = value.as_array() else { return Err(bad(cmd, "`views` must be a list of view names or \"auto\"")) };
    if list.len() > max {
        return Err(bad(cmd, format!("choose at most {max} views")));
    }
    let views = list
        .iter()
        .map(|item| {
            let name = item.as_str().ok_or_else(|| bad(cmd, "`views` must be a list of view names"))?;
            named(name).ok_or_else(|| {
                let shown: String = name.chars().take(32).collect();
                bad(cmd, format!("unknown view `{shown}`: use front, top, right, back, bottom or left"))
            })
        })
        .collect::<Result<Vec<StandardView>>>()?;
    Ok(ViewsParam::Explicit(views))
}

fn tolerance(p: &Value, cmd: &str) -> Result<f64> {
    match p.get("tolerance") {
        None => Ok(0.01),
        Some(v) => v.as_f64().filter(|t| (MIN_TOLERANCE..=MAX_TOLERANCE).contains(t)).ok_or_else(|| bad(cmd, "tolerance must be 0.0001..1 mm")),
    }
}

fn segments(list: &[crate::drawing::Seg2mm]) -> Vec<Value> {
    list.iter().map(|s| json!([s.a.x, s.a.y, s.b.x, s.b.y])).collect()
}

fn views(s: &mut Session, p: &Value) -> Result<Value> {
    let requested = requested_views(p, CMD, MAX_VIEWS)?;
    let tolerance = tolerance(p, CMD)?;
    let hidden = match p.get("hidden") {
        None => true,
        Some(Value::Bool(v)) => *v,
        Some(_) => return Err(bad(CMD, "`hidden` must be true or false")),
    };
    let started = std::time::Instant::now();
    let (selection, mut projected) = match requested {
        // The automatic choice needs the hidden edges to recognise a repeated view; they are
        // dropped below when the caller asked for visible edges only.
        ViewsParam::Auto => plan_views_with(s, None, tolerance, TangentEdges::Draw)?,
        ViewsParam::Explicit(views) => {
            // The raw view command keeps every edge it always drew; the sheet's tangent rule is
            // a drawing-sheet rule, not a property of a projection.
            let projected = project_views(s, &views, &ViewOptions { tolerance, hidden, tangent: TangentEdges::Draw, curves: true })?;
            (crate::drawing::ViewSelection { views, choice: ViewChoice::Explicit }, projected)
        }
    };
    if !hidden {
        for view in &mut projected {
            view.hidden.clear();
        }
    }
    let mut out = Vec::with_capacity(projected.len());
    let mut visible_segments = 0usize;
    let mut hidden_segments = 0usize;
    for view in &projected {
        visible_segments = visible_segments.saturating_add(view.visible.len());
        hidden_segments = hidden_segments.saturating_add(view.hidden.len());
        out.push(json!({
            "view": view_name(view.view),
            "visible": segments(&view.visible),
            "hidden": segments(&view.hidden),
            "bbox": [[view.bbox[0].x, view.bbox[0].y], [view.bbox[1].x, view.bbox[1].y]],
            "camera": view.camera,
        }));
    }
    Ok(json!({
        "view_choice": selection.choice.name(),
        "views": out,
        "stats": {
            "views": projected.len(),
            "visible_segments": visible_segments,
            "hidden_segments": hidden_segments,
            "tolerance": tolerance,
            "hidden": hidden,
            "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
        },
    }))
}

/// Parse `scale` as an ISO 5455 pair; `None` means choose one that fits.
fn sheet_scale(p: &Value, cmd: &str) -> Result<Option<(u32, u32)>> {
    match p.get("scale").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .and_then(parse_scale)
            .map(Some)
            .ok_or_else(|| bad(cmd, "`scale` must be an ISO 5455 scale such as \"1:2\" (two positive numbers n:m)")),
    }
}

/// `dd.mm.yyyy` with a day, month and two- to four-digit year.
fn valid_date(text: &str) -> bool {
    let mut parts = text.trim().split('.');
    let (Some(day), Some(month), Some(year), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(day), Ok(month)) = (day.parse::<u32>(), month.parse::<u32>()) else { return false };
    (1..=31).contains(&day) && (1..=12).contains(&month) && (2..=4).contains(&year.len()) && year.chars().all(|c| c.is_ascii_digit())
}

/// `n/m` with 1 ≤ n ≤ m.
fn valid_blatt(text: &str) -> bool {
    let Some((n, m)) = text.trim().split_once('/') else { return false };
    let (Ok(n), Ok(m)) = (n.trim().parse::<u32>(), m.trim().parse::<u32>()) else { return false };
    n >= 1 && n <= m
}

/// The title block for this call: the design's stored values with the optional `title` object
/// applied. Control characters and empty mandatory values are refused.
fn title_block(s: &Session, p: &Value, cmd: &str) -> Result<TitleBlock> {
    let mut title = crate::drawing::stored_title(&s.doc, rohmass(s));
    let Some(value) = p.get("title").filter(|v| !v.is_null()) else { return Ok(title) };
    let Some(fields) = value.as_object() else {
        return Err(bad(cmd, "`title` must be an object with the title-block fields"));
    };
    apply_title_object(fields, &mut title, cmd, "title")?;
    Ok(title)
}

/// The notes block for this call: the design's stored values with the optional `notes_block`
/// object applied.
fn notes_block(s: &Session, p: &Value, cmd: &str) -> Result<NotesBlock> {
    let mut notes = crate::drawing::stored_notes(&s.doc);
    apply_notes_object(p.get("notes_block"), &mut notes, cmd)?;
    Ok(notes)
}

/// A checked title string: at most [`MAX_NOTE_CHARS`] characters, no control characters and, for
/// the mandatory fields, not empty.
fn title_text(object: &Map<String, Value>, key: &str, cmd: &str, what: &str, required: bool) -> Result<Option<String>> {
    let Some(value) = object.get(key) else { return Ok(None) };
    let Some(text) = value.as_str() else { return Err(bad(cmd, format!("{what}.{key} must be a string"))) };
    if text.chars().count() > MAX_NOTE_CHARS {
        return Err(bad(cmd, format!("{what}.{key} is longer than {MAX_NOTE_CHARS} characters")));
    }
    if text.chars().any(char::is_control) {
        return Err(bad(cmd, format!("{what}.{key} must not contain control characters")));
    }
    let text = text.trim();
    if required && text.is_empty() {
        return Err(bad(cmd, format!("{what}.{key} must not be empty")));
    }
    Ok(Some(text.to_string()))
}

/// Apply a title object to `title`: the new D8 fields and the D5 ones. `erstellt` is accepted as
/// the old name of the one creator field.
fn apply_title_object(fields: &Map<String, Value>, title: &mut TitleBlock, cmd: &str, what: &str) -> Result<()> {
    for key in fields.keys() {
        let known = [
            "benennung",
            "zeichnungsnummer",
            "werkstoff",
            "allgemeintoleranz",
            "gezeichnet",
            "erstellt",
            "datum",
            "gezeichnet_datum",
            "geprueft",
            "geprueft_datum",
            "aend",
            "aend_datum",
            "firma",
            "rohmass",
            "dokumentart",
            "blatt",
        ];
        if !known.contains(&key.as_str()) {
            return Err(bad(cmd, format!("unknown {what} field `{key}`")));
        }
    }
    for (key, required) in [
        ("benennung", true),
        ("zeichnungsnummer", true),
        ("werkstoff", true),
        ("allgemeintoleranz", true),
        ("gezeichnet", true),
        ("erstellt", true),
        ("dokumentart", true),
        ("geprueft", false),
        ("firma", false),
        ("aend", false),
        ("rohmass", false),
    ] {
        if let Some(text) = title_text(fields, key, cmd, what, required)? {
            match key {
                "benennung" => title.benennung = text,
                "zeichnungsnummer" => title.zeichnungsnummer = text,
                "werkstoff" => {
                    if !text.chars().any(|c| c.is_ascii_alphanumeric()) {
                        return Err(bad(cmd, format!("{what}.werkstoff must contain a letter or digit")));
                    }
                    title.werkstoff = text;
                }
                "allgemeintoleranz" => title.allgemeintoleranz = text,
                "gezeichnet" | "erstellt" => title.gezeichnet = text,
                "dokumentart" => title.dokumentart = text,
                "geprueft" => title.geprueft = text,
                "firma" => title.firma = text,
                "aend" => title.aend = text,
                "rohmass" => title.rohmass = text,
                _ => {}
            }
        }
    }
    if let Some(text) = title_text(fields, "datum", cmd, what, false)?
        && !text.is_empty()
    {
        if !valid_date(&text) {
            return Err(bad(cmd, format!("{what}.datum must be dd.mm.yyyy")));
        }
        title.datum = text;
    }
    for (key, slot) in [("gezeichnet_datum", &mut title.datum), ("geprueft_datum", &mut title.geprueft_datum), ("aend_datum", &mut title.aend_datum)]
    {
        if let Some(text) = title_text(fields, key, cmd, what, false)? {
            if !text.is_empty() && !valid_date(&text) {
                return Err(bad(cmd, format!("{what}.{key} must be dd.mm.yyyy")));
            }
            *slot = text;
        }
    }
    if let Some(text) = title_text(fields, "blatt", cmd, what, true)? {
        if !valid_blatt(&text) {
            return Err(bad(cmd, format!("{what}.blatt must be n/m with 1 <= n <= m")));
        }
        title.blatt = text;
    }
    Ok(())
}

/// Apply a `notes_block` object: `enabled`, `ra`, `edge_note` and `extra_notes` (at most
/// [`MAX_EXTRA_NOTES`] lines of at most [`MAX_NOTE_CHARS`] characters).
fn apply_notes_object(value: Option<&Value>, notes: &mut NotesBlock, cmd: &str) -> Result<()> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(()) };
    let Some(object) = value.as_object() else {
        return Err(bad(cmd, "`notes_block` must be an object with enabled, ra, edge_note or extra_notes"));
    };
    only_fields(object, cmd, &["enabled", "ra", "edge_note", "extra_notes"], "notes_block")?;
    if let Some(enabled) = object.get("enabled") {
        notes.enabled = enabled.as_bool().ok_or_else(|| bad(cmd, "notes_block.enabled must be true or false"))?;
    }
    if let Some(ra) = object.get("ra") {
        notes.ra = parse_ra(ra, cmd)?;
    }
    if let Some(edge) = title_text(object, "edge_note", cmd, "notes_block", false)? {
        notes.edge_note = edge;
    }
    if let Some(extra) = object.get("extra_notes") {
        let Some(list) = extra.as_array() else { return Err(bad(cmd, "notes_block.extra_notes must be a list of lines")) };
        if list.len() > MAX_EXTRA_NOTES {
            return Err(bad(cmd, format!("notes_block.extra_notes takes at most {MAX_EXTRA_NOTES} lines")));
        }
        let mut lines = Vec::with_capacity(list.len());
        for line in list {
            let Some(text) = line.as_str() else { return Err(bad(cmd, "notes_block.extra_notes must be a list of lines")) };
            if text.chars().count() > MAX_NOTE_CHARS {
                return Err(bad(cmd, format!("a notes_block.extra_notes line is longer than {MAX_NOTE_CHARS} characters")));
            }
            if text.chars().any(char::is_control) {
                return Err(bad(cmd, "notes_block.extra_notes must not contain control characters"));
            }
            if !text.trim().is_empty() {
                lines.push(text.trim().to_string());
            }
        }
        notes.extra_notes = lines;
    }
    Ok(())
}

/// A default roughness value: `3,2`, `3.2` or the number `3.2`; empty turns the surface line off.
fn parse_ra(value: &Value, cmd: &str) -> Result<String> {
    let text = match value {
        Value::Number(number) => number.as_f64().map(|v| format!("{v}")),
        Value::String(text) => Some(text.trim().replace(',', ".")),
        _ => None,
    };
    let Some(text) = text else { return Err(bad(cmd, "notes_block.ra must be a number such as \"3,2\"")) };
    if text.is_empty() {
        return Ok(String::new());
    }
    let Ok(number) = text.parse::<f64>() else { return Err(bad(cmd, "notes_block.ra must be a number such as \"3,2\"")) };
    if !(number.is_finite() && (0.001..=10_000.0).contains(&number)) {
        return Err(bad(cmd, "notes_block.ra must be between 0.001 and 10000 µm"));
    }
    Ok(format_value(number))
}

/// A finite number within the sheet's coordinate range.
fn number(value: Option<&Value>, cmd: &str, what: &str) -> Result<f64> {
    let Some(n) = value.and_then(Value::as_f64) else {
        return Err(bad(cmd, format!("{what} must be a finite number")));
    };
    if !(n.is_finite() && n.abs() <= MAX_VALUE_MM) {
        return Err(bad(cmd, format!("{what} must be finite and within ±{MAX_VALUE_MM} mm")));
    }
    Ok(n)
}

/// A `[x, y]` point of at most [`MAX_VALUE_MM`].
fn point(value: Option<&Value>, cmd: &str, what: &str) -> Result<Vec2> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Err(bad(cmd, format!("{what} must be [x, y]")));
    };
    if list.len() != 2 {
        return Err(bad(cmd, format!("{what} must be [x, y]")));
    }
    Ok(Vec2::new(number(list.first(), cmd, what)?, number(list.get(1), cmd, what)?))
}

/// A checked string of at most [`MAX_TEXT_CHARS`] characters without control characters.
fn annotation_text(value: Option<&Value>, cmd: &str, what: &str, required: bool) -> Result<Option<String>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return if required { Err(bad(cmd, format!("{what} is required"))) } else { Ok(None) };
    };
    let Some(text) = value.as_str() else { return Err(bad(cmd, format!("{what} must be a string"))) };
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(bad(cmd, format!("{what} is longer than {MAX_TEXT_CHARS} characters")));
    }
    if text.chars().any(char::is_control) {
        return Err(bad(cmd, format!("{what} must not contain control characters")));
    }
    if required && text.trim().is_empty() {
        return Err(bad(cmd, format!("{what} must not be empty")));
    }
    Ok(Some(text.to_string()))
}

/// The annotation's view, by the same names `views` takes.
fn annotation_view(value: Option<&Value>, cmd: &str) -> Result<StandardView> {
    let Some(name) = value.and_then(Value::as_str) else {
        return Err(bad(cmd, "an annotation needs a `view` name"));
    };
    named(name).ok_or_else(|| {
        let shown: String = name.chars().take(32).collect();
        bad(cmd, format!("unknown view `{shown}`: use front, top, right, back, bottom or left"))
    })
}

/// Reject unknown keys so a misspelt parameter is an error, not silently ignored.
fn only_fields(object: &serde_json::Map<String, Value>, cmd: &str, allowed: &[&str], what: &str) -> Result<()> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(bad(cmd, format!("unknown {what} field `{key}`")));
        }
    }
    Ok(())
}

/// One `dimensions` entry.
fn dimension_param(value: &Value, cmd: &str, index: usize) -> Result<Dimension> {
    let Some(object) = value.as_object() else {
        return Err(bad(cmd, format!("dimension {} must be an object", index + 1)));
    };
    only_fields(
        object,
        cmd,
        &["view", "type", "p1", "p2", "angle", "offset", "centre", "radius", "at_angle", "text", "suffix", "text_shift"],
        "dimension",
    )?;
    let view = annotation_view(object.get("view"), cmd)?;
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Err(bad(cmd, format!("dimension {} needs a `type`", index + 1)));
    };
    let kind = kind.trim().to_ascii_lowercase();
    let geometry = match kind.as_str() {
        "linear" | "diameter" => {
            let p1 = point(object.get("p1"), cmd, "p1")?;
            let p2 = point(object.get("p2"), cmd, "p2")?;
            let angle_deg = match object.get("angle").filter(|v| !v.is_null()) {
                Some(_) => number(object.get("angle"), cmd, "angle")?,
                None => 0.0,
            };
            let offset_mm = match object.get("offset").filter(|v| !v.is_null()) {
                Some(_) => number(object.get("offset"), cmd, "offset")?,
                None => 0.0,
            };
            if kind == "diameter" {
                DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm }
            } else {
                DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm }
            }
        }
        "radius" => DimensionGeometry::Radius {
            centre: point(object.get("centre"), cmd, "centre")?,
            radius: number(object.get("radius"), cmd, "radius")?,
            at_angle_deg: match object.get("at_angle").filter(|v| !v.is_null()) {
                Some(_) => number(object.get("at_angle"), cmd, "at_angle")?,
                None => 0.0,
            },
        },
        "diameter_on_circle" => DimensionGeometry::DiameterOnCircle {
            centre: point(object.get("centre"), cmd, "centre")?,
            radius: number(object.get("radius"), cmd, "radius")?,
            at_angle_deg: match object.get("at_angle").filter(|v| !v.is_null()) {
                Some(_) => number(object.get("at_angle"), cmd, "at_angle")?,
                None => 0.0,
            },
        },
        other => {
            let shown: String = other.chars().take(32).collect();
            return Err(bad(cmd, format!("unknown dimension type `{shown}`: use linear, diameter, radius or diameter_on_circle")));
        }
    };
    if matches!(geometry, DimensionGeometry::Radius { radius, .. } | DimensionGeometry::DiameterOnCircle { radius, .. } if !(radius > 0.0)) {
        return Err(bad(cmd, format!("dimension {}: radius must be positive", index + 1)));
    }
    let text = annotation_text(object.get("text"), cmd, "dimension text", false)?;
    let suffix = annotation_text(object.get("suffix"), cmd, "dimension suffix", false)?;
    let text_shift_mm = match object.get("text_shift").filter(|v| !v.is_null()) {
        Some(_) => number(object.get("text_shift"), cmd, "text_shift")?,
        None => 0.0,
    };
    Ok(Dimension { view, geometry, text, suffix, text_shift_mm })
}

/// One `notes` entry.
fn note_param(value: &Value, cmd: &str, index: usize) -> Result<Note> {
    let Some(object) = value.as_object() else {
        return Err(bad(cmd, format!("note {} must be an object", index + 1)));
    };
    only_fields(object, cmd, &["view", "text", "at", "leader", "target"], "note")?;
    let view = annotation_view(object.get("view"), cmd)?;
    let text = annotation_text(object.get("text"), cmd, "note text", true)?.unwrap_or_default();
    let at = point(object.get("at"), cmd, "note at")?;
    let Some(leader_values) = object.get("leader").and_then(Value::as_array) else {
        return Err(bad(cmd, "a note needs `leader` as a list of [x, y] points"));
    };
    if leader_values.is_empty() || leader_values.len() > 64 {
        return Err(bad(cmd, "a note needs 1..=64 leader points"));
    }
    let mut leader = Vec::with_capacity(leader_values.len());
    for value in leader_values {
        leader.push(point(Some(value), cmd, "note leader point")?);
    }
    let target = match object.get("target").filter(|v| !v.is_null()) {
        Some(_) => Some(point(object.get("target"), cmd, "note target")?),
        None => None,
    };
    Ok(Note { view, text, at, leader, target })
}

/// How the `dimensions` parameter selects the dimensions a sheet carries.
#[derive(Clone, Debug, PartialEq)]
enum DimensionMode {
    /// Read them from the B-rep (the default).
    Auto,
    /// Draw none.
    None,
    /// Use the caller's list (D6 behaviour).
    Explicit(Vec<Dimension>),
}

impl DimensionMode {
    fn name(&self) -> &'static str {
        match self {
            DimensionMode::Auto => "auto",
            DimensionMode::None => "none",
            DimensionMode::Explicit(_) => "explicit",
        }
    }
}

/// `dimensions` absent or `"auto"`: automatic; `"none"`: no dimensions; a list: explicit ones.
fn dimensions_param(p: &Value, cmd: &str) -> Result<DimensionMode> {
    let Some(value) = p.get("dimensions").filter(|v| !v.is_null()) else { return Ok(DimensionMode::Auto) };
    if let Some(text) = value.as_str() {
        let text = text.trim().to_ascii_lowercase();
        return match text.as_str() {
            "auto" => Ok(DimensionMode::Auto),
            "none" | "off" => Ok(DimensionMode::None),
            _ => Err(bad(cmd, "`dimensions` must be \"auto\", \"none\" or a list of dimension objects")),
        };
    }
    let Some(list) = value.as_array() else {
        return Err(bad(cmd, "`dimensions` must be \"auto\", \"none\" or a list of dimension objects"));
    };
    if list.len() > MAX_DIMENSIONS {
        return Err(bad(cmd, format!("at most {MAX_DIMENSIONS} dimensions are allowed")));
    }
    let mut dimensions = Vec::with_capacity(list.len());
    for (index, value) in list.iter().enumerate() {
        dimensions.push(dimension_param(value, cmd, index)?);
    }
    Ok(DimensionMode::Explicit(dimensions))
}

/// The `sections` parameter: `"auto"` (default), `"none"`, or a list of section objects. Each
/// entry is `{type: "full", view, plane: {point: [x,y,z], normal: [x,y,z]}}`, `{type: "full",
/// from: {view, line: [[x1,y1],[x2,y2]]}}` or `{type: "breakout", view, around: [x, y, r]}`,
/// with an optional `letter`; unknown fields are errors. The same parser reads the stored spec.
fn sections_param(p: &Value, cmd: &str) -> Result<SectionMode> {
    crate::drawing::parse_sections(p.get("sections"), cmd)
}

/// The optional `notes` array.
fn notes_param(p: &Value, cmd: &str) -> Result<Vec<Note>> {
    let mut notes = Vec::new();
    if let Some(value) = p.get("notes").filter(|v| !v.is_null()) {
        let Some(list) = value.as_array() else { return Err(bad(cmd, "`notes` must be a list of note objects")) };
        if list.len() > MAX_NOTES {
            return Err(bad(cmd, format!("at most {MAX_NOTES} notes are allowed")));
        }
        for (index, value) in list.iter().enumerate() {
            notes.push(note_param(value, cmd, index)?);
        }
    }
    Ok(notes)
}

/// A laid-out sheet and the automatic dimensions that fed it.
struct SheetRequest {
    plan: SheetPlan,
    auto: Vec<AutoDimensionSource>,
    mode: DimensionMode,
    sections: SectionMode,
    /// Where the sections came from: the call's parameter, the design, or the default.
    section_source: &'static str,
    /// How tangent (G1) edges are drawn: `omit` (the ISO default) or `thin`.
    tangent: TangentEdges,
    /// Where the tangent rule came from: the call's parameter, the design, or the default.
    tangent_source: &'static str,
}

/// The `tangent_edges` parameter: `omit` or `thin`.
fn tangent_param(p: &Value, cmd: &str) -> Result<Option<TangentEdges>> {
    match p.get("tangent_edges").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(Value::String(text)) => TangentEdges::parse(text).map(Some).ok_or_else(|| bad(cmd, "`tangent_edges` must be \"omit\" or \"thin\"")),
        Some(_) => Err(bad(cmd, "`tangent_edges` must be \"omit\" or \"thin\"")),
    }
}

/// The tangent rule a sheet call uses: its own parameter (never stored), else the design's stored
/// value, else `omit`.
fn tangent_for(s: &Session, p: &Value, cmd: &str) -> Result<(TangentEdges, &'static str)> {
    if let Some(mode) = tangent_param(p, cmd)? {
        Ok((mode, "param"))
    } else if let Some(mode) = crate::drawing::stored_tangent_edges(&s.doc) {
        Ok((mode, "stored"))
    } else {
        Ok((TangentEdges::Omit, "default"))
    }
}

/// The sections a sheet call draws: its own `sections` parameter (which it never stores), else
/// the design's stored spec, else `auto`. The source names which of the three applied.
fn sections_for(s: &Session, p: &Value, cmd: &str) -> Result<(SectionMode, &'static str)> {
    if p.get("sections").filter(|v| !v.is_null()).is_some() {
        Ok((sections_param(p, cmd)?, "param"))
    } else if let Some(mode) = crate::drawing::stored_sections(&s.doc) {
        Ok((mode, "stored"))
    } else {
        Ok((SectionMode::Auto, "default"))
    }
}

/// Everything the two sheet commands share: the views (explicit or chosen), the title block, the
/// scale and the annotation mode, laid out as one sheet.
fn request(s: &Session, p: &Value, cmd: &str) -> Result<SheetRequest> {
    let requested = requested_views(p, cmd, MAX_SHEET_VIEWS)?;
    if let ViewsParam::Explicit(views) = &requested {
        for (i, view) in views.iter().enumerate() {
            if views[..i].contains(view) {
                return Err(bad(cmd, format!("the view `{}` is given more than once", view_name(*view))));
            }
        }
    }
    let tolerance = tolerance(p, cmd)?;
    let scale = sheet_scale(p, cmd)?;
    let title = title_block(s, p, cmd)?;
    let notes_block = notes_block(s, p, cmd)?;
    let mode = dimensions_param(p, cmd)?;
    let (sections, section_source) = sections_for(s, p, cmd)?;
    let (tangent, tangent_source) = tangent_for(s, p, cmd)?;
    let notes = notes_param(p, cmd)?;
    let (plan, auto) = match &mode {
        DimensionMode::Auto => {
            let extra = Annotations { dimensions: Vec::new(), notes };
            let (plan, auto) =
                plan_sheet_auto_sections_tangent(s, requested.as_deref(), tolerance, &title, &notes_block, scale, &extra, &sections, tangent)?;
            (plan, auto)
        }
        DimensionMode::None => {
            let extra = Annotations { dimensions: Vec::new(), notes };
            (
                plan_sheet_with_sections_tangent(s, requested.as_deref(), tolerance, &title, &notes_block, scale, &extra, &sections, tangent)?,
                Vec::new(),
            )
        }
        DimensionMode::Explicit(dimensions) => {
            let extra = Annotations { dimensions: dimensions.clone(), notes };
            (
                plan_sheet_with_sections_tangent(s, requested.as_deref(), tolerance, &title, &notes_block, scale, &extra, &sections, tangent)?,
                Vec::new(),
            )
        }
    };
    Ok(SheetRequest { plan, auto, mode, sections, section_source, tangent, tangent_source })
}

fn rect_json([a, b]: [solidcraft_geom::Vec2; 2]) -> Value {
    json!([[a.x, a.y], [b.x, b.y]])
}

fn placements(sheet: &Sheet) -> Value {
    Value::Array(
        sheet
            .views
            .iter()
            .map(|v| {
                json!({
                    "view": view_name(v.view),
                    "origin_mm": [v.origin.x, v.origin.y],
                    "bbox_mm": [[v.bbox[0].x, v.bbox[0].y], [v.bbox[1].x, v.bbox[1].y]],
                    "centre_lines": v.centre_lines,
                })
            })
            .collect(),
    )
}

fn layer_counts(sheet: &Sheet) -> Value {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for prim in &sheet.prims {
        *counts.entry(prim.layer().dxf_name()).or_default() += 1;
    }
    json!(counts)
}

fn title_json(title: &TitleBlock) -> Value {
    json!({
        "benennung": title.benennung,
        "zeichnungsnummer": title.zeichnungsnummer,
        "werkstoff": title.werkstoff,
        "allgemeintoleranz": title.allgemeintoleranz,
        "gezeichnet": title.gezeichnet,
        "datum": title.datum,
        "gezeichnet_datum": title.datum,
        "geprueft": title.geprueft,
        "geprueft_datum": title.geprueft_datum,
        "aend": title.aend,
        "aend_datum": title.aend_datum,
        "firma": title.firma,
        "rohmass": title.rohmass,
        "dokumentart": title.dokumentart,
        "blatt": title.blatt,
    })
}

fn notes_json(title: &TitleBlock, notes: &NotesBlock) -> Value {
    json!({
        "enabled": notes.enabled,
        "ra": notes.ra,
        "edge_note": notes.edge_note,
        "extra_notes": notes.extra_notes,
        "lines": notes_block_lines(title, notes).iter().map(|line| line.text().to_string()).collect::<Vec<_>>(),
    })
}

/// The drawn dimensions and notes: the text each one finally carries, its measured model value
/// and where the collision search put the text (sheet mm along the dimension line).
fn annotations_json(sheet: &Sheet) -> Value {
    let dimensions: Vec<Value> = sheet
        .dimensions
        .iter()
        .map(|d| {
            json!({
                "view": view_name(d.view),
                "text": d.text,
                "measured": d.measurement,
                "text_shift_mm": d.text_shift_mm,
                "angle_deg": d.angle_deg,
                "text_mm": [d.text_midpoint.x, d.text_midpoint.y],
                "def_points_mm": [[d.def_point.x, d.def_point.y], [d.def_point2.x, d.def_point2.y], [d.def_point3.x, d.def_point3.y]],
            })
        })
        .collect();
    let notes: Vec<Value> = sheet
        .notes
        .iter()
        .map(|n| {
            json!({
                "view": view_name(n.view),
                "text": n.text_string,
                "leader": n.leader.iter().map(|(a, b)| json!([[a.x, a.y], [b.x, b.y]])).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "dimensions": dimensions, "notes": notes })
}

fn undimensioned_json(undimensioned: &[Undimensioned]) -> Value {
    Value::Array(
        undimensioned
            .iter()
            .map(|item| {
                json!({
                    "face": item.face,
                    "kind": item.kind,
                    "value": item.value,
                })
            })
            .collect(),
    )
}

/// The conditions the automatic pass noticed (`skew_axis`), for the UI to tell the user about.
fn warnings_json(warnings: &[crate::drawing::Warning]) -> Value {
    Value::Array(
        warnings
            .iter()
            .map(|item| {
                json!({
                    "kind": item.kind,
                    "face": item.face,
                    "angle_deg": item.angle_deg,
                })
            })
            .collect(),
    )
}

fn auto_json(auto: &[AutoDimensionSource]) -> Value {
    Value::Array(
        auto.iter()
            .map(|source| {
                json!({
                    "view": view_name(source.view),
                    "face": source.face,
                    "kind": source.kind,
                    "value": source.value,
                    "text": source.text,
                })
            })
            .collect(),
    )
}

/// The sections as drawn: kind, view, label, cut regions, hatch count and spacing, the cutting
/// plane the view was cut with and (for a line section) the picked line.
fn sections_json(sheet: &Sheet) -> Value {
    Value::Array(
        sheet
            .sections
            .iter()
            .map(|section| {
                let plane = section.plane.map(|plane| {
                    json!({
                        "point": [plane.point.x, plane.point.y, plane.point.z],
                        "normal": [plane.normal.x, plane.normal.y, plane.normal.z],
                    })
                });
                let from = section.from.map(|from| {
                    json!({
                        "view": view_name(from.view),
                        "line": [[from.line[0].x, from.line[0].y], [from.line[1].x, from.line[1].y]],
                    })
                });
                json!({
                    "view": view_name(section.view),
                    "type": if section.full { "full" } else { "breakout" },
                    "label": section.label.map(|letter| letter.to_string()),
                    "regions": section.regions.len(),
                    "hatch_lines": section.hatch.len(),
                    "spacing_mm": section.spacing_mm,
                    "angle_deg": section.angle_deg,
                    "plane": plane,
                    "from": from,
                })
            })
            .collect(),
    )
}

fn sheet_json(request: &SheetRequest) -> Value {
    let sheet = &request.plan.sheet;
    let (w, h) = sheet.paper.landscape_mm();
    json!({
        "paper": sheet.paper.name(),
        "size_mm": [w, h],
        "scale": sheet.scale_text(),
        "arrangement": sheet.arrangement.name(),
        "view_choice": request.plan.choice.name(),
        "dimension_mode": request.mode.name(),
        "section_mode": request.sections.name(),
        "section_source": request.section_source,
        "sections": sections_json(sheet),
        "tangent_edges": request.tangent.name(),
        "tangent_source": request.tangent_source,
        "title": title_json(&sheet.title),
        "notes_block": notes_json(&sheet.title, &sheet.notes_block),
        "views": placements(sheet),
        "frame_mm": rect_json(sheet.frame_rect()),
        "title_block_mm": rect_json(sheet.title_block_rect()),
        "notes_block_mm": rect_json(sheet.notes_block_rect()),
        "dimensions": sheet.dimensions.len(),
        "notes": sheet.notes.len(),
        "auto_dimensions": auto_json(&request.auto),
        "unplaced": sheet.unplaced,
        "undimensioned": undimensioned_json(&request.plan.undimensioned),
        "warnings": warnings_json(&request.plan.warnings),
        "annotations": annotations_json(sheet),
        "prims": sheet.prims.len(),
        "layers": layer_counts(sheet),
    })
}

fn sheet(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("path").is_some_and(|v| !v.is_null()) {
        return Err(bad(SHEET, "drawing.sheet writes nothing: use drawing.export with a path"));
    }
    let request = request(s, p, SHEET)?;
    Ok(sheet_json(&request))
}

/// The file format `drawing.export` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportFormat {
    Dxf,
    Pdf,
}

impl ExportFormat {
    /// The command's `format` value and the JSON's `format` field.
    fn name(self) -> &'static str {
        match self {
            ExportFormat::Dxf => "dxf",
            ExportFormat::Pdf => "pdf",
        }
    }

    /// The format a file name's extension names; `None` for anything else.
    fn from_extension(extension: Option<&str>) -> Option<ExportFormat> {
        match extension.map(str::to_ascii_lowercase).as_deref() {
            Some("dxf") => Some(ExportFormat::Dxf),
            Some("pdf") => Some(ExportFormat::Pdf),
            _ => None,
        }
    }
}

/// The output path and format: the `format` parameter when given (it must agree with the path's
/// extension), else the format the extension names. An unknown extension is an error.
fn export_target(p: &Value) -> Result<(&str, ExportFormat)> {
    let path = str_(p, "path")
        .filter(|x| !x.trim().is_empty() && x.len() < 4096 && !x.chars().any(char::is_control))
        .ok_or_else(|| bad(EXPORT, "`path` is required"))?;
    let extension = std::path::Path::new(path).extension().and_then(|e| e.to_str());
    let named = ExportFormat::from_extension(extension).ok_or_else(|| bad(EXPORT, "`path` must end in .dxf or .pdf"))?;
    let format = match p.get("format").filter(|v| !v.is_null()) {
        None => named,
        Some(v) => {
            let text = v.as_str().ok_or_else(|| bad(EXPORT, "`format` must be \"dxf\" or \"pdf\""))?;
            let chosen = match text.trim().to_ascii_lowercase().as_str() {
                "dxf" => ExportFormat::Dxf,
                "pdf" => ExportFormat::Pdf,
                _ => return Err(bad(EXPORT, "`format` must be \"dxf\" or \"pdf\"")),
            };
            if chosen != named {
                return Err(bad(EXPORT, "`format` does not match the path's extension"));
            }
            chosen
        }
    };
    Ok((path, format))
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let (path, format) = export_target(p)?;
    let request = request(s, p, EXPORT)?;
    let sheet = &request.plan.sheet;
    match format {
        ExportFormat::Dxf => {
            let text = sheet_to_dxf(sheet)?;
            solidcraft_io::vfs::write(path, text.as_bytes()).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        }
        ExportFormat::Pdf => {
            let page = sheet_to_pdf(sheet)?;
            let bytes = solidcraft_io::write_pdf(&page).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            solidcraft_io::vfs::write(path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        }
    }
    Ok(json!({
        "path": path,
        "format": format.name(),
        "scale": sheet.scale_text(),
        "arrangement": sheet.arrangement.name(),
        "view_choice": request.plan.choice.name(),
        "dimension_mode": request.mode.name(),
        "section_mode": request.sections.name(),
        "section_source": request.section_source,
        "sections": sections_json(sheet),
        "tangent_edges": request.tangent.name(),
        "tangent_source": request.tangent_source,
        "title": title_json(&sheet.title),
        "notes_block": notes_json(&sheet.title, &sheet.notes_block),
        "views": placements(sheet),
        "title_block_mm": rect_json(sheet.title_block_rect()),
        "notes_block_mm": rect_json(sheet.notes_block_rect()),
        "dimensions": sheet.dimensions.len(),
        "notes": sheet.notes.len(),
        "auto_dimensions": auto_json(&request.auto),
        "unplaced": sheet.unplaced,
        "undimensioned": undimensioned_json(&request.plan.undimensioned),
        "warnings": warnings_json(&request.plan.warnings),
        "annotations": annotations_json(sheet),
        "entities": sheet.prims.len(),
        "prims": sheet.prims.len(),
    }))
}

/// Write one change to the design's `app.solidcraft.drawing` object through
/// `document.metadata.set`, together with `values` (the title command's top-level keys), in one
/// undo step.
///
/// `change` starts from the object **as stored**, so every key it does not touch — the stored
/// sections, the tangent-edge rule, keys a later version adds — is kept unchanged. The object is
/// only removed when it ends up empty (the documented "nothing stored" state).
fn merge_drawing_metadata(s: &mut Session, mut values: Map<String, Value>, change: impl FnOnce(&mut Map<String, Value>)) -> Result<Value> {
    let mut object = s.doc.metadata.get(crate::drawing::DRAWING_METADATA).and_then(Value::as_object).cloned().unwrap_or_default();
    change(&mut object);
    values.insert(crate::drawing::DRAWING_METADATA.into(), if object.is_empty() { Value::Null } else { Value::Object(object) });
    s.execute("document.metadata.set", &json!({ "values": Value::Object(values) }))
}

/// `drawing.title.set`: store the title-block and notes values with the design.
///
/// The parameters are the title fields at the top level plus `notes_block`, as `drawing.sheet`
/// takes them. Everything is written through `document.metadata.set` in one call, so the change
/// is one undo step and dirties the file like any other edit; a value equal to the design's
/// computed default is not stored (and an existing stored value is removed), so a renamed file
/// or a new material still reaches the sheet.
fn title_set(s: &mut Session, p: &Value) -> Result<Value> {
    let Some(object) = p.as_object() else {
        return Err(bad(TITLE_SET, "give the title fields and notes_block as an object"));
    };
    let mut fields = Map::new();
    for (key, value) in object {
        if key != "notes_block" {
            fields.insert(key.clone(), value.clone());
        }
    }
    let mut title = crate::drawing::stored_title(&s.doc, rohmass(s));
    apply_title_object(&fields, &mut title, TITLE_SET, "title")?;
    let mut notes = crate::drawing::stored_notes(&s.doc);
    apply_notes_object(p.get("notes_block"), &mut notes, TITLE_SET)?;

    // The computed defaults. The drawing-number and description keys compare against the name
    // alone, so a rename still reaches a sheet whose Benennung was never overridden.
    let named = TitleBlock::for_design(&s.doc.name);
    let defaults = TitleBlock::for_document(&s.doc, rohmass(s));
    let mut values = Map::new();
    values.insert("description".into(), text_or_null(&title.benennung, &named.benennung));
    values.insert("part_number".into(), text_or_null(&title.zeichnungsnummer, &named.zeichnungsnummer));
    values.insert("author".into(), text_or_null(&title.gezeichnet, &named.gezeichnet));
    values.insert("organization".into(), text_or_null(&title.firma, &named.firma));
    values.insert("revision".into(), text_or_null(&title.aend, &named.aend));
    let (ra_default, edge_default) = (DEFAULT_RA.to_string(), DEFAULT_EDGE_NOTE.to_string());
    let written = merge_drawing_metadata(s, values, |own| {
        // Only the keys this command owns are rewritten; every other key in the drawing object
        // (the stored sections, the tangent-edge rule) stays exactly as it is.
        for key in OWNED_TITLE_KEYS {
            own.remove(key);
        }
        for (key, value, default) in [
            ("werkstoff", &title.werkstoff, &defaults.werkstoff),
            ("dokumentart", &title.dokumentart, &defaults.dokumentart),
            ("allgemeintoleranz", &title.allgemeintoleranz, &defaults.allgemeintoleranz),
            ("datum", &title.datum, &defaults.datum),
            ("aend_datum", &title.aend_datum, &defaults.aend_datum),
            ("blatt", &title.blatt, &defaults.blatt),
            ("ra", &notes.ra, &ra_default),
            ("edge_note", &notes.edge_note, &edge_default),
        ] {
            if value != default {
                own.insert(key.to_string(), json!(value));
            }
        }
        // An empty raw size means "use the computed one"; every other value is an override.
        if !title.rohmass.trim().is_empty() && title.rohmass != defaults.rohmass {
            own.insert("rohmass".into(), json!(title.rohmass));
        }
        if !title.geprueft.is_empty() {
            own.insert("geprueft".into(), json!(title.geprueft));
        }
        if !title.geprueft_datum.is_empty() {
            own.insert("geprueft_datum".into(), json!(title.geprueft_datum));
        }
        if !notes.extra_notes.is_empty() {
            own.insert("extra_notes".into(), json!(notes.extra_notes));
        }
        if !notes.enabled {
            own.insert("enabled".into(), json!(false));
        }
    })?;
    Ok(json!({
        "title": title_json(&title),
        "notes_block": notes_json(&title, &notes),
        "changed": written.get("changed").cloned().unwrap_or_else(|| json!([])),
        "metadata": s.doc.metadata,
    }))
}

/// `drawing.sections.set`: store the section spec with the design (D9.2).
///
/// The value is parsed exactly as `drawing.sheet` parses its `sections` parameter, then written
/// under [`crate::drawing::DRAWING_METADATA`] through `document.metadata.set`, so the change is
/// one undo step and the design becomes unsaved. `"auto"` removes the stored value (it is the
/// default again); every other value is stored canonically, so the sheet, the PDF/DXF export and
/// a CLI export of the saved file all read one spec.
fn sections_set(s: &mut Session, p: &Value) -> Result<Value> {
    let Some(object) = p.as_object() else {
        return Err(bad(SECTIONS_SET, "give `sections`: \"auto\", \"none\" or a list of section objects"));
    };
    only_fields(object, SECTIONS_SET, &["sections"], "drawing.sections.set")?;
    let Some(value) = object.get("sections").filter(|v| !v.is_null()) else {
        return Err(bad(SECTIONS_SET, "`sections` is required (\"auto\", \"none\" or a list)"));
    };
    let mode = crate::drawing::parse_sections(Some(value), SECTIONS_SET)?;
    let written = merge_drawing_metadata(s, Map::new(), |own| match &mode {
        SectionMode::Auto => {
            own.remove("sections");
        }
        _ => {
            own.insert("sections".into(), crate::drawing::section_mode_value(&mode));
        }
    })?;
    let letters: Vec<String> = mode.letters().iter().map(char::to_string).collect();
    Ok(json!({
        "sections": crate::drawing::section_mode_value(&mode),
        "section_mode": mode.name(),
        "letters": letters,
        "changed": written.get("changed").cloned().unwrap_or_else(|| json!([])),
        "metadata": s.doc.metadata,
    }))
}

/// `drawing.tangent_edges.set`: store the tangent-edge rule with the design.
///
/// The choice is one key of the drawing metadata object, written through
/// `document.metadata.set`, so it is one undo step. `omit` is the default and removes the key; a
/// stored `thin` makes `drawing.sheet` / `drawing.export` draw tangent seams as light edges.
fn tangent_set(s: &mut Session, p: &Value) -> Result<Value> {
    let Some(object) = p.as_object() else {
        return Err(bad(TANGENT_SET, "give `tangent_edges`: \"omit\" or \"thin\""));
    };
    only_fields(object, TANGENT_SET, &["tangent_edges"], "drawing.tangent_edges.set")?;
    let Some(value) = object.get("tangent_edges").filter(|v| !v.is_null()) else {
        return Err(bad(TANGENT_SET, "`tangent_edges` is required (\"omit\" or \"thin\")"));
    };
    let text = value.as_str().ok_or_else(|| bad(TANGENT_SET, "`tangent_edges` must be \"omit\" or \"thin\""))?;
    let mode = TangentEdges::parse(text).ok_or_else(|| bad(TANGENT_SET, "`tangent_edges` must be \"omit\" or \"thin\""))?;
    let written = merge_drawing_metadata(s, Map::new(), |own| match mode {
        TangentEdges::Omit => {
            own.remove("tangent_edges");
        }
        TangentEdges::Thin | TangentEdges::Draw => {
            own.insert("tangent_edges".into(), json!("thin"));
        }
    })?;
    let stored = crate::drawing::stored_tangent_edges(&s.doc).unwrap_or(TangentEdges::Omit);
    Ok(json!({
        "tangent_edges": stored.name(),
        "source": if stored == TangentEdges::Omit { "default" } else { "stored" },
        "changed": written.get("changed").cloned().unwrap_or_else(|| json!([])),
        "metadata": s.doc.metadata,
    }))
}

/// `value` as design metadata, or null when it equals the computed default (so the key is
/// removed and the default follows the design again).
fn text_or_null(value: &str, default: &str) -> Value {
    if value.trim() == default.trim() { Value::Null } else { json!(value.trim()) }
}

#[cfg(test)]
#[path = "drawing_tests.rs"]
mod tests;
