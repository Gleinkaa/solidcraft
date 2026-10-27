//! Sections, break-outs and their hatching (ISO 128-3, ISO 128-50).
//!
//! A [`SectionMode`] decides which views are cut. `auto` follows the ISO 128-44 practice for the
//! parts this module knows: a solid turned part gets a local break-out per internal feature (an
//! end bore, a cross hole) and is never sectioned lengthwise; a hollow turned part, whose bore or
//! recess takes a large share of the section, gets a full axial section A–A. Flat and general
//! parts are not sectioned by default.
//!
//! The cutting plane is a world plane whose normal points towards the viewer; the half behind it
//! is kept. [`crate::drawing::project_views_cut`] clips the mesh scene with it and projects the
//! kept half through the same hidden-line projection as an ordinary view, so bore walls and
//! shoulders behind the plane become visible edges. The plane–mesh intersection is chained into
//! closed loops ([`chain_loops`]) and drawn as the cut outline; the area between the outer loop
//! and the hole loops is hatched ([`hatch_spans`]).
//!
//! A break-out replaces the view's geometry only inside its freehand boundary ([`Boundary`]): a
//! smooth sine-modulated closed curve with a 1.5 mm amplitude, never a zigzag. Its cut region is
//! clipped to that boundary, so the hatch stops on the freehand line.

use std::f64::consts::{PI, TAU};

use serde_json::{Map, Value, json};
use solidcraft_geom::{Vec2, Vec3};
use solidcraft_render::StandardView;

use crate::Result;
use crate::Session;
use crate::drawing::centre;
use crate::drawing::{ProjectedView, Seg2mm, ViewChoice, ViewSelection, view_name};
use crate::params::bad;

/// One degree in radians.
const ONE_DEG: f64 = PI / 180.0;
/// Faces lie on the part axis when their lines are closer than this fraction of the part size.
const AXIS_DISTANCE_FRACTION: f64 = 1e-4;
/// A full cylinder turns this close to a whole turn.
const FULL_TURN_TOL: f64 = 1e-3;
/// A bore counts as taking a large share of the section when its radius reaches this fraction of
/// the largest external radius (the flywheel's Ø90 recess on a Ø120 disc is 0.75).
pub const HOLLOW_SHARE: f64 = 1.0 / 3.0;
/// Amplitude of the freehand break-out boundary (mm), ISO 128-2 type 01.1: the cap of the
/// window-scaled amplitude.
pub const FREEHAND_AMPLITUDE_MM: f64 = 1.5;
/// Least amplitude of the freehand boundary (mm), so a tiny window still reads as a freehand line.
pub const FREEHAND_MIN_MM: f64 = 0.3;
/// The freehand amplitude grows with the window: this share of the smaller window radius.
pub const FREEHAND_SHARE: f64 = 0.1;
/// Most sections one sheet may carry.
pub const MAX_SECTIONS: usize = 16;
/// Largest accepted coordinate, radius or plane component (mm).
pub const MAX_SECTION_VALUE_MM: f64 = 1.0e6;
/// The freehand amplitude for a window of radius `radius`: `min(1.5, 0.1 × radius)`, at least
/// 0.3 mm. The smaller half axis of an ellipse is its radius for this rule.
pub fn freehand_amplitude(radius: f64) -> f64 {
    if !(radius.is_finite() && radius > 0.0) {
        return FREEHAND_MIN_MM;
    }
    (FREEHAND_SHARE * radius).clamp(FREEHAND_MIN_MM, FREEHAND_AMPLITUDE_MM)
}

/// A cutting plane in world millimetres. `normal` points towards the viewer; the kept half is
/// where `(p − point)·normal <= 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CuttingPlane {
    pub point: Vec3,
    pub normal: Vec3,
}

impl CuttingPlane {
    /// A plane with a unit normal, or `None` for a non-finite or zero normal.
    pub fn new(point: Vec3, normal: Vec3) -> Option<CuttingPlane> {
        let len = normal.len();
        (point.is_finite() && len.is_finite() && len > 0.0).then(|| CuttingPlane { point, normal: normal * (1.0 / len) })
    }

    /// Every component finite and within [`MAX_SECTION_VALUE_MM`].
    pub fn is_valid(&self) -> bool {
        let ok = |v: f64| v.is_finite() && v.abs() <= MAX_SECTION_VALUE_MM;
        ok(self.point.x) && ok(self.point.y) && ok(self.point.z) && ok(self.normal.x) && ok(self.normal.y) && ok(self.normal.z)
    }
}

/// A cutting line in another view's own millimetres (D9.2): the full section's plane contains
/// this line and the picked view's direction, so the line stays editable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionLine {
    /// The view the line was picked in.
    pub view: StandardView,
    pub line: [Vec2; 2],
}

/// What one `sections` entry asks for. A full section carries its plane, or the line it was
/// picked from; a break-out is a freehand region around `[x, y, r]` in the view's own
/// millimetres. Every entry may carry an identifying letter (drawn for a full section).
#[derive(Clone, Debug, PartialEq)]
pub enum SectionSpec {
    Full { view: StandardView, plane: CuttingPlane, letter: Option<char> },
    FullFrom { from: SectionLine, letter: Option<char> },
    Breakout { view: StandardView, around: [f64; 3], letter: Option<char> },
}

impl SectionSpec {
    /// The stored letter, if the caller gave one.
    pub fn letter(&self) -> Option<char> {
        match self {
            SectionSpec::Full { letter, .. } | SectionSpec::FullFrom { letter, .. } | SectionSpec::Breakout { letter, .. } => *letter,
        }
    }

    /// The view the spec names directly: a full section's plane view, or a break-out's view. A
    /// line section names the view it is picked in, not the sectioned view.
    pub fn named_view(&self) -> Option<StandardView> {
        match self {
            SectionSpec::Full { view, .. } | SectionSpec::Breakout { view, .. } => Some(*view),
            SectionSpec::FullFrom { .. } => None,
        }
    }

    /// The picked view of a line section.
    pub fn line_view(&self) -> Option<StandardView> {
        match self {
            SectionSpec::FullFrom { from, .. } => Some(from.view),
            _ => None,
        }
    }
}

/// The assigned letter of every list entry: the stored one, else A, B, C… by list position
/// (D9's rule). Duplicates are refused.
pub fn assigned_letters(specs: &[SectionSpec]) -> Result<Vec<char>> {
    let mut letters = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let letter = spec.letter().unwrap_or(section_letter(index));
        if letters.contains(&letter) {
            return Err(bad("drawing.sheet", format!("the section letter `{letter}` is used twice")));
        }
        letters.push(letter);
    }
    Ok(letters)
}

/// The canonical JSON value of a section mode: `"auto"`, `"none"` or the explicit list. This is
/// both the value `drawing.sections.set` stores and what the control state reports.
pub fn section_mode_value(mode: &SectionMode) -> Value {
    match mode {
        SectionMode::Auto => json!("auto"),
        SectionMode::None => json!("none"),
        SectionMode::Explicit(specs) => Value::Array(specs.iter().map(spec_value).collect()),
    }
}

/// One spec as the parameter grammar states it (the `letter` only when it was given).
pub fn spec_value(spec: &SectionSpec) -> Value {
    let mut object = Map::new();
    match spec {
        SectionSpec::Full { view, plane, letter } => {
            object.insert("type".into(), json!("full"));
            object.insert("view".into(), json!(view_name(*view)));
            object.insert(
                "plane".into(),
                json!({"point": [plane.point.x, plane.point.y, plane.point.z], "normal": [plane.normal.x, plane.normal.y, plane.normal.z]}),
            );
            if let Some(letter) = letter {
                object.insert("letter".into(), json!(letter.to_string()));
            }
        }
        SectionSpec::FullFrom { from, letter } => {
            object.insert("type".into(), json!("full"));
            object.insert(
                "from".into(),
                json!({"view": view_name(from.view), "line": [[from.line[0].x, from.line[0].y], [from.line[1].x, from.line[1].y]]}),
            );
            if let Some(letter) = letter {
                object.insert("letter".into(), json!(letter.to_string()));
            }
        }
        SectionSpec::Breakout { view, around, letter } => {
            object.insert("type".into(), json!("breakout"));
            object.insert("view".into(), json!(view_name(*view)));
            object.insert("around".into(), json!([around[0], around[1], around[2]]));
            if let Some(letter) = letter {
                object.insert("letter".into(), json!(letter.to_string()));
            }
        }
    }
    Value::Object(object)
}

/// Parse the `sections` value: `"auto"` (also absent), `"none"`, or the explicit list. Every
/// object field is checked here, so a misspelt field is an error rather than silently ignored.
pub fn parse_value(value: Option<&Value>, cmd: &str) -> Result<SectionMode> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(SectionMode::Auto) };
    if let Some(text) = value.as_str() {
        let text = text.trim().to_ascii_lowercase();
        return match text.as_str() {
            "auto" => Ok(SectionMode::Auto),
            "none" | "off" => Ok(SectionMode::None),
            _ => Err(bad(cmd, "`sections` must be \"auto\", \"none\" or a list of section objects")),
        };
    }
    let Some(list) = value.as_array() else {
        return Err(bad(cmd, "`sections` must be \"auto\", \"none\" or a list of section objects"));
    };
    if list.len() > MAX_SECTIONS {
        return Err(bad(cmd, format!("at most {MAX_SECTIONS} sections are allowed")));
    }
    let mut specs = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        specs.push(section_param(item, index, cmd)?);
    }
    assigned_letters(&specs).map_err(|e| bad(cmd, message_of(&e)))?;
    // Static conflict checks that do not need the derived plane: a view is not cut twice in two
    // ways (the hidden-line projection and the cut loops are computed once per plane).
    for (index, spec) in specs.iter().enumerate() {
        let (view, plane) = match spec {
            SectionSpec::Full { view, plane, .. } => (Some(*view), Some(*plane)),
            SectionSpec::FullFrom { .. } => (None, None),
            SectionSpec::Breakout { view, .. } => (Some(*view), None),
        };
        let Some(view) = view else { continue };
        for other in &specs[..index] {
            let (other_view, other_plane) = match other {
                SectionSpec::Full { view, plane, .. } => (Some(*view), Some(*plane)),
                SectionSpec::FullFrom { .. } => (None, None),
                SectionSpec::Breakout { view, .. } => (Some(*view), None),
            };
            if other_view != Some(view) {
                continue;
            }
            if plane.is_some() != other_plane.is_some() {
                return Err(bad(cmd, format!("view `{}` cannot be a full section and a break-out at once", view_name(view))));
            }
            if let (Some(a), Some(b)) = (plane, other_plane)
                && a != b
            {
                return Err(bad(cmd, format!("view `{}` can carry one cutting plane", view_name(view))));
            }
        }
    }
    Ok(SectionMode::Explicit(specs))
}

/// The text of an engine error without its command prefix (`drawing.sheet: …`).
fn message_of(error: &crate::EngineError) -> String {
    match error {
        crate::EngineError::BadParams { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

/// One list entry.
fn section_param(value: &Value, index: usize, cmd: &str) -> Result<SectionSpec> {
    let Some(object) = value.as_object() else {
        return Err(bad(cmd, format!("section {} must be an object", index + 1)));
    };
    only_fields(object, cmd, &["type", "view", "plane", "from", "around", "letter"], "section")?;
    let letter = section_letter_param(object.get("letter"), index, cmd)?;
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Err(bad(cmd, format!("section {} needs a `type`", index + 1)));
    };
    match kind.trim().to_ascii_lowercase().as_str() {
        "full" => {
            if object.get("around").is_some_and(|v| !v.is_null()) {
                return Err(bad(cmd, format!("section {}: a full section does not take `around`", index + 1)));
            }
            let has_plane = object.get("plane").is_some_and(|v| !v.is_null());
            let has_from = object.get("from").is_some_and(|v| !v.is_null());
            if has_plane == has_from {
                return Err(bad(cmd, format!("section {} needs exactly one of `plane`: {{point, normal}} or `from`: {{view, line}}", index + 1)));
            }
            if has_from {
                if object.get("view").is_some_and(|v| !v.is_null()) {
                    return Err(bad(cmd, format!("section {}: a line section names its view in `from`", index + 1)));
                }
                let Some(from) = object.get("from").and_then(Value::as_object) else {
                    return Err(bad(cmd, format!("section {}: `from` must be an object", index + 1)));
                };
                only_fields(from, cmd, &["view", "line"], "section from")?;
                let view = annotation_view(from.get("view"), cmd)?;
                let line = section_line(from.get("line"), index, cmd)?;
                Ok(SectionSpec::FullFrom { from: SectionLine { view, line }, letter })
            } else {
                let view = annotation_view(object.get("view"), cmd)?;
                let Some(plane) = object.get("plane").and_then(Value::as_object) else {
                    return Err(bad(cmd, format!("section {} needs `plane`: {{point, normal}}", index + 1)));
                };
                only_fields(plane, cmd, &["point", "normal"], "section plane")?;
                let point = point3(plane.get("point"), cmd, "section plane point")?;
                let normal = point3(plane.get("normal"), cmd, "section plane normal")?;
                let Some(plane) = CuttingPlane::new(point, normal) else {
                    return Err(bad(cmd, format!("section {}: the plane normal must not be zero", index + 1)));
                };
                if !plane.is_valid() {
                    return Err(bad(cmd, format!("section {}: the plane is out of range", index + 1)));
                }
                // A section is only visible when its plane faces the view it is drawn in.
                let back = crate::drawing::standard_camera(view).basis().2;
                if plane.normal.dot(back).abs() < 0.2 {
                    return Err(bad(cmd, format!("section {}: the cutting plane must face the view", index + 1)));
                }
                Ok(SectionSpec::Full { view, plane, letter })
            }
        }
        "breakout" => {
            if object.get("plane").is_some_and(|v| !v.is_null()) || object.get("from").is_some_and(|v| !v.is_null()) {
                return Err(bad(cmd, format!("section {}: a break-out takes `around`, not `plane` or `from`", index + 1)));
            }
            let view = annotation_view(object.get("view"), cmd)?;
            let Some(around) = object.get("around").and_then(Value::as_array) else {
                return Err(bad(cmd, format!("section {} needs `around`: [x, y, r]", index + 1)));
            };
            if around.len() != 3 {
                return Err(bad(cmd, format!("section {}: `around` must be [x, y, r]", index + 1)));
            }
            let x = number(around.first(), cmd, "section around x")?;
            let y = number(around.get(1), cmd, "section around y")?;
            let r = number(around.get(2), cmd, "section around r")?;
            if !(r > 0.0) {
                return Err(bad(cmd, format!("section {}: the break-out radius must be positive", index + 1)));
            }
            Ok(SectionSpec::Breakout { view, around: [x, y, r], letter })
        }
        other => {
            let shown: String = other.chars().take(32).collect();
            Err(bad(cmd, format!("unknown section type `{shown}`: use full or breakout")))
        }
    }
}

/// A `[x, y]` cutting line in the picked view's millimetres, with some length.
fn section_line(value: Option<&Value>, index: usize, cmd: &str) -> Result<[Vec2; 2]> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Err(bad(cmd, format!("section {}: `from.line` must be [[x1,y1],[x2,y2]]", index + 1)));
    };
    if list.len() != 2 {
        return Err(bad(cmd, format!("section {}: `from.line` must be [[x1,y1],[x2,y2]]", index + 1)));
    }
    let p1 = point(list.first(), cmd, "section line start")?;
    let p2 = point(list.get(1), cmd, "section line end")?;
    if p1.dist(p2) <= 1e-6 {
        return Err(bad(cmd, format!("section {}: the cutting line has no length", index + 1)));
    }
    Ok([p1, p2])
}

/// An optional one-letter identifier, upper-cased.
fn section_letter_param(value: Option<&Value>, index: usize, cmd: &str) -> Result<Option<char>> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(text) = value.as_str() else {
        return Err(bad(cmd, format!("section {}: `letter` must be one letter A–Z", index + 1)));
    };
    let mut chars = text.trim().chars();
    let (Some(letter), None) = (chars.next(), chars.next()) else {
        return Err(bad(cmd, format!("section {}: `letter` must be one letter A–Z", index + 1)));
    };
    if !letter.is_ascii_alphabetic() {
        return Err(bad(cmd, format!("section {}: `letter` must be one letter A–Z", index + 1)));
    }
    Ok(Some(letter.to_ascii_uppercase()))
}

/// Reject unknown keys so a misspelt parameter is an error, not silently ignored.
fn only_fields(object: &Map<String, Value>, cmd: &str, allowed: &[&str], what: &str) -> Result<()> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(bad(cmd, format!("unknown {what} field `{key}`")));
        }
    }
    Ok(())
}

/// The view a section names, by the names the drawing commands use.
fn annotation_view(value: Option<&Value>, cmd: &str) -> Result<StandardView> {
    let Some(name) = value.and_then(Value::as_str) else {
        return Err(bad(cmd, "a section needs a `view` name"));
    };
    named_view(name).ok_or_else(|| {
        let shown: String = name.chars().take(32).collect();
        bad(cmd, format!("unknown view `{shown}`: use front, top, right, back, bottom or left"))
    })
}

/// The six sheet views by name.
pub fn named_view(name: &str) -> Option<StandardView> {
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

/// A finite number within [`MAX_SECTION_VALUE_MM`].
fn number(value: Option<&Value>, cmd: &str, what: &str) -> Result<f64> {
    let Some(n) = value.and_then(Value::as_f64) else {
        return Err(bad(cmd, format!("{what} must be a finite number")));
    };
    if !(n.is_finite() && n.abs() <= MAX_SECTION_VALUE_MM) {
        return Err(bad(cmd, format!("{what} must be finite and within ±{MAX_SECTION_VALUE_MM} mm")));
    }
    Ok(n)
}

/// A `[x, y]` point of at most [`MAX_SECTION_VALUE_MM`].
fn point(value: Option<&Value>, cmd: &str, what: &str) -> Result<Vec2> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Err(bad(cmd, format!("{what} must be [x, y]")));
    };
    if list.len() != 2 {
        return Err(bad(cmd, format!("{what} must be [x, y]")));
    }
    Ok(Vec2::new(number(list.first(), cmd, what)?, number(list.get(1), cmd, what)?))
}

/// A `[x, y, z]` point of at most [`MAX_SECTION_VALUE_MM`].
fn point3(value: Option<&Value>, cmd: &str, what: &str) -> Result<Vec3> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Err(bad(cmd, format!("{what} must be [x, y, z]")));
    };
    if list.len() != 3 {
        return Err(bad(cmd, format!("{what} must be [x, y, z]")));
    }
    Ok(Vec3::new(number(list.first(), cmd, what)?, number(list.get(1), cmd, what)?, number(list.get(2), cmd, what)?))
}

/// How the sheet chooses its sections.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SectionMode {
    /// Read them from the part ([`SectionMode::Auto`] is D9's default).
    #[default]
    Auto,
    /// Draw none.
    None,
    /// Use the caller's list.
    Explicit(Vec<SectionSpec>),
}

impl SectionMode {
    /// The word the commands report (`"auto"`, `"none"`, `"explicit"`).
    pub fn name(&self) -> &'static str {
        match self {
            SectionMode::Auto => "auto",
            SectionMode::None => "none",
            SectionMode::Explicit(_) => "explicit",
        }
    }

    /// The letter identifiers of an explicit list (empty for auto and none).
    pub fn letters(&self) -> Vec<char> {
        match self {
            SectionMode::Explicit(specs) => assigned_letters(specs).unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// A freehand break-out region in a view's own millimetres: an ellipse in the local frame
/// (`centre + u·rx·cos θ + v·ry·sin θ`) with a smooth 1.5 mm sine modulation.
#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    pub centre: Vec2,
    /// Local x direction (unit).
    pub u: Vec2,
    /// Local y direction (unit).
    pub v: Vec2,
    pub rx: f64,
    pub ry: f64,
    /// Phase of the sine so two boundaries on one sheet do not look stamped.
    pub phase: f64,
}

impl Boundary {
    /// A circular boundary (an explicit `around: [x, y, r]`).
    pub fn circle(x: f64, y: f64, r: f64) -> Boundary {
        Boundary { centre: Vec2::new(x, y), u: Vec2::X, v: Vec2::Y, rx: r, ry: r, phase: 0.0 }
    }

    /// A boundary in the axis frame: `along` is the projected part axis, `rx` the half length
    /// along it and `ry` the half width across it.
    pub fn around_axis(centre: Vec2, along: Vec2, rx: f64, ry: f64, phase: f64) -> Option<Boundary> {
        let u = along.normalized()?;
        Some(Boundary { centre, u, v: u.perp(), rx, ry, phase })
    }

    /// The closed freehand curve: 64 points, smooth (a sine of its radius, never a straight line
    /// and never a zigzag). The last point is not repeated.
    ///
    /// The wobble is two or three incommensurate sines whose phases come from the window's
    /// position, so two windows on one sheet do not look stamped and the curve does not read as a
    /// flower. Its peak is the window-scaled amplitude ([`freehand_amplitude`]).
    pub fn points(&self) -> Vec<Vec2> {
        const SEGMENTS: usize = 64;
        let ok = self.centre.is_finite()
            && self.u.is_finite()
            && self.v.is_finite()
            && self.rx.is_finite()
            && self.ry.is_finite()
            && self.phase.is_finite();
        if !ok || self.rx <= 0.0 || self.ry <= 0.0 {
            return Vec::new();
        }
        let phases = [
            self.phase + self.centre.x * 0.7 + self.centre.y * 0.31,
            self.phase + self.centre.y * 1.3 - self.centre.x * 0.43 + 1.1,
            self.phase + self.centre.x * 0.29 - self.centre.y * 0.91 + 2.2,
        ];
        let wave = |t: f64| -> f64 { 0.62 * (3.0 * t + phases[0]).sin() + 0.28 * (7.0 * t + phases[1]).sin() + 0.10 * (11.0 * t + phases[2]).sin() };
        let mut peak = 0.0_f64;
        for i in 0..SEGMENTS {
            let t = TAU * i as f64 / SEGMENTS as f64;
            peak = peak.max(wave(t).abs());
        }
        if !(peak > 1e-9) {
            return Vec::new();
        }
        let amplitude = freehand_amplitude(self.rx.min(self.ry)) / peak;
        (0..SEGMENTS)
            .map(|i| {
                let t = TAU * i as f64 / SEGMENTS as f64;
                let wobble = amplitude * wave(t);
                self.centre + self.u * ((self.rx + wobble) * t.cos()) + self.v * ((self.ry + wobble) * t.sin())
            })
            .collect()
    }

    /// The boundary's bounding box.
    pub fn bounds(&self) -> [Vec2; 2] {
        let points = self.points();
        let mut lo = Vec2::new(f64::INFINITY, f64::INFINITY);
        let mut hi = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in points {
            lo = Vec2::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Vec2::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        [lo, hi]
    }
}

/// One view the sheet cuts: the cutting plane, the freehand boundary (break-outs only) and the
/// letter of a full section. The boundary is in the view's own millimetres. A section picked from
/// a line keeps that line, so the GUI can show its handles.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewCut {
    pub view: StandardView,
    pub plane: CuttingPlane,
    pub boundary: Option<Boundary>,
    pub label: Option<char>,
    /// The line the plane was derived from (D9.2's `from` form).
    pub from: Option<SectionLine>,
}

impl ViewCut {
    /// Whether this is a full section rather than a break-out.
    pub fn is_full(&self) -> bool {
        self.boundary.is_none()
    }
}

/// The section letter for index `i` of a sheet: A, B, C…
pub fn section_letter(index: usize) -> char {
    char::from(b'A'.saturating_add(u8::try_from(index % 26).unwrap_or(0)))
}

/// The cuts a plan asks for: the automatic ones, none, or the caller's list with its letters.
pub fn plan_cuts(s: &Session, selection: &ViewSelection, views: &[ProjectedView], mode: &SectionMode) -> Result<Vec<ViewCut>> {
    let cuts = match mode {
        SectionMode::None => Vec::new(),
        SectionMode::Auto => auto_cuts(s, selection, views),
        SectionMode::Explicit(specs) => {
            let mut cuts = Vec::new();
            for (index, spec) in specs.iter().take(MAX_SECTIONS).enumerate() {
                let letter = spec.letter().unwrap_or(section_letter(index));
                match spec {
                    SectionSpec::Full { view, plane, .. } => {
                        cuts.push(ViewCut { view: *view, plane: *plane, boundary: None, label: Some(letter), from: None });
                    }
                    SectionSpec::FullFrom { from, .. } => {
                        let (view, plane) = derive_full_from(selection, from)?;
                        cuts.push(ViewCut { view, plane, boundary: None, label: Some(letter), from: Some(*from) });
                    }
                    SectionSpec::Breakout { view, around, .. } => {
                        let [x, y, r] = *around;
                        if r > 0.0 && x.is_finite() && y.is_finite() {
                            cuts.push(ViewCut {
                                view: *view,
                                plane: world_plane_for(*view, s),
                                boundary: Some(Boundary::circle(x, y, r)),
                                label: None,
                                from: None,
                            });
                        }
                    }
                }
            }
            cuts
        }
    };
    // The derived planes can bring two entries onto one view: D9's rules still hold there.
    if matches!(mode, SectionMode::Explicit(_)) {
        validate_cut_conflicts(&cuts)?;
    }
    Ok(cuts)
}

/// Refuse a view carrying both a full section and a break-out, or two different cutting planes.
fn validate_cut_conflicts(cuts: &[ViewCut]) -> Result<()> {
    for (index, cut) in cuts.iter().enumerate() {
        for other in &cuts[..index] {
            if other.view != cut.view {
                continue;
            }
            if other.is_full() != cut.is_full() {
                return Err(bad("drawing.sheet", format!("view `{}` cannot be a full section and a break-out at once", view_name(cut.view))));
            }
            if other.plane != cut.plane {
                return Err(bad("drawing.sheet", format!("view `{}` can carry one cutting plane", view_name(cut.view))));
            }
        }
    }
    Ok(())
}

/// The cutting plane and sectioned view of a line section: the plane contains the line and the
/// picked view's direction, and its normal is the in-plane perpendicular to the line. The
/// sectioned view is the chosen view looking along ±that normal within 1°; without one the line
/// cannot be shown as a section (there are no auxiliary views in this phase).
pub fn derive_full_from(selection: &ViewSelection, from: &SectionLine) -> Result<(StandardView, CuttingPlane)> {
    let camera = crate::drawing::standard_camera(from.view);
    let (right, up, back) = camera.basis();
    let to_world = |p: Vec2| right * p.x + up * p.y;
    let (p1, p2) = (to_world(from.line[0]), to_world(from.line[1]));
    let direction = p2 - p1;
    if !(direction.len() > 1e-6) {
        return Err(bad("drawing.sheet", "the cutting line has no length"));
    }
    let Some(normal) = direction.cross(back).normalized() else {
        return Err(bad("drawing.sheet", "the cutting line does not define a cutting plane"));
    };
    let within_one_degree = ONE_DEG.cos();
    for view in &selection.views {
        let (_, _, view_back) = crate::drawing::standard_camera(*view).basis();
        if view_back.dot(normal).abs() >= within_one_degree {
            // The normal faces the view the section is drawn in.
            let sign = if view_back.dot(normal) >= 0.0 { 1.0 } else { -1.0 };
            return Ok((*view, CuttingPlane { point: p1, normal: normal * sign }));
        }
    }
    Err(bad("drawing.sheet", "no view looks along this cutting plane"))
}

/// Margin a freehand window keeps from the step rectangle it sits in (view mm): the test allows
/// 0.1 mm outside the outline, so 0.6 mm stays safe at every scale.
const FIT_MARGIN_MM: f64 = 0.6;

/// Shrink a break-out window until it stays inside the rectangle of the external step it belongs
/// to, less [`FIT_MARGIN_MM`]: the freehand line never leaves the part's silhouette (D9.1). The
/// window keeps its centre and its proportions; the wobble amplitude follows the new size.
fn fit_inside_step(boundary: &mut Boundary, step_centre: Vec2, half_along: f64, half_across: f64) {
    if !(half_along.is_finite() && half_across.is_finite() && half_along > 0.0 && half_across > 0.0) {
        return;
    }
    let u = boundary.u;
    let v = boundary.v;
    let offset = boundary.centre - step_centre;
    let along = offset.dot(u).abs();
    let across = offset.dot(v).abs();
    for _ in 0..4 {
        let amplitude = freehand_amplitude(boundary.rx.min(boundary.ry));
        let max_rx = (half_along - along - amplitude - FIT_MARGIN_MM).max(0.35);
        let max_ry = (half_across - across - amplitude - FIT_MARGIN_MM).max(0.35);
        let (rx, ry) = (boundary.rx.min(max_rx), boundary.ry.min(max_ry));
        if (rx - boundary.rx).abs() <= 1e-12 && (ry - boundary.ry).abs() <= 1e-12 {
            break;
        }
        boundary.rx = rx;
        boundary.ry = ry;
    }
}

/// The automatic cuts for a sheet.
fn auto_cuts(s: &Session, selection: &ViewSelection, views: &[ProjectedView]) -> Vec<ViewCut> {
    if selection.choice != ViewChoice::Turned {
        return Vec::new();
    }
    let Some(axis) = centre::turned_axis(s) else { return Vec::new() };
    let world = centre::world_axis(axis.axis);
    let Some(view) = longitudinal(selection, views, world) else { return Vec::new() };
    let Some(projected) = views.iter().find(|v| v.view == view) else { return Vec::new() };
    let a = solidcraft_geom::UnitVec3::new(centre::world_axis(axis.axis)).unwrap_or(axis.axis);
    let back = crate::drawing::standard_camera(view).basis().2;
    let plane = CuttingPlane { point: axis.origin, normal: back };
    let faces = centre::surface_faces(s);
    let size = s.world_state().bodies.iter().map(|b| b.outline_bounds().diagonal()).fold(1.0, f64::max);
    let tolerance = AXIS_DISTANCE_FRACTION * size;
    let z_tol = (1e-3 * size).max(1e-5);

    // External cylinders and bores on the part axis.
    let mut external: Vec<(f64, f64, f64)> = Vec::new();
    let mut bores: Vec<(f64, f64, f64)> = Vec::new();
    for face in &faces {
        let solidcraft_kernel::FaceSurface::Cylinder { origin, axis: face_axis, radius } = face.surface else { continue };
        if !parallel(face_axis, a) || axis_line_distance(axis.origin, a, origin, face_axis) > tolerance {
            continue;
        }
        if face.coverage() < TAU - FULL_TURN_TOL {
            continue;
        }
        let Some((lo, hi)) = face.extent(axis.origin, a) else { continue };
        let step = (radius, lo.min(hi), lo.max(hi));
        if face.radial_side(origin, face_axis) > 0.0 {
            external.push(step);
        } else {
            bores.push(step);
        }
    }
    external.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    external.dedup_by(|a, b| (a.0 - b.0).abs() <= 1e-6 && (a.1 - b.1).abs() <= z_tol && (a.2 - b.2).abs() <= z_tol);
    bores.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    bores.dedup_by(|a, b| (a.0 - b.0).abs() <= 1e-6 && (a.1 - b.1).abs() <= z_tol && (a.2 - b.2).abs() <= z_tol);
    let max_radius = external.iter().map(|(r, _, _)| *r).fold(0.0, f64::max);
    if !(max_radius > 0.0) {
        return Vec::new();
    }
    let (lo, hi) = external.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (_, z0, z1)| (lo.min(*z0), hi.max(*z1)));
    if !(lo.is_finite() && hi.is_finite()) {
        return Vec::new();
    }

    // Hollow: a bore or recess that takes a large share of the section.
    let hollow = bores.iter().any(|(r, _, _)| *r >= max_radius * HOLLOW_SHARE);
    if hollow {
        return vec![ViewCut { view, plane, boundary: None, label: Some('A'), from: None }];
    }

    // Solid turned part: a local break-out around every internal feature that opens from the
    // outside. The keyway stays as it is: its floor is already a visible outline in the
    // longitudinal view (or, when it faces away, the axial cutting plane does not meet it).
    let along = projected_axis(projected, a.vec()).unwrap_or(Vec2::X);
    let perp = Vec2::new(-along.y, along.x);
    let axis_point = |t: f64| project_point(projected, axis.origin + a * t);
    let local_radius = |t: f64| external.iter().filter(|(_, z0, z1)| t >= *z0 - z_tol && t <= *z1 + z_tol).map(|(r, _, _)| *r).fold(0.0, f64::max);
    let mut cuts: Vec<ViewCut> = Vec::new();
    let mut phase = 0.0;
    // End bores: a band from the mouth to just past the far end of the bore.
    let mut seen: Vec<(f64, f64, f64)> = Vec::new();
    for (radius, z0, z1) in &bores {
        let open_lo = (*z0 - lo).abs() <= z_tol || *z0 < lo;
        let open_hi = (*z1 - hi).abs() <= z_tol || *z1 > hi;
        if !(open_lo || open_hi) {
            continue;
        }
        if seen.iter().any(|(r, a0, a1)| (*r - radius).abs() <= 1e-6 && (*a0 - z0).abs() <= z_tol && (*a1 - z1).abs() <= z_tol) {
            continue;
        }
        seen.push((*radius, *z0, *z1));
        let mid = (z0 + z1) * 0.5;
        let outer = local_radius(mid);
        if !(outer > 0.0) {
            continue;
        }
        let half_len = (z1 - z0) * 0.5 + 2.0;
        // The sine wobble must stay inside the silhouette: keep 2 mm off the outline.
        let half_width = (radius * 1.8).max(radius + 1.5).min((outer - 2.0).max(radius + 1.0));
        if let Some(mut boundary) = Boundary::around_axis(axis_point(mid), along, half_len, half_width, phase) {
            // A window around a bore that reaches the end face would cross the outline there:
            // fit it into the step's own rectangle, 0.6 mm inside the face (D9.1).
            if let Some((r, s0, s1)) = external.iter().find(|(_, s0, s1)| mid >= *s0 - z_tol && mid <= *s1 + z_tol).copied() {
                fit_inside_step(&mut boundary, axis_point((s0 + s1) * 0.5), (s1 - s0) * 0.5, r);
            }
            cuts.push(ViewCut { view, plane, boundary: Some(boundary), label: None, from: None });
            phase += 1.7;
        }
    }
    // Cross holes: a band from the outline to the other side, through the hole axis.
    let mut cross_seen: Vec<f64> = Vec::new();
    for face in &faces {
        let solidcraft_kernel::FaceSurface::Cylinder { origin, axis: hole_axis, radius } = face.surface else { continue };
        if parallel(hole_axis, a) || face.coverage() < TAU - FULL_TURN_TOL || face.radial_side(origin, hole_axis) >= 0.0 {
            continue;
        }
        if axis_line_distance(axis.origin, a, origin, hole_axis) > max_radius * 0.25 {
            continue;
        }
        let t_h = (origin - axis.origin).dot(a);
        if cross_seen.iter().any(|t| (*t - t_h).abs() <= z_tol) {
            continue;
        }
        cross_seen.push(t_h);
        let outer = local_radius(t_h);
        if !(outer > 0.0) {
            continue;
        }
        // A band from the near outline to just past the hole (ISO 128-3): the boundary is
        // centred between the outline and the axis on the side the hole opens to. A hole that
        // points at the viewer has no side; its band is symmetric.
        let hole2 = project_dir(projected, hole_axis.vec());
        let side = if hole2.len() > 0.5 { hole2.dot(perp).signum() } else { 0.0 };
        let half_len = (radius * 3.0).max(outer * 0.35).min((hi - lo) * 0.5);
        // A larger part gets the one-sided band from the near outline to just past the hole;
        // a small one (a Ø10 end) a symmetric window that still keeps the 1.5 mm
        // freehand wobble inside its silhouette.
        let one_sided = side.abs() > 0.5 && outer >= 10.0;
        let offset = if one_sided { outer * 0.5 } else { 0.0 };
        let half_width = (outer - offset - 1.6).max(2.0);
        let centre = axis_point(t_h) + perp * (side * offset);
        if let Some(mut boundary) = Boundary::around_axis(centre, along, half_len, half_width, phase) {
            // A cross hole near an end face gets the same silhouette fit.
            if let Some((r, s0, s1)) = external.iter().find(|(_, s0, s1)| t_h >= *s0 - z_tol && t_h <= *s1 + z_tol).copied() {
                fit_inside_step(&mut boundary, axis_point((s0 + s1) * 0.5), (s1 - s0) * 0.5, r);
            }
            cuts.push(ViewCut { view, plane, boundary: Some(boundary), label: None, from: None });
            phase += 1.7;
        }
    }
    let _ = perp;
    cuts.truncate(MAX_SECTIONS);
    cuts
}

/// The longitudinal view of a turned part: the chosen view whose camera looks across `axis`,
/// preferring one where the axis lies horizontal.
fn longitudinal(selection: &ViewSelection, views: &[ProjectedView], axis: Vec3) -> Option<StandardView> {
    let mut best: Option<(StandardView, bool)> = None;
    for view in &selection.views {
        if !views.iter().any(|v| v.view == *view) {
            continue;
        }
        let (_, up, back) = crate::drawing::standard_camera(*view).basis();
        if back.dot(axis).abs() > ONE_DEG.sin() {
            continue;
        }
        let horizontal = up.dot(axis).abs() <= ONE_DEG.sin();
        match best {
            None => best = Some((*view, horizontal)),
            Some((_, was_horizontal)) if horizontal && !was_horizontal => best = Some((*view, true)),
            _ => {}
        }
    }
    best.map(|(view, _)| view)
}

/// The world plane of an explicit break-out: through the part axis (or, without a turned axis,
/// through the visible bounds' centre) with the view's viewing direction as the normal.
pub fn world_plane_for(view: StandardView, s: &Session) -> CuttingPlane {
    let back = crate::drawing::standard_camera(view).basis().2;
    let point = centre::turned_axis(s).map_or_else(
        || {
            let mut b = solidcraft_geom::Aabb3::EMPTY;
            for body in s.world_state().bodies.iter().filter(|b| s.body_visible(&b.name)) {
                b = b.union(&body.outline_bounds());
            }
            if b.is_empty() { Vec3::ZERO } else { b.center() }
        },
        |axis| axis.origin,
    );
    CuttingPlane { point, normal: back }
}

fn parallel(a: solidcraft_geom::UnitVec3, b: solidcraft_geom::UnitVec3) -> bool {
    a.cross(b).len() <= 1e-6
}

fn axis_line_distance(p1: Vec3, d1: solidcraft_geom::UnitVec3, p2: Vec3, d2: solidcraft_geom::UnitVec3) -> f64 {
    let n = d1.cross(d2);
    if n.len() < 1e-9 { (p2 - p1).cross(d1.vec()).len() } else { (p2 - p1).dot(n.normalized().unwrap_or(d1.vec())).abs() }
}

/// Project a world point into a view's own millimetres.
pub fn project_point(view: &ProjectedView, p: Vec3) -> Vec2 {
    let (right, up, _) = view.camera.basis();
    let d = p - view.camera.target;
    Vec2::new(d.dot(right), d.dot(up))
}

/// The projected unit direction of a world axis in a view, if it lies in the view plane.
fn projected_axis(view: &ProjectedView, axis: Vec3) -> Option<Vec2> {
    let (right, up, _) = view.camera.basis();
    let d = Vec2::new(axis.dot(right), axis.dot(up));
    (d.len() > 0.5).then(|| d.normalized()).flatten()
}

/// The projected direction of a world axis in a view (not normalised; zero when it points at
/// the viewer).
fn project_dir(view: &ProjectedView, axis: Vec3) -> Vec2 {
    let (right, up, _) = view.camera.basis();
    Vec2::new(axis.dot(right), axis.dot(up))
}

// --------------------------------------------------------------------------- cut loops and hatch

/// Pairs of nearly equal points are the same vertex of the cut outline.
fn same(p: Vec2, q: Vec2, tol: f64) -> bool {
    p.dist(q) <= tol
}

/// Chain plane–mesh intersection segments into closed loops (view or sheet millimetres). At a
/// vertex where more than two segments meet, the straightest continuation wins, so a plane
/// through a tessellation vertex still follows the contour.
pub fn chain_loops(segments: &[(Vec2, Vec2)], tol: f64) -> Vec<Vec<Vec2>> {
    let tol = tol.max(1e-9);
    let usable: Vec<(Vec2, Vec2)> = segments.iter().copied().filter(|(a, b)| a.is_finite() && b.is_finite() && a.dist(*b) > tol).collect();
    let mut used = vec![false; usable.len()];
    let mut loops = Vec::new();
    for start in 0..usable.len() {
        if used.get(start).copied().unwrap_or(true) {
            continue;
        }
        let Some((first, second)) = usable.get(start).copied() else { continue };
        if let Some(slot) = used.get_mut(start) {
            *slot = true;
        }
        let mut points = vec![first, second];
        let origin = first;
        let mut at = second;
        let mut previous = first;
        for _ in 0..usable.len().saturating_add(1) {
            if same(at, origin, tol) {
                break;
            }
            let mut best: Option<(usize, bool, f64)> = None;
            for (index, (a, b)) in usable.iter().enumerate() {
                if used.get(index).copied().unwrap_or(true) {
                    continue;
                }
                let (next, from_a) = if same(*a, at, tol) {
                    (*b, true)
                } else if same(*b, at, tol) {
                    (*a, false)
                } else {
                    continue;
                };
                let incoming = at - previous;
                let outgoing = next - at;
                let turn = match (incoming.normalized(), outgoing.normalized()) {
                    (Some(u), Some(v)) => u.dot(v),
                    _ => 1.0,
                };
                if best.is_none_or(|(_, _, score)| turn > score + 1e-12) {
                    best = Some((index, from_a, turn));
                }
            }
            let Some((index, _, _)) = best else { break };
            let Some((a, b)) = usable.get(index).copied() else { break };
            if let Some(slot) = used.get_mut(index) {
                *slot = true;
            }
            let next = if same(a, at, tol) { b } else { a };
            points.push(next);
            previous = at;
            at = next;
        }
        if points.len() >= 3 {
            if same(at, origin, tol) {
                points.pop();
            }
            loops.push(points);
        }
    }
    loops
}

/// The signed area of a closed polygon (positive counter-clockwise).
pub fn polygon_area(points: &[Vec2]) -> f64 {
    let mut sum = 0.0;
    for pair in points.windows(2) {
        if let [a, b] = pair {
            sum += a.cross(*b);
        }
    }
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        sum += last.cross(*first);
    }
    sum * 0.5
}

/// Whether `p` lies inside the closed polygon (ray casting, boundary counts as inside).
pub fn inside_polygon(p: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (Some(a), Some(b)) = (polygon.get(i), polygon.get(j)) else { break };
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

/// The parameters `t` along `a + d·t` where the segment crosses the line at normal offset
/// `offset`; `normal` is the unit normal of the hatch direction.
fn line_crossings(a: Vec2, b: Vec2, dir: Vec2, normal: Vec2, offset: f64) -> Option<f64> {
    let sa = a.dot(normal) - offset;
    let sb = b.dot(normal) - offset;
    if (sa > 0.0) == (sb > 0.0) || (sa - sb).abs() < 1e-15 {
        return None;
    }
    let t = sa / (sa - sb);
    let p = a + (b - a) * t;
    Some(p.dot(dir))
}

/// One material region of a cut: the outer loop and its hole loops.
#[derive(Clone, Debug, PartialEq)]
pub struct CutRegion {
    pub outer: Vec<Vec2>,
    pub islands: Vec<Vec<Vec2>>,
}

/// Group closed cut loops into material regions: every loop not contained in another is an outer
/// boundary, and every loop directly inside one is one of its islands (a bore or a recess).
pub fn regions(loops: &[Vec<Vec2>]) -> Vec<CutRegion> {
    let mut ordered: Vec<(usize, f64)> =
        loops.iter().enumerate().map(|(i, loop_)| (i, polygon_area(loop_).abs())).filter(|(_, area)| area.is_finite() && *area > 1e-6).collect();
    ordered.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<CutRegion> = Vec::new();
    for (index, _) in ordered {
        let Some(loop_) = loops.get(index) else { continue };
        let Some(point) = loop_.first().copied() else { continue };
        // The smallest already accepted loop that contains this one.
        let mut parent: Option<(usize, f64)> = None;
        for (candidate, region) in out.iter().enumerate() {
            let mut best = if inside_polygon(point, &region.outer) { Some((candidate, polygon_area(&region.outer).abs())) } else { None };
            for island in &region.islands {
                if inside_polygon(point, island) {
                    let area = polygon_area(island).abs();
                    if best.is_none_or(|(_, best_area)| area < best_area) {
                        best = Some((candidate, area));
                    }
                }
            }
            if let Some((candidate, area)) = best
                && parent.is_none_or(|(_, best_area)| area < best_area)
            {
                parent = Some((candidate, area));
            }
        }
        match parent {
            Some((candidate, _)) => {
                // A loop inside an island of a region is a new outer loop (a material island in
                // a void); a loop directly inside the outer region is an island.
                let in_island = out.get(candidate).is_some_and(|region| region.islands.iter().any(|island| inside_polygon(point, island)));
                if in_island {
                    out.push(CutRegion { outer: loop_.clone(), islands: Vec::new() });
                } else if let Some(region) = out.get_mut(candidate) {
                    region.islands.push(loop_.clone());
                }
            }
            None => out.push(CutRegion { outer: loop_.clone(), islands: Vec::new() }),
        }
    }
    out
}

/// Hatch spans of one region in millimetres: lines of direction `angle_deg`, `spacing` apart,
/// inside the outer loop and outside its islands, clipped to `boundary` when a break-out gives
/// one. The spans stop on the cut outline and on the freehand boundary.
pub fn hatch_spans(region: &CutRegion, spacing: f64, angle_deg: f64, boundary: Option<&[Vec2]>) -> Vec<(Vec2, Vec2)> {
    let mut out = Vec::new();
    if !(spacing.is_finite() && spacing > 1e-6) || !angle_deg.is_finite() {
        return out;
    }
    let theta = angle_deg.to_radians();
    let dir = Vec2::from_angle(theta);
    let normal = Vec2::new(-dir.y, dir.x);
    let all: Vec<&Vec<Vec2>> = std::iter::once(&region.outer).chain(region.islands.iter()).collect();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in region.outer.iter() {
        let t = point.dot(normal);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    if !(lo.is_finite() && hi.is_finite()) {
        return out;
    }
    let first = (lo / spacing).floor() * spacing;
    let count = ((hi - first) / spacing).ceil().max(0.0);
    if !count.is_finite() || count > 100_000.0 {
        return out;
    }
    for k in 0..=(count as usize) {
        let offset = first + spacing * k as f64;
        let mut crossings: Vec<f64> = Vec::new();
        for loop_ in &all {
            for pair in loop_.windows(2) {
                if let [a, b] = pair
                    && let Some(t) = line_crossings(*a, *b, dir, normal, offset)
                {
                    crossings.push(t);
                }
            }
            if let (Some(a), Some(b)) = (loop_.last(), loop_.first())
                && let Some(t) = line_crossings(*a, *b, dir, normal, offset)
            {
                crossings.push(t);
            }
        }
        crossings.retain(|t| t.is_finite());
        crossings.sort_by(f64::total_cmp);
        for [t0, t1] in crossings.as_chunks::<2>().0 {
            if t1 - t0 <= 1e-6 {
                continue;
            }
            for (start, end) in clip_span(*t0, *t1, boundary, dir, normal, offset) {
                if end - start > 1e-6 {
                    out.push((dir * start + normal * offset, dir * end + normal * offset));
                }
            }
        }
    }
    out
}

/// The parts of a span `[t0, t1]` along the hatch line that lie inside the freehand boundary.
fn clip_span(t0: f64, t1: f64, boundary: Option<&[Vec2]>, dir: Vec2, normal: Vec2, offset: f64) -> Vec<(f64, f64)> {
    let Some(boundary) = boundary.filter(|b| b.len() >= 3) else {
        return vec![(t0, t1)];
    };
    let _ = dir;
    let mut crossings = Vec::new();
    for pair in boundary.windows(2) {
        if let [a, b] = pair
            && let Some(t) = line_crossings(*a, *b, dir, normal, offset)
        {
            crossings.push(t);
        }
    }
    if let (Some(a), Some(b)) = (boundary.last(), boundary.first())
        && let Some(t) = line_crossings(*a, *b, dir, normal, offset)
    {
        crossings.push(t);
    }
    crossings.retain(|t| t.is_finite());
    crossings.sort_by(f64::total_cmp);
    let mut pieces = Vec::new();
    for [a, b] in crossings.as_chunks::<2>().0 {
        let (lo, hi) = (t0.max(*a), t1.min(*b));
        if hi > lo {
            pieces.push((lo, hi));
        }
    }
    pieces
}

/// Whether a cut region is narrower than `limit` in at least one direction: its bounding box's
/// minor extent (the paper-space test for 2 mm instead of 3 mm hatching).
pub fn region_narrow(region: &CutRegion, limit: f64) -> bool {
    let mut lo = Vec2::new(f64::INFINITY, f64::INFINITY);
    let mut hi = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in &region.outer {
        lo = Vec2::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Vec2::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    let size = hi - lo;
    size.x.is_finite() && size.y.is_finite() && size.x.min(size.y) < limit
}

/// The cut regions clipped to a break-out boundary: the material inside the freehand curve, as
/// closed loops. The region's loops are clipped to the boundary, the boundary's edges to the
/// material (inside the outer loop, outside every island), and both are chained together. This
/// is what the DXF `HATCH` writes, so a break-out is hatched only where the cut is shown.
pub fn clip_regions(cut_regions: &[CutRegion], boundary: &[Vec2], tol: f64) -> Vec<CutRegion> {
    if boundary.len() < 3 {
        return Vec::new();
    }
    let tol = tol.max(1e-9);
    let edges = |loop_: &[Vec2]| -> Vec<Seg2mm> {
        let mut out: Vec<Seg2mm> = loop_.windows(2).map(|pair| Seg2mm { a: pair[0], b: pair[1] }).collect();
        if let (Some(first), Some(last)) = (loop_.first(), loop_.last())
            && first.dist(*last) > tol
        {
            out.push(Seg2mm { a: *last, b: *first });
        }
        out
    };
    let mut segments: Vec<(Vec2, Vec2)> = Vec::new();
    for region in cut_regions {
        for loop_ in std::iter::once(&region.outer).chain(region.islands.iter()) {
            for seg in split_by_boundary(&edges(loop_), boundary, true, tol) {
                segments.push((seg.a, seg.b));
            }
        }
        for seg in edges(boundary) {
            let mut parts = split_by_boundary(std::slice::from_ref(&seg), &region.outer, true, tol);
            for island in &region.islands {
                parts = split_by_boundary(&parts, island, false, tol);
            }
            for part in parts {
                segments.push((part.a, part.b));
            }
        }
    }
    let loops = chain_loops(&segments, tol);
    regions(&loops)
}

/// The parts of `segments` inside (`keep_inside`) or outside a freehand boundary, split at the
/// boundary's edges. Segments wholly on one side keep their exact endpoints.
pub fn split_by_boundary(segments: &[Seg2mm], boundary: &[Vec2], keep_inside: bool, tol: f64) -> Vec<Seg2mm> {
    let mut out = Vec::new();
    if boundary.len() < 3 {
        return if keep_inside { Vec::new() } else { segments.to_vec() };
    }
    let mut lo = Vec2::new(f64::INFINITY, f64::INFINITY);
    let mut hi = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in boundary {
        lo = Vec2::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Vec2::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    for seg in segments {
        let seg_lo = Vec2::new(seg.a.x.min(seg.b.x), seg.a.y.min(seg.b.y));
        let seg_hi = Vec2::new(seg.a.x.max(seg.b.x), seg.a.y.max(seg.b.y));
        let disjoint = seg_hi.x < lo.x - tol || seg_lo.x > hi.x + tol || seg_hi.y < lo.y - tol || seg_lo.y > hi.y + tol;
        if disjoint {
            if !keep_inside {
                out.push(*seg);
            }
            continue;
        }
        let d = seg.b - seg.a;
        let length = d.len();
        if !(length > 1e-12) {
            continue;
        }
        let mut cuts: Vec<f64> = Vec::new();
        for pair in boundary.windows(2) {
            if let [c, e] = pair
                && let Some(t) = segment_intersection(seg.a, seg.b, *c, *e)
                && t > 1e-9
                && t < 1.0 - 1e-9
            {
                cuts.push(t);
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let mut params = vec![0.0];
        params.extend(cuts);
        params.push(1.0);
        for pair in params.windows(2) {
            let (Some(t0), Some(t1)) = (pair.first(), pair.get(1)) else { continue };
            if t1 - t0 <= 1e-9 {
                continue;
            }
            let mid = (t0 + t1) * 0.5;
            let point = seg.a + d * mid;
            if inside_polygon(point, boundary) == keep_inside {
                out.push(Seg2mm { a: seg.a + d * *t0, b: seg.a + d * *t1 });
            }
        }
    }
    out
}

/// The parameter on `a`→`b` where it crosses `c`→`d` (strictly inside both), if it does.
fn segment_intersection(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<f64> {
    let r = b - a;
    let s = d - c;
    let denom = r.cross(s);
    if denom.abs() < 1e-15 {
        return None;
    }
    let t = (c - a).cross(s) / denom;
    let u = (c - a).cross(r) / denom;
    (0.0..=1.0).contains(&t).then_some(())?;
    (0.0..=1.0).contains(&u).then_some(t)
}

/// The cut geometry of one projected view, in the view's own millimetres: the closed cut-outline
/// loops, the freehand boundary of a break-out, and the letter of a full section.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSection {
    pub full: bool,
    pub loops: Vec<Vec<Vec2>>,
    /// The freehand boundaries of the break-outs on this view (one per break-out).
    pub boundaries: Vec<Vec<Vec2>>,
    pub label: Option<char>,
    /// The cutting plane the view was cut with (all its cuts share it).
    pub plane: Option<CuttingPlane>,
    /// The picked line, when the plane came from the `from` form.
    pub from: Option<SectionLine>,
}

/// One section mark in an end view: the trace of the cutting plane, its outline half extent and
/// the direction the section is viewed in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionMark {
    /// A point of the trace in the end view's millimetres (the plane can be off the view's origin).
    pub centre: Vec2,
    /// The trace direction (unit, view mm).
    pub dir: Vec2,
    /// The half extent of the part's outline along `dir` from `centre` (view mm).
    pub half: f64,
    /// The viewing direction (unit, view mm): the arrows point this way.
    pub arrow: Vec2,
    pub label: char,
}

/// The section mark a full section draws in `end`: the trace of `plane` across the end view and
/// the viewing direction projected into it. The trace passes through the plane's own point, so a
/// section picked off the part centre draws its line where the user placed it.
pub fn section_mark(plane: CuttingPlane, end: &ProjectedView, label: char) -> Option<SectionMark> {
    let (right, up, back) = end.camera.basis();
    let trace = plane.normal.cross(back);
    if trace.len() < 1e-6 {
        return None;
    }
    let dir = Vec2::new(trace.dot(right), trace.dot(up)).normalized()?;
    let arrow = Vec2::new(-plane.normal.dot(right), -plane.normal.dot(up)).normalized()?;
    let centre = project_point(end, plane.point);
    let half = [
        Vec2::new(end.bbox[0].x, end.bbox[0].y),
        Vec2::new(end.bbox[1].x, end.bbox[0].y),
        Vec2::new(end.bbox[1].x, end.bbox[1].y),
        Vec2::new(end.bbox[0].x, end.bbox[1].y),
    ]
    .iter()
    .map(|p| (*p - centre).dot(dir).abs())
    .fold(0.0, f64::max);
    (half.is_finite() && half > 0.0).then_some(SectionMark { centre, dir, half, arrow, label })
}

#[cfg(test)]
#[path = "sections_tests.rs"]
mod tests;

/// The silhouette of a turned part in its longitudinal view, in the view's own millimetres: the
/// union of its external steps' rectangles (the test reference for the break-out containment
/// rule, D9.1). `None` when the part has no turned axis.
#[cfg(test)]
pub(crate) fn turned_silhouette(s: &Session, view: StandardView) -> Option<Vec<Vec2>> {
    let axis = centre::turned_axis(s)?;
    let a = solidcraft_geom::UnitVec3::new(centre::world_axis(axis.axis)).unwrap_or(axis.axis);
    let size = s.world_state().bodies.iter().map(|b| b.outline_bounds().diagonal()).fold(1.0, f64::max);
    let tolerance = AXIS_DISTANCE_FRACTION * size;
    let mut steps: Vec<(f64, f64, f64)> = Vec::new(); // radius, z0, z1
    for face in centre::surface_faces(s) {
        let solidcraft_kernel::FaceSurface::Cylinder { origin, axis: face_axis, radius } = face.surface else { continue };
        if !parallel(face_axis, a) || axis_line_distance(axis.origin, a, origin, face_axis) > tolerance {
            continue;
        }
        if face.coverage() < TAU - FULL_TURN_TOL || face.radial_side(origin, face_axis) <= 0.0 {
            continue;
        }
        let Some((lo, hi)) = face.extent(axis.origin, a) else { continue };
        let (z0, z1) = (lo.min(hi), lo.max(hi));
        match steps.iter_mut().find(|(r, _, end)| (*r - radius).abs() <= 1e-6 && z0 <= *end + 1e-6) {
            Some(step) => {
                step.1 = step.1.min(z0);
                step.2 = step.2.max(z1);
            }
            None => steps.push((radius, z0, z1)),
        }
    }
    if steps.is_empty() {
        return None;
    }
    steps.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut cuts: Vec<f64> = steps.iter().flat_map(|(_, z0, z1)| [*z0, *z1]).collect();
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() <= 1e-6);
    if cuts.len() < 2 {
        return None;
    }
    let camera = crate::drawing::standard_camera(view);
    let (right, up, back) = camera.basis();
    let project3 = |p: Vec3| Vec2::new((p - camera.target).dot(right), (p - camera.target).dot(up));
    let perp3 = a.cross(back).normalized()?;
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for pair in cuts.windows(2) {
        let [z0, z1] = [pair[0], pair[1]];
        let mid = (z0 + z1) * 0.5;
        let radius = steps.iter().filter(|(_, a0, a1)| mid >= *a0 - 1e-6 && mid <= *a1 + 1e-6).map(|(r, _, _)| *r).fold(0.0_f64, f64::max);
        if radius <= 0.0 {
            continue;
        }
        let point = |z: f64, r: f64| project3(axis.origin + a * z + perp3 * r);
        top.push(point(z0, radius));
        top.push(point(z1, radius));
        bottom.push(point(z0, -radius));
        bottom.push(point(z1, -radius));
    }
    let mut polygon = top;
    bottom.reverse();
    polygon.extend(bottom);
    (polygon.len() >= 4).then_some(polygon)
}
