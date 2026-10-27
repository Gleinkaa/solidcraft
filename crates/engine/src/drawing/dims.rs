//! Dimensions and leader notes: the primitive, its ISO 129-1 drawing and the sheet-mm geometry
//! both writers serialise.
//!
//! A [`Dimension`] belongs to a [`StandardView`] and keeps its feature points in that view's own
//! millimetres — the coordinates of [`crate::drawing::ProjectedView`]'s segments, before scale and
//! placement. Only the dimension line's distance from the feature (`offset_mm`) is a sheet
//! millimetre value: dimension spacing must not grow with the scale. Text values are the measured
//! model millimetres, formatted with a decimal comma.
//!
//! [`draw_annotations`] turns the data into [`Prim`]s in sheet millimetres and records what the
//! DXF writer needs to emit real `DIMENSION` entities with anonymous blocks. The PDF writer and
//! the page paint the same prims, so there is one layout path.
//!
//! Drawing rules (STANDARDS DM-6, ISO 129-1): extension lines have no gap at the feature and
//! overshoot the dimension line by 2 mm; terminators are closed filled arrowheads 2.5 mm long with
//! a 30° included angle; the text is 3.5 mm Helvetica, centred on the dimension line with its
//! baseline 1 mm above it, rotated with the line and readable from the bottom or the right. If the
//! space between the extension lines is too small, the arrows point in from outside with 2.5 mm
//! tails and the text moves outside. Dimension text never crosses a line: it is shifted along the
//! dimension line in 1 mm steps (and finally outside) until its box keeps 0.5 mm clear of every
//! line of the sheet (DM-5, DM-8a).

use std::f64::consts::TAU;

use solidcraft_geom::Vec2;
use solidcraft_io::text_width_mm;
use solidcraft_render::StandardView;

use crate::Result;
use crate::drawing::PlacedView;
use crate::drawing::sheet::{Layer, Paper, Prim, TextAnchor, frame_rect_for};
use crate::params::bad;

/// Height of dimension and note text (mm).
pub const DIM_TEXT_HEIGHT_MM: f64 = 3.5;
/// Distance of the text baseline above the dimension line (mm).
pub const DIM_TEXT_GAP_MM: f64 = 1.0;
/// Length of a closed filled arrowhead (mm).
pub const ARROW_LENGTH_MM: f64 = 2.5;
/// Half of the arrowhead's included angle: 2 × 15° = 30°.
pub const ARROW_HALF_ANGLE_DEG: f64 = 15.0;
/// How far an extension line reaches past the dimension line (mm).
pub const EXT_OVERSHOOT_MM: f64 = 2.0;
/// Least clear distance between a dimension text's box and every line of the sheet (mm).
pub const TEXT_CLEAR_MM: f64 = 0.5;
/// The clearance the placement searches aim for: a little more than the required minimum, so a
/// checker that rounds to 0.5 mm never reads an exactly-0.5 mm gap as a crossing.
pub(crate) const PLACEMENT_CLEAR_MM: f64 = 0.75;
/// Most dimensions one request may carry.
pub const MAX_DIMENSIONS: usize = 500;
/// Most notes one request may carry.
pub const MAX_NOTES: usize = 500;
/// Longest accepted text of one dimension or note (characters).
pub const MAX_TEXT_CHARS: usize = 200;
/// Largest accepted coordinate, radius or offset (mm). Beyond this the sheet cannot hold it.
pub const MAX_VALUE_MM: f64 = 1.0e6;
/// The first dimension line of a stack sits this far from the outline (sheet mm).
pub const STACK_FIRST_MM: f64 = 10.0;
/// Each further dimension line of a stack is this much further out (sheet mm).
pub const STACK_STEP_MM: f64 = 8.0;
/// Air kept beyond the outermost dimension line of a stack (sheet mm).
pub const STACK_AIR_MM: f64 = 5.0;
/// The dimension band around a view without dimensions (sheet mm), as D5 reserved it.
pub const DIM_BAND_MM: f64 = 12.0;
/// The command id used in errors from this module.
const CMD: &str = "drawing.sheet";

/// The geometry of one dimension, in the view's own millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DimensionGeometry {
    /// A linear dimension along `angle_deg` (0 = horizontal, 90 = vertical).
    Linear { p1: Vec2, p2: Vec2, angle_deg: f64, offset_mm: f64 },
    /// A linear dimension across a round feature, prefixed `Ø`. In a longitudinal view the
    /// dimension line may run through the part when `offset_mm` is 0.
    Diameter { p1: Vec2, p2: Vec2, angle_deg: f64, offset_mm: f64 },
    /// One arrow at the arc, text `R…`.
    Radius { centre: Vec2, radius: f64, at_angle_deg: f64 },
    /// A `Ø` dimension across a circle in an end view.
    DiameterOnCircle { centre: Vec2, radius: f64, at_angle_deg: f64 },
}

impl DimensionGeometry {
    /// The measured model value (mm): the projected distance for a linear dimension, the radius
    /// for a radius, the diameter for a circle dimension.
    pub fn measured(&self) -> f64 {
        match *self {
            DimensionGeometry::Linear { p1, p2, angle_deg, .. } | DimensionGeometry::Diameter { p1, p2, angle_deg, .. } => {
                let u = Vec2::from_angle(angle_deg.to_radians());
                (p2 - p1).dot(u).abs()
            }
            DimensionGeometry::Radius { radius, .. } => radius.abs(),
            DimensionGeometry::DiameterOnCircle { radius, .. } => 2.0 * radius.abs(),
        }
    }

    /// The symbol the kind puts in front of the value (`Ø`, `R`, or nothing).
    pub fn prefix(&self) -> &'static str {
        match self {
            DimensionGeometry::Linear { .. } => "",
            DimensionGeometry::Diameter { .. } | DimensionGeometry::DiameterOnCircle { .. } => "Ø",
            DimensionGeometry::Radius { .. } => "R",
        }
    }

    /// The side of the sheet this dimension's line lies on, for the stacking band: a horizontal
    /// dimension is above or below, a vertical one to the left or right. A line through the
    /// feature (offset 0), a radius and a circle dimension need no band.
    pub fn side(&self) -> Option<DimSide> {
        match *self {
            DimensionGeometry::Linear { angle_deg, offset_mm, .. } | DimensionGeometry::Diameter { angle_deg, offset_mm, .. } => {
                if !(offset_mm.is_finite() && offset_mm.abs() > 1e-9) {
                    return None;
                }
                let u = Vec2::from_angle(angle_deg.to_radians());
                if u.x.abs() >= u.y.abs() {
                    Some(if offset_mm > 0.0 { DimSide::Above } else { DimSide::Below })
                } else {
                    // The normal of a 90° dimension is (-1, 0): a positive offset goes left.
                    Some(if offset_mm > 0.0 { DimSide::Left } else { DimSide::Right })
                }
            }
            DimensionGeometry::Radius { .. } | DimensionGeometry::DiameterOnCircle { .. } => None,
        }
    }

    /// Whether every number is finite and within [`MAX_VALUE_MM`] and a radius is positive.
    pub fn is_valid(&self) -> bool {
        let ok = |v: f64| v.is_finite() && v.abs() <= MAX_VALUE_MM;
        let point = |p: Vec2| ok(p.x) && ok(p.y);
        match *self {
            DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm } | DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm } => {
                point(p1) && point(p2) && angle_deg.is_finite() && ok(offset_mm)
            }
            DimensionGeometry::Radius { centre, radius, at_angle_deg } | DimensionGeometry::DiameterOnCircle { centre, radius, at_angle_deg } => {
                point(centre) && ok(radius) && radius > 0.0 && at_angle_deg.is_finite()
            }
        }
    }
}

/// One side of a view a run of parallel dimensions is stacked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimSide {
    Above,
    Below,
    Left,
    Right,
}

impl DimSide {
    /// Index into the four-side arrays of [`side_rows`].
    fn index(self) -> usize {
        match self {
            DimSide::Above => 0,
            DimSide::Below => 1,
            DimSide::Left => 2,
            DimSide::Right => 3,
        }
    }
}

/// The sides as the four-element arrays of [`side_rows`]: above, below, left, right.
pub const SIDES: [DimSide; 4] = [DimSide::Above, DimSide::Below, DimSide::Left, DimSide::Right];

/// One dimension attached to a view.
#[derive(Clone, Debug, PartialEq)]
pub struct Dimension {
    /// The view whose projected millimetres the geometry uses.
    pub view: StandardView,
    pub geometry: DimensionGeometry,
    /// A full text override; when set it replaces prefix, measured value and suffix.
    pub text: Option<String>,
    /// A fit or tolerance written after one space (`h6`, `H7`, `±0,1`).
    pub suffix: Option<String>,
    /// The user's shift of the text along the dimension line (sheet mm, + along the line).
    pub text_shift_mm: f64,
}

impl Dimension {
    /// The value as the sheet writes it: the measured model millimetres with at most two
    /// decimals, trailing zeros dropped and a decimal comma (`40`, `1,5`, `0,25`).
    pub fn value_text(&self) -> String {
        format_value(self.geometry.measured())
    }

    /// The full text on the sheet. An override replaces everything; otherwise the kind's prefix
    /// and the measured value, then one space and the suffix when one is given.
    pub fn label(&self) -> String {
        if let Some(text) = &self.text {
            return text.clone();
        }
        let mut label = String::with_capacity(16);
        label.push_str(self.geometry.prefix());
        label.push_str(&self.value_text());
        if let Some(suffix) = &self.suffix
            && !suffix.is_empty()
        {
            label.push(' ');
            label.push_str(suffix);
        }
        label
    }
}

/// A leader note. Its text starts at `at` (sheet mm offset from the view's placed origin, at the
/// text baseline); `leader` and `target` are view millimetres like a dimension's points.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub view: StandardView,
    pub text: String,
    /// Left end of the text baseline, in sheet mm offset from the view's placed origin.
    pub at: Vec2,
    /// The leader polyline in view mm, starting near the text.
    pub leader: Vec<Vec2>,
    /// The point the note refers to, in view mm; the last leader point when omitted.
    pub target: Option<Vec2>,
}

/// Everything the sheet commands accept besides views and the title block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Annotations {
    pub dimensions: Vec<Dimension>,
    pub notes: Vec<Note>,
}

impl Annotations {
    pub fn is_empty(&self) -> bool {
        self.dimensions.is_empty() && self.notes.is_empty()
    }
}

/// Format a measured value: at most two decimals, trailing zeros dropped, decimal comma.
pub fn format_value(value: f64) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    let mut text = format!("{value:.2}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        text = "0".to_string();
    }
    text.replace('.', ",")
}

/// The dimension band a side needs for `rows` stacked dimension lines: the first line 10 mm from
/// the outline, 8 mm per further row and 5 mm of air. No row keeps D5's 12 mm.
pub fn band_mm(rows: usize) -> f64 {
    if rows == 0 { DIM_BAND_MM } else { STACK_FIRST_MM + STACK_STEP_MM * rows.saturating_sub(1) as f64 + STACK_AIR_MM }
}

/// The number of distinct dimension-line rows on each side of a view's dimensions, in [`SIDES`]
/// order (above, below, left, right).
pub fn side_rows(dims: &[Dimension]) -> [usize; 4] {
    let mut offsets: [Vec<f64>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for dim in dims {
        let Some(side) = dim.geometry.side() else { continue };
        let offset = match dim.geometry {
            DimensionGeometry::Linear { offset_mm, .. } | DimensionGeometry::Diameter { offset_mm, .. } => offset_mm,
            DimensionGeometry::Radius { .. } | DimensionGeometry::DiameterOnCircle { .. } => continue,
        };
        let list = &mut offsets[side.index()];
        if !list.iter().any(|other| (other - offset).abs() < 1e-6) {
            list.push(offset);
        }
    }
    [offsets[0].len(), offsets[1].len(), offsets[2].len(), offsets[3].len()]
}

/// Assign the offsets of one stack of parallel dimensions on `side` of `view`: the shortest line
/// first, the first 10 mm from the view's outline, each next one 8 mm further out. Offsets are
/// sheet millimetres; `scale` is the sheet's scale factor so the view's outline is known.
///
/// The returned dimensions are ordered shortest first, as the stack is drawn.
pub fn stack(view: &crate::drawing::ProjectedView, side: DimSide, dims: &[Dimension], scale: f64) -> Vec<Dimension> {
    let mut ordered: Vec<Dimension> = dims.to_vec();
    ordered.sort_by(|a, b| a.geometry.measured().total_cmp(&b.geometry.measured()));
    let rows: Vec<Vec<Dimension>> = ordered.into_iter().map(|dim| vec![dim]).collect();
    stack_rows(view, side, &rows, scale).into_iter().flatten().collect()
}

/// [`stack`] with groups: every dimension of one inner vector shares one row and one dimension
/// line (a chain of end-to-end lengths), the first group at [`STACK_FIRST_MM`] from the outline,
/// each next one [`STACK_STEP_MM`] further out. Groups keep the caller's order; the rows are
/// returned in that order.
pub fn stack_rows(view: &crate::drawing::ProjectedView, side: DimSide, rows: &[Vec<Dimension>], scale: f64) -> Vec<Vec<Dimension>> {
    let (lo, hi) = (view.bbox[0] * scale, view.bbox[1] * scale);
    let mut out = Vec::with_capacity(rows.len());
    for (index, group) in rows.iter().enumerate() {
        let distance = STACK_FIRST_MM + STACK_STEP_MM * index as f64;
        let mut placed = Vec::with_capacity(group.len());
        for dim in group {
            let mut dim = dim.clone();
            dim.geometry = match dim.geometry {
                DimensionGeometry::Linear { p1, p2, .. } => {
                    let p = p1 * scale;
                    let (angle_deg, offset_mm) = match side {
                        DimSide::Above => (0.0, hi.y + distance - p.y),
                        DimSide::Below => (0.0, lo.y - distance - p.y),
                        DimSide::Left => (90.0, p.x - (lo.x - distance)),
                        DimSide::Right => (90.0, p.x - (hi.x + distance)),
                    };
                    DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm }
                }
                DimensionGeometry::Diameter { p1, p2, .. } => {
                    let p = p1 * scale;
                    let (angle_deg, offset_mm) = match side {
                        DimSide::Above => (0.0, hi.y + distance - p.y),
                        DimSide::Below => (0.0, lo.y - distance - p.y),
                        DimSide::Left => (90.0, p.x - (lo.x - distance)),
                        DimSide::Right => (90.0, p.x - (hi.x + distance)),
                    };
                    DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm }
                }
                other => other,
            };
            placed.push(dim);
        }
        out.push(placed);
    }
    out
}

/// Check every annotation before it reaches the sheet: finite numbers, known views, text lengths.
pub fn validate(annotations: &Annotations, views: &[StandardView]) -> Result<()> {
    if annotations.dimensions.len() > MAX_DIMENSIONS {
        return Err(bad(CMD, format!("at most {MAX_DIMENSIONS} dimensions are allowed")));
    }
    if annotations.notes.len() > MAX_NOTES {
        return Err(bad(CMD, format!("at most {MAX_NOTES} notes are allowed")));
    }
    for (index, dim) in annotations.dimensions.iter().enumerate() {
        if !views.contains(&dim.view) {
            return Err(bad(CMD, format!("dimension {} is on view `{}`, which is not on the sheet", index + 1, crate::drawing::view_name(dim.view))));
        }
        if !dim.geometry.is_valid() {
            return Err(bad(CMD, format!("dimension {} has a non-finite or out-of-range value", index + 1)));
        }
        if !(dim.text_shift_mm.is_finite() && dim.text_shift_mm.abs() <= MAX_VALUE_MM) {
            return Err(bad(CMD, format!("dimension {} has a non-finite or out-of-range text shift", index + 1)));
        }
        for (name, text) in [("text", dim.text.as_deref()), ("suffix", dim.suffix.as_deref())] {
            if let Some(text) = text {
                if text.chars().count() > MAX_TEXT_CHARS {
                    return Err(bad(CMD, format!("dimension {}: {name} is longer than {MAX_TEXT_CHARS} characters", index + 1)));
                }
                if text.chars().any(char::is_control) {
                    return Err(bad(CMD, format!("dimension {}: {name} must not contain control characters", index + 1)));
                }
            }
        }
    }
    for (index, note) in annotations.notes.iter().enumerate() {
        if !views.contains(&note.view) {
            return Err(bad(CMD, format!("note {} is on view `{}`, which is not on the sheet", index + 1, crate::drawing::view_name(note.view))));
        }
        if note.text.trim().is_empty() {
            return Err(bad(CMD, format!("note {} has empty text", index + 1)));
        }
        if note.text.chars().count() > MAX_TEXT_CHARS {
            return Err(bad(CMD, format!("note {} is longer than {MAX_TEXT_CHARS} characters", index + 1)));
        }
        if note.text.chars().any(char::is_control) {
            return Err(bad(CMD, format!("note {} must not contain control characters", index + 1)));
        }
        let ok = |v: f64| v.is_finite() && v.abs() <= MAX_VALUE_MM;
        if !(ok(note.at.x) && ok(note.at.y)) {
            return Err(bad(CMD, format!("note {} has a non-finite or out-of-range position", index + 1)));
        }
        if note.leader.is_empty() || note.leader.len() > 64 {
            return Err(bad(CMD, format!("note {} must have 1..=64 leader points", index + 1)));
        }
        if note.leader.iter().any(|p| !(ok(p.x) && ok(p.y))) || note.target.is_some_and(|p| !(ok(p.x) && ok(p.y))) {
            return Err(bad(CMD, format!("note {} has a non-finite or out-of-range leader point", index + 1)));
        }
    }
    Ok(())
}

/// A drawn dimension, kept so the DXF writer can emit a real `DIMENSION` entity and its anonymous
/// block from exactly the prims the PDF paints.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnDimension {
    pub view: StandardView,
    /// The position of this dimension in the request's `dimensions` list (automatic dimensions
    /// keep their traceability record under the same index).
    pub index: usize,
    pub kind: DimensionKind,
    /// The prims of this dimension alone (dimension line, extension lines, arrows, text).
    pub prims: Vec<Prim>,
    /// Group 10: the dimension line's definition point.
    pub def_point: Vec2,
    /// Group 11: the text midpoint.
    pub text_midpoint: Vec2,
    /// Group 13: the first extension-line origin.
    pub def_point2: Vec2,
    /// Group 14: the second extension-line origin.
    pub def_point3: Vec2,
    /// Group 15 for a diametric or radial dimension: the opposite point / point on the arc.
    pub opposite: Option<Vec2>,
    /// The measured model value (group 42).
    pub measurement: f64,
    /// The text's rotation (group 50 for a rotated dimension).
    pub angle_deg: f64,
    pub text: String,
    /// The shift the text finally used (sheet mm along the dimension line).
    pub text_shift_mm: f64,
}

/// How a drawn dimension maps onto a DXF `DIMENSION` entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimensionKind {
    /// Rotated linear (`AcDbAlignedDimension` + `AcDbRotatedDimension`).
    Rotated,
    /// Diametric (`AcDbDiametricDimension`).
    Diametric,
    /// Radial (`AcDbRadialDimension`).
    Radial,
}

/// A drawn leader note.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnNote {
    pub view: StandardView,
    pub prims: Vec<Prim>,
    /// The horizontal reference line under the text, if one is drawn.
    pub reference: Option<(Vec2, Vec2)>,
    /// The leader polyline, including the snapped end point.
    pub leader: Vec<(Vec2, Vec2)>,
    /// The note's text primitive.
    pub text: Prim,
    /// The filled arrowhead or dot (an empty vector when there is none).
    pub terminator: Vec<Vec2>,
    pub text_string: String,
}

/// The annotation geometry of one sheet: what was drawn and where.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawnAnnotations {
    pub dimensions: Vec<DrawnDimension>,
    pub notes: Vec<DrawnNote>,
    /// Every placed text's box (grown by the placement clearance), in sheet mm: the hatching is
    /// interrupted around them (ISO 128-50) and the DXF HATCH gets them as islands.
    pub text_boxes: Vec<[Vec2; 4]>,
    /// Text that could not be placed without crossing a line or leaving the frame. Such a
    /// dimension is not drawn at all; the text names it for the command result.
    pub unplaced: Vec<String>,
}

/// One line of the sheet, used for the text clear-space check.
#[derive(Clone, Copy, Debug)]
struct Seg {
    a: Vec2,
    b: Vec2,
}

impl Seg {
    fn from_prim(prim: &Prim, out: &mut Vec<Seg>) {
        match prim {
            Prim::Line { a, b, .. } => out.push(Seg { a: *a, b: *b }),
            Prim::Polyline { points, closed, .. } => {
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push(Seg { a: *a, b: *b });
                    }
                }
                if *closed
                    && points.len() > 2
                    && let (Some(a), Some(b)) = (points.last(), points.first())
                {
                    out.push(Seg { a: *a, b: *b });
                }
            }
            Prim::Arc { c, r, a0, a1, .. } => {
                // A collision obstacle only needs the shape: a 10° polyline is enough.
                let steps = (((a1 - a0).abs() / (10.0_f64).to_radians()).ceil() as usize).clamp(2, 72);
                let points: Vec<Vec2> = (0..=steps).map(|i| *c + Vec2::from_angle(a0 + (a1 - a0) * i as f64 / steps as f64) * *r).collect();
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push(Seg { a: *a, b: *b });
                    }
                }
            }
            Prim::Circle { c, r, .. } => {
                let points: Vec<Vec2> = (0..=32).map(|i| *c + Vec2::from_angle(TAU * i as f64 / 32.0) * *r).collect();
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push(Seg { a: *a, b: *b });
                    }
                }
            }
            Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
                let steps = (((t1 - t0).abs() / (10.0_f64).to_radians()).ceil() as usize).clamp(2, 72);
                let minor = major.perp() * *ratio;
                let point = |t: f64| *c + *major * t.cos() + minor * t.sin();
                let points: Vec<Vec2> = (0..=steps).map(|i| point(t0 + (t1 - t0) * i as f64 / steps as f64)).collect();
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push(Seg { a: *a, b: *b });
                    }
                }
            }
            Prim::Filled { points, .. } => {
                for pair in points.windows(2) {
                    if let [a, b] = pair {
                        out.push(Seg { a: *a, b: *b });
                    }
                }
                if points.len() > 2
                    && let (Some(a), Some(b)) = (points.last(), points.first())
                {
                    out.push(Seg { a: *a, b: *b });
                }
            }
            Prim::Text { .. } => {}
        }
    }
}

/// The text of one dimension waiting for a collision-free spot.
struct PendingText {
    /// Midpoint of the dimension line (sheet mm).
    mid: Vec2,
    /// Reading direction (unit).
    u: Vec2,
    /// Text-up direction (unit).
    n: Vec2,
    width: f64,
    /// Half the distance between the dimension line's ends.
    half: f64,
    /// How far the arrows' tails reach past the ends (0 or [`ARROW_LENGTH_MM`]).
    tail: f64,
    /// The user's requested shift.
    shift0: f64,
    /// Whether the text starts outside because the dimension is narrow.
    narrow: bool,
    /// How far outside the dimension line's end the text may sit. A radius keeps within one text
    /// height of its arrow; a diameter may never leave its arrows by more than that (D9.1).
    outside_cap: Option<f64>,
    /// The cap is hard for a diameter drawn on extension lines (the disc rows): no spot within it
    /// drops the dimension rather than floating the value on a long run-out. A radius and an
    /// in-view diameter may still take a farther spot.
    hard_cap: bool,
    /// The dimension draws extension lines; a value outside it starts past their 2 mm overshoot
    /// as well as past its arrow, so it never sits on an extension line.
    has_extension: bool,
    /// Start the inside search at the extension-line limit (a radius text sits at its arc end).
    inside_hint: bool,
}

impl PendingText {
    /// The shift limits within the extension lines: `|shift| ≤ inside` keeps the placement clearance.
    fn inside_limit(&self) -> f64 {
        (self.half - self.width * 0.5 - PLACEMENT_CLEAR_MM).max(0.0)
    }

    /// The smallest shift that puts the box outside the dimension line's end, clear of the arrow
    /// tip and, when the dimension draws extension lines, past their 2 mm overshoot. The box may
    /// sit above a narrow dimension's arrow tail; its own arrowheads are not obstacles.
    fn outside(&self) -> f64 {
        let beyond = if self.has_extension { self.tail.max(EXT_OVERSHOOT_MM) } else { self.tail };
        self.half + beyond + PLACEMENT_CLEAR_MM + self.width * 0.5
    }

    /// Whether the text box at `shift` stays inside the sheet frame.
    fn inside_frame(&self, shift: f64, frame_rect: [Vec2; 2]) -> bool {
        let [lo, hi] = frame_rect;
        let clear = PLACEMENT_CLEAR_MM;
        let s = (shift - self.width * 0.5 - clear, shift + self.width * 0.5 + clear);
        let t = (DIM_TEXT_GAP_MM - clear, DIM_TEXT_GAP_MM + DIM_TEXT_HEIGHT_MM + clear);
        let mut corners = [Vec2::ZERO; 4];
        for (i, (s, t)) in [(s.0, t.0), (s.1, t.0), (s.1, t.1), (s.0, t.1)].into_iter().enumerate() {
            corners[i] = self.mid + self.u * s + self.n * t;
        }
        corners.iter().all(|p| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y)
    }

    /// Whether the text box at `shift` (grown by [`PLACEMENT_CLEAR_MM`]) hits any line outside
    /// `own` (the dimension's own arrowheads, which the value may sit beside).
    fn collides(&self, shift: f64, lines: &[Seg], own: &[Seg]) -> bool {
        let clear = PLACEMENT_CLEAR_MM;
        let lo = Vec2::new(shift - self.width * 0.5 - clear, DIM_TEXT_GAP_MM - clear);
        let hi = Vec2::new(shift + self.width * 0.5 + clear, DIM_TEXT_GAP_MM + DIM_TEXT_HEIGHT_MM + clear);
        for seg in lines {
            if own.iter().any(|skip| skip.a == seg.a && skip.b == seg.b) {
                continue;
            }
            let local = |p: Vec2| Vec2::new((p - self.mid).dot(self.u), (p - self.mid).dot(self.n));
            if segment_hits_rect(local(seg.a), local(seg.b), lo, hi) {
                return true;
            }
        }
        false
    }
}

/// Whether the segment `a`→`b` meets the axis-aligned rectangle `lo`..`hi` (slab clipping).
pub(crate) fn segment_hits_rect(a: Vec2, b: Vec2, lo: Vec2, hi: Vec2) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [(-d.x, a.x - lo.x), (d.x, hi.x - a.x), (-d.y, a.y - lo.y), (d.y, hi.y - a.y)] {
        if p.abs() < 1e-12 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return false;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                if r < t0 {
                    return false;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
    }
    t0 <= t1
}

/// The local (s, t) text frame of a dimension line from `a` to `b`.
struct Frame {
    mid: Vec2,
    u: Vec2,
    n: Vec2,
    half: f64,
}

fn frame(a: Vec2, b: Vec2, angle_deg: f64) -> Option<Frame> {
    let theta = angle_deg.rem_euclid(180.0);
    let u = Vec2::from_angle(theta.to_radians());
    if !(u.x.is_finite() && u.y.is_finite()) {
        return None;
    }
    let n = u.perp();
    let half = a.dist(b) * 0.5;
    if !half.is_finite() {
        return None;
    }
    Some(Frame { mid: (a + b) * 0.5, u, n, half })
}

/// The three corners of a closed filled arrowhead: `apex` on the extension line, the base
/// `length` away along `direction` (unit), half-width from the 15° half angle.
fn arrowhead(apex: Vec2, direction: Vec2, length: f64) -> Vec<Vec2> {
    let half_width = length * ARROW_HALF_ANGLE_DEG.to_radians().tan();
    let base = apex + direction * length;
    let n = direction.perp();
    vec![apex, base - n * half_width, base + n * half_width]
}

/// A filled circle of diameter `diameter` as a convex polygon.
fn dot(centre: Vec2, diameter: f64) -> Vec<Vec2> {
    const SEGMENTS: usize = 16;
    let r = diameter * 0.5;
    (0..SEGMENTS)
        .map(|i| {
            let t = std::f64::consts::TAU * i as f64 / SEGMENTS as f64;
            centre + Vec2::new(r * t.cos(), r * t.sin())
        })
        .collect()
}

fn line(a: Vec2, b: Vec2) -> Prim {
    Prim::Line { a, b, layer: Layer::Dimension }
}

fn filled(points: Vec<Vec2>) -> Prim {
    Prim::Filled { points, layer: Layer::Dimension }
}

fn text(at: Vec2, angle_deg: f64, value: &str, anchor: TextAnchor) -> Prim {
    Prim::Text { at, height: DIM_TEXT_HEIGHT_MM, angle_deg, text: value.to_string(), anchor, layer: Layer::Dimension }
}

/// The point on the nearest outline segment to `target` within `max_dist` (view mm).
///
/// The view's visible and hidden edges are searched; a note lands on an edge (arrowhead) when one
/// is close enough, otherwise it points into a surface and gets a dot.
pub(crate) fn snap_to_edge(view: &crate::drawing::ProjectedView, target: Vec2, max_dist: f64) -> Option<Vec2> {
    let mut best: Option<(f64, Vec2)> = None;
    for seg in view.visible.iter().chain(&view.hidden) {
        let d = seg.b - seg.a;
        let len2 = d.dot(d);
        if !(len2 > 1e-18) {
            continue;
        }
        let t = ((target - seg.a).dot(d) / len2).clamp(0.0, 1.0);
        let p = seg.a + d * t;
        let dist = p.dist(target);
        if dist <= max_dist && best.is_none_or(|(best_dist, _)| dist < best_dist) {
            best = Some((dist, p));
        }
    }
    best.map(|(_, p)| p)
}

/// One dimension's line geometry and its pending text.
struct DimensionDraw {
    record: DrawnDimension,
    pending: PendingText,
}

/// Draw one dimension into its own prims.
fn draw_dimension(placed: &PlacedView, scale: f64, dim: &Dimension, index: usize) -> Result<DimensionDraw> {
    let origin = placed.origin;
    let to_sheet = |p: Vec2| origin + p * scale;
    let mut prims = Vec::new();
    let label = dim.label();
    let width = text_width_mm(&label, DIM_TEXT_HEIGHT_MM);
    // `a`/`b` are the dimension line's own ends (offset included); `def_point` is the DXF
    // definition point, which for a diametric dimension stays on the feature.
    let (kind, a, b, angle_deg, def_point, def_point2, def_point3, opposite, measurement, narrow, tail, has_extension) = match dim.geometry {
        DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm } | DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm } => {
            let kind = if matches!(dim.geometry, DimensionGeometry::Diameter { .. }) { DimensionKind::Diametric } else { DimensionKind::Rotated };
            let theta = angle_deg.rem_euclid(180.0);
            let u = Vec2::from_angle(theta.to_radians());
            let n = u.perp();
            let p1s = to_sheet(p1);
            let p2s = to_sheet(p2);
            let a = p1s + n * offset_mm;
            let b = p2s + n * (offset_mm - (p2 - p1).dot(n) * scale);
            let half = a.dist(b) * 0.5;
            if !half.is_finite() {
                return Err(bad(CMD, "a dimension line is not finite"));
            }
            let narrow = half * 2.0 < 2.0 * ARROW_LENGTH_MM + width + 2.0;
            let along = (b - a).normalized().unwrap_or(u);
            // Extension lines: no gap at the feature, 2 mm past the dimension line, none for a
            // dimension line that runs through the feature.
            let mut extension_lines = false;
            for (p, foot) in [(p1s, a), (p2s, b)] {
                let dist = (foot - p).dot(n);
                if dist.abs() > 1e-9 {
                    extension_lines = true;
                    prims.push(line(p, foot + n * dist.signum() * EXT_OVERSHOOT_MM));
                }
            }
            // The outside arrows of a narrow dimension belong to extension-line dimensions; a
            // diameter line through the part keeps its arrows on the outline.
            let outside_arrows = narrow && extension_lines;
            let tail = if outside_arrows { ARROW_LENGTH_MM } else { 0.0 };
            if outside_arrows {
                prims.push(line(a - along * ARROW_LENGTH_MM, b + along * ARROW_LENGTH_MM));
                prims.push(filled(arrowhead(a, -along, ARROW_LENGTH_MM)));
                prims.push(filled(arrowhead(b, along, ARROW_LENGTH_MM)));
            } else {
                prims.push(line(a, b));
                prims.push(filled(arrowhead(a, along, ARROW_LENGTH_MM)));
                prims.push(filled(arrowhead(b, -along, ARROW_LENGTH_MM)));
            }
            let (def_point, opposite) = if kind == DimensionKind::Diametric { (p1s, Some(p2s)) } else { (a, None) };
            (kind, a, b, theta, def_point, p1s, p2s, opposite, dim.geometry.measured(), narrow, tail, extension_lines)
        }
        DimensionGeometry::DiameterOnCircle { centre, radius, at_angle_deg } => {
            let theta = at_angle_deg.rem_euclid(180.0);
            let u = Vec2::from_angle(theta.to_radians());
            let cs = to_sheet(centre);
            let r = radius * scale;
            let a = cs - u * r;
            let b = cs + u * r;
            let half = a.dist(b) * 0.5;
            if !half.is_finite() {
                return Err(bad(CMD, "a dimension line is not finite"));
            }
            let narrow = half * 2.0 < 2.0 * ARROW_LENGTH_MM + width + 2.0;
            let tail = if narrow { ARROW_LENGTH_MM } else { 0.0 };
            if narrow {
                prims.push(line(a - u * ARROW_LENGTH_MM, b + u * ARROW_LENGTH_MM));
            } else {
                prims.push(line(a, b));
            }
            prims.push(filled(arrowhead(a, u, ARROW_LENGTH_MM)));
            prims.push(filled(arrowhead(b, -u, ARROW_LENGTH_MM)));
            (DimensionKind::Diametric, a, b, theta, a, a, b, Some(b), dim.geometry.measured(), narrow, tail, false)
        }
        DimensionGeometry::Radius { centre, radius, at_angle_deg } => {
            // A radius is not a full circle: its leader keeps the arc's true direction. (The
            // half-turn normalisation belongs to a diameter through a circle; it made an arc's
            // arrow point where no arc exists, D9.2 §0.) `frame` still makes the text upright.
            let theta = at_angle_deg.rem_euclid(360.0);
            let u = Vec2::from_angle(theta.to_radians());
            let cs = to_sheet(centre);
            let p = cs + u * radius * scale;
            let half = cs.dist(p) * 0.5;
            if !half.is_finite() {
                return Err(bad(CMD, "a radius dimension is not finite"));
            }
            let narrow = half * 2.0 < ARROW_LENGTH_MM + width + 2.0;
            let tail = if narrow { ARROW_LENGTH_MM } else { 0.0 };
            if narrow {
                prims.push(line(cs, p + u * ARROW_LENGTH_MM));
            } else {
                prims.push(line(cs, p));
            }
            prims.push(filled(arrowhead(p, u, ARROW_LENGTH_MM)));
            (DimensionKind::Radial, cs, p, theta, cs, cs, p, Some(p), dim.geometry.measured(), narrow, tail, false)
        }
    };
    let frame_data = frame(a, b, angle_deg).ok_or_else(|| bad(CMD, "a dimension line is not finite"))?;
    let record = DrawnDimension {
        view: dim.view,
        index,
        kind,
        prims,
        def_point,
        text_midpoint: frame_data.mid,
        def_point2,
        def_point3,
        opposite,
        measurement,
        angle_deg,
        text: label,
        text_shift_mm: dim.text_shift_mm,
    };
    // A radius text stays within one text height of the arrow (D9): the box's near corner is
    // sqrt(gap² + 1²) from the tip, so the gap along the line is at most 2.5 mm. A diameter
    // value may sit outside its arrows, but never farther than 3.5 mm from the nearest one
    // (D9.1): a value on a long run-out would otherwise float 40 mm from its part.
    let outside_cap = match kind {
        DimensionKind::Radial => Some(frame_data.half + width * 0.5 + 2.5),
        // A diameter's value stays within one text height of its nearest arrow: the box's near
        // edge may pass the tip by sqrt(3.5² - 1²) = 3.35 mm at most, or the dimension is
        // dropped rather than floating its value on a long run-out.
        DimensionKind::Diametric => Some(frame_data.half + width * 0.5 + 3.3),
        DimensionKind::Rotated => None,
    };
    let pending = PendingText {
        mid: frame_data.mid,
        u: frame_data.u,
        n: frame_data.n,
        width,
        half: frame_data.half,
        tail,
        shift0: dim.text_shift_mm,
        narrow,
        outside_cap,
        // A diameter in an extension-line row (the disc's outer and inner diameters) never floats
        // its value; an in-view diameter may still need a farther spot to clear a boss.
        hard_cap: kind == DimensionKind::Diametric && has_extension,
        has_extension,
        inside_hint: kind == DimensionKind::Radial,
    };
    Ok(DimensionDraw { record, pending })
}

/// Draw one leader note into its own prims.
fn draw_note(placed: &PlacedView, scale: f64, note: &Note, view: &crate::drawing::ProjectedView) -> Result<DrawnNote> {
    let origin = placed.origin;
    let to_sheet = |p: Vec2| origin + p * scale;
    let mut prims = Vec::new();
    let label = note.text.trim_end().to_string();
    let width = text_width_mm(&label, DIM_TEXT_HEIGHT_MM);
    // The text sits on a reference line as long as itself; `at` is the text baseline's left end.
    let text_at = origin + note.at;
    let reference = (Vec2::new(text_at.x, text_at.y - DIM_TEXT_GAP_MM), Vec2::new(text_at.x + width, text_at.y - DIM_TEXT_GAP_MM));
    prims.push(line(reference.0, reference.1));
    prims.push(text(text_at, 0.0, &label, TextAnchor::Left));

    let target = note.target.unwrap_or_else(|| note.leader.last().copied().unwrap_or(Vec2::ZERO));
    let target_sheet = to_sheet(target);
    // A note lands on an edge with a closed filled arrowhead; inside a surface it gets a dot.
    let snapped = snap_to_edge(view, target, 2.0);
    let tip = snapped.map_or(target_sheet, to_sheet);
    let mut points: Vec<Vec2> = note.leader.iter().map(|p| to_sheet(*p)).collect();
    match points.last_mut() {
        // The stored target is the leader's end point: snap that point onto the edge.
        Some(last) if last.dist(target_sheet) <= 1e-6 => *last = tip,
        Some(_) => points.push(tip),
        None => points.push(tip),
    }
    let mut leader = Vec::new();
    for pair in points.windows(2) {
        if let [a, b] = pair {
            prims.push(line(*a, *b));
            leader.push((*a, *b));
        }
    }
    let terminator = if snapped.is_some() {
        let direction = points.iter().rev().nth(1).and_then(|previous| (tip - *previous).normalized()).unwrap_or(Vec2::X);
        let arrow = arrowhead(tip, -direction, ARROW_LENGTH_MM);
        prims.push(filled(arrow.clone()));
        arrow
    } else {
        let circle = dot(tip, 1.0);
        prims.push(filled(circle.clone()));
        circle
    };
    let text_prim = text(text_at, 0.0, &label, TextAnchor::Left);
    Ok(DrawnNote { view: note.view, prims, reference: Some(reference), leader, text: text_prim, terminator, text_string: label })
}

/// Draw every annotation of the sheet on top of the already built `prims` (frame, title block and
/// views). Returns the records the DXF writer needs. Dimension lines and arrows come first, then
/// the notes in full; the dimension texts are appended last, once every line of the sheet is
/// known, so the clear-space check sees the whole drawing.
pub fn draw_annotations(
    prims: &mut Vec<Prim>,
    placed: &[PlacedView],
    views: &[crate::drawing::ProjectedView],
    scale: f64,
    annotations: &Annotations,
) -> Result<DrawnAnnotations> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(bad(CMD, "the sheet scale is not a finite positive number"));
    }
    let placed_for = |view: StandardView| placed.iter().find(|p| p.view == view);
    let projected_for = |view: StandardView| views.iter().find(|p| p.view == view);

    let mut drawn = DrawnAnnotations::default();
    let mut dimensions = Vec::new();
    // Long values have the fewest free spots: place them first, so a short value moves around
    // them and not the other way round (the traceability index stays the request's).
    let mut order: Vec<usize> = (0..annotations.dimensions.len()).collect();
    order.sort_by(|a, b| {
        let width = |i: usize| annotations.dimensions.get(i).map_or(0.0, |dim| text_width_mm(&dim.label(), DIM_TEXT_HEIGHT_MM));
        width(*b).total_cmp(&width(*a)).then_with(|| a.cmp(b))
    });
    for index in order {
        let Some(dim) = annotations.dimensions.get(index) else { continue };
        let Some(place) = placed_for(dim.view) else {
            return Err(bad(CMD, format!("dimension {} is on a view that is not placed", index + 1)));
        };
        dimensions.push(draw_dimension(place, scale, dim, index)?);
    }
    for (index, note) in annotations.notes.iter().enumerate() {
        let Some(place) = placed_for(note.view) else {
            return Err(bad(CMD, format!("note {} is on a view that is not placed", index + 1)));
        };
        let Some(view) = projected_for(note.view) else {
            return Err(bad(CMD, format!("note {} is on a view that is not projected", index + 1)));
        };
        let record = draw_note(place, scale, note, view)?;
        prims.extend(record.prims.iter().cloned());
        drawn.notes.push(record);
    }

    // Every line of the sheet, including the annotation lines, is an obstacle. The dimension
    // lines count even for a dimension whose text is later dropped: a conservative obstacle set
    // cannot hide a collision.
    let mut lines = Vec::new();
    // Hatching is an obstacle first (a value should rather move outside the hatch), but a text
    // may fall back onto it: the hatching then gets a gap around the text box (ISO 128-50).
    let mut soft_lines = Vec::new();
    for prim in prims.iter() {
        if prim.layer() != Layer::Hatch {
            Seg::from_prim(prim, &mut soft_lines);
        }
        Seg::from_prim(prim, &mut lines);
    }
    for dimension in &dimensions {
        for prim in &dimension.record.prims {
            Seg::from_prim(prim, &mut lines);
            Seg::from_prim(prim, &mut soft_lines);
        }
    }
    // Text boxes count too: two texts must not sit on top of each other.
    let mut text_boxes: Vec<[Vec2; 4]> = Vec::new();
    for note in &drawn.notes {
        if let Prim::Text { at, text: value, .. } = &note.text {
            let corners = expanded_note_box(*at, text_width_mm(value, DIM_TEXT_HEIGHT_MM));
            text_boxes.push(corners);
            drawn.text_boxes.push(corners);
        }
    }
    for corners in &text_boxes {
        for i in 0..4 {
            lines.push(Seg { a: corners[i], b: corners[(i + 1) % 4] });
        }
    }

    let frame_rect = frame_rect_for(Paper::A4);
    for mut dimension in dimensions {
        let pending = &dimension.pending;
        let mut own: Vec<Seg> = Vec::new();
        for prim in &dimension.record.prims {
            if let Prim::Filled { .. } = prim {
                Seg::from_prim(prim, &mut own);
            }
        }
        let Some(shift) =
            place_text(pending, &lines, &text_boxes, frame_rect, &own).or_else(|| place_text(pending, &soft_lines, &text_boxes, frame_rect, &own))
        else {
            drawn.unplaced.push(format!("{}: {}", crate::drawing::view_name(dimension.record.view), dimension.record.text));
            continue;
        };
        let at = pending.mid + pending.u * shift + pending.n * DIM_TEXT_GAP_MM;
        let angle = pending.u.y.atan2(pending.u.x).to_degrees();
        let label = dimension.record.text.clone();
        // A value placed outside the span keeps its dimension line running under it (ISO 129-1:
        // the line is not broken, it is extended); this is also what makes the value's arrows
        // recognisable as terminators of that line.
        let text_lo = shift - pending.width * 0.5;
        let text_hi = shift + pending.width * 0.5;
        // The extension starts where the drawn line ends (past the outside arrows' tails), so
        // the two are one connected line and the checker never reads a tail as a leader. It is
        // interrupted where it would run under another value: ISO 129-1 allows the line to be
        // broken for lettering, and a value's box must stay clear of every line (DM-5).
        let line_end = pending.half + pending.tail;
        for (a, b) in [
            (pending.mid + pending.u * line_end, pending.mid + pending.u * text_hi),
            (pending.mid - pending.u * line_end, pending.mid + pending.u * text_lo),
        ] {
            let outside = (b - pending.mid).dot(pending.u).abs() > line_end + 1e-9;
            if !outside {
                continue;
            }
            for (p, q) in free_parts(a, b, &text_boxes) {
                let tail = line(p, q);
                Seg::from_prim(&tail, &mut lines);
                dimension.record.prims.push(tail.clone());
                prims.push(tail);
            }
        }
        let prim = text(at, angle, &label, TextAnchor::Center);
        prims.extend(dimension.record.prims.iter().cloned());
        prims.push(prim.clone());
        dimension.record.prims.push(prim);
        dimension.record.text_shift_mm = shift;
        dimension.record.text_midpoint = at + pending.n * (DIM_TEXT_HEIGHT_MM * 0.5);
        // A placed text's box becomes an obstacle for the texts and line extensions after it.
        let corners = expanded_text_box(at, pending.width, angle);
        for i in 0..4 {
            lines.push(Seg { a: corners[i], b: corners[(i + 1) % 4] });
            soft_lines.push(Seg { a: corners[i], b: corners[(i + 1) % 4] });
        }
        text_boxes.push(corners);
        drawn.text_boxes.push(corners);
        drawn.dimensions.push(dimension.record);
    }
    // The placement order was the value width's; the records keep the request's order.
    drawn.dimensions.sort_by_key(|d| d.index);
    Ok(drawn)
}

/// The text box of a text whose baseline centre is `at`, rotated with its reading direction
/// (`angle_deg`) and grown by [`PLACEMENT_CLEAR_MM`]. `width` is the text's advance width.
///
/// A vertical value's box is tall and narrow, not the other way round: the collision checks of
/// the texts placed after it depend on that.
fn expanded_text_box(at: Vec2, width: f64, angle_deg: f64) -> [Vec2; 4] {
    let clear = PLACEMENT_CLEAR_MM;
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    let (half, height) = (width * 0.5 + clear, DIM_TEXT_HEIGHT_MM + clear);
    [at - u * half - n * clear, at + u * half - n * clear, at + u * half + n * height, at - u * half + n * height]
}

/// The box of a horizontal, left-anchored note text at `at`, grown by [`PLACEMENT_CLEAR_MM`].
fn expanded_note_box(at: Vec2, width: f64) -> [Vec2; 4] {
    let clear = PLACEMENT_CLEAR_MM;
    [
        at - Vec2::new(clear, clear),
        at + Vec2::new(width + clear, -clear),
        at + Vec2::new(width + clear, DIM_TEXT_HEIGHT_MM + clear),
        at + Vec2::new(-clear, DIM_TEXT_HEIGHT_MM + clear),
    ]
}

/// The parts of `a`→`b` outside every placed text box: a dimension line's extension is
/// interrupted where it would run under lettering (ISO 129-1), so no line crosses a value.
pub(crate) fn free_parts(a: Vec2, b: Vec2, boxes: &[[Vec2; 4]]) -> Vec<(Vec2, Vec2)> {
    let d = b - a;
    let length = d.len();
    if !(length > 1e-9 && length.is_finite()) {
        return Vec::new();
    }
    let u = d * (1.0 / length);
    let n = u.perp();
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    for corners in boxes {
        let (mut min_t, mut max_t, mut min_n, mut max_n) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in corners {
            let t = (*p - a).dot(u);
            let s = (*p - a).dot(n);
            min_t = min_t.min(t);
            max_t = max_t.max(t);
            min_n = min_n.min(s);
            max_n = max_n.max(s);
        }
        if min_n <= 0.0 && 0.0 <= max_n {
            let (lo, hi) = (min_t.max(0.0), max_t.min(length));
            if hi > lo {
                cuts.push((lo, hi));
            }
        }
    }
    cuts.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut parts = Vec::new();
    let mut cursor = 0.0_f64;
    for (lo, hi) in cuts {
        if lo > cursor + 1e-9 {
            parts.push((a + u * cursor, a + u * lo));
        }
        cursor = cursor.max(hi);
    }
    if cursor < length - 1e-9 {
        parts.push((a + u * cursor, b));
    }
    parts.retain(|(p, q)| p.dist(*q) > 0.3);
    parts
}

/// The shift a dimension text finally takes: the user's value when it is free, else 1 mm steps
/// inside the extension lines, else outside on the side with more room. `None` when no free spot
/// exists: the caller drops the dimension rather than drawing overlapping text.
fn place_text(pending: &PendingText, lines: &[Seg], text_boxes: &[[Vec2; 4]], frame_rect: [Vec2; 2], own: &[Seg]) -> Option<f64> {
    // An outside value draws a tail from the dimension line's end to its text. Keep that
    // corridor free of other values, so the tail stays one piece from the arrow to the value
    // (the checker associates a value's terminators through that line) and no line crosses
    // the other value.
    let corridor_clear = |shift: f64| -> bool {
        let sign = if shift >= 0.0 { 1.0 } else { -1.0 };
        let from = pending.mid + pending.u * (sign * (pending.half + pending.tail));
        let to = pending.mid + pending.u * (shift - sign * pending.width * 0.5);
        if from.dist(to) <= 1e-9 {
            return true;
        }
        free_parts(from, to, text_boxes).len() == 1
    };
    let free = |shift: f64| {
        shift.is_finite()
            && shift.abs() <= MAX_VALUE_MM
            && !pending.collides(shift, lines, own)
            && pending.inside_frame(shift, frame_rect)
            && (shift.abs() <= pending.inside_limit() + 1e-9 || corridor_clear(shift))
    };
    if pending.shift0.abs() > 1e-9 && free(pending.shift0) {
        return Some(pending.shift0);
    }
    if !pending.narrow {
        let inside = pending.inside_limit();
        let start = if pending.inside_hint && pending.shift0.abs() < 1e-9 { inside.floor() } else { pending.shift0.round() };
        let steps = inside.ceil().max(1.0) as i64;
        for step in 0..=steps {
            for sign in [1.0_f64, -1.0] {
                if step == 0 && sign < 0.0 {
                    continue;
                }
                let shift = start + sign * step as f64;
                if shift.abs() <= inside + 1e-9 && free(shift) {
                    return Some(shift);
                }
            }
        }
    }
    // Outside: the closest free spot on either side of the dimension line wins, so the text
    // stays beside its feature and lands on the side with more usable room. Ties prefer +u. A
    // radius keeps its text within its cap first and only then takes a farther spot.
    let base = pending.outside();
    let outside = |cap: f64| -> Option<f64> {
        let first_free = |sign: f64| -> Option<f64> {
            (0..=1000).map(|step| sign * (base + step as f64)).find(|shift| shift.abs() <= cap.max(base) && free(*shift))
        };
        let (positive, negative) = (first_free(1.0), first_free(-1.0));
        match (positive, negative) {
            (Some(p), Some(n)) => Some(if p.abs() <= n.abs() { p } else { n }),
            (Some(p), None) => Some(p),
            (None, Some(n)) => Some(n),
            (None, None) => None,
        }
    };
    if let Some(cap) = pending.outside_cap
        && let Some(shift) = outside(cap)
    {
        return Some(shift);
    }
    if pending.hard_cap {
        return None;
    }
    outside(f64::INFINITY)
}

/// The DXF text override for a drawn dimension, when its text is more than the plain measurement
/// (`Ø`, `R`, a suffix or an override).
pub fn dxf_text_override(dim: &Dimension) -> Option<String> {
    (dim.text.is_some() || !dim.geometry.prefix().is_empty() || dim.suffix.as_deref().is_some_and(|s| !s.is_empty())).then(|| dim.label())
}

#[cfg(test)]
#[path = "dims_tests.rs"]
mod tests;
