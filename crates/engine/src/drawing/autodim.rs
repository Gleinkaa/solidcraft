//! Automatic dimensions and size notes, read off the B-rep's exact surfaces.
//!
//! [`plan`] walks the analytic faces of the visible bodies (the same [`FaceSurface`] records the
//! centre-line code reads) and decides what a drawing of the part needs:
//!
//! - a turned part gets one `Ø` per external cylinder through its longitudinal view, its
//!   shoulder-to-shoulder lengths chained on one row from the datum end (the last segment left
//!   out) and the overall length on the next row, plus leader notes for bores, cross holes and
//!   chamfers;
//! - a flat part gets the plan view's length and width, the edge view's thickness, one grouped
//!   diameter per set of equal holes, hole positions from the datum edges (or from the largest
//!   hole's centre when the outline is mostly arcs) and one radius per distinct outer arc or
//!   fillet radius;
//! - a general part spreads its overall L, W and H over the views so each value appears once.
//!
//! Every value comes from an exact surface parameter or a plane position in model millimetres,
//! never from the tessellation. Material sides come from the face normals. A dimension that
//! cannot be placed without crossing a line is dropped by [`crate::drawing::dims`] and listed in
//! the command result, never drawn overlapping.
//!
//! D7.1 tightens what a sheet must look like: extension lines start on the outline (an extreme
//! vertex or an arc's tangent point), a radius arrow only points into an arc that exists, a
//! through hole is not described by a bare `durchgehend` note, note leaders avoid every other
//! annotation (and an outline except within their last 5 mm), and a completeness pass adds
//! recesses/counterbores and reports whatever still has no dimension in `undimensioned`.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use solidcraft_geom::{Aabb3, UnitVec3, Vec2, Vec3};

use solidcraft_io::text_width_mm;
use solidcraft_kernel::FaceSurface;
use solidcraft_render::StandardView;

use crate::Session;
use crate::drawing::centre;
use crate::drawing::centre::SurfaceFace;
use crate::drawing::centre::ViewCentre;
use crate::drawing::dims::{
    Annotations, DIM_TEXT_GAP_MM, DIM_TEXT_HEIGHT_MM, DimSide, Dimension, DimensionGeometry, EXT_OVERSHOOT_MM, MAX_VALUE_MM, Note,
    PLACEMENT_CLEAR_MM, segment_hits_rect, snap_to_edge, stack_rows,
};
use crate::drawing::sheet::{CENTRE_OVERSHOOT_MM, Paper, Prim, Sheet, TextAnchor, frame_rect_for};
use crate::drawing::{ProjectedView, ViewChoice, ViewSelection, standard_camera, view_name};

/// Faces lie on the part's axis when their lines are closer than this fraction of the part size.
const AXIS_DISTANCE_FRACTION: f64 = 1e-4;
/// Parallel test for axes and normals (radians).
const PARALLEL_TOL: f64 = 1e-6;
/// A full turn, minus the tessellation's last bits.
const FULL_TURN_TOL: f64 = 1e-2;
/// A bore's depth or a feature length may differ from its exact plane by this (mm).
const LENGTH_TOL_MM: f64 = 0.05;
/// One degree in radians.
const ONE_DEG: f64 = PI / 180.0;
/// Holes below this diameter at sheet scale get a leader note instead of an in-view diameter.
pub const SMALL_HOLE_MM: f64 = 8.0;
/// The largest chamfer axial length that gets a `Fase` note (mm).
pub const MAX_CHAMFER_MM: f64 = 5.0;
/// A cross hole counts as through when its wall spans this share of the local material.
const THROUGH_SHARE: f64 = 0.9;
/// A hole whose axis leans at most this far from a view direction is drawn and noted in that
/// view; it gets no auxiliary view (D9.1).
const MAX_HOLE_SKEW_DEG: f64 = 5.0;
/// A hole whose axis misses the view direction by more than this is reported as `skew_axis`.
const HOLE_SKEW_WARN_DEG: f64 = 0.5;
/// A hole's cylinder may be broken out by another face by this much of a turn and still count as
/// a hole (the test part's Ø5 bore breaks the bottom face by about 50°).
const HOLE_TURN_TOL: f64 = 1.0;
/// How far outside the two arc centres a centre-distance dimension is drawn (sheet mm).
pub const CENTRE_DISTANCE_OFFSET_MM: f64 = 6.0;

/// Where one automatic dimension came from, for the checker's traceability.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoDimensionSource {
    pub view: StandardView,
    /// The B-rep face the value was read from.
    pub face: usize,
    /// `diameter`, `length`, `radius`, `hole`, `position`, `overall` or `fillet`.
    pub kind: &'static str,
    /// The model value in mm (a diameter for `diameter`/`hole`, a radius for `radius`/`fillet`).
    pub value: f64,
    /// The text the sheet carries.
    pub text: String,
}

/// One in-view diameter's sliding station along its external step, kept so the final layout can
/// move the line away from other features (D7.1 defect 5).
#[derive(Clone, Copy, Debug)]
struct Station {
    /// The projected axis origin in view millimetres.
    origin: Vec2,
    /// The unit direction of the part axis in the view.
    along: Vec2,
    /// The unit direction across the part axis in the view.
    perp: Vec2,
    /// The step's extent along `along`, relative to `origin` (model millimetres).
    z0: f64,
    z1: f64,
    radius: f64,
}

/// One dimension the automatic pass wants, before the scale-dependent stacking.
#[derive(Clone, Debug)]
struct PlannedDim {
    dimension: Dimension,
    side: Option<DimSide>,
    /// A row this dimension must keep (a chain group); `None` sorts by length with its peers.
    row: Option<usize>,
    source: AutoDimensionSource,
    /// The external step an in-view diameter is drawn across; its station may still move.
    station: Option<Station>,
}

/// One leader note the automatic pass wants, before its collision-free text position is found.
#[derive(Clone, Debug)]
struct PlannedNote {
    view: StandardView,
    text: String,
    /// Candidate feature points in view millimetres; a hole offers its four cardinal rim points
    /// so a leader can avoid an earlier leader's tip.
    targets: Vec<Vec2>,
    /// The B-rep face the note refers to, for the completeness pass.
    face: Option<usize>,
}

/// A hole axis that is not parallel to a principal axis, reported so the UI can say the hole is
/// skewed (usually a modelling slip); the hole still gets its size note.
#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    /// `skew_axis` today.
    pub kind: &'static str,
    /// The B-rep face of the hole's cylinder.
    pub face: usize,
    /// The angle between the hole axis and the view direction it is drawn in (degrees).
    pub angle_deg: f64,
}

/// A feature the automatic pass found but could not dimension (the completeness pass).
#[derive(Clone, Debug, PartialEq)]
pub struct Undimensioned {
    /// The B-rep face index of the feature's surface.
    pub face: usize,
    /// `diameter` for a round face, `plane` for a bounding plane.
    pub kind: &'static str,
    /// The feature's size: a diameter in mm, or a plane's position along the part axis.
    pub value: f64,
}

/// The automatic dimensions and notes for one part, before the sheet's scale is known.
#[derive(Clone, Debug, Default)]
pub struct AutoPlan {
    dims: Vec<PlannedDim>,
    notes: Vec<PlannedNote>,
    undimensioned: Vec<Undimensioned>,
    warnings: Vec<Warning>,
}

impl AutoPlan {
    /// The traceability list: one record per automatic dimension.
    pub fn sources(&self) -> Vec<AutoDimensionSource> {
        self.dims.iter().map(|d| d.source.clone()).collect()
    }

    /// The features the pass found but could not reference (empty when the part is complete).
    pub fn undimensioned(&self) -> Vec<Undimensioned> {
        self.undimensioned.clone()
    }

    /// The conditions worth telling the user about (`skew_axis`).
    pub fn warnings(&self) -> Vec<Warning> {
        self.warnings.clone()
    }

    /// The annotations to lay out for the scale search: every dimension is present with an
    /// offset that names its row and side (so the band calculation sees the real row count),
    /// but the exact sheet-scale offsets are not needed yet.
    pub fn provisional_annotations(&self) -> Annotations {
        let rows = assigned_rows(&self.dims);
        let mut annotations = Annotations::default();
        for (index, planned) in self.dims.iter().enumerate() {
            let mut dim = planned.dimension.clone();
            if let Some(side) = planned.side {
                let row = rows.get(index).copied().unwrap_or(0);
                let magnitude = 1.0 + row as f64;
                dim.geometry = offset_geometry(dim.geometry, side, magnitude, false);
            }
            annotations.dimensions.push(dim);
        }
        annotations
    }

    /// The automatic dimensions with their final sheet-mm offsets for `scale`.
    pub fn annotations_at(&self, views: &[ProjectedView], scale: f64) -> Annotations {
        let rows = assigned_rows(&self.dims);
        let mut out: Vec<Option<Dimension>> = vec![None; self.dims.len()];
        // One stack per (view, side): rows in order, the dimensions of a row end to end.
        let mut groups: BTreeMap<(usize, u8), BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
        for (index, planned) in self.dims.iter().enumerate() {
            let Some(side) = planned.side else {
                out[index] = Some(planned.dimension.clone());
                continue;
            };
            let Some(view_index) = views.iter().position(|v| v.view == planned.dimension.view) else {
                out[index] = Some(planned.dimension.clone());
                continue;
            };
            let row = rows.get(index).copied().unwrap_or(0);
            groups.entry((view_index, side_index(side))).or_default().entry(row).or_default().push(index);
        }
        for ((view_index, side), row_map) in groups {
            let Some(view) = views.get(view_index) else { continue };
            let side = side_from_index(side);
            let order: Vec<Vec<usize>> = row_map.values().cloned().collect();
            let input: Vec<Vec<Dimension>> =
                order.iter().map(|row| row.iter().filter_map(|i| self.dims.get(*i).map(|d| d.dimension.clone())).collect()).collect();
            let placed = stack_rows(view, side, &input, scale);
            for (row, dims) in order.iter().zip(placed) {
                for (index, dim) in row.iter().zip(dims) {
                    if let Some(slot) = out.get_mut(*index) {
                        *slot = Some(dim);
                    }
                }
            }
        }
        adjust_stations(&self.dims, views, scale, &mut out);
        let mut annotations = Annotations::default();
        for (index, dim) in out.into_iter().enumerate() {
            if let Some(dim) = dim {
                annotations.dimensions.push(dim);
            } else if let Some(planned) = self.dims.get(index) {
                annotations.dimensions.push(planned.dimension.clone());
            }
        }
        annotations
    }

    /// Find collision-free positions for the notes against the already built (dimensioned)
    /// sheet. Returns the notes and the texts that could not be placed.
    pub fn notes_at(&self, sheet: &Sheet, scale: f64, ignore_last_notes: usize, views: &[ProjectedView]) -> (Vec<Note>, Vec<String>) {
        // The previous round's automatic notes are not obstacles to their own replacement; the
        // explicit notes and every dimension line and text are.
        let first_ignored = sheet.notes.len().saturating_sub(ignore_last_notes);
        let mut ignored: Vec<Prim> = Vec::new();
        for note in sheet.notes.iter().skip(first_ignored) {
            ignored.extend(note.prims.iter().cloned());
        }
        let mut all_lines: Vec<(Vec2, Vec2)> = Vec::new();
        // The same lines without the hatching and the freehand break-out boundary: a note text
        // that finds no free spot outside the hatch falls back onto it, and the sheet interrupts
        // the hatching around the text (ISO 128-50).
        let mut soft_lines: Vec<(Vec2, Vec2)> = Vec::new();
        let mut annotation_lines: Vec<(Vec2, Vec2)> = Vec::new();
        let mut outline_lines: Vec<(Vec2, Vec2)> = Vec::new();
        let mut boxes: Vec<[Vec2; 2]> = Vec::new();
        for prim in &sheet.prims {
            if ignored.contains(prim) {
                continue;
            }
            let layer = prim.layer();
            let outline = matches!(layer, crate::drawing::sheet::Layer::Visible | crate::drawing::sheet::Layer::Hidden);
            // A centre line is no annotation obstacle for a leader (rule 4 names dimension lines,
            // extension lines, leaders and text boxes); neither is hatching or a freehand
            // boundary, which a leader may cross. All of them are still text-box obstacles.
            let annotation =
                !matches!(layer, crate::drawing::sheet::Layer::Center | crate::drawing::sheet::Layer::Hatch | crate::drawing::sheet::Layer::Freehand);
            let soft = !matches!(layer, crate::drawing::sheet::Layer::Hatch | crate::drawing::sheet::Layer::Freehand);
            match prim {
                Prim::Line { a, b, .. } => {
                    all_lines.push((*a, *b));
                    if soft {
                        soft_lines.push((*a, *b));
                    }
                    if outline {
                        outline_lines.push((*a, *b));
                    } else if annotation {
                        annotation_lines.push((*a, *b));
                    }
                }
                Prim::Polyline { points, closed, .. } => {
                    let mut add = |a: Vec2, b: Vec2| {
                        all_lines.push((a, b));
                        if soft {
                            soft_lines.push((a, b));
                        }
                        if outline {
                            outline_lines.push((a, b));
                        } else if annotation {
                            annotation_lines.push((a, b));
                        }
                    };
                    for pair in points.windows(2) {
                        if let [a, b] = pair {
                            add(*a, *b);
                        }
                    }
                    if *closed
                        && points.len() > 2
                        && let (Some(a), Some(b)) = (points.last(), points.first())
                    {
                        add(*a, *b);
                    }
                }
                Prim::Arc { c, r, a0, a1, .. } => {
                    let mut add = |a: Vec2, b: Vec2| {
                        all_lines.push((a, b));
                        if soft {
                            soft_lines.push((a, b));
                        }
                        if outline {
                            outline_lines.push((a, b));
                        } else if annotation {
                            annotation_lines.push((a, b));
                        }
                    };
                    let steps = (((a1 - a0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
                    let point = |i: usize| *c + Vec2::from_angle(a0 + (a1 - a0) * i as f64 / steps as f64) * *r;
                    for i in 0..steps {
                        add(point(i), point(i + 1));
                    }
                }
                Prim::Circle { c, r, .. } => {
                    let mut add = |a: Vec2, b: Vec2| {
                        all_lines.push((a, b));
                        if soft {
                            soft_lines.push((a, b));
                        }
                        if outline {
                            outline_lines.push((a, b));
                        } else if annotation {
                            annotation_lines.push((a, b));
                        }
                    };
                    let point = |i: usize| *c + Vec2::from_angle(std::f64::consts::TAU * i as f64 / 32.0) * *r;
                    for i in 0..32 {
                        add(point(i), point(i + 1));
                    }
                }
                Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
                    let mut add = |a: Vec2, b: Vec2| {
                        all_lines.push((a, b));
                        if soft {
                            soft_lines.push((a, b));
                        }
                        if outline {
                            outline_lines.push((a, b));
                        } else if annotation {
                            annotation_lines.push((a, b));
                        }
                    };
                    let minor = major.perp() * *ratio;
                    let steps = (((t1 - t0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
                    let point = |i: usize| {
                        let t = t0 + (t1 - t0) * i as f64 / steps as f64;
                        *c + *major * t.cos() + minor * t.sin()
                    };
                    for i in 0..steps {
                        add(point(i), point(i + 1));
                    }
                }
                Prim::Filled { points, .. } => {
                    let mut add = |a: Vec2, b: Vec2| {
                        all_lines.push((a, b));
                        if soft {
                            soft_lines.push((a, b));
                        }
                        if outline {
                            outline_lines.push((a, b));
                        } else if annotation {
                            annotation_lines.push((a, b));
                        }
                    };
                    for pair in points.windows(2) {
                        if let [a, b] = pair {
                            add(*a, *b);
                        }
                    }
                }
                Prim::Text { at, height, text, anchor, .. } => {
                    let width = text_width_mm(text, *height);
                    let (left, right) = match anchor {
                        TextAnchor::Left => (at.x, at.x + width),
                        TextAnchor::Center => (at.x - width * 0.5, at.x + width * 0.5),
                        TextAnchor::Right => (at.x - width, at.x),
                    };
                    boxes.push([Vec2::new(left, at.y), Vec2::new(right, at.y + *height)]);
                }
            }
        }
        let frame = frame_rect_for(Paper::A4);
        let mut notes = Vec::new();
        let mut unplaced = Vec::new();
        for planned in &self.notes {
            let Some(placed) = sheet.views.iter().find(|v| v.view == planned.view) else {
                unplaced.push(format!("{}: {}", view_name(planned.view), planned.text));
                continue;
            };
            let Some(projected) = views.iter().find(|v| v.view == planned.view) else {
                unplaced.push(format!("{}: {}", view_name(planned.view), planned.text));
                continue;
            };
            let width = text_width_mm(&planned.text, DIM_TEXT_HEIGHT_MM);
            let mut chosen: Option<(f64, Vec2, Vec2, Vec2, Vec2)> = None; // length, at, start, target sheet, tip view
            for target_view in &planned.targets {
                // The tip the sheet will draw: on the nearest edge, as the note's arrowhead. It
                // must not land on an annotation line (DM-12).
                let tip_view = snap_to_edge(projected, *target_view, 2.0).unwrap_or(*target_view);
                let target = placed.origin + tip_view * scale;
                // A leader tip must not land on an annotation line (DM-12).
                let tip_is_free = annotation_lines.iter().all(|(a, b)| {
                    let d = *b - *a;
                    let len2 = d.dot(d);
                    let dist = if len2 > 1e-18 {
                        let t = ((target - *a).dot(d) / len2).clamp(0.0, 1.0);
                        target.dist(*a + d * t)
                    } else {
                        target.dist(*a)
                    };
                    dist > 0.6
                });
                if !tip_is_free {
                    continue;
                }
                let mut candidates = note_candidates(target, width, frame, placed.bbox, &all_lines, &annotation_lines, &outline_lines, &boxes);
                if candidates.is_empty() {
                    candidates = note_candidates(target, width, frame, placed.bbox, &soft_lines, &annotation_lines, &outline_lines, &boxes);
                }
                for (leader_len, at, start) in candidates {
                    if chosen.as_ref().is_none_or(|(best, ..)| leader_len < *best - 1e-9) {
                        chosen = Some((leader_len, at, start, target, tip_view));
                    }
                }
            }
            let Some((_, at, start, target, tip_view)) = chosen else {
                unplaced.push(format!("{}: {}", view_name(planned.view), planned.text));
                continue;
            };
            let (start_x, end_x) = (at.x, at.x + width);
            let reference_y = at.y - DIM_TEXT_GAP_MM;
            let to_view = |p: Vec2| (p - placed.origin) * (1.0 / scale);
            notes.push(Note {
                view: planned.view,
                text: planned.text.clone(),
                at: at - placed.origin,
                leader: vec![to_view(start), tip_view],
                target: Some(tip_view),
            });
            let grown = [
                Vec2::new(start_x - PLACEMENT_CLEAR_MM, at.y - PLACEMENT_CLEAR_MM),
                Vec2::new(end_x + PLACEMENT_CLEAR_MM, at.y + DIM_TEXT_HEIGHT_MM + PLACEMENT_CLEAR_MM),
            ];
            boxes.push(grown);
            // The note's own reference line and leader are obstacles for the notes after it.
            let reference = (Vec2::new(start_x, reference_y), Vec2::new(end_x, reference_y));
            all_lines.push(reference);
            annotation_lines.push(reference);
            all_lines.push((start, target));
            annotation_lines.push((start, target));
        }
        (notes, unplaced)
    }
}

/// Whether the view draws a full circle of `radius` around `centre` as a visible edge: the test
/// for putting a disc's inner diameter on the end view (D9.1). A recess on the far side is hidden
/// there, and its diameter stays in the section.
fn circle_drawn(view: &ProjectedView, centre: Vec2, radius: f64) -> bool {
    if !(radius.is_finite() && radius > 0.0) {
        return false;
    }
    const SAMPLES: usize = 32;
    let mut on_edge = 0usize;
    for i in 0..SAMPLES {
        let t = 2.0 * PI * i as f64 / SAMPLES as f64;
        let p = centre + Vec2::new(radius * t.cos(), radius * t.sin());
        let hit = view.visible.iter().any(|seg| {
            let d = seg.b - seg.a;
            let len2 = d.dot(d);
            if !(len2 > 1e-18) {
                return p.dist(seg.a) <= 0.05;
            }
            let u = ((p - seg.a).dot(d) / len2).clamp(0.0, 1.0);
            p.dist(seg.a + d * u) <= 0.05
        });
        if hit {
            on_edge += 1;
        }
    }
    on_edge * 10 >= SAMPLES * 9
}

/// The row number of every planned dimension: fixed rows first, then the auto rows sorted by
/// measured length on their own view and side.
fn assigned_rows(dims: &[PlannedDim]) -> Vec<usize> {
    let mut rows = vec![0usize; dims.len()];
    let mut groups: Vec<(StandardView, u8, Vec<usize>)> = Vec::new();
    for (index, planned) in dims.iter().enumerate() {
        let Some(side) = planned.side else { continue };
        let key = (planned.dimension.view, side_index(side));
        match groups.iter_mut().find(|(view, s, _)| (*view, *s) == key) {
            Some((_, _, indices)) => indices.push(index),
            None => groups.push((key.0, key.1, vec![index])),
        }
    }
    for (_, _, indices) in &groups {
        let mut fixed_max: Option<usize> = None;
        for index in indices {
            if let Some(row) = dims.get(*index).and_then(|d| d.row) {
                fixed_max = Some(fixed_max.map_or(row, |m: usize| m.max(row)));
            }
        }
        let base = fixed_max.map_or(0, |m| m + 1);
        let mut automatic: Vec<usize> = indices.iter().copied().filter(|i| dims.get(*i).is_some_and(|d| d.row.is_none())).collect();
        automatic.sort_by(|a, b| {
            let va = dims.get(*a).map_or(0.0, |d| d.dimension.geometry.measured());
            let vb = dims.get(*b).map_or(0.0, |d| d.dimension.geometry.measured());
            va.total_cmp(&vb).then_with(|| a.cmp(b))
        });
        for (position, index) in automatic.into_iter().enumerate() {
            if let Some(slot) = rows.get_mut(index) {
                *slot = base + position;
            }
        }
        for index in indices {
            if let Some(row) = dims.get(*index).and_then(|d| d.row)
                && let Some(slot) = rows.get_mut(*index)
            {
                *slot = row;
            }
        }
    }
    rows
}

/// Move every in-view diameter to a station along its step that keeps 3 sheet mm clear of the
/// other features' centre lines, hidden lines and extension lines (D7.1 defect 5). The middle is
/// tried first, then the quarter points.
fn adjust_stations(dims: &[PlannedDim], views: &[ProjectedView], scale: f64, out: &mut [Option<Dimension>]) {
    if !(scale.is_finite() && scale > 0.0) {
        return;
    }
    for (index, planned) in dims.iter().enumerate() {
        let Some(station) = planned.station else { continue };
        let Some(view_index) = views.iter().position(|v| v.view == planned.dimension.view) else { continue };
        let Some(view) = views.get(view_index) else { continue };
        let mut obstacles: Vec<(Vec2, Vec2)> = Vec::new();
        for item in &view.centre {
            match *item {
                ViewCentre::Line { a, b } => obstacles.push((a * scale, b * scale)),
                ViewCentre::Cross { c, radius } => {
                    let centre = c * scale;
                    let arm = radius * scale + CENTRE_OVERSHOOT_MM;
                    obstacles.push((Vec2::new(centre.x - arm, centre.y), Vec2::new(centre.x + arm, centre.y)));
                    obstacles.push((Vec2::new(centre.x, centre.y - arm), Vec2::new(centre.x, centre.y + arm)));
                }
            }
        }
        for seg in &view.hidden {
            obstacles.push((seg.a * scale, seg.b * scale));
        }
        // A visible edge the line would run along is no station either (D9.1: a dimension line
        // may cross an edge but not lie on one). Perpendicular crossings are harmless.
        for seg in &view.visible {
            obstacles.push((seg.a * scale, seg.b * scale));
        }
        for (other_index, other) in out.iter().enumerate() {
            if other_index == index {
                continue;
            }
            let Some(dim) = other else { continue };
            if dim.view != planned.dimension.view {
                continue;
            }
            let (p1, p2, angle_deg, offset_mm) = match dim.geometry {
                DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm } | DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm } => {
                    (p1, p2, angle_deg, offset_mm)
                }
                _ => continue,
            };
            let u = Vec2::from_angle(angle_deg.to_radians());
            let n = u.perp();
            let (p1s, p2s) = (p1 * scale, p2 * scale);
            if offset_mm.abs() > 1e-9 {
                for p in [p1s, p2s] {
                    let foot = p + n * offset_mm;
                    obstacles.push((p, foot + n * offset_mm.signum() * EXT_OVERSHOOT_MM));
                }
            }
            let a = p1s + n * offset_mm;
            let b = p2s + n * (offset_mm - (p2 - p1).dot(n) * scale);
            obstacles.push((a, b));
        }
        let fractions = [0.5, 0.25, 0.75, 0.125, 0.375, 0.625, 0.875];
        let mut chosen: Option<(Vec2, Vec2)> = None;
        for fraction in fractions {
            let t = station.z0 + (station.z1 - station.z0) * fraction;
            let centre = station.origin + station.along * t;
            let p1 = centre + station.perp * station.radius;
            let p2 = centre - station.perp * station.radius;
            let (a, b) = (p1 * scale, p2 * scale);
            if obstacles.iter().all(|(c, d)| parallel_line_distance(a, b, *c, *d).is_none_or(|distance| distance >= 3.0 - 1e-9)) {
                chosen = Some((p1, p2));
                break;
            }
        }
        let Some((p1, p2)) = chosen else { continue };
        if let Some(Some(dim)) = out.get_mut(index) {
            dim.geometry = match dim.geometry {
                DimensionGeometry::Diameter { angle_deg, offset_mm, .. } => DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm },
                other => other,
            };
        }
    }
}

fn side_index(side: DimSide) -> u8 {
    match side {
        DimSide::Above => 0,
        DimSide::Below => 1,
        DimSide::Left => 2,
        DimSide::Right => 3,
    }
}

fn side_from_index(index: u8) -> DimSide {
    match index {
        1 => DimSide::Below,
        2 => DimSide::Left,
        3 => DimSide::Right,
        _ => DimSide::Above,
    }
}

/// A linear/diameter dimension moved `distance` mm to `side`; with `from_outline` the distance
/// is already the final offset, otherwise it is a provisional one.
fn offset_geometry(geometry: DimensionGeometry, side: DimSide, distance: f64, from_outline: bool) -> DimensionGeometry {
    let _ = from_outline;
    match geometry {
        DimensionGeometry::Linear { p1, p2, .. } => {
            let (angle_deg, offset_mm) = match side {
                DimSide::Above => (0.0, distance),
                DimSide::Below => (0.0, -distance),
                DimSide::Left => (90.0, distance),
                DimSide::Right => (90.0, -distance),
            };
            DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm }
        }
        DimensionGeometry::Diameter { p1, p2, .. } => {
            let (angle_deg, offset_mm) = match side {
                DimSide::Above => (0.0, distance),
                DimSide::Below => (0.0, -distance),
                DimSide::Left => (90.0, distance),
                DimSide::Right => (90.0, -distance),
            };
            DimensionGeometry::Diameter { p1, p2, angle_deg, offset_mm }
        }
        other => other,
    }
}

/// Every collision-free note placement around `target`, in sheet millimetres: the text position,
/// the leader's start on the reference line and the leader length. A placement is only offered
/// when its text box keeps clear of every line and text, and its leader crosses no annotation and
/// leaves the visible outline only within the last 5 mm before the tip (D7.1 defect 4).
fn note_candidates(
    target: Vec2,
    width: f64,
    frame: [Vec2; 2],
    view_bbox: [Vec2; 2],
    all_lines: &[(Vec2, Vec2)],
    annotation_lines: &[(Vec2, Vec2)],
    outline_lines: &[(Vec2, Vec2)],
    boxes: &[[Vec2; 2]],
) -> Vec<(f64, Vec2, Vec2)> {
    let [f0, f1] = frame;
    let height = DIM_TEXT_HEIGHT_MM;
    let clear = PLACEMENT_CLEAR_MM;
    let directions = [
        Vec2::new(0.0, 1.0),
        Vec2::new(0.0, -1.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(-1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(-1.0, 1.0),
        Vec2::new(1.0, -1.0),
        Vec2::new(-1.0, -1.0),
    ];
    let mut out = Vec::new();
    let lateral = [0.0, 8.0, -8.0, 16.0, -16.0, 24.0, -24.0, 32.0, -32.0, 40.0, -40.0];
    for dir in directions {
        for step in 0..=33 {
            let d = 6.0 + 2.0 * step as f64;
            let mut positions = Vec::new();
            if dir.y.abs() >= dir.x.abs() {
                for offset in lateral {
                    positions.push((target.x - width * 0.5 + offset, if dir.y > 0.0 { target.y + d } else { target.y - d - height }));
                }
            } else {
                for offset in lateral {
                    positions.push((if dir.x > 0.0 { target.x + d + 2.0 } else { target.x - d - 2.0 - width }, target.y - height * 0.5 + offset));
                }
            }
            for (at_x, at_y) in positions {
                let at = Vec2::new(at_x, at_y);
                if !(at.x.is_finite() && at.y.is_finite()) {
                    continue;
                }
                let lo = Vec2::new(at.x - clear, at.y - clear);
                let hi = Vec2::new(at.x + width + clear, at.y + height + clear);
                // Notes may use the full frame, including the top band: the checker's note-height
                // rule only filters texts there, and a view placed high leaves no other free room.
                if lo.x < f0.x || lo.y < f0.y || hi.x > f1.x || hi.y > f1.y - 2.0 {
                    continue;
                }
                if all_lines.iter().any(|(a, b)| segment_hits_rect(*a, *b, lo, hi)) {
                    continue;
                }
                if boxes.iter().any(|[b0, b1]| lo.x <= b1.x && hi.x >= b0.x && lo.y <= b1.y && hi.y >= b0.y) {
                    continue;
                }
                let reference_y = at.y - DIM_TEXT_GAP_MM;
                let starts = [Vec2::new(at.x - 2.0, reference_y), Vec2::new(at.x + width + 2.0, reference_y)];
                for start in starts {
                    if !start.is_finite() || !target.is_finite() {
                        continue;
                    }
                    if !segment_clear_of(start, target, annotation_lines) {
                        continue;
                    }
                    if !leader_hugs_outline(start, target, outline_lines, view_bbox) {
                        continue;
                    }
                    if boxes.iter().any(|[b0, b1]| segment_hits_rect(start, target, *b0, *b1)) {
                        continue;
                    }
                    // The leader must not run through its own lettering either.
                    if segment_hits_rect(start, target, Vec2::new(at.x, at.y), Vec2::new(at.x + width, at.y + height)) {
                        continue;
                    }
                    out.push((start.dist(target), at, start));
                }
            }
        }
    }
    out
}

/// The raw size (Rohmaß) the title block states unless the designer overrides it, from the
/// exact B-rep bounds and the D7 classification: a turned part `Ø<largest outer diameter> x
/// <overall length>`, any other part `<L> x <W> x <H>` sorted largest first. The values are
/// rounded up to 0.1 mm and written with a decimal comma; `None` when nothing is visible.
pub fn rohmass(s: &Session) -> Option<String> {
    let faces = centre::surface_faces(s);
    if faces.is_empty() {
        return None;
    }
    let bounds = exact_bounds(&faces);
    if bounds.is_empty() {
        return None;
    }
    if let Some(axis) = centre::turned_axis(s) {
        let a = UnitVec3::new(centre::world_axis(axis.axis)).unwrap_or(axis.axis);
        let tolerance = AXIS_DISTANCE_FRACTION * s.world_state().bodies.iter().map(|b| b.outline_bounds().diagonal()).fold(1.0, f64::max);
        let mut max_radius = 0.0_f64;
        for face in &faces {
            let FaceSurface::Cylinder { origin, axis: face_axis, radius } = face.surface else { continue };
            if !parallel(face_axis, a) || line_distance(axis.origin, a, origin) > tolerance {
                continue;
            }
            if face.coverage() < 2.0 * PI - FULL_TURN_TOL || face.radial_side(origin, face_axis) <= 0.0 {
                continue;
            }
            max_radius = max_radius.max(radius);
        }
        let (lo, hi) = projected_span(&bounds, a.vec());
        let length = (hi - lo).abs();
        if max_radius > 0.0 && length > 0.0 {
            return Some(format!("\u{00d8}{} x {}", size_text(2.0 * max_radius), size_text(length)));
        }
    }
    let mut extents = [bounds.max.x - bounds.min.x, bounds.max.y - bounds.min.y, bounds.max.z - bounds.min.z];
    extents.sort_by(|a, b| b.total_cmp(a));
    if !extents.iter().all(|e| e.is_finite() && *e > 0.0) {
        return None;
    }
    Some(format!("{} x {} x {}", size_text(extents[0]), size_text(extents[1]), size_text(extents[2])))
}

/// A hole depth as the sheet states it: to 0.1 mm, and a measured depth within 0.15 mm of a
/// whole millimetre reads as that number. A hole drilled at an angle meets the entry face in an
/// ellipse, so its wall measures a few hundredths longer than the depth that was drilled (the
/// test plate's Ø5 x 10 reads 10.13 off the wall).
fn depth_text(value: f64) -> String {
    if !value.is_finite() {
        return crate::drawing::format_value(0.0);
    }
    let rounded = (value * 10.0).round() / 10.0;
    let whole = rounded.round();
    if (rounded - whole).abs() <= 0.15 { crate::drawing::format_value(whole) } else { crate::drawing::format_value(rounded) }
}

/// A raw-size value in mm, rounded **up** to 0.1 mm and written with a decimal comma. The tiny
/// epsilon keeps an exact 14.0 from becoming 14.1 because of an analytic radius' last bits.
fn size_text(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "0".to_string();
    }
    let rounded = ((value * 10.0 - 1e-6).ceil() / 10.0).max(0.0);
    if rounded.abs() <= 1e9 && (rounded - rounded.round()).abs() < 1e-9 {
        return format!("{}", rounded.round() as i64);
    }
    format!("{rounded:.1}").replace('.', ",")
}

/// Plan the automatic dimensions and notes for the chosen views.
pub fn plan(s: &Session, selection: &ViewSelection, views: &[ProjectedView]) -> AutoPlan {
    let faces = centre::surface_faces(s);
    if faces.is_empty() {
        return AutoPlan::default();
    }
    match selection.choice {
        ViewChoice::Turned => plan_turned(s, selection, views, &faces),
        ViewChoice::Flat => plan_flat(s, selection, views, &faces),
        ViewChoice::General | ViewChoice::Explicit => plan_general(s, selection, views, &faces),
    }
}

// ------------------------------------------------------------------ surface helpers

fn parallel(a: UnitVec3, b: UnitVec3) -> bool {
    a.cross(b).len() <= PARALLEL_TOL
}

fn line_distance(p1: Vec3, d1: UnitVec3, p2: Vec3) -> f64 {
    let w = p2 - p1;
    (w - d1 * w.dot(d1)).len()
}

/// The distance between two axis lines, parallel or crossing.
fn axis_line_distance(p1: Vec3, d1: UnitVec3, p2: Vec3, d2: UnitVec3) -> f64 {
    let n = d1.cross(d2);
    if n.len() > PARALLEL_TOL { ((p2 - p1).dot(n)).abs() / n.len() } else { line_distance(p1, d1, p2) }
}

/// The line of a round face: its origin and axis.
fn face_line(face: &SurfaceFace) -> Option<(Vec3, UnitVec3)> {
    match face.surface {
        FaceSurface::Cylinder { origin, axis, .. } => Some((origin, axis)),
        FaceSurface::Cone { apex, axis, .. } => Some((apex, axis)),
        FaceSurface::Torus { center, axis, .. } => Some((center, axis)),
        FaceSurface::Plane { .. } | FaceSurface::Sphere { .. } => None,
    }
}

/// The radial extent of a face's points about an axis: `(min, max)` distance.
fn radial_range(face: &SurfaceFace, origin: Vec3, axis: UnitVec3) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = 0.0_f64;
    for p in &face.points {
        let d = *p - origin;
        let radial = (d - axis * d.dot(axis)).len();
        if radial.is_finite() {
            lo = lo.min(radial);
            hi = hi.max(radial);
        }
    }
    (lo, hi)
}

fn mean_point(points: &[Vec3]) -> Option<Vec3> {
    let mut sum = Vec3::ZERO;
    let mut count = 0usize;
    for p in points {
        if p.is_finite() {
            sum += *p;
            count += 1;
        }
    }
    (count > 0).then(|| sum * (1.0 / count as f64))
}

fn project(view: StandardView, p: Vec3) -> Vec2 {
    let (right, up, _) = standard_camera(view).basis();
    Vec2::new(p.dot(right), p.dot(up))
}

fn project_dir(view: StandardView, d: UnitVec3) -> Vec2 {
    let (right, up, _) = standard_camera(view).basis();
    Vec2::new(d.dot(right), d.dot(up))
}

/// The closest point to `target` on the view's drawn edges (visible and hidden), keeping the
/// target's coordinate along `u` where an edge crosses that line. The result is the point a
/// dimension's extension line must start at: an extreme vertex, or the tangent point of an arc.
fn snap_to_edges(view: &ProjectedView, target: Vec2, u: Vec2) -> Vec2 {
    if !(target.x.is_finite() && target.y.is_finite() && u.x.is_finite() && u.y.is_finite()) {
        return target;
    }
    let n = u.perp();
    let want_u = target.dot(u);
    let want_n = target.dot(n);
    let mut best: Option<(f64, f64, Vec2)> = None;
    let mut consider = |p: Vec2| {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return;
        }
        let du = (p.dot(u) - want_u).abs();
        let dn = (p.dot(n) - want_n).abs();
        // Points on the wanted line first, then the closest point beside it. The reading
        // coordinate is compared in 0.005 mm steps: rounding noise (a far corner that happens to
        // sit exactly on the line) must not outrank an end point, while a point 0.03 mm off the
        // line is farther away than one that is exact (D9.1: an overall size keeps its value).
        let key = if du <= 0.05 { ((du / 0.005).round() * 0.005, dn) } else { (1000.0 + du, dn) };
        if best.is_none_or(|(bu, bn, _)| key.0.total_cmp(&bu).then(key.1.total_cmp(&bn)).is_lt()) {
            best = Some((key.0, key.1, p));
        }
    };
    for seg in view.visible.iter().chain(&view.hidden) {
        consider(seg.a);
        consider(seg.b);
        let (ua, ub) = (seg.a.dot(u), seg.b.dot(u));
        let (lo, hi) = (ua.min(ub), ua.max(ub));
        // A segment nearly perpendicular to the dimension's reading direction has no meaningful
        // crossing: its two ends differ only by rounding noise, and a crossing point there would
        // beat both ends and put the extension line at the far corner (D9.1).
        let length = seg.a.dist(seg.b);
        if want_u >= lo - 1e-9 && want_u <= hi + 1e-9 && (ub - ua).abs() > 1e-9 * length.max(1.0) {
            let t = ((want_u - ua) / (ub - ua)).clamp(0.0, 1.0);
            consider(seg.a.lerp(seg.b, t));
        }
    }
    best.map_or(target, |(_, _, p)| p)
}

/// Snap both ends of a linear or diameter dimension onto drawn edges, keeping each point's
/// reading coordinate: only the transverse position comes from the outline. The measured value is
/// then the designed value even when the outline is cut away at that spot (a break-out over a
/// hole's centre line, D9.1: the shaft's cross-hole position stays 55, not 54,98).
fn snap_linear(view: &ProjectedView, p1: Vec2, p2: Vec2, angle_deg: f64) -> (Vec2, Vec2) {
    let u = Vec2::from_angle(angle_deg.to_radians());
    let n = u.perp();
    let snap = |p: Vec2| {
        let edge = snap_to_edges(view, p, u);
        u * p.dot(u) + n * edge.dot(n)
    };
    (snap(p1), snap(p2))
}

/// The silhouette point at coordinate `want` along `u`: the drawn edge point farthest out along
/// `out`. A length dimension's extension line starts here, so it leaves the part instead of
/// running along the outline (D9.1). `None` when no edge lies at that coordinate.
fn silhouette_point(view: &ProjectedView, want: f64, u: Vec2, out: Vec2) -> Option<Vec2> {
    if !(want.is_finite() && u.is_finite() && out.is_finite()) {
        return None;
    }
    let mut best: Option<(bool, f64, Vec2)> = None;
    let consider = |p: Vec2, best: &mut Option<(bool, f64, Vec2)>| {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return;
        }
        let du = (p.dot(u) - want).abs();
        let on_line = du <= 0.03;
        let score = p.dot(out);
        let better = match *best {
            None => true,
            Some((was_on, was_score, _)) => match (on_line, was_on) {
                (true, false) => true,
                (false, true) => false,
                _ => score > was_score + 1e-9,
            },
        };
        if better {
            *best = Some((on_line, score, p));
        }
    };
    for seg in view.visible.iter().chain(&view.hidden) {
        let (ua, ub) = (seg.a.dot(u), seg.b.dot(u));
        let (lo, hi) = (ua.min(ub), ua.max(ub));
        consider(seg.a, &mut best);
        consider(seg.b, &mut best);
        if want >= lo - 1e-9 && want <= hi + 1e-9 && (ub - ua).abs() > 1e-12 {
            let t = ((want - ua) / (ub - ua)).clamp(0.0, 1.0);
            consider(seg.a.lerp(seg.b, t), &mut best);
        }
    }
    best.map(|(_, _, p)| p)
}

/// The two extension-line origins of an axial length: the silhouette points at the two axial
/// coordinates, outward on the side the dimension is drawn (D9.1: apart from its start on the
/// feature, an extension line overlaps no edge).
fn length_origins(view: &ProjectedView, p0: Vec2, p1: Vec2, horizontal: bool, outward: Vec2) -> (Vec2, Vec2) {
    let u = if horizontal { Vec2::X } else { Vec2::Y };
    let n = u.perp();
    let point = |p: Vec2| match silhouette_point(view, p.dot(u), u, outward) {
        // Keep the reading coordinate; the outline supplies the transverse one.
        Some(edge) => u * p.dot(u) + n * edge.dot(n),
        None => p,
    };
    (point(p0), point(p1))
}

/// The distance between two segments in the plane; zero when they meet.
#[cfg(test)]
fn segment_distance(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f64 {
    let point_segment = |p: Vec2, s0: Vec2, s1: Vec2| {
        let d = s1 - s0;
        let len2 = d.len2();
        if len2 <= 1e-18 {
            return p.dist(s0);
        }
        let t = ((p - s0).dot(d) / len2).clamp(0.0, 1.0);
        p.dist(s0 + d * t)
    };
    if segments_intersect(a, b, c, d) {
        return 0.0;
    }
    point_segment(a, c, d).min(point_segment(b, c, d)).min(point_segment(c, a, b)).min(point_segment(d, a, b))
}

/// The distance between two roughly parallel segments' lines; `None` when they cross at a
/// noticeable angle (a dimension line is allowed to cross a hidden line at right angles).
fn parallel_line_distance(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<f64> {
    let ab = (b - a).normalized()?;
    let cd = (d - c).normalized()?;
    if ab.cross(cd).abs() > 0.2588 {
        // more than 15 degrees apart
        return None;
    }
    let w = c - a;
    Some((w - ab * w.dot(ab)).len())
}

/// Whether the segments `a`→`b` and `c`→`d` meet (including a shared end point).
fn segments_intersect(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    let orient = |p: Vec2, q: Vec2, r: Vec2| (q - p).cross(r - p);
    let on = |p: Vec2, q: Vec2, r: Vec2| {
        q.x >= p.x.min(r.x) - 1e-9 && q.x <= p.x.max(r.x) + 1e-9 && q.y >= p.y.min(r.y) - 1e-9 && q.y <= p.y.max(r.y) + 1e-9
    };
    let o1 = orient(a, b, c);
    let o2 = orient(a, b, d);
    let o3 = orient(c, d, a);
    let o4 = orient(c, d, b);
    if ((o1 > 0.0) != (o2 > 0.0)) && ((o3 > 0.0) != (o4 > 0.0)) && o1.abs() > 1e-12 && o2.abs() > 1e-12 {
        return true;
    }
    (o1.abs() <= 1e-12 && on(a, c, b))
        || (o2.abs() <= 1e-12 && on(a, d, b))
        || (o3.abs() <= 1e-12 && on(c, a, d))
        || (o4.abs() <= 1e-12 && on(c, b, d))
}

/// The angle `t` on `a`→`b` (0..1) where it meets `c`→`d`, if the two cross strictly.
fn segment_crossing(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<f64> {
    let r = b - a;
    let s = d - c;
    let denom = r.cross(s);
    if denom.abs() <= 1e-12 {
        return None;
    }
    let t = (c - a).cross(s) / denom;
    let u = (c - a).cross(r) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// The angular intervals [start, end] (radians, end > start) the face's points cover in the view.
fn angular_intervals(face: &SurfaceFace, centre: Vec2, view: StandardView) -> Vec<(f64, f64)> {
    let mut angles: Vec<f64> = Vec::new();
    for p in &face.points {
        let d = project(view, *p) - centre;
        if d.len() <= 1e-9 {
            continue;
        }
        angles.push(d.y.atan2(d.x).rem_euclid(2.0 * PI));
    }
    if angles.is_empty() {
        return Vec::new();
    }
    angles.sort_by(f64::total_cmp);
    angles.dedup_by(|a, b| (*a - *b).abs() <= 1e-4);
    let gap = 0.087; // 5 degrees: the tessellation's own spacing is far below it.
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    let Some(first) = angles.first().copied() else { return intervals };
    let mut start = first;
    let mut previous = first;
    for angle in angles.iter().copied().skip(1) {
        if angle - previous > gap {
            intervals.push((start, previous));
            start = angle;
        }
        previous = angle;
    }
    intervals.push((start, previous));
    // A sector crossing the seam has one group at each end; join them through 2π.
    if intervals.len() >= 2 {
        let Some(&(last_lo, last_hi)) = intervals.last() else { return intervals };
        let Some(&(first_lo, first_hi)) = intervals.first() else { return intervals };
        if first_lo + 2.0 * PI - last_hi <= gap {
            intervals.pop();
            intervals.remove(0);
            intervals.push((last_lo, first_hi + 2.0 * PI));
            let _ = (first_lo, last_hi);
        }
    }
    if intervals.len() == 1 {
        let Some(&(lo, hi)) = intervals.first() else { return intervals };
        if lo + 2.0 * PI - hi <= gap {
            return vec![(lo, lo + 2.0 * PI)];
        }
    }
    intervals
}

/// The angle for a radius dimension: the middle of the face's largest covered sector, moved to a
/// point that lies on a visible edge of the view.
fn radius_angle(face: &SurfaceFace, centre: Vec2, radius: f64, view: &ProjectedView) -> f64 {
    let intervals = angular_intervals(face, centre, view.view);
    let mut best: Option<(f64, f64)> = None; // (interval length, angle)
    for (start, end) in intervals {
        let length = end - start;
        for fraction in [0.5, 0.25, 0.75, 0.1, 0.9] {
            let angle = start + length * fraction;
            let tip = centre + Vec2::from_angle(angle) * radius;
            let on_edge = view.visible.iter().any(|seg| {
                let d = seg.b - seg.a;
                let len2 = d.len2();
                if len2 <= 1e-18 {
                    return tip.dist(seg.a) <= 0.05;
                }
                let t = ((tip - seg.a).dot(d) / len2).clamp(0.0, 1.0);
                tip.dist(seg.a + d * t) <= 0.05
            });
            if on_edge && best.is_none_or(|(best_len, _)| length > best_len + 1e-9) {
                best = Some((length, angle));
                break;
            }
        }
    }
    let angle = best.map_or_else(|| mean_angle(face, centre, view.view).to_radians(), |(_, angle)| angle);
    angle.to_degrees()
}

/// Whether a leader's crossings with drawn edges all fall within the last 5 mm before its tip, or
/// on the view's outer silhouette box (a leader may enter the view from outside).
fn leader_hugs_outline(start: Vec2, tip: Vec2, outline: &[(Vec2, Vec2)], view_bbox: [Vec2; 2]) -> bool {
    let length = start.dist(tip);
    if !(length.is_finite() && length > 1e-9) {
        return true;
    }
    let [lo, hi] = view_bbox;
    let on_silhouette_box = |p: Vec2| {
        let tol = 0.1;
        (p.x - lo.x).abs() <= tol || (p.x - hi.x).abs() <= tol || (p.y - lo.y).abs() <= tol || (p.y - hi.y).abs() <= tol
    };
    for (a, b) in outline {
        if let Some(t) = segment_crossing(start, tip, *a, *b) {
            let from_tip = length * (1.0 - t);
            let at = start + (tip - start) * t;
            if from_tip > 5.0 + 1e-9 && !on_silhouette_box(at) {
                return false;
            }
        }
    }
    true
}

/// Whether the segment crosses any of `obstacles` (a family of annotation lines).
fn segment_clear_of(start: Vec2, tip: Vec2, obstacles: &[(Vec2, Vec2)]) -> bool {
    obstacles.iter().all(|(a, b)| !segments_intersect(start, tip, *a, *b))
}

/// Whether the direction lies in the view plane (within one degree).
fn in_view_plane(view: StandardView, d: UnitVec3) -> bool {
    let (_, _, back) = standard_camera(view).basis();
    back.dot(d).abs() <= ONE_DEG.sin()
}

/// Whether the direction points along the view direction (within one degree).
fn along_view(view: StandardView, d: UnitVec3) -> bool {
    let (_, _, back) = standard_camera(view).basis();
    back.dot(d).abs() >= ONE_DEG.cos()
}

/// The world axis (0 = X, 1 = Y, 2 = Z) a view direction or camera basis vector lies on.
fn world_axis_index(v: Vec3) -> usize {
    let components = [v.x.abs(), v.y.abs(), v.z.abs()];
    let mut best = 0;
    for (index, value) in components.iter().enumerate() {
        if *value > components.get(best).copied().unwrap_or(0.0) {
            best = index;
        }
    }
    best
}

fn axis_vec(index: usize) -> Vec3 {
    match index {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

/// The world axis `v` lies on, signed like `v` (a camera's right/up direction may point against
/// the positive axis).
fn signed_axis(v: Vec3) -> Vec3 {
    let index = world_axis_index(v);
    let component = match index {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    };
    axis_vec(index) * if component < 0.0 { -1.0 } else { 1.0 }
}

/// The component of `v` on the world axis `index`.
fn component(v: Vec3, index: usize) -> f64 {
    match index {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn bounds_extent(bounds: &Aabb3, index: usize) -> f64 {
    match index {
        0 => bounds.max.x - bounds.min.x,
        1 => bounds.max.y - bounds.min.y,
        _ => bounds.max.z - bounds.min.z,
    }
}

/// The projected coordinate span of the model bounds along a view direction: `(lo, hi)` in the
/// view's own millimetres.
fn projected_span(bounds: &Aabb3, direction: Vec3) -> (f64, f64) {
    let corner = |x: f64, y: f64, z: f64| x * direction.x + y * direction.y + z * direction.z;
    let values = [
        corner(bounds.min.x, bounds.min.y, bounds.min.z),
        corner(bounds.min.x, bounds.min.y, bounds.max.z),
        corner(bounds.min.x, bounds.max.y, bounds.min.z),
        corner(bounds.min.x, bounds.max.y, bounds.max.z),
        corner(bounds.max.x, bounds.min.y, bounds.min.z),
        corner(bounds.max.x, bounds.min.y, bounds.max.z),
        corner(bounds.max.x, bounds.max.y, bounds.min.z),
        corner(bounds.max.x, bounds.max.y, bounds.max.z),
    ];
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for value in values {
        lo = lo.min(value);
        hi = hi.max(value);
    }
    (lo, hi)
}

/// The span of a view's drawn outline (visible and hidden edges) along `u`: the extremes a
/// dimension can start on. The exact bounds use full capsules for round faces, which overstate a
/// partial arc's reach; the drawing itself is the truth for an overall size (D9.1).
fn view_span(view: &ProjectedView, u: Vec2) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for seg in view.visible.iter().chain(&view.hidden) {
        for p in [seg.a, seg.b] {
            let value = p.dot(u);
            if value.is_finite() {
                lo = lo.min(value);
                hi = hi.max(value);
            }
        }
    }
    (lo.is_finite() && hi.is_finite() && hi >= lo).then_some((lo, hi))
}

/// Extend `bounds` by the sphere of `radius` around `p`, intersected with the slab perpendicular
/// to `axis`: the exact box contribution of a cylinder's end circle.
fn add_capsule(bounds: &mut Aabb3, p: Vec3, axis: UnitVec3, radius: f64) {
    let d = axis.vec();
    let spread = [radius * (1.0 - d.x * d.x).max(0.0).sqrt(), radius * (1.0 - d.y * d.y).max(0.0).sqrt(), radius * (1.0 - d.z * d.z).max(0.0).sqrt()];
    let (Some(sx), Some(sy), Some(sz)) = (spread.first(), spread.get(1), spread.get(2)) else { return };
    bounds.add(Vec3::new(p.x - sx, p.y - sy, p.z - sz));
    bounds.add(Vec3::new(p.x + sx, p.y + sy, p.z + sz));
}

/// The exact box of the analytic faces: plane positions along their normals and the capsules of
/// cylinders and spheres bound the part without the control-polygon overshoot a B-spline seam
/// curve can add to the outline bounds.
fn exact_bounds(faces: &[SurfaceFace]) -> Aabb3 {
    let mut bounds = Aabb3::EMPTY;
    for face in faces {
        match face.surface {
            FaceSurface::Plane { .. } => {
                for p in &face.points {
                    bounds.add(*p);
                }
            }
            FaceSurface::Cylinder { origin, axis, radius } => {
                let Some((lo, hi)) = face.extent(origin, axis) else { continue };
                let (lo, hi) = (lo.min(hi), lo.max(hi));
                add_capsule(&mut bounds, origin + axis * lo, axis, radius);
                add_capsule(&mut bounds, origin + axis * hi, axis, radius);
            }
            FaceSurface::Sphere { center, radius } => {
                for dx in [-1.0, 1.0] {
                    for dy in [-1.0, 1.0] {
                        for dz in [-1.0, 1.0] {
                            bounds.add(center + Vec3::new(dx, dy, dz) * radius);
                        }
                    }
                }
            }
            FaceSurface::Cone { .. } | FaceSurface::Torus { .. } => {
                for p in &face.points {
                    bounds.add(*p);
                }
            }
        }
    }
    if bounds.is_empty() {
        return bounds;
    }
    bounds
}

/// The representative plane face at `coordinate` along `axis`, for traceability.
fn plane_face_at(faces: &[SurfaceFace], axis: Vec3, coordinate: f64) -> usize {
    faces
        .iter()
        .find(|face| {
            let FaceSurface::Plane { normal, point } = face.surface else { return false };
            normal.cross(UnitVec3::new(axis).unwrap_or(normal)).len() <= PARALLEL_TOL && (point.dot(axis) - coordinate).abs() <= LENGTH_TOL_MM
        })
        .map_or(0, |face| face.face)
}

/// A plane face perpendicular to `axis` at `coordinate`: its face index and largest radial extent.
fn plane_max_radial(faces: &[SurfaceFace], origin: Vec3, axis: UnitVec3, coordinate: f64, tolerance: f64) -> Option<(usize, f64)> {
    for face in faces {
        let FaceSurface::Plane { normal, point } = face.surface else { continue };
        if normal.dot(axis).abs() < ONE_DEG.cos() {
            continue;
        }
        let t = (point - origin).dot(axis.vec());
        if (t - coordinate).abs() > tolerance {
            continue;
        }
        let (_, max_r) = radial_range(face, origin, axis);
        return Some((face.face, max_r));
    }
    None
}

/// The faces a planned dimension or note already references.
fn referenced_faces(plan: &AutoPlan) -> std::collections::BTreeSet<usize> {
    let mut out = std::collections::BTreeSet::new();
    for dim in &plan.dims {
        out.insert(dim.source.face);
    }
    for note in &plan.notes {
        if let Some(face) = note.face {
            out.insert(face);
        }
    }
    out
}

/// Whether a round face's size is stated by an existing dimension (by its exact value), even when
/// the dimension's traceability face names another piece of the same smooth surface.
fn size_referenced(plan: &AutoPlan, face: &SurfaceFace) -> bool {
    match face.surface {
        FaceSurface::Cylinder { radius, .. } | FaceSurface::Sphere { radius, .. } => {
            let diameter = 2.0 * radius;
            plan.dims.iter().any(|d| match d.source.kind {
                "diameter" | "hole" => (d.source.value - diameter).abs() <= 1e-4,
                "radius" | "fillet" => (d.source.value - radius).abs() <= 1e-4,
                _ => false,
            })
        }
        FaceSurface::Cone { apex, axis, .. } | FaceSurface::Torus { center: apex, axis, .. } => {
            let (r_lo, r_hi) = radial_range(face, apex, axis);
            plan.dims.iter().any(|d| match d.source.kind {
                "diameter" | "hole" => (d.source.value - 2.0 * r_hi).abs() <= 1e-4 || (d.source.value - 2.0 * r_lo).abs() <= 1e-4,
                "radius" | "fillet" => (d.source.value - r_hi).abs() <= 1e-4 || (d.source.value - r_lo).abs() <= 1e-4,
                _ => false,
            })
        }
        FaceSurface::Plane { .. } => false,
    }
}

/// The completeness pass for round faces: a full or half turn face that no dimension or note
/// references goes into `undimensioned`. Drill points belong to their bore's depth note.
fn finish_round_faces(plan: &mut AutoPlan, faces: &[SurfaceFace]) {
    let referenced = referenced_faces(plan);
    for face in faces {
        let round = matches!(face.surface, FaceSurface::Cylinder { .. } | FaceSurface::Cone { .. } | FaceSurface::Torus { .. });
        if !round || face.coverage() < PI - FULL_TURN_TOL {
            continue;
        }
        if let FaceSurface::Cone { apex, axis, .. } = face.surface {
            let (r_lo, r_hi) = radial_range(face, apex, axis);
            if r_hi > 1e-9 && r_lo <= r_hi * 0.02 {
                // A drill point: the bore's depth note covers it.
                continue;
            }
        }
        if referenced.contains(&face.face) || size_referenced(plan, face) {
            continue;
        }
        let value = match face.surface {
            FaceSurface::Cylinder { radius, .. } | FaceSurface::Sphere { radius, .. } => 2.0 * radius,
            FaceSurface::Cone { apex, axis, .. } | FaceSurface::Torus { center: apex, axis, .. } => radial_range(face, apex, axis).1,
            FaceSurface::Plane { .. } => continue,
        };
        plan.undimensioned.push(Undimensioned { face: face.face, kind: "diameter", value });
    }
}

/// The completeness pass for a turned part's planes perpendicular to its axis: each must be a
/// chain end, an end face, a bore's end or a recess's face.
fn finish_turned_planes(plan: &mut AutoPlan, faces: &[SurfaceFace], origin: Vec3, axis: UnitVec3, referenced: &[f64], tolerance: f64) {
    for face in faces {
        let FaceSurface::Plane { normal, point } = face.surface else { continue };
        if normal.dot(axis).abs() < ONE_DEG.cos() {
            continue;
        }
        let (_, max_r) = radial_range(face, origin, axis);
        if max_r <= 1e-9 {
            continue;
        }
        let t = (point - origin).dot(axis.vec());
        if referenced.iter().any(|value| (value - t).abs() <= tolerance) {
            continue;
        }
        plan.undimensioned.push(Undimensioned { face: face.face, kind: "plane", value: t });
    }
}

// ------------------------------------------------------------------ turned parts

#[derive(Clone, Copy, Debug)]
struct CylinderStep {
    face: usize,
    radius: f64,
    z0: f64,
    z1: f64,
}

/// Sort steps and join the several faces one smooth cylinder may be cut into (a drilled hole's
/// halves, a revolve's 120° pieces) when they share a radius and overlap.
fn merge_steps(mut steps: Vec<CylinderStep>) -> Vec<CylinderStep> {
    steps.sort_by(|a, b| a.z0.total_cmp(&b.z0).then(a.radius.total_cmp(&b.radius)));
    let mut out: Vec<CylinderStep> = Vec::new();
    for step in steps {
        match out.iter_mut().find(|kept| (kept.radius - step.radius).abs() <= 1e-6 && step.z0 <= kept.z1 + LENGTH_TOL_MM) {
            Some(kept) => {
                kept.z0 = kept.z0.min(step.z0);
                kept.z1 = kept.z1.max(step.z1);
            }
            None => out.push(step),
        }
    }
    out.sort_by(|a, b| a.z0.total_cmp(&b.z0).then(a.radius.total_cmp(&b.radius)));
    out
}

fn plan_turned(s: &Session, selection: &ViewSelection, views: &[ProjectedView], faces: &[SurfaceFace]) -> AutoPlan {
    let Some(axis) = centre::turned_axis(s) else { return AutoPlan::default() };
    // The datum is the end face at minimum z: orient the axis with the positive world axis so
    // "the last segment" is always the one at the far end from the datum.
    let a = UnitVec3::new(centre::world_axis(axis.axis)).unwrap_or(axis.axis);
    let Some(view) = selection
        .views
        .iter()
        .copied()
        .find(|v| in_view_plane(*v, a) && views.iter().any(|p| p.view == *v))
        .or_else(|| selection.views.first().copied())
    else {
        return AutoPlan::default();
    };
    let Some(projected) = views.iter().find(|v| v.view == view) else { return AutoPlan::default() };
    let tolerance = AXIS_DISTANCE_FRACTION * s.world_state().bodies.iter().map(|b| b.outline_bounds().diagonal()).fold(1.0, f64::max);
    let view_axis = project_dir(view, a);
    let horizontal = view_axis.x.abs() >= view_axis.y.abs();
    let along = if horizontal { Vec2::new(view_axis.x.signum(), 0.0) } else { Vec2::new(0.0, view_axis.y.signum()) };
    let along = if along.len() > 0.5 { along } else { Vec2::X };
    let perp = Vec2::new(-along.y, along.x);
    let (right, up, _) = standard_camera(view).basis();
    let world_axis = UnitVec3::new(axis_vec(world_axis_index(a.vec()))).unwrap_or(a);
    let _ = (right, up);

    // Cylinders on the axis: external (material inside) and bores (material outside).
    let mut external: Vec<CylinderStep> = Vec::new();
    let mut bores: Vec<CylinderStep> = Vec::new();
    for face in faces {
        let FaceSurface::Cylinder { origin, axis: face_axis, radius } = face.surface else { continue };
        if !parallel(face_axis, a) || line_distance(axis.origin, a, origin) > tolerance {
            continue;
        }
        if face.coverage() < 2.0 * PI - FULL_TURN_TOL {
            continue;
        }
        let Some((lo, hi)) = face.extent(axis.origin, a) else { continue };
        let step = CylinderStep { face: face.face, radius, z0: lo.min(hi), z1: lo.max(hi) };
        if face.radial_side(origin, face_axis) > 0.0 {
            external.push(step);
        } else {
            bores.push(step);
        }
    }
    let external = merge_steps(external);
    let bores = merge_steps(bores);
    let max_radius = external.iter().map(|s| s.radius).fold(0.0, f64::max);
    if !(max_radius > 0.0) {
        return AutoPlan::default();
    }
    // A disc: its axial length does not exceed its outer radius, so a diameter drawn inside the
    // longitudinal view would lie on a step or recess edge. Its outer diameters go into their own
    // rows outside the view (ISO 129-1), its inner ones onto the end view.
    let axial = external.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), step| (lo.min(step.z0), hi.max(step.z1)));
    let axial_length = (axial.1 - axial.0).abs();
    let disc = axial_length > 0.0 && axial_length <= max_radius;
    let end_view = selection.views.iter().copied().find(|v| along_view(*v, a) && views.iter().any(|p| p.view == *v));

    // Shoulder planes: planes perpendicular to the axis whose radius matches an external step end.
    let z_tol = (LENGTH_TOL_MM).max(1e-5 * max_radius * 100.0);
    let r_tol = 1e-5 * max_radius.max(1.0);
    let mut shoulders: Vec<(f64, usize)> = Vec::new();
    for face in faces {
        let FaceSurface::Plane { normal, point } = face.surface else { continue };
        if !parallel(normal, a) {
            continue;
        }
        let t = (point - axis.origin).dot(a.vec());
        let (_, max_r) = radial_range(face, axis.origin, a);
        let matches_step =
            external.iter().any(|step| (step.radius - max_r).abs() <= r_tol && ((step.z0 - t).abs() <= z_tol || (step.z1 - t).abs() <= z_tol));
        if matches_step {
            shoulders.push((t, face.face));
        }
    }
    shoulders.sort_by(|a, b| a.0.total_cmp(&b.0));
    shoulders.dedup_by(|a, b| (a.0 - b.0).abs() <= z_tol);
    // Every shoulder plane is stated by the chain or the overall length; the completeness pass
    // reads this list rather than re-deriving which length spans which plane.
    let mut referenced_planes: Vec<f64> = shoulders.iter().map(|s| s.0).collect();

    let mut plan = AutoPlan::default();
    // A horizontal view's lengths sit below it and start on the bottom silhouette; a vertical
    // view's lengths sit to its right and start on the right silhouette.
    let axis_point = |t: f64| project(view, axis.origin + a * t);

    // External diameters, each step its own. A shaft keeps them through the longitudinal view at
    // a station on the step; a disc puts them outside in rows above (a vertical axis) or beside
    // (a horizontal one) the view, each with its own extension lines, so no line runs on a step
    // or recess edge and no value floats on a long run-out.
    let outside_side = if horizontal { DimSide::Left } else { DimSide::Above };
    for step in &external {
        if !(step.radius.is_finite() && step.radius > 0.0) {
            continue;
        }
        let mid = (step.z0 + step.z1) * 0.5;
        let centre = if disc { axis_point(step.z1) } else { axis_point(mid) };
        let p1 = centre + perp * step.radius;
        let p2 = centre - perp * step.radius;
        let angle = perp.y.atan2(perp.x).to_degrees();
        let value = 2.0 * step.radius;
        let text = format!("Ø{}", crate::drawing::format_value(value));
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view,
                geometry: DimensionGeometry::Diameter { p1, p2, angle_deg: angle, offset_mm: 0.0 },
                text: None,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: disc.then_some(outside_side),
            row: None,
            source: AutoDimensionSource { view, face: step.face, kind: "diameter", value, text },
            station: (!disc).then(|| Station { origin: axis_point(0.0), along, perp, z0: step.z0, z1: step.z1, radius: step.radius }),
        });
    }

    // Length chain: every shoulder-to-shoulder length but the last, one row; overall on the next.
    if shoulders.len() >= 2 {
        let mut chain: Vec<Dimension> = Vec::new();
        for pair in shoulders.windows(2) {
            let (Some(a0), Some(a1)) = (pair.first(), pair.get(1)) else { continue };
            if (a1.0 - a0.0).abs() <= z_tol {
                continue;
            }
            let p0 = axis_point(a0.0);
            let p1 = axis_point(a1.0);
            let (p1v, p2v) = length_origins(projected, p0, p1, horizontal, if horizontal { -Vec2::Y } else { Vec2::X });
            let angle = if horizontal { 0.0 } else { 90.0 };
            chain.push(Dimension {
                view,
                geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: angle, offset_mm: 0.0 },
                text: None,
                suffix: None,
                text_shift_mm: 0.0,
            });
        }
        // Leave the last segment out: the overall length states it.
        chain.pop();
        for (index, dim) in chain.iter().enumerate() {
            let Some(pair) = shoulders.get(index).zip(shoulders.get(index + 1)) else { continue };
            let value = (pair.1.0 - pair.0.0).abs();
            plan.dims.push(PlannedDim {
                dimension: dim.clone(),
                side: Some(if horizontal { DimSide::Below } else { DimSide::Right }),
                row: Some(0),
                station: None,
                source: AutoDimensionSource { view, face: pair.0.1, kind: "length", value, text: crate::drawing::format_value(value) },
            });
        }
        let Some(first) = shoulders.first() else { return plan };
        let Some(last) = shoulders.last() else { return plan };
        let overall = (last.0 - first.0).abs();
        let p0 = axis_point(first.0);
        let p1 = axis_point(last.0);
        let (p1v, p2v) = length_origins(projected, p0, p1, horizontal, if horizontal { -Vec2::Y } else { Vec2::X });
        let angle = if horizontal { 0.0 } else { 90.0 };
        let dim = Dimension {
            view,
            geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: angle, offset_mm: 0.0 },
            text: None,
            suffix: None,
            text_shift_mm: 0.0,
        };
        plan.dims.push(PlannedDim {
            dimension: dim,
            side: Some(if horizontal { DimSide::Below } else { DimSide::Right }),
            row: Some(1),
            station: None,
            source: AutoDimensionSource { view, face: first.1, kind: "length", value: overall, text: crate::drawing::format_value(overall) },
        });
    }

    let overall_lo = shoulders.first().map_or(f64::INFINITY, |s| s.0);
    let overall_hi = shoulders.last().map_or(f64::NEG_INFINITY, |s| s.0);

    // Keyway or milled flat: a plane parallel to the axis at a constant depth below the outline.
    // Its size note states the width (the floor's extent between the two side planes) and the
    // depth from the outline above it to the floor (D7.1 defect 7).
    let mut keyways: Vec<(f64, f64, usize, f64, f64, Vec<Vec2>)> = Vec::new();
    for face in faces {
        let FaceSurface::Plane { normal, point } = face.surface else { continue };
        let along_dot = normal.dot(a);
        if along_dot.abs() > ONE_DEG.sin() {
            continue;
        }
        let d = point - axis.origin;
        let radial = d - a * d.dot(a);
        let Some(unit) = radial.normalized() else { continue };
        // A floor plane faces away from the axis; a side wall of the slot does not.
        if normal.dot(unit) < 0.9 {
            continue;
        }
        let depth_floor = radial.len();
        if depth_floor >= max_radius * 0.98 || depth_floor <= 1e-6 {
            continue;
        }
        let Some((lo, hi)) = face.extent(axis.origin, a) else { continue };
        let (t0, t1) = (lo.min(hi), lo.max(hi));
        if t1 - t0 <= 0.5 || t1 - t0 >= (overall_hi - overall_lo) * 0.98 {
            continue;
        }
        // The floor's extent across the slot is the distance between its side planes.
        let tangent = UnitVec3::new(a.cross(unit)).unwrap_or(a);
        let mut w_lo = f64::INFINITY;
        let mut w_hi = f64::NEG_INFINITY;
        for p in &face.points {
            let w = (*p - axis.origin).dot(tangent);
            if w.is_finite() {
                w_lo = w_lo.min(w);
                w_hi = w_hi.max(w);
            }
        }
        let width = w_hi - w_lo;
        if !(width.is_finite() && width > 0.1) {
            continue;
        }
        // The depth is measured from the outline (the external radius over the slot) to the floor.
        let mid_t = (t0 + t1) * 0.5;
        let outer = external.iter().find(|step| mid_t >= step.z0 - z_tol && mid_t <= step.z1 + z_tol).map_or(max_radius, |step| step.radius);
        // The depth is measured from the outline at the slot's side: the cylinder surface there
        // is lower than its crown by the slot's half width (the reference's 1,5 tief).
        let surface = (outer * outer - (width * 0.5) * (width * 0.5)).max(0.0).sqrt();
        let depth = surface - depth_floor;
        if !(depth.is_finite() && depth > 1e-6) {
            continue;
        }
        let mut targets = Vec::new();
        if let Some(centre) = mean_point(&face.points) {
            let centre_view = project(view, centre);
            targets.push(centre_view);
            let tangent_view = project_dir(view, tangent);
            if tangent_view.len() > 0.5 {
                let offset = tangent_view.normalized().unwrap_or(Vec2::X) * (width * 0.5);
                targets.push(centre_view + offset);
                targets.push(centre_view - offset);
            }
            // Points along the floor, away from the middle where a diameter's station may sit.
            let along_view = project_dir(view, a);
            if along_view.len() > 0.5 {
                let offset = along_view.normalized().unwrap_or(Vec2::X) * ((t1 - t0) * 0.25);
                targets.push(centre_view + offset);
                targets.push(centre_view - offset);
            }
        }
        if targets.is_empty() {
            continue;
        }
        keyways.push((t0, t1, face.face, width, depth, targets));
    }
    keyways.sort_by(|a, b| a.0.total_cmp(&b.0));
    keyways.dedup_by(|a, b| (a.0 - b.0).abs() <= LENGTH_TOL_MM && (a.1 - b.1).abs() <= LENGTH_TOL_MM);
    for (t0, t1, face, width, depth, targets) in keyways {
        let p0 = axis_point(t0);
        let p1 = axis_point(t1);
        // The keyway length sits above a horizontal view (left of a vertical one).
        let (p1v, p2v) = length_origins(projected, p0, p1, horizontal, if horizontal { Vec2::Y } else { -Vec2::X });
        let angle = if horizontal { 0.0 } else { 90.0 };
        let value = (t1 - t0).abs();
        referenced_planes.push(t0);
        referenced_planes.push(t1);
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view,
                geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: angle, offset_mm: 0.0 },
                text: None,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: Some(if horizontal { DimSide::Above } else { DimSide::Left }),
            row: None,
            station: None,
            source: AutoDimensionSource { view, face, kind: "length", value, text: crate::drawing::format_value(value) },
        });
        let text = format!("Keilnut {} x {} tief", crate::drawing::format_value(width), crate::drawing::format_value(depth));
        plan.notes.push(PlannedNote { view, text, targets, face: Some(face) });
    }

    // A disc's inner diameters read on the end view (D9.1): one diameter per bore or recess,
    // with its extension lines rising from the circle's waist points into a row above the view,
    // so every value has room of its own and both arrows sit on extension lines. The section
    // itself stays free for the outer diameters' rows.
    let mut bore_seen: Vec<(f64, f64, f64)> = Vec::new();
    if disc
        && let Some(end) = end_view
        && let Some(end_projected) = views.iter().find(|v| v.view == end)
        && overall_lo.is_finite()
        && overall_hi.is_finite()
    {
        let circle_centre = project(end, axis.origin);
        let opening = |t: f64, radius: f64| -> Option<f64> {
            if t <= overall_lo + z_tol || t >= overall_hi - z_tol {
                return None;
            }
            let (_, max_r) = plane_max_radial(faces, axis.origin, a, t, z_tol)?;
            (max_r > radius + z_tol).then_some(t)
        };
        for bore in &bores {
            if bore_seen
                .iter()
                .any(|(r, z0, z1)| (*r - bore.radius).abs() <= 1e-6 && (*z0 - bore.z0).abs() <= z_tol && (*z1 - bore.z1).abs() <= z_tol)
            {
                continue;
            }
            // Only a circle the end view really draws gets its dimension there; a recess or bore
            // on the far side stays in the section.
            if !circle_drawn(end_projected, circle_centre, bore.radius) {
                continue;
            }
            let open_lo = (bore.z0 - overall_lo).abs() <= z_tol;
            let open_hi = (bore.z1 - overall_hi).abs() <= z_tol;
            let value = 2.0 * bore.radius;
            // A through bore reads as its diameter alone: the section shows that it goes through.
            let text = if open_lo && open_hi {
                format!("Ø{}", crate::drawing::format_value(value))
            } else if open_lo || open_hi {
                let depth = (bore.z1 - bore.z0).abs();
                format!("Ø{} x {} tief", crate::drawing::format_value(value), depth_text(depth))
            } else {
                let open_t = match (opening(bore.z0, bore.radius), opening(bore.z1, bore.radius)) {
                    (Some(t), _) => t,
                    (None, Some(t)) => t,
                    (None, None) => continue,
                };
                let floor_t = if (open_t - bore.z1).abs() <= z_tol { bore.z0 } else { bore.z1 };
                let depth = (open_t - floor_t).abs();
                if !(depth.is_finite() && depth > z_tol) {
                    continue;
                }
                referenced_planes.push(open_t);
                referenced_planes.push(floor_t);
                format!("Ø{} x {} tief", crate::drawing::format_value(value), depth_text(depth))
            };
            if open_lo || open_hi {
                referenced_planes.push(bore.z0);
                referenced_planes.push(bore.z1);
            }
            bore_seen.push((bore.radius, bore.z0, bore.z1));
            let rim = Vec2::new(bore.radius, 0.0);
            plan.dims.push(PlannedDim {
                dimension: Dimension {
                    view: end,
                    geometry: DimensionGeometry::Diameter { p1: circle_centre - rim, p2: circle_centre + rim, angle_deg: 0.0, offset_mm: 0.0 },
                    text: Some(text.clone()),
                    suffix: None,
                    text_shift_mm: 0.0,
                },
                side: Some(DimSide::Above),
                row: None,
                station: None,
                source: AutoDimensionSource { view: end, face: bore.face, kind: "diameter", value, text },
            });
        }
    }
    // Axial bores: a note at the mouth, blind with its cylindrical depth or through.
    if overall_lo.is_finite() && overall_hi.is_finite() {
        for bore in &bores {
            let open_lo = (bore.z0 - overall_lo).abs() <= z_tol;
            let open_hi = (bore.z1 - overall_hi).abs() <= z_tol;
            if !(open_lo || open_hi) {
                continue;
            }
            if bore_seen
                .iter()
                .any(|(r, z0, z1)| (*r - bore.radius).abs() <= 1e-6 && (*z0 - bore.z0).abs() <= z_tol && (*z1 - bore.z1).abs() <= z_tol)
            {
                continue;
            }
            bore_seen.push((bore.radius, bore.z0, bore.z1));
            let depth = (bore.z1 - bore.z0).abs();
            let through = open_lo && open_hi;
            let text = if through {
                format!("Bohrung Ø{} durchgehend", crate::drawing::format_value(2.0 * bore.radius))
            } else {
                format!("Bohrung Ø{} x {} tief", crate::drawing::format_value(2.0 * bore.radius), depth_text(depth))
            };
            let mouth = if open_hi { bore.z1 } else { bore.z0 };
            let centre = axis_point(mouth);
            // Both mouth corners (and the mouth's middle) are on the drawn opening edge.
            let targets = vec![centre + perp * bore.radius, centre - perp * bore.radius, centre];
            plan.notes.push(PlannedNote { view, text, targets, face: Some(bore.face) });
            // A blind bore's bottom is stated by its depth.
            referenced_planes.push(bore.z0);
            referenced_planes.push(bore.z1);
        }
    }

    // Recesses and counterbores that do not open at an end face (the flywheel's Ø90 recess): a
    // diameter across the hidden wall, with the depth from the face the recess starts at.
    let mut recess_seen: Vec<(f64, f64, f64)> = bore_seen.clone();
    for bore in &bores {
        if recess_seen.iter().any(|(r, z0, z1)| (*r - bore.radius).abs() <= 1e-6 && (*z0 - bore.z0).abs() <= z_tol && (*z1 - bore.z1).abs() <= z_tol)
        {
            continue;
        }
        let opening = |t: f64| -> Option<f64> {
            if t <= overall_lo + z_tol || t >= overall_hi - z_tol {
                return None;
            }
            let (_, max_r) = plane_max_radial(faces, axis.origin, a, t, z_tol)?;
            (max_r > bore.radius + z_tol).then_some(t)
        };
        let open_t = match (opening(bore.z0), opening(bore.z1)) {
            (Some(t), _) => t,
            (None, Some(t)) => t,
            (None, None) => continue,
        };
        let floor_t = if (open_t - bore.z1).abs() <= z_tol { bore.z0 } else { bore.z1 };
        let depth = (open_t - floor_t).abs();
        if !(depth.is_finite() && depth > z_tol) {
            continue;
        }
        recess_seen.push((bore.radius, bore.z0, bore.z1));
        // Draw just inside the mouth, on the hidden wall: a section does not exist yet, and the
        // value stays clear of the other diameters' lines.
        let mid = open_t + (floor_t - open_t) * 0.2;
        let centre = axis_point(mid);
        let p1 = centre + perp * bore.radius;
        let p2 = centre - perp * bore.radius;
        let angle = perp.y.atan2(perp.x).to_degrees();
        let value = 2.0 * bore.radius;
        let text = format!("Ø{} x {} tief", crate::drawing::format_value(value), depth_text(depth));
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view,
                geometry: DimensionGeometry::Diameter { p1, p2, angle_deg: angle, offset_mm: 0.0 },
                text: Some(text.clone()),
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: None,
            row: None,
            station: None,
            source: AutoDimensionSource { view, face: bore.face, kind: "diameter", value, text },
        });
        referenced_planes.push(open_t);
        referenced_planes.push(floor_t);
    }

    // Cross holes: through the part, a size note and one position from the nearer end.
    let mut cross_seen: Vec<(f64, f64, f64)> = Vec::new();
    for face in faces {
        let FaceSurface::Cylinder { origin, axis: hole_axis, radius } = face.surface else { continue };
        if parallel(hole_axis, a) {
            continue;
        }
        if face.coverage() < 2.0 * PI - FULL_TURN_TOL || face.radial_side(origin, hole_axis) >= 0.0 {
            continue;
        }
        if axis_line_distance(axis.origin, a, origin, hole_axis) > max_radius * 0.25 {
            continue;
        }
        let Some((lo, hi)) = face.extent(origin, hole_axis) else { continue };
        let depth = (hi - lo).abs();
        let t_h = (origin - axis.origin).dot(a.vec());
        let local_r = external.iter().find(|step| t_h >= step.z0 - z_tol && t_h <= step.z1 + z_tol).map_or(max_radius, |step| step.radius);
        let through = depth >= 2.0 * local_r * THROUGH_SHARE;
        if cross_seen.iter().any(|(x, r, _)| (*x - t_h).abs() <= z_tol && (*r - radius).abs() <= 1e-6) {
            continue;
        }
        cross_seen.push((t_h, radius, depth));
        let hole2 = project_dir(view, hole_axis);
        let hole_dir = if hole2.len() > 0.5 { hole2.normalized().unwrap_or(Vec2::X) } else { Vec2::X };
        let hole_perp = Vec2::new(-hole_dir.y, hole_dir.x);
        // Both mouths are candidates, so the leader can go to the one nearest its text.
        let mut targets = Vec::new();
        for end in [origin + hole_axis * lo, origin + hole_axis * hi] {
            let mouth = project(view, end);
            targets.push(mouth + hole_perp * radius);
            targets.push(mouth - hole_perp * radius);
            targets.push(mouth);
        }
        let text = if through {
            format!("Bohrung Ø{} durchgehend", crate::drawing::format_value(2.0 * radius))
        } else {
            format!("Bohrung Ø{} x {} tief", crate::drawing::format_value(2.0 * radius), depth_text(depth))
        };
        plan.notes.push(PlannedNote { view, text, targets, face: Some(face.face) });
        // The position from the nearer end face to the hole axis.
        if !(overall_lo.is_finite() && overall_hi.is_finite()) {
            continue;
        }
        let near = if (t_h - overall_lo).abs() <= (overall_hi - t_h).abs() { overall_lo } else { overall_hi };
        let p_end = axis_point(near);
        let p_hole = axis_point(t_h);
        let (p1v, p2v) = length_origins(projected, p_end, p_hole, horizontal, if horizontal { Vec2::Y } else { -Vec2::X });
        let angle = if horizontal { 0.0 } else { 90.0 };
        let value = p_hole.dist(p_end);
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view,
                geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: angle, offset_mm: 0.0 },
                text: None,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: Some(if horizontal { DimSide::Above } else { DimSide::Left }),
            row: None,
            station: None,
            source: AutoDimensionSource { view, face: face.face, kind: "position", value, text: crate::drawing::format_value(value) },
        });
    }

    // Chamfers: a 45° cone up to 5 mm long gets a Fase note; other cones get diameters at both ends.
    for face in faces {
        let FaceSurface::Cone { apex, axis: cone_axis, half_angle } = face.surface else { continue };
        if !parallel(cone_axis, a) || line_distance(axis.origin, a, apex) > tolerance {
            continue;
        }
        let Some((lo, hi)) = face.extent(axis.origin, a) else { continue };
        let (t0, t1) = (lo.min(hi), lo.max(hi));
        let length = t1 - t0;
        let (r_lo, r_hi) = radial_range(face, axis.origin, a);
        if r_hi <= 1e-9 || r_lo <= r_hi * 0.02 {
            // A drill point, not a chamfer.
            continue;
        }
        let degrees = half_angle.to_degrees();
        if (degrees - 45.0).abs() <= 0.5 && length > 0.0 && length <= MAX_CHAMFER_MM {
            let Some(centre) = mean_point(&face.points) else { continue };
            let target = project(view, centre);
            let text = format!("Fase {} x 45°", crate::drawing::format_value(length));
            plan.notes.push(PlannedNote { view, text, targets: vec![target], face: Some(face.face) });
        } else if length > 0.0 {
            for (t, radius) in [(t0, r_lo), (t1, r_hi)] {
                if radius <= 1e-9 {
                    continue;
                }
                let centre = axis_point(t);
                let p1 = centre + perp * radius;
                let p2 = centre - perp * radius;
                let angle = perp.y.atan2(perp.x).to_degrees();
                let value = 2.0 * radius;
                let (p1, p2) = snap_linear(projected, p1, p2, angle);
                plan.dims.push(PlannedDim {
                    dimension: Dimension {
                        view,
                        geometry: DimensionGeometry::Diameter { p1, p2, angle_deg: angle, offset_mm: 0.0 },
                        text: None,
                        suffix: None,
                        text_shift_mm: 0.0,
                    },
                    side: None,
                    row: None,
                    station: None,
                    source: AutoDimensionSource {
                        view,
                        face: face.face,
                        kind: "diameter",
                        value,
                        text: format!("Ø{}", crate::drawing::format_value(value)),
                    },
                });
            }
        }
    }
    finish_round_faces(&mut plan, faces);
    finish_turned_planes(&mut plan, faces, axis.origin, a, &referenced_planes, z_tol);
    let _ = (right, up, world_axis);
    plan
}

// ------------------------------------------------------------------ flat and general parts

fn plan_flat(_s: &Session, selection: &ViewSelection, views: &[ProjectedView], faces: &[SurfaceFace]) -> AutoPlan {
    let mut plan = AutoPlan::default();
    let bounds = exact_bounds(faces);
    if bounds.is_empty() {
        return plan;
    }
    let Some(plan_view) = selection.views.first().copied().and_then(|v| views.iter().find(|p| p.view == v)) else {
        return plan;
    };
    let Some(edge_view) = selection.views.get(1).copied().and_then(|v| views.iter().find(|p| p.view == v)) else {
        return plan;
    };
    let (right, up, back) = standard_camera(plan_view.view).basis();
    let thin_axis = world_axis_index(back);
    let (x_lo, x_hi) = view_span(plan_view, Vec2::X).unwrap_or_else(|| projected_span(&bounds, right));
    let (y_lo, y_hi) = view_span(plan_view, Vec2::Y).unwrap_or_else(|| projected_span(&bounds, up));
    let length = x_hi - x_lo;
    let width = y_hi - y_lo;
    // Length and width on the plan view; thickness on the edge view, along the thin direction.
    plan.dims.push(overall_linear(
        plan_view,
        Vec2::new(x_lo, y_lo),
        Vec2::new(x_hi, y_lo),
        0.0,
        DimSide::Below,
        length,
        "overall",
        plane_face_at(faces, signed_axis(right), x_lo),
    ));
    plan.dims.push(overall_linear(
        plan_view,
        Vec2::new(x_lo, y_lo),
        Vec2::new(x_lo, y_hi),
        90.0,
        DimSide::Left,
        width,
        "overall",
        plane_face_at(faces, signed_axis(up), y_lo),
    ));
    let _ = component(right, world_axis_index(right));
    let (eright, eup, _) = standard_camera(edge_view.view).basis();
    if world_axis_index(eright) == thin_axis {
        let (t_lo, t_hi) = view_span(edge_view, Vec2::X).unwrap_or_else(|| projected_span(&bounds, eright));
        plan.dims.push(overall_linear(
            edge_view,
            Vec2::new(t_lo, edge_view.bbox[0].y),
            Vec2::new(t_hi, edge_view.bbox[0].y),
            0.0,
            DimSide::Below,
            t_hi - t_lo,
            "overall",
            plane_face_at(faces, signed_axis(eright), t_lo),
        ));
    } else if world_axis_index(eup) == thin_axis {
        let (t_lo, t_hi) = view_span(edge_view, Vec2::Y).unwrap_or_else(|| projected_span(&bounds, eup));
        plan.dims.push(overall_linear(
            edge_view,
            Vec2::new(edge_view.bbox[0].x, t_lo),
            Vec2::new(edge_view.bbox[0].x, t_hi),
            90.0,
            DimSide::Left,
            t_hi - t_lo,
            "overall",
            plane_face_at(faces, signed_axis(eup), t_lo),
        ));
    }

    // The plan view's holes, their positions and the outer arcs and fillets around them. The edge
    // view is searched too: a hole whose axis lies in the plan (a cross hole or a skewed bore) is
    // seen there and gets its size note (D9.1), and its cored outline is not repeated in the plan.
    plan_view_features(&mut plan, plan_view, faces, &bounds, views);
    plan_view_features(&mut plan, edge_view, faces, &bounds, views);
    finish_round_faces(&mut plan, faces);
    plan
}

fn plan_general(_s: &Session, selection: &ViewSelection, views: &[ProjectedView], faces: &[SurfaceFace]) -> AutoPlan {
    let mut plan = AutoPlan::default();
    let bounds = exact_bounds(faces);
    if bounds.is_empty() {
        return plan;
    }
    // Overall L, W and H: each world axis once, on the first selected view that shows it.
    let mut used = [false; 3];
    for axis in 0..3 {
        if used.get(axis).copied().unwrap_or(true) {
            continue;
        }
        'views: for view in &selection.views {
            let Some(projected) = views.iter().find(|p| p.view == *view) else { continue };
            let (right, up, _) = standard_camera(*view).basis();
            if world_axis_index(right) == axis {
                let (lo, hi) = view_span(projected, Vec2::X).unwrap_or_else(|| projected_span(&bounds, right));
                let value = hi - lo;
                plan.dims.push(overall_linear(
                    projected,
                    Vec2::new(lo, projected.bbox[0].y),
                    Vec2::new(hi, projected.bbox[0].y),
                    0.0,
                    DimSide::Below,
                    value,
                    "overall",
                    plane_face_at(faces, axis_vec(axis), lo),
                ));
                if let Some(slot) = used.get_mut(axis) {
                    *slot = true;
                }
                break 'views;
            }
            if world_axis_index(up) == axis {
                let (lo, hi) = view_span(projected, Vec2::Y).unwrap_or_else(|| projected_span(&bounds, up));
                let value = hi - lo;
                plan.dims.push(overall_linear(
                    projected,
                    Vec2::new(projected.bbox[0].x, lo),
                    Vec2::new(projected.bbox[0].x, hi),
                    90.0,
                    DimSide::Left,
                    value,
                    "overall",
                    plane_face_at(faces, axis_vec(axis), lo),
                ));
                if let Some(slot) = used.get_mut(axis) {
                    *slot = true;
                }
                break 'views;
            }
        }
    }
    for view in &selection.views {
        if let Some(projected) = views.iter().find(|p| p.view == *view) {
            plan_view_features(&mut plan, projected, faces, &bounds, views);
        }
    }
    finish_round_faces(&mut plan, faces);
    plan
}

fn overall_linear(
    view: &ProjectedView,
    p1: Vec2,
    p2: Vec2,
    angle_deg: f64,
    side: DimSide,
    value: f64,
    kind: &'static str,
    face: usize,
) -> PlannedDim {
    // Start the extension lines on the outline, not at the bounding-box corner.
    let (p1, p2) = snap_linear(view, p1, p2, angle_deg);
    PlannedDim {
        dimension: Dimension {
            view: view.view,
            geometry: DimensionGeometry::Linear { p1, p2, angle_deg, offset_mm: 0.0 },
            text: None,
            suffix: None,
            text_shift_mm: 0.0,
        },
        side: Some(side),
        row: None,
        station: None,
        source: AutoDimensionSource { view: view.view, face, kind, value, text: crate::drawing::format_value(value) },
    }
}

/// One full-turn inward cylinder in a view looking along its axis.
#[derive(Clone, Copy, Debug)]
struct Hole {
    face: usize,
    centre: Vec2,
    radius: f64,
    through: bool,
    depth: f64,
    /// The angle between the hole's axis and the view direction (degrees).
    skew_deg: f64,
}

/// Whether another chosen view draws the hole with hidden lines: its axis lies in that view's
/// plane and both hidden silhouettes at the hole radius are drawn over the hole's extent.
fn hole_shown_elsewhere(faces: &[SurfaceFace], face: usize, all_views: &[ProjectedView], current: StandardView) -> bool {
    let Some(surface) = faces.iter().find(|f| f.face == face) else { return false };
    let FaceSurface::Cylinder { origin, axis, radius } = surface.surface else { return false };
    let Some((lo, hi)) = surface.extent(origin, axis) else { return false };
    let (depth_lo, depth_hi) = (lo.min(hi), lo.max(hi));
    let tolerance = (radius * 0.02).max(0.03);
    for other in all_views {
        if other.view == current {
            continue;
        }
        let (_, _, back) = standard_camera(other.view).basis();
        if back.dot(axis).abs() > ONE_DEG.sin() {
            continue;
        }
        let d = project_dir(other.view, axis);
        let Some(u) = d.normalized() else { continue };
        let n = u.perp();
        let p0 = project(other.view, origin);
        let (mut plus, mut minus) = (false, false);
        for seg in &other.hidden {
            let (a, b) = (seg.a - p0, seg.b - p0);
            let (sa, sb) = (a.dot(n), b.dot(n));
            let (ta, tb) = (a.dot(u), b.dot(u));
            let overlaps = ta.max(tb) >= depth_lo - tolerance && ta.min(tb) <= depth_hi + tolerance;
            if !overlaps {
                continue;
            }
            if (sa - radius).abs() <= tolerance && (sb - radius).abs() <= tolerance {
                plus = true;
            }
            if (sa + radius).abs() <= tolerance && (sb + radius).abs() <= tolerance {
                minus = true;
            }
        }
        if plus && minus {
            return true;
        }
    }
    false
}

/// Add the holes, hole positions, outer arcs and fillets of one view.
fn plan_view_features(plan: &mut AutoPlan, view: &ProjectedView, faces: &[SurfaceFace], bounds: &Aabb3, all_views: &[ProjectedView]) {
    let (right, up, back) = standard_camera(view.view).basis();
    let (x_lo, _x_hi) = projected_span(bounds, right);
    let (y_lo, _y_hi) = projected_span(bounds, up);
    let mut holes: Vec<Hole> = Vec::new();
    let mut used_faces: Vec<usize> = Vec::new();
    for face in faces {
        let FaceSurface::Cylinder { origin, axis, radius } = face.surface else { continue };
        let skew_deg = back.dot(axis).abs().clamp(0.0, 1.0).acos().to_degrees();
        if !(skew_deg <= MAX_HOLE_SKEW_DEG) || face.coverage() < 2.0 * PI - HOLE_TURN_TOL {
            continue;
        }
        if face.radial_side(origin, axis) >= 0.0 || !(radius > 0.0) {
            continue;
        }
        let axis_index = world_axis_index(back);
        let Some((lo, hi)) = face.extent(origin, axis) else { continue };
        let depth = (hi - lo).abs();
        let material = bounds_extent(bounds, axis_index);
        let through = material > 0.0 && depth >= material * THROUGH_SHARE;
        holes.push(Hole { face: face.face, centre: project(view.view, origin), radius, through, depth, skew_deg });
        used_faces.push(face.face);
    }
    holes.sort_by(|a, b| a.centre.x.total_cmp(&b.centre.x).then(a.centre.y.total_cmp(&b.centre.y)).then(a.radius.total_cmp(&b.radius)));
    holes.dedup_by(|a, b| a.centre.dist(b.centre) <= 1e-6 && (a.radius - b.radius).abs() <= 1e-6);

    // Group equal holes and their through/blind state: one diameter per group.
    let mut groups: Vec<Vec<Hole>> = Vec::new();
    for hole in &holes {
        let found = groups
            .iter_mut()
            .find(|group| group.first().is_some_and(|first| (first.radius - hole.radius).abs() <= 1e-6 && first.through == hole.through));
        match found {
            Some(group) => group.push(*hole),
            None => groups.push(vec![*hole]),
        }
    }
    for group in &groups {
        let Some(first) = group.first() else { continue };
        let count = group.len();
        let value = 2.0 * first.radius;
        let diameter = crate::drawing::format_value(value);
        // A skewed hole still gets its size note; the UI is told the axis is off a principal
        // direction (usually a modelling slip). No auxiliary view is drawn (D9.1).
        if first.skew_deg > HOLE_SKEW_WARN_DEG && !plan.warnings.iter().any(|w| w.kind == "skew_axis" && w.face == first.face) {
            plan.warnings.push(Warning { kind: "skew_axis", face: first.face, angle_deg: first.skew_deg });
        }
        let targets = vec![
            first.centre + Vec2::new(first.radius, 0.0),
            first.centre + Vec2::new(0.0, first.radius),
            first.centre - Vec2::new(first.radius, 0.0),
            first.centre - Vec2::new(0.0, first.radius),
        ];
        let shown_elsewhere = group.iter().any(|hole| hole_shown_elsewhere(faces, hole.face, all_views, view.view));
        if value < SMALL_HOLE_MM {
            // A hole too small for an in-view diameter gets a leader note naming its size; the
            // note is not dimensioned again (DM-2). The note always names the hole, never just
            // its through state (D7.1 defect 3).
            let state = if first.through { "durchgehend".to_string() } else { format!("x {} tief", depth_text(first.depth)) };
            let text = if count > 1 { format!("{count}x Bohrung Ø{diameter} {state}") } else { format!("Bohrung Ø{diameter} {state}") };
            plan.notes.push(PlannedNote { view: view.view, text, targets, face: Some(first.face) });
            continue;
        }
        // The through state belongs in the diameter text unless another chosen view already
        // shows it with hidden lines (D7.1 defect 3).
        let mut text = if count > 1 { format!("{count}x Ø{diameter}") } else { format!("Ø{diameter}") };
        if first.through {
            if !shown_elsewhere {
                text.push_str(" durchgehend");
            }
        } else {
            text.push_str(&format!(" x {} tief", depth_text(first.depth)));
        }
        let plain = if count > 1 { format!("{count}x Ø{diameter}") } else { format!("Ø{diameter}") };
        let override_text = (count > 1 || text != plain).then(|| text.clone());
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view: view.view,
                geometry: DimensionGeometry::DiameterOnCircle { centre: first.centre, radius: first.radius, at_angle_deg: 45.0 },
                text: override_text,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: None,
            row: None,
            station: None,
            source: AutoDimensionSource { view: view.view, face: first.face, kind: "hole", value, text },
        });
    }

    // Positions from the datum edges when the outline has straight edges at min x and min y.
    // A skewed hole gets no position here: its outline is an ellipse, so both a position and an
    // auxiliary view would be needed (D9.1 draws neither).
    if hole_datum_edges(view) {
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for hole in &holes {
            if hole.skew_deg > HOLE_SKEW_WARN_DEG {
                continue;
            }
            if !xs.iter().any(|x| (x - hole.centre.x).abs() <= 1e-6) {
                xs.push(hole.centre.x);
            }
            if !ys.iter().any(|y| (y - hole.centre.y).abs() <= 1e-6) {
                ys.push(hole.centre.y);
            }
        }
        xs.sort_by(f64::total_cmp);
        ys.sort_by(f64::total_cmp);
        for x in xs {
            let value = (x - x_lo).abs();
            if value <= 1e-9 {
                continue;
            }
            let face = holes.iter().find(|h| (h.centre.x - x).abs() <= 1e-6).map_or(0, |h| h.face);
            let (p1v, p2v) = snap_linear(view, Vec2::new(x_lo, y_lo), Vec2::new(x, y_lo), 0.0);
            plan.dims.push(PlannedDim {
                dimension: Dimension {
                    view: view.view,
                    geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: 0.0, offset_mm: 0.0 },
                    text: None,
                    suffix: None,
                    text_shift_mm: 0.0,
                },
                side: Some(DimSide::Below),
                row: None,
                station: None,
                source: AutoDimensionSource { view: view.view, face, kind: "position", value, text: crate::drawing::format_value(value) },
            });
        }
        for y in ys {
            let value = (y - y_lo).abs();
            if value <= 1e-9 {
                continue;
            }
            let face = holes.iter().find(|h| (h.centre.y - y).abs() <= 1e-6).map_or(0, |h| h.face);
            let (p1v, p2v) = snap_linear(view, Vec2::new(x_lo, y_lo), Vec2::new(x_lo, y), 90.0);
            plan.dims.push(PlannedDim {
                dimension: Dimension {
                    view: view.view,
                    geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: 90.0, offset_mm: 0.0 },
                    text: None,
                    suffix: None,
                    text_shift_mm: 0.0,
                },
                side: Some(DimSide::Left),
                row: None,
                station: None,
                source: AutoDimensionSource { view: view.view, face, kind: "position", value, text: crate::drawing::format_value(value) },
            });
        }
    } else if holes.len() >= 2 {
        // Mostly arcs: the largest hole's centre is the datum; the other centres are measured
        // from it (the crank's centre-to-centre distance).
        let mut largest: Option<Hole> = None;
        for hole in &holes {
            if hole.skew_deg > HOLE_SKEW_WARN_DEG {
                continue;
            }
            if largest.is_none_or(|h| hole.radius > h.radius + 1e-9) {
                largest = Some(*hole);
            }
        }
        if let Some(datum) = largest {
            for hole in &holes {
                if hole.skew_deg > HOLE_SKEW_WARN_DEG || hole.centre.dist(datum.centre) <= 1e-6 {
                    continue;
                }
                let delta = hole.centre - datum.centre;
                let length = delta.len();
                if length <= 1e-9 {
                    continue;
                }
                let (angle, side) = if delta.y.abs() <= 1e-6 {
                    (0.0, Some(DimSide::Above))
                } else if delta.x.abs() <= 1e-6 {
                    (90.0, Some(DimSide::Right))
                } else {
                    (delta.y.atan2(delta.x).to_degrees(), None)
                };
                // Extension lines start on the hole rims, not in mid-air at the centres. Shifting
                // both points along the dimension's normal keeps the measured centre distance.
                let u = Vec2::from_angle(angle.to_radians());
                let n = u.perp();
                let toward = match side {
                    Some(DimSide::Above) | Some(DimSide::Left) | None => 1.0,
                    Some(DimSide::Below) | Some(DimSide::Right) => -1.0,
                };
                let (p1v, p2v) = snap_linear(view, datum.centre + n * (datum.radius * toward), hole.centre + n * (hole.radius * toward), angle);
                plan.dims.push(PlannedDim {
                    dimension: Dimension {
                        view: view.view,
                        geometry: DimensionGeometry::Linear { p1: p1v, p2: p2v, angle_deg: angle, offset_mm: 0.0 },
                        text: None,
                        suffix: None,
                        text_shift_mm: 0.0,
                    },
                    side,
                    row: None,
                    station: None,
                    source: AutoDimensionSource {
                        view: view.view,
                        face: hole.face,
                        kind: "position",
                        value: length,
                        text: crate::drawing::format_value(length),
                    },
                });
            }
        }
    }

    // Outer arcs concentric with a hole: one R per distinct boss radius.
    let mut bosses: Vec<(f64, usize, f64)> = Vec::new(); // radius, face, at_angle
    for face in faces {
        let FaceSurface::Cylinder { origin, axis, radius } = face.surface else { continue };
        if !along_view(view.view, axis) || !(radius > 0.0) {
            continue;
        }
        if face.radial_side(origin, axis) <= 0.0 {
            continue;
        }
        let centre = project(view.view, origin);
        let concentric = holes.iter().any(|hole| hole.centre.dist(centre) <= 1e-4 && hole.radius < radius - 1e-6);
        if !concentric {
            continue;
        }
        bosses.push((radius, face.face, radius_angle(face, centre, radius, view)));
        used_faces.push(face.face);
    }
    bosses.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut boss_groups: Vec<Vec<(f64, usize, f64)>> = Vec::new();
    for boss in bosses {
        match boss_groups.iter_mut().find(|group| group.first().is_some_and(|first| (first.0 - boss.0).abs() <= 1e-6)) {
            Some(group) => group.push(boss),
            None => boss_groups.push(vec![boss]),
        }
    }
    for group in &boss_groups {
        let Some(first) = group.first() else { continue };
        let count = group.len();
        let override_text = (count > 1).then(|| format!("{count}x R{}", crate::drawing::format_value(first.0)));
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view: view.view,
                geometry: DimensionGeometry::Radius { centre: project_center_of(faces, first.1, view.view), radius: first.0, at_angle_deg: first.2 },
                text: override_text,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: None,
            row: None,
            station: None,
            source: AutoDimensionSource {
                view: view.view,
                face: first.1,
                kind: "radius",
                value: first.0,
                text: format!("R{}", crate::drawing::format_value(first.0)),
            },
        });
    }

    // Arcs of the outline: every cylinder the view sees along its axis that covers less than a
    // whole turn is an arc, so it gets R (not Ø) once per distinct radius (D9.1: every arc of a
    // flat part's outline gets its radius).
    let mut fillets: Vec<(f64, usize, f64, Vec2)> = Vec::new();
    for face in faces {
        let FaceSurface::Cylinder { origin, axis, radius } = face.surface else { continue };
        if !along_view(view.view, axis) || !(radius > 0.0) || used_faces.contains(&face.face) {
            continue;
        }
        if face.coverage() >= 2.0 * PI - FULL_TURN_TOL {
            continue;
        }
        let centre = project(view.view, origin);
        fillets.push((radius, face.face, radius_angle(face, centre, radius, view), centre));
    }
    fillets.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut fillet_groups: Vec<Vec<(f64, usize, f64, Vec2)>> = Vec::new();
    for fillet in fillets {
        match fillet_groups.iter_mut().find(|group| group.first().is_some_and(|first| (first.0 - fillet.0).abs() <= 1e-6)) {
            Some(group) => group.push(fillet),
            None => fillet_groups.push(vec![fillet]),
        }
    }
    for group in &fillet_groups {
        let Some(first) = group.first() else { continue };
        let count = group.len();
        let override_text = (count > 1).then(|| format!("{count}x R{}", crate::drawing::format_value(first.0)));
        plan.dims.push(PlannedDim {
            dimension: Dimension {
                view: view.view,
                geometry: DimensionGeometry::Radius { centre: first.3, radius: first.0, at_angle_deg: first.2 },
                text: override_text,
                suffix: None,
                text_shift_mm: 0.0,
            },
            side: None,
            row: None,
            station: None,
            source: AutoDimensionSource {
                view: view.view,
                face: first.1,
                kind: "fillet",
                value: first.0,
                text: format!("R{}", crate::drawing::format_value(first.0)),
            },
        });
    }

    // The centre distance between the two largest distinct arcs of this view (D9.1). It reads as
    // one linear dimension across the two centres, with extension lines so both arrowheads sit on
    // them; a position dimension that already states the distance keeps it out.
    let mut centres: Vec<(f64, usize, Vec2)> =
        fillet_groups.iter().filter_map(|group| group.first().map(|first| (first.0, first.1, first.3))).collect();
    centres.sort_by(|a, b| b.0.total_cmp(&a.0));
    centres.dedup_by(|a, b| a.2.dist(b.2) <= 1e-6);
    if let (Some(first), Some(second)) = (centres.first().copied(), centres.get(1).copied()) {
        let length = second.2.dist(first.2);
        let already = plan.dims.iter().any(|d| d.source.kind == "position" && (d.source.value - length).abs() <= 1e-4);
        if length > 1e-6 && !already {
            let delta = second.2 - first.2;
            let angle = delta.y.atan2(delta.x).to_degrees().rem_euclid(180.0);
            let u = Vec2::from_angle(angle.to_radians());
            let n = u.perp();
            let mid = (first.2 + second.2) * 0.5;
            // Put the line, and the value band above it, where the most free space is: a point
            // in the material's interior has room for the value, one on a hidden hole outline
            // has none (D9.1).
            let clearance = |q: Vec2| {
                view.visible
                    .iter()
                    .chain(&view.hidden)
                    .map(|seg| {
                        let d = seg.b - seg.a;
                        let len2 = d.dot(d);
                        if !(len2 > 1e-18) {
                            return q.dist(seg.a);
                        }
                        let t = ((q - seg.a).dot(d) / len2).clamp(0.0, 1.0);
                        q.dist(seg.a + d * t)
                    })
                    .fold(f64::INFINITY, f64::min)
            };
            let room = |sign: f64| {
                let line = mid + n * (sign * CENTRE_DISTANCE_OFFSET_MM);
                let band = line + n * 2.5;
                clearance(line).min(clearance(band))
            };
            let sign = if room(-1.0) >= room(1.0) { -1.0 } else { 1.0 };
            plan.dims.push(PlannedDim {
                dimension: Dimension {
                    view: view.view,
                    geometry: DimensionGeometry::Linear { p1: first.2, p2: second.2, angle_deg: angle, offset_mm: sign * CENTRE_DISTANCE_OFFSET_MM },
                    text: None,
                    suffix: None,
                    text_shift_mm: 0.0,
                },
                side: None,
                row: None,
                station: None,
                source: AutoDimensionSource {
                    view: view.view,
                    face: second.1,
                    kind: "position",
                    value: length,
                    text: crate::drawing::format_value(length),
                },
            });
        }
    }
}

/// Whether the view has straight visible edges along its min x and min y (the datum edges).
fn hole_datum_edges(view: &ProjectedView) -> bool {
    let [lo, hi] = view.bbox;
    let (w, h) = (hi.x - lo.x, hi.y - lo.y);
    let horizontal = view.visible.iter().any(|s| (s.a.y - lo.y).abs() <= 0.01 && (s.b.y - lo.y).abs() <= 0.01 && (s.a.x - s.b.x).abs() >= w * 0.5);
    let vertical = view.visible.iter().any(|s| (s.a.x - lo.x).abs() <= 0.01 && (s.b.x - lo.x).abs() <= 0.01 && (s.a.y - s.b.y).abs() >= h * 0.3);
    horizontal && vertical
}

/// The mean direction of a cylinder face's points around `centre`, in degrees.
fn mean_angle(face: &SurfaceFace, centre: Vec2, view: StandardView) -> f64 {
    let mut sx = 0.0;
    let mut sy = 0.0;
    for p in &face.points {
        let d = project(view, *p) - centre;
        let Some(unit) = d.normalized() else { continue };
        sx += unit.x;
        sy += unit.y;
    }
    if sx.abs() + sy.abs() <= 1e-9 { 45.0 } else { sy.atan2(sx).to_degrees() }
}

fn project_center_of(faces: &[SurfaceFace], face: usize, view: StandardView) -> Vec2 {
    faces.iter().find(|f| f.face == face).and_then(face_line).map_or(Vec2::ZERO, |(origin, _)| project(view, origin))
}

/// Whether every planned number is finite and within the sheet's range.
pub fn is_sane(plan: &AutoPlan) -> bool {
    plan.dims.iter().all(|d| {
        d.dimension.geometry.is_valid()
            && d.dimension.text_shift_mm.is_finite()
            && d.dimension.text_shift_mm.abs() <= MAX_VALUE_MM
            && d.source.value.is_finite()
            && d.source.value.abs() <= MAX_VALUE_MM
    }) && plan.notes.iter().all(|n| {
        !n.text.trim().is_empty()
            && !n.targets.is_empty()
            && n.targets.iter().all(|t| t.x.is_finite() && t.y.is_finite() && t.x.abs() <= MAX_VALUE_MM && t.y.abs() <= MAX_VALUE_MM)
    })
}

#[cfg(test)]
#[path = "autodim_tests.rs"]
mod tests;
