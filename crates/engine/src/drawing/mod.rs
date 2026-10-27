//! Read-only drawing views of the model: the same vector hidden-line projection that
//! `file.export_view_dxf` writes, for the standard orthographic cameras a drawing sheet places.
//!
//! Coordinates are millimetres in the view's own frame: x to the right and y up as the camera
//! shows them. The camera looks at the world origin, so the origin of every view is the projected
//! world origin (not a fitted target) and the views of one part line up on their shared axes.
//! Every view also carries the centre lines of the model's round features ([`centre`]), and
//! [`choice`] picks the views a sheet shows: an explicit list, a turned part's two, a flat part's
//! two, or front/top/right less repeated drawings. Nothing here changes the document or the undo
//! history.

use std::f64::consts::TAU;

use solidcraft_geom::{Vec2, Vec3};
use solidcraft_render::{Camera, StandardView};

use crate::cmd::view_dxf::{meshes, projection, sketch};
use crate::params::bad;
use crate::{Result, Session};

pub mod autodim;
pub mod centre;
pub mod choice;
pub mod dims;
pub mod sections;
pub mod sheet;
pub use autodim::{AutoDimensionSource, AutoPlan, Undimensioned, Warning, rohmass};
pub use centre::{Axis, ViewCentre, axes, turned_axis, view_centres};
pub use choice::{
    SheetPlan, ViewChoice, ViewSelection, choose_views, drawings_match, end_view, plan_sheet, plan_sheet_auto, plan_sheet_auto_reuse,
    plan_sheet_auto_sections, plan_sheet_auto_sections_tangent, plan_sheet_with, plan_sheet_with_sections, plan_sheet_with_sections_tangent,
    plan_views, plan_views_with,
};
pub use dims::{
    Annotations, DimSide, Dimension, DimensionGeometry, DrawnAnnotations, Note, band_mm, format_value, side_rows, stack,
    validate as validate_annotations,
};
pub use sections::{
    Boundary, CutRegion, CuttingPlane, MAX_SECTIONS, SectionLine, SectionMark, SectionMode, SectionSpec, ViewCut, ViewSection, assigned_letters,
    chain_loops, clip_regions, derive_full_from, hatch_spans, parse_value as parse_sections, plan_cuts, region_narrow, regions, section_mark,
    section_mode_value, spec_value, split_by_boundary,
};
pub use sheet::{
    Arrangement, DEFAULT_EDGE_NOTE, DEFAULT_RA, DRAWING_METADATA, DrawnSection, Layer, LineKind, MAX_EXTRA_NOTES, MAX_NOTE_CHARS,
    NOTES_LINE_PITCH_MM, NOTES_TEXT_HEIGHT_MM, NoteLine, NotesBlock, Paper, Pen, PlacedView, Prim, SECTION_END_MM, SECTION_LETTER_MM, Sheet,
    TextAnchor, TitleBlock, TitleCell, arrangements_for, build_sheet, build_sheet_with, design_names, general_tolerance_note, notes_block_height,
    notes_block_lines, notes_block_rect_for, parse_scale, sheet_to_dxf, sheet_to_pdf, shown, stored_notes, stored_sections, stored_tangent_edges,
    stored_text, stored_title, title_block_cells, title_block_rect_for,
};

/// Command id used in errors from this module.
const CMD: &str = "drawing.views";
/// Most views one request may ask for.
const MAX_VIEWS: usize = 64;
/// Smallest accepted curve chord tolerance (mm), as `file.export_view_dxf` takes it.
pub const MIN_TOLERANCE: f64 = 0.0001;
/// Largest accepted curve chord tolerance (mm).
pub const MAX_TOLERANCE: f64 = 1.0;

/// One projected 2D segment in millimetres, in the view's right/up frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg2mm {
    pub a: Vec2,
    pub b: Vec2,
}

/// An analytic curve of a projected view, in view millimetres: the exact image of a B-rep
/// circle (a circle seen straight on, or an ellipse seen obliquely). The DXF writer emits it as
/// `ARC`/`CIRCLE`/`ELLIPSE` and the PDF as cubic Béziers; the straight chords it replaces stay
/// in [`ProjectedView::visible`] for the measuring code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve2mm {
    /// A circular arc, counter-clockwise from `a0` to `a1` radians; a whole turn is a circle.
    Arc { c: Vec2, r: f64, a0: f64, a1: f64 },
    /// An elliptical arc: the point at parameter `t` is `c + major·cos t + perp(major)·ratio·sin t`.
    Ellipse { c: Vec2, major: Vec2, ratio: f64, t0: f64, t1: f64 },
}

impl Curve2mm {
    /// The point at parameter `t` (radians; a circle's parameter is its angle).
    pub fn point_at(self, t: f64) -> Vec2 {
        match self {
            Curve2mm::Arc { c, r, .. } => c + Vec2::from_angle(t) * r,
            Curve2mm::Ellipse { c, major, ratio, .. } => c + major * t.cos() + major.perp() * (ratio * t.sin()),
        }
    }

    /// The parameter interval `(start, end)`.
    pub fn range(self) -> (f64, f64) {
        match self {
            Curve2mm::Arc { a0, a1, .. } => (a0, a1),
            Curve2mm::Ellipse { t0, t1, .. } => (t0, t1),
        }
    }

    /// A whole circle or ellipse.
    pub fn is_closed(self) -> bool {
        let (a, b) = self.range();
        (b - a - std::f64::consts::TAU).abs() < 1e-6
    }

    /// The painted length in mm (Ramanujan's ellipse approximation for an oblique circle).
    pub fn length(self) -> f64 {
        match self {
            Curve2mm::Arc { r, a0, a1, .. } => r * (a1 - a0).abs(),
            Curve2mm::Ellipse { major, ratio, t0, t1, .. } => {
                let (a, b) = (major.len(), major.len() * ratio);
                let h = if a + b > 0.0 { ((a - b) / (a + b)).powi(2) } else { 0.0 };
                std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt())) * ((t1 - t0).abs() / std::f64::consts::TAU)
            }
        }
    }

    /// `count` points from the start to the end, ends included.
    pub fn sample(self, count: usize) -> Vec<Vec2> {
        let (a, b) = self.range();
        let count = count.max(2);
        (0..count).map(|i| self.point_at(a + (b - a) * i as f64 / (count - 1) as f64)).collect()
    }
}

/// How a sheet draws tangent (G1) edges: the seam where an arc face meets a face it is tangent
/// to. ISO 128 omits them or draws them as thin light edges; a visible thick edge is wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TangentEdges {
    /// Leave tangent edges out (the ISO default).
    #[default]
    Omit,
    /// Draw them thin and continuous on their own `TANGENT` layer.
    Thin,
    /// Draw them like any other visible edge (the projection command's own behaviour).
    Draw,
}

impl TangentEdges {
    /// The value `drawing.sheet` / `drawing.export` accept and report.
    pub const fn name(self) -> &'static str {
        match self {
            TangentEdges::Omit => "omit",
            TangentEdges::Thin => "thin",
            TangentEdges::Draw => "draw",
        }
    }

    /// Parse the parameter value: `omit` or `thin`.
    pub fn parse(value: &str) -> Option<TangentEdges> {
        match value.trim().to_ascii_lowercase().as_str() {
            "omit" => Some(TangentEdges::Omit),
            "thin" => Some(TangentEdges::Thin),
            _ => None,
        }
    }
}

/// What a projection includes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewOptions {
    /// Curve chord tolerance in mm, within [`MIN_TOLERANCE`]..=[`MAX_TOLERANCE`].
    pub tolerance: f64,
    /// Keep the occluded segments in [`ProjectedView::hidden`]. They are always projected, so
    /// they still count in the bounding box when this is false.
    pub hidden: bool,
    /// What to do with tangent (G1) edges.
    pub tangent: TangentEdges,
    /// Replace the projected circles and arcs with their exact `CIRCLE`/`ARC`/`ELLIPSE`
    /// curves. `false` keeps every curve as its chords — the geometry before D10, which the
    /// acceptance test builds to compare the analytic curves against.
    pub curves: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self { tolerance: 0.01, hidden: true, tangent: TangentEdges::Omit, curves: true }
    }
}

/// One standard view of the visible bodies and shown sketches.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedView {
    pub view: StandardView,
    /// Unoccluded edges, in millimetres: every chord, including those of the analytic curves
    /// below. The measuring code reads this list; the sheet draws `visible_plain` and
    /// `visible_curves` instead, which are the same geometry without drawing the chords twice.
    pub visible: Vec<Seg2mm>,
    /// Edges behind material, in millimetres (empty unless [`ViewOptions::hidden`]).
    pub hidden: Vec<Seg2mm>,
    /// The visible edges that are not part of an analytic curve.
    pub visible_plain: Vec<Seg2mm>,
    /// The exact projected circles and arcs of the visible edges.
    pub visible_curves: Vec<Curve2mm>,
    /// The hidden edges that are not part of an analytic curve.
    pub hidden_plain: Vec<Seg2mm>,
    /// The exact projected circles and arcs of the hidden edges.
    pub hidden_curves: Vec<Curve2mm>,
    /// Visible tangent (G1) seams, drawn thin on their own layer when the sheet asks for `thin`.
    pub tangent: Vec<Seg2mm>,
    /// `[[min x, min y], [max x, max y]]` in mm; zeros when nothing is drawn.
    pub bbox: [Vec2; 2],
    /// The orthographic camera the view was projected with.
    pub camera: Camera,
    /// The centre lines of round features in this view's own millimetres (see
    /// [`crate::drawing::centre`]).
    pub centre: Vec<ViewCentre>,
    /// The cut geometry when the view is sectioned (D9): the cut-outline loops and, for a
    /// break-out, the freehand boundary. `visible` and `hidden` already carry the sectioned
    /// edges; this is what the hatching and the cut outline are built from.
    pub section: Option<ViewSection>,
    /// The section marks a full section of another view draws in this end view.
    pub section_marks: Vec<SectionMark>,
}

/// The name `drawing.views` returns for a view and accepts in its `views` parameter.
pub fn view_name(view: StandardView) -> &'static str {
    match view {
        StandardView::Front => "front",
        StandardView::Back => "back",
        StandardView::Top => "top",
        StandardView::Bottom => "bottom",
        StandardView::Left => "left",
        StandardView::Right => "right",
        StandardView::Iso => "iso",
    }
}

/// The orthographic camera of a standard view, aimed at the world origin so the view's
/// coordinates are the model's own projected onto that view's right/up axes. The distance is
/// fixed (orthographic projection does not depend on it) and the field of view is 0.
pub fn standard_camera(view: StandardView) -> Camera {
    let (yaw, pitch) = view.angles();
    Camera { target: Vec3::ZERO, yaw, pitch, distance: 1000.0, fov: 0.0, ..Camera::default() }
}

/// Project the visible bodies and shown sketch profile curves into `views`, reusing the export's
/// mesh preparation and vector hidden-line removal. Read-only: the session is not changed.
pub fn project_views(s: &Session, views: &[StandardView], opts: &ViewOptions) -> Result<Vec<ProjectedView>> {
    project_views_cut(s, views, opts, &[])
}

/// Two parallel edges whose perpendicular distance is at most this are one line on paper (mm):
/// half the chord tolerance, far below what a plot can show.
const PARALLEL_MERGE_MM: f64 = 0.005;
/// Shorter fragments are not drawn: they are below the 0.01 mm chord tolerance and invisible.
const MIN_SEGMENT_MM: f64 = 0.01;

/// Merge collinear fragments and drop sub-tolerance slivers, with visible geometry taking
/// precedence over hidden geometry. The hidden-line pass splits a curved edge chord by chord and
/// a tessellation seam can hand the same line many times at slightly different offsets; without
/// this pass a cylinder seen from the side is thousands of micrometre-long LINEs.
fn merge_segments(raw: &[(Seg2mm, bool, bool)]) -> Vec<(Seg2mm, bool, bool)> {
    struct Group {
        origin: Vec2,
        direction: Vec2,
        offset: f64,
        tangent: bool,
        intervals: Vec<(f64, f64, bool)>,
    }
    let key = |v: f64| (v / PARALLEL_MERGE_MM).round() as i64;
    let mut groups: std::collections::BTreeMap<(i64, i64, i64, bool), Vec<usize>> = std::collections::BTreeMap::new();
    let mut list: Vec<Group> = Vec::new();
    for (seg, hidden, tangent) in raw {
        let delta = seg.b - seg.a;
        let length = delta.len();
        if !(length.is_finite() && length > 0.0) {
            continue;
        }
        let mut direction = delta * (1.0 / length);
        if (direction.x.abs() > 1e-12 && direction.x < 0.0) || (direction.x.abs() <= 1e-12 && direction.y < 0.0) {
            direction = -direction;
        }
        let offset = direction.cross(seg.a);
        // Look in the neighbouring cells: a line exactly on a cell boundary would otherwise
        // start a second group.
        let (kx, ky, ko) = (key(direction.x * 1e6), key(direction.y * 1e6), key(offset));
        let mut found: Option<(usize, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dk in -1..=1 {
                    for index in groups.get(&(kx + dx, ky + dy, ko + dk, *tangent)).into_iter().flatten() {
                        let Some(group) = list.get(*index) else { continue };
                        let difference = (group.offset - offset).abs();
                        if group.direction.dot(direction) > 1.0 - 1e-7
                            && difference <= PARALLEL_MERGE_MM
                            && found.is_none_or(|(_, best)| difference < best)
                        {
                            found = Some((*index, difference));
                        }
                    }
                }
            }
        }
        let index = match found {
            Some((index, _)) => index,
            None => {
                let index = list.len();
                list.push(Group { origin: seg.a, direction, offset, tangent: *tangent, intervals: Vec::new() });
                groups.entry((kx, ky, ko, *tangent)).or_default().push(index);
                index
            }
        };
        let Some(group) = list.get_mut(index) else { continue };
        let (t0, t1) = ((seg.a - group.origin).dot(group.direction), (seg.b - group.origin).dot(group.direction));
        group.intervals.push((t0.min(t1), t0.max(t1), *hidden));
    }
    let merge = |mut spans: Vec<(f64, f64)>| {
        spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut out: Vec<(f64, f64)> = Vec::new();
        for (a, b) in spans {
            if let Some(last) = out.last_mut().filter(|last| a <= last.1 + 1e-6) {
                last.1 = last.1.max(b);
            } else {
                out.push((a, b));
            }
        }
        out
    };
    let mut result = Vec::new();
    for group in list {
        let visible = merge(group.intervals.iter().filter(|(.., hidden)| !hidden).map(|(a, b, _)| (*a, *b)).collect());
        let hidden = merge(group.intervals.iter().filter(|(.., hidden)| *hidden).map(|(a, b, _)| (*a, *b)).collect());
        let mut emit = |a: f64, b: f64, hidden: bool| {
            if b - a >= MIN_SEGMENT_MM {
                let seg = Seg2mm { a: group.origin + group.direction * a, b: group.origin + group.direction * b };
                result.push((seg, hidden, group.tangent));
            }
        };
        for (a, b) in &visible {
            emit(*a, *b, false);
        }
        // Hidden parts under a visible line are not drawn at all.
        for (a, b) in &hidden {
            let mut cursor = *a;
            for (va, vb) in &visible {
                if *vb <= cursor || *va >= *b {
                    continue;
                }
                if *va > cursor {
                    emit(cursor, *va, true);
                }
                cursor = cursor.max(*vb);
            }
            if cursor < *b {
                emit(cursor, *b, true);
            }
        }
    }
    result
}

/// What one projection hands over: the visible and hidden edges, the visible tangent (light)
/// edges, and the bounding box of everything drawn.
struct ProjectedSegments {
    visible: Vec<Seg2mm>,
    hidden: Vec<Seg2mm>,
    tangent: Vec<Seg2mm>,
    bbox: [Vec2; 2],
}

/// The projected edges of one [`projection::Drawing`], with the bounding box of everything drawn.
/// A tangent edge is a light edge: it is never a visible outline and only a hidden line while
/// nothing covers it, so a hidden tangent seam is left out entirely.
fn drawing_segments(drawing: &projection::Drawing, hidden: bool) -> Result<ProjectedSegments> {
    let mut min = Vec2::new(f64::INFINITY, f64::INFINITY);
    let mut max = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut raw: Vec<(Seg2mm, bool, bool)> = Vec::new();
    let mut include = |a: Vec2, b: Vec2, is_hidden: bool, is_tangent: bool| -> Result<()> {
        if !(a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite()) {
            return Err(bad(CMD, "projected coordinates are not finite"));
        }
        min = Vec2::new(min.x.min(a.x).min(b.x), min.y.min(a.y).min(b.y));
        max = Vec2::new(max.x.max(a.x).max(b.x), max.y.max(a.y).max(b.y));
        raw.push((Seg2mm { a, b }, is_hidden, is_tangent));
        Ok(())
    };
    for line in &drawing.lines {
        include(line.a, line.b, line.hidden, line.tangent)?;
    }
    for path in &drawing.sketch {
        for pair in path.points.windows(2) {
            if let (Some(a), Some(b)) = (pair.first(), pair.get(1)) {
                include(*a, *b, path.hidden, false)?;
            }
        }
    }
    let mut visible = Vec::new();
    let mut occluded = Vec::new();
    let mut tangent = Vec::new();
    for (seg, is_hidden, is_tangent) in merge_segments(&raw) {
        match (is_hidden, is_tangent) {
            (false, true) => tangent.push(seg),
            (false, false) => visible.push(seg),
            (true, true) => {}
            (true, false) if hidden => occluded.push(seg),
            _ => {}
        }
    }
    let bbox = if min.x.is_finite() && min.y.is_finite() && max.x.is_finite() && max.y.is_finite() { [min, max] } else { [Vec2::ZERO, Vec2::ZERO] };
    Ok(ProjectedSegments { visible, hidden: occluded, tangent, bbox })
}

/// One analytic curve of a view, projected from an exact B-rep circle. `lo..hi` is the part of
/// the ellipse parameter that the edge spans; `major` is a semi-axis (the longer one), `ratio`
/// the minor over the major.
struct ProjectedCurve {
    c: Vec2,
    major: Vec2,
    ratio: f64,
    lo: f64,
    hi: f64,
}

impl ProjectedCurve {
    fn point(&self, t: f64) -> Vec2 {
        self.c + self.major * t.cos() + self.major.perp() * (self.ratio * t.sin())
    }

    /// The ellipse parameter of a point near the ellipse.
    fn parameter(&self, p: Vec2) -> f64 {
        let q = p - self.c;
        let minor = self.major.perp() * self.ratio;
        let (a2, b2) = (self.major.dot(self.major), minor.dot(minor));
        if !(a2 > 0.0 && b2 > 0.0) {
            return 0.0;
        }
        (q.dot(minor) / b2).atan2(q.dot(self.major) / a2)
    }

    /// The tangent direction at parameter `t` (not normalised).
    fn tangent(&self, t: f64) -> Vec2 {
        let (sin, cos) = t.sin_cos();
        -self.major * sin + self.major.perp() * (self.ratio * cos)
    }

    /// The parameter `t` brought into `lo..=hi`, if it is on the edge's part of the ellipse.
    fn within(&self, t: f64) -> Option<f64> {
        let span = self.hi - self.lo;
        let u = (t - self.lo).rem_euclid(TAU);
        (u <= span + 1e-9).then_some(self.lo + u)
    }
}

/// Project every exact circle edge of `scene` into `camera`'s view. A circle seen straight on
/// gives a circle, one seen obliquely an ellipse; the edge's start, middle and end fix which part
/// of the ellipse it spans.
fn projected_curves(scene: &meshes::Scene, camera: &Camera) -> Vec<ProjectedCurve> {
    let (right, up, _) = camera.basis();
    let to_view = |p: Vec3| Vec2::new((p - camera.target).dot(right), (p - camera.target).dot(up));
    let mut out: Vec<ProjectedCurve> = Vec::new();
    for placed in &scene.placements {
        let Some(circles) = scene.curves.get(placed.mesh) else { continue };
        for (index, circle) in circles.iter().enumerate() {
            let Some(circle) = circle else { continue };
            if !placed.visibility.get(index).copied().unwrap_or(false) {
                continue;
            }
            let transform = &placed.transform;
            let centre = solidcraft_doc::apply_point(transform, circle.center);
            let start = solidcraft_doc::apply_point(transform, circle.start);
            let axis = solidcraft_doc::apply_vector(transform, Vec3::new(circle.axis.x, circle.axis.y, circle.axis.z));
            let Some(x) = (start - centre).normalized() else { continue };
            if !(circle.radius.is_finite() && circle.radius > 0.0) {
                continue;
            }
            let y = axis.cross(x);
            let radius = circle.radius;
            let point = |theta: f64| centre + x * (radius * theta.cos()) + y * (radius * theta.sin());
            // The projected circle is `c + u·cos θ + v·sin θ` with these conjugate semi-diameters.
            let at = |p: Vec3| to_view(p);
            let u = at(centre + x * radius) - at(centre);
            let v = at(centre + y * radius) - at(centre);
            let (uu, vv, uv) = (u.dot(u), v.dot(v), u.dot(v));
            if !(uu > 0.0 && vv > 0.0) || ((uu * vv - uv * uv) / (uu * vv)).abs() < 1e-10 {
                // Seen edge-on: the edge is a straight line, and the chords stay LINEs.
                continue;
            }
            let mut turn = 0.5 * (2.0 * uv).atan2(uu - vv);
            let (mut major, mut minor) = (u * turn.cos() + v * turn.sin(), v * turn.cos() - u * turn.sin());
            if major.len() < minor.len() {
                turn += std::f64::consts::FRAC_PI_2;
                major = u * turn.cos() + v * turn.sin();
                minor = v * turn.cos() - u * turn.sin();
            }
            if !(major.len() > 1e-12) {
                continue;
            }
            let ratio = (minor.len() / major.len()).clamp(1e-12, 1.0);
            let c = to_view(centre);
            // A circle's major-axis direction is arbitrary (it depends on the edge's start
            // vertex), so canonicalise it: the same circle reached from two edges must have one
            // parameter, or their runs would be merged in two different angles.
            let radius = major.len();
            let major = if (ratio - 1.0).abs() < 1e-9 {
                Vec2::new(radius, 0.0)
            } else if major.x < -1e-12 || (major.x.abs() <= 1e-12 && major.y < 0.0) {
                -major
            } else {
                major
            };
            let ratio = if (ratio - 1.0).abs() < 1e-9 { 1.0 } else { ratio };
            let closed = circle.angle.abs() >= TAU - 1e-9;
            let end = if closed { circle.start } else { circle.end };
            let middle = point(circle.angle * 0.5);
            let probe = ProjectedCurve { c, major, ratio, lo: 0.0, hi: TAU };
            let (f0, f1, fm) = (probe.parameter(at(start)), probe.parameter(at(end)), probe.parameter(at(middle)));
            let (d01, d0m) = ((f1 - f0).rem_euclid(TAU), (fm - f0).rem_euclid(TAU));
            // A whole circle is the same curve wherever its B-rep edge's vertex happens to be:
            // give every closed circle the same parameter range so coincident circles (the two
            // ends of a cylinder, a shoulder and a front edge) become one drawing curve.
            let (lo, hi) = if closed {
                (0.0, TAU)
            } else if d0m <= d01 + 1e-9 {
                (f0, f0 + d01)
            } else {
                (f1, f1 + (TAU - d01))
            };
            out.push(ProjectedCurve { c, major, ratio, lo, hi });
        }
    }
    out
}

/// Split a chord list into the chords no analytic curve supersedes and the merged analytic runs.
/// A chord belongs to a curve when both ends lie on it, both parameters are inside the edge's
/// span, and its direction is the ellipse's tangent there (a chord of at most a 20° facet).
fn split_analytic(
    visible: &[Seg2mm],
    hidden: &[Seg2mm],
    curves: &[ProjectedCurve],
    tolerance: f64,
) -> (Vec<Seg2mm>, Vec<Curve2mm>, Vec<Seg2mm>, Vec<Curve2mm>) {
    // A chord of the display mesh lies inside the true curve by its sagitta, which the chord
    // tolerance bounds; the hidden-line pass then cuts chords at arbitrary places, so a
    // point on a chord can be that far from the exact circle.
    let point_tolerance = (if tolerance.is_finite() { tolerance } else { 0.01 }).max(1e-5) * 1.5;
    let mut tagged: Vec<(usize, f64, f64, bool)> = Vec::new();
    let mut plain_visible = Vec::new();
    let mut plain_hidden = Vec::new();
    for (segments, is_hidden) in [(visible, false), (hidden, true)] {
        let plain = if is_hidden { &mut plain_hidden } else { &mut plain_visible };
        for seg in segments {
            let chord = seg.b - seg.a;
            let length = chord.len();
            if !(length > 1e-9) {
                continue;
            }
            let mut best: Option<(f64, usize, f64, f64)> = None;
            for (ci, curve) in curves.iter().enumerate() {
                let (Some(t0), Some(t1)) = (curve.within(curve.parameter(seg.a)), curve.within(curve.parameter(seg.b))) else { continue };
                // Both ends must lie on the ellipse, not merely have a parameter in range.
                let (p0, p1) = (curve.point(t0), curve.point(t1));
                let error = p0.dist(seg.a).max(p1.dist(seg.b));
                if error > point_tolerance {
                    continue;
                }
                // The HLR's own orientation is arbitrary: a chord may run against the curve.
                let span = (t1 - t0).abs();
                if !(span > 0.0 && span <= std::f64::consts::FRAC_PI_2) {
                    continue;
                }
                // A chord of the curve lies inside it; a straight edge *tangent* to the circle
                // has the same direction at the touch point and both ends within the tolerance,
                // but its midpoint is outside. That distinction keeps a tangent line a LINE.
                let mid = (seg.a + seg.b) * 0.5;
                let minor = curve.major.perp() * curve.ratio;
                let q = mid - curve.c;
                let (a2, b2) = (curve.major.dot(curve.major), minor.dot(minor));
                let inside = q.dot(curve.major).powi(2) / (a2 * a2) + q.dot(minor).powi(2) / (b2 * b2);
                if !(inside <= 1.0 + 1e-6) {
                    continue;
                }
                let middle = (t0 + t1) * 0.5;
                let tangent = curve.tangent(middle);
                if tangent.len() < 1e-12 || tangent.dot(chord).abs() / (tangent.len() * length) < 20.0_f64.to_radians().cos() {
                    continue;
                }
                if best.is_none_or(|(e, ..)| error < e) {
                    best = Some((error, ci, t0, t1));
                }
            }
            match best {
                Some((_, ci, t0, t1)) => tagged.push((ci, t0, t1, is_hidden)),
                None => plain.push(*seg),
            }
        }
    }
    // Merge the runs by the curve's geometry, not by its B-rep edge: a circle is often three
    // 120° edges of one surface, the two ends of a cylinder project onto the same circle, and
    // the runs of a clipped edge arrive twice. Each run is kept in the curve's own parameter,
    // which is the same for every edge on one ellipse, so complementary arcs join into the
    // drawing's one arc, and a visible run hides a hidden one on the same geometry.
    struct Geometry {
        c: Vec2,
        major: Vec2,
        ratio: f64,
        visible: Vec<(f64, f64)>,
        hidden: Vec<(f64, f64)>,
    }
    let mut geometries: std::collections::BTreeMap<[i64; 5], usize> = std::collections::BTreeMap::new();
    let mut list: Vec<Geometry> = Vec::new();
    // A thousandth of a millimetre and a millionth of a ratio are far below the drawing
    // tolerance, and keep circles computed from two edges in one group.
    let quantise = |v: f64| (v * 1e3).round() as i64;
    for (ci, t0, t1, is_hidden) in tagged {
        let Some(curve) = curves.get(ci) else { continue };
        let key = [quantise(curve.c.x), quantise(curve.c.y), quantise(curve.major.x), quantise(curve.major.y), (curve.ratio * 1e6).round() as i64];
        let index = *geometries.entry(key).or_insert_with(|| {
            list.push(Geometry { c: curve.c, major: curve.major, ratio: curve.ratio, visible: Vec::new(), hidden: Vec::new() });
            list.len() - 1
        });
        let Some(geometry) = list.get_mut(index) else { continue };
        // Keep the chord's span; it is at most a 90° facet, and a run that crosses 2π is split
        // at the seam below. The HLR's orientation is arbitrary, so the interval starts at the
        // smaller parameter — not always at `t0`.
        let span = (t1 - t0).abs();
        let start = t0.min(t1).rem_euclid(TAU);
        if is_hidden {
            geometry.hidden.push((start, start + span));
        } else {
            geometry.visible.push((start, start + span));
        }
    }
    // A sliver dropped at a facet joint leaves a gap far below print resolution: join runs
    // whose gap is at most 0.05 mm of arc.
    let merge_runs = |runs: Vec<(f64, f64)>, max_gap: f64| {
        // Split a run at the 2π seam, merge, then join the two halves that meet there.
        let mut split: Vec<(f64, f64)> = Vec::new();
        for (a, b) in runs {
            if b > TAU {
                split.push((a, TAU));
                split.push((0.0, b - TAU));
            } else {
                split.push((a, b));
            }
        }
        split.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (a, b) in split {
            if let Some(last) = merged.last_mut().filter(|last: &&mut (f64, f64)| a <= last.1 + max_gap) {
                last.1 = last.1.max(b);
            } else {
                merged.push((a, b));
            }
        }
        if merged.len() > 1
            && merged.first().is_some_and(|first| first.0 <= 1e-9)
            && merged.last().is_some_and(|last| last.1 >= TAU - 1e-9)
            && let Some(combined) = merged.last().map(|last| (last.0, merged[0].1 + TAU))
        {
            merged.remove(0);
            if let Some(last) = merged.last_mut() {
                *last = combined;
            }
        }
        merged
    };
    let mut visible_curves = Vec::new();
    let mut hidden_curves = Vec::new();
    for geometry in list {
        let max_gap = 0.05 / geometry.major.len().max(1e-6);
        let visible = merge_runs(geometry.visible, max_gap);
        let hidden = merge_runs(geometry.hidden, max_gap);
        let emit = |runs: &[(f64, f64)], out: &mut Vec<Curve2mm>| {
            for (a, b) in runs {
                let sweep = b - *a;
                if !(sweep > 1e-9) {
                    continue;
                }
                let full = sweep >= TAU - 1e-6;
                if (geometry.ratio - 1.0).abs() < 1e-9 {
                    out.push(Curve2mm::Arc { c: geometry.c, r: geometry.major.len(), a0: *a, a1: if full { a + TAU } else { *b } });
                } else {
                    out.push(Curve2mm::Ellipse {
                        c: geometry.c,
                        major: geometry.major,
                        ratio: geometry.ratio,
                        t0: *a,
                        t1: if full { a + TAU } else { *b },
                    });
                }
            }
        };
        emit(&visible, &mut visible_curves);
        // Hidden parts under a visible arc are not drawn at all.
        let mut remaining: Vec<(f64, f64)> = Vec::new();
        for (a, b) in &hidden {
            let mut cursor = *a;
            for (va, vb) in &visible {
                if *vb <= cursor || *va >= *b {
                    continue;
                }
                if *va > cursor {
                    remaining.push((cursor, *va));
                }
                cursor = cursor.max(*vb);
            }
            if cursor < *b {
                remaining.push((cursor, *b));
            }
        }
        emit(&remaining, &mut hidden_curves);
    }
    (plain_visible, visible_curves, plain_hidden, hidden_curves)
}

/// Fit circular arcs to chains of chords that have no exact B-rep curve behind them: a curved
/// silhouette and the outline of a break-out both come out of the hidden-line projection as
/// polylines. Only chains whose every vertex lies within the chord tolerance of one circle are
/// converted, and only when 32 points of the fitted arc stay within [`MIN_SEGMENT_MM`] of the
/// chain; everything else is left as the lines it was.
fn fit_arcs(segments: &[Seg2mm], reference: &[Seg2mm], tolerance: f64) -> (Vec<Seg2mm>, Vec<Curve2mm>) {
    const JOIN_MM: f64 = 0.01;
    let fit_tolerance = (if tolerance.is_finite() { tolerance } else { 0.01 }).max(1e-5) * 0.5;
    let n = segments.len();
    if n < 4 {
        return (segments.to_vec(), Vec::new());
    }
    // Endpoint adjacency with a tolerance: a chord joint from two HLR pieces can differ by
    // float noise, but is one point of the chain.
    let cell = |v: f64| (v / JOIN_MM).floor() as i64;
    let mut grid: std::collections::BTreeMap<(i64, i64), Vec<(usize, bool)>> = std::collections::BTreeMap::new();
    for (i, seg) in segments.iter().enumerate() {
        grid.entry((cell(seg.a.x), cell(seg.a.y))).or_default().push((i, true));
        grid.entry((cell(seg.b.x), cell(seg.b.y))).or_default().push((i, false));
    }
    let near = |p: Vec2| -> Vec<(usize, bool)> {
        let (cx, cy) = (cell(p.x), cell(p.y));
        let mut out = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &(index, at_start) in grid.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    let Some(seg) = segments.get(index) else { continue };
                    let endpoint = if at_start { seg.a } else { seg.b };
                    if endpoint.dist(p) <= JOIN_MM {
                        out.push((index, at_start));
                    }
                }
            }
        }
        out
    };
    let mut used = vec![false; n];
    let mut chains: Vec<Vec<Vec2>> = Vec::new();
    for start in 0..n {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut points = vec![segments[start].a, segments[start].b];
        // Walk both ways; a junction (two or more unused continuations) ends the chain.
        for forward in [true, false] {
            loop {
                let at = if forward { *points.last().unwrap_or(&Vec2::ZERO) } else { *points.first().unwrap_or(&Vec2::ZERO) };
                let candidates: Vec<(usize, bool)> = near(at).into_iter().filter(|(index, _)| !used.get(*index).copied().unwrap_or(true)).collect();
                let [(index, at_start)] = candidates.as_slice() else { break };
                let Some(seg) = segments.get(*index) else { break };
                if let Some(slot) = used.get_mut(*index) {
                    *slot = true;
                }
                // The new endpoint is the one not at `at`.
                let next = if *at_start { seg.b } else { seg.a };
                if forward {
                    points.push(next);
                } else {
                    points.insert(0, next);
                }
            }
        }
        if points.len() >= 4 {
            chains.push(points);
        }
    }
    let mut plain = Vec::new();
    let mut arcs = Vec::new();
    let mut consumed = vec![false; n];
    for chain in &chains {
        let fitted = fit_chain(chain, fit_tolerance, reference).map(Fitted::Arc).or_else(|| fit_line(chain, fit_tolerance).map(Fitted::Line));
        if let Some(fitted) = fitted {
            match fitted {
                Fitted::Line(seg) => plain.push(seg),
                Fitted::Arc(arc) => arcs.push(arc),
            }
            // Mark the chain's segments as consumed: find them by endpoints along the chain.
            for (i, seg) in segments.iter().enumerate() {
                if chain.iter().any(|p| p.dist(seg.a) <= JOIN_MM) && chain.iter().any(|p| p.dist(seg.b) <= JOIN_MM) {
                    consumed[i] = true;
                }
            }
        }
    }
    for (i, seg) in segments.iter().enumerate() {
        if !consumed[i] {
            plain.push(*seg);
        }
    }
    (plain, arcs)
}

/// The distance from `p` to the segment `a`→`b`.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.len2();
    if !(len2 > 0.0) {
        return p.dist(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.dist(a + d * t)
}

enum Fitted {
    Line(Seg2mm),
    Arc(Curve2mm),
}

/// Debris shorter than this (mm) is clustered onto one supporting line when it fits.
const DEBRIS_MM: f64 = 1.0;

/// A tessellated cylinder's silhouette is a zigzag of tenth-of-a-millimetre edges around the true
/// straight line, and its joints form a network that a chain walk cannot follow. Cluster such
/// short fragments by their supporting line and replace each cluster that really is one line
/// (every endpoint within `tolerance` of it) by that line. Anything else stays as it was.
fn cluster_debris(segments: &[Seg2mm], reference: &[Seg2mm], tolerance: f64) -> (Vec<Seg2mm>, Vec<Curve2mm>) {
    struct Cluster {
        direction: Vec2,
        offset: f64,
        members: Vec<usize>,
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut short = Vec::new();
    let mut long = Vec::new();
    for (index, seg) in segments.iter().enumerate() {
        if seg.a.dist(seg.b) >= DEBRIS_MM {
            long.push(*seg);
            continue;
        }
        short.push(index);
        let delta = seg.b - seg.a;
        let Some(mut direction) = delta.normalized() else { continue };
        if (direction.x.abs() > 1e-12 && direction.x < 0.0) || (direction.x.abs() <= 1e-12 && direction.y < 0.0) {
            direction = -direction;
        }
        let offset = direction.cross(seg.a);
        match clusters
            .iter_mut()
            .find(|cluster| cluster.direction.dot(direction) > 5.0_f64.to_radians().cos() && (cluster.offset - offset).abs() <= 0.03)
        {
            Some(cluster) => cluster.members.push(index),
            None => clusters.push(Cluster { direction, offset, members: vec![index] }),
        }
    }
    let mut out = long;
    let mut arcs: Vec<Curve2mm> = Vec::new();
    for cluster in &clusters {
        let keep = |out: &mut Vec<Seg2mm>| {
            for index in &cluster.members {
                if let Some(seg) = segments.get(*index) {
                    out.push(*seg);
                }
            }
        };
        if cluster.members.len() < 2 {
            keep(&mut out);
            continue;
        }
        // The longest fragment gives the axis; then every endpoint must lie on one line.
        let Some(mut direction) = cluster
            .members
            .iter()
            .filter_map(|i| segments.get(*i))
            .max_by(|a, b| a.a.dist(a.b).total_cmp(&b.a.dist(b.b)))
            .and_then(|seg| (seg.b - seg.a).normalized())
        else {
            keep(&mut out);
            continue;
        };
        if (direction.x.abs() > 1e-12 && direction.x < 0.0) || (direction.x.abs() <= 1e-12 && direction.y < 0.0) {
            direction = -direction;
        }
        let mut points: Vec<Vec2> = cluster.members.iter().filter_map(|i| segments.get(*i)).flat_map(|s| [s.a, s.b]).collect();
        // A bowed cluster is a shallow arc, not a line: try the circle first, on points ordered
        // along the cluster.
        let origin = points.first().copied().unwrap_or(Vec2::ZERO);
        points.sort_unstable_by(|a, b| (*a - origin).dot(direction).total_cmp(&(*b - origin).dot(direction)));
        if let Some(arc) = fit_chain(&points, tolerance.min(0.005), reference) {
            arcs.push(arc);
            continue;
        }
        let mean = points.iter().fold(Vec2::ZERO, |sum, p| sum + *p) * (1.0 / points.len().max(1) as f64);
        if points.iter().any(|p| (*p - mean).cross(direction).abs() > tolerance) {
            keep(&mut out);
            continue;
        }
        let mut spans: Vec<(f64, f64)> = cluster
            .members
            .iter()
            .filter_map(|i| segments.get(*i))
            .map(|seg| {
                let (a, b) = ((seg.a - mean).dot(direction), (seg.b - mean).dot(direction));
                (a.min(b), a.max(b))
            })
            .collect();
        spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (a, b) in spans {
            if let Some(last) = merged.last_mut().filter(|last| a <= last.1 + 0.05) {
                last.1 = last.1.max(b);
            } else {
                merged.push((a, b));
            }
        }
        for (a, b) in merged {
            if b - a >= MIN_SEGMENT_MM {
                out.push(Seg2mm { a: mean + direction * a, b: mean + direction * b });
            }
        }
    }
    (out, arcs)
}

/// The straight line a chain of noisy silhouette pieces lies on, if every point is within
/// `tolerance` of one line. The hidden-line pass builds a cylinder's straight silhouette from
/// tessellation edges whose directions wobble by degrees at a tenth of a millimetre.
fn fit_line(points: &[Vec2], tolerance: f64) -> Option<Seg2mm> {
    let first = *points.first()?;
    let last = *points.last()?;
    let d = last - first;
    let length = d.len();
    if !(length > 0.05) {
        return None;
    }
    let u = d * (1.0 / length);
    if points.iter().any(|p| (*p - first).cross(u).abs() > tolerance) {
        return None;
    }
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in points {
        let t = (*p - first).dot(u);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    (hi - lo >= length.min(0.05)).then(|| Seg2mm { a: first + u * lo, b: first + u * hi })
}

/// The least-squares circle through `points` (Kåsa's algebraic fit): `x² + y² = 2a·x + 2b·y + c`.
fn circle_least_squares(points: &[Vec2]) -> Option<(Vec2, f64)> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let (mut sx, mut sy, mut sxx, mut sxy, mut syy, mut sxz, mut syz, mut sz) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in points {
        let z = p.dot(*p);
        sx += p.x;
        sy += p.y;
        sxx += p.x * p.x;
        sxy += p.x * p.y;
        syy += p.y * p.y;
        sxz += p.x * z;
        syz += p.y * z;
        sz += z;
    }
    let n = n as f64;
    // Normal equations for (2a, 2b, c).
    let m = [[sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n]];
    let rhs = [sxz, syz, sz];
    let det = |a: [[f64; 3]; 3]| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det(m);
    if !(d.is_finite() && d.abs() > 1e-12) {
        return None;
    }
    let column = |index: usize| {
        let mut a = m;
        for row in 0..3 {
            a[row][index] = rhs[row];
        }
        det(a)
    };
    let (a, b, c) = (column(0) / d, column(1) / d, column(2) / d);
    let centre = Vec2::new(a, b);
    let radius2 = c + a * a + b * b;
    (radius2.is_finite() && radius2 > 0.0).then(|| (centre, radius2.sqrt()))
}

/// The circle of a chain and its arc, if the chain is a circular arc within `tolerance` of its
/// fitted circle and its chords.
fn fit_chain(points: &[Vec2], tolerance: f64, reference: &[Seg2mm]) -> Option<Curve2mm> {
    let n = points.len();
    let last = n.checked_sub(1)?;
    let closed = n > 3 && points[0].dist(points[last]) <= 0.01;
    // Three well-spread points: a closed chain would otherwise fit through its own start twice.
    let (a, b, c) = if closed {
        (*points.first()?, *points.get(n / 3)?, *points.get(2 * n / 3)?)
    } else {
        (*points.first()?, *points.get(n / 2)?, *points.get(last)?)
    };
    // The three spread points must not be collinear; the circle itself is a least-squares fit
    // over every point of the chain, which centres the residuals better than three of them.
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    let scale = (b - a).len() * (c - a).len();
    if !(scale > 0.0) || d.abs() <= scale * 1e-9 {
        return None;
    }
    let (centre, radius) = circle_least_squares(points)?;
    if !(radius.is_finite() && radius > 0.05) {
        return None;
    }
    if points.iter().any(|p| (p.dist(centre) - radius).abs() > tolerance) {
        return None;
    }
    // Angles must run monotonically along the chain (one direction only).
    let angles: Vec<f64> = points.iter().map(|p| (p.y - centre.y).atan2(p.x - centre.x)).collect();
    let mut unwrapped = Vec::with_capacity(n);
    let mut turn = 0.0;
    let mut previous = *angles.first()?;
    let mut sign = 0.0_f64;
    unwrapped.push(previous);
    for angle in angles.iter().skip(1) {
        let mut delta = angle - previous;
        if delta > std::f64::consts::PI {
            delta -= TAU;
        } else if delta < -std::f64::consts::PI {
            delta += TAU;
        }
        // A chain that turns back on itself (or jumps from an arc to a tangent fragment) is
        // not one arc, even when every point happens to be near the circle.
        if delta.abs() > 30.0_f64.to_radians() {
            return None;
        }
        if delta.abs() > 1e-6 {
            if sign != 0.0 && delta.signum() != sign {
                return None;
            }
            sign = delta.signum();
        }
        turn += delta;
        previous += delta;
        unwrapped.push(previous);
    }
    if turn.abs() < 0.02 || turn.abs() > TAU + 0.2 {
        return None;
    }
    let (start, end) = if closed {
        let start = unwrapped[0].min(*unwrapped.last()?);
        (start, start + TAU)
    } else if turn >= 0.0 {
        (unwrapped[0], *unwrapped.last()?)
    } else {
        (*unwrapped.last()?, unwrapped[0])
    };
    // The fitted arc must stay on the old polyline within the drawing tolerance.
    // 64 samples must stay within the chord tolerance of *every* old chord in the view, not
    // only of the chain this fit was made from; a fit that passes its own vertices alone is not
    // good enough.
    let steps = 64;
    let limit = 0.009_f64.min(tolerance.max(0.002));
    for i in 0..=steps {
        let p = centre + Vec2::from_angle(start + (end - start) * i as f64 / steps as f64) * radius;
        let distance = reference.iter().map(|seg| segment_distance(p, seg.a, seg.b)).fold(f64::INFINITY, f64::min);
        if distance > limit {
            return None;
        }
    }
    Some(Curve2mm::Arc { c: centre, r: radius, a0: start, a1: end })
}

/// Fill a view's plain lists and analytic curves from its complete chord lists.
fn attach_curves(view: &mut ProjectedView, scene: &meshes::Scene, camera: &Camera, tolerance: f64) {
    // A cut view's edges are split again by the freehand boundary and joined with the cut
    // outline, so the collinear cleanup runs once more on the finished lists.
    let clean = |list: &[Seg2mm], hidden: bool| {
        let raw: Vec<(Seg2mm, bool, bool)> = list.iter().map(|seg| (*seg, hidden, false)).collect();
        merge_segments(&raw).into_iter().map(|(seg, ..)| seg).collect::<Vec<_>>()
    };
    view.visible = clean(&view.visible, false);
    view.hidden = clean(&view.hidden, true);
    view.tangent = clean(&view.tangent, false);
    let curves = projected_curves(scene, camera);
    let (visible_plain, mut visible_curves, hidden_plain, mut hidden_curves) = split_analytic(&view.visible, &view.hidden, &curves, tolerance);
    // Silhouettes and cut outlines have no B-rep circle behind them; fit those chains only. The
    // reference is every old chord of the view, so a fitted arc is measured against the same
    // polyline geometry as an exact one.
    let reference: Vec<Seg2mm> = view.visible.iter().chain(&view.hidden).copied().collect();
    let (visible_plain, fitted) = fit_arcs(&visible_plain, &reference, tolerance);
    visible_curves.extend(fitted);
    let debris_tolerance = (if tolerance.is_finite() { tolerance } else { 0.01 }) * 2.0;
    let (visible_plain, debris) = cluster_debris(&visible_plain, &reference, debris_tolerance);
    visible_curves.extend(debris);
    let (hidden_plain, fitted) = fit_arcs(&hidden_plain, &reference, tolerance);
    hidden_curves.extend(fitted);
    let (hidden_plain, debris) = cluster_debris(&hidden_plain, &reference, debris_tolerance);
    hidden_curves.extend(debris);
    view.visible_plain = visible_plain;
    view.visible_curves = visible_curves;
    view.hidden_plain = hidden_plain;
    view.hidden_curves = hidden_curves;
}

/// Project `views`, replacing the views named by `cuts` with their sectioned drawings: the mesh
/// scene is clipped with each cutting plane, the kept half projected, and the plane–mesh
/// intersection chained into the cut-outline loops.
///
/// A full section keeps the clipped half's visible edges only (no hidden lines) and gets its
/// section mark in the sheet's other view. A break-out keeps the ordinary view outside its
/// freehand boundary and the clipped view inside it, with no hidden lines inside.
pub fn project_views_cut(s: &Session, views: &[StandardView], opts: &ViewOptions, cuts: &[ViewCut]) -> Result<Vec<ProjectedView>> {
    if !opts.tolerance.is_finite() || !(MIN_TOLERANCE..=MAX_TOLERANCE).contains(&opts.tolerance) {
        return Err(bad(CMD, format!("tolerance must be {MIN_TOLERANCE}..{MAX_TOLERANCE} mm")));
    }
    if views.len() > MAX_VIEWS {
        return Err(bad(CMD, format!("choose at most {MAX_VIEWS} views")));
    }
    if views.is_empty() {
        return Ok(Vec::new());
    }
    if !crate::cmd::view_dxf_exportable(s) {
        return Err(bad(CMD, "no visible bodies or sketches to view"));
    }
    let scene = meshes::prepare(s, opts.tolerance, opts.tangent, opts.curves)?;
    let sketches = sketch::parts(s, opts.tolerance).map_err(|why| bad(CMD, why))?;
    let axes = centre::axes(s);
    let mut result = Vec::with_capacity(views.len());
    for &view in views {
        let camera = standard_camera(view);
        let drawing = projection::drawing(&scene, &sketches, &camera).map_err(|why| bad(CMD, why))?;
        let segments = drawing_segments(&drawing, opts.hidden)?;
        result.push(ProjectedView {
            view,
            visible: segments.visible,
            hidden: segments.hidden,
            visible_plain: Vec::new(),
            visible_curves: Vec::new(),
            hidden_plain: Vec::new(),
            hidden_curves: Vec::new(),
            tangent: segments.tangent,
            bbox: segments.bbox,
            camera,
            centre: centre::view_centres(&axes, view),
            section: None,
            section_marks: Vec::new(),
        });
    }
    if cuts.is_empty() {
        if opts.curves {
            for view in &mut result {
                let camera = view.camera;
                attach_curves(view, &scene, &camera, opts.tolerance);
            }
        } else {
            for view in &mut result {
                view.visible_plain = view.visible.clone();
                view.hidden_plain = view.hidden.clone();
            }
        }
        return Ok(result);
    }
    // Clip each distinct cutting plane once; its projected edges and cut loops serve every view
    // it is used on.
    let mut clipped: Vec<(CuttingPlane, meshes::Scene, projection::Drawing, Vec<Vec<Vec2>>)> = Vec::new();
    for cut in cuts.iter().take(sections::MAX_SECTIONS) {
        if !cut.plane.is_valid() {
            return Err(bad(CMD, "a cutting plane is not finite or is out of range"));
        }
        if clipped.iter().any(|(plane, ..)| *plane == cut.plane) {
            continue;
        }
        let (scene, segments) = meshes::clip_scene(&scene, cut.plane.point, cut.plane.normal).map_err(|why| bad(CMD, why))?;
        if scene.meshes.is_empty() {
            if cut.is_full() {
                return Err(bad(CMD, format!("the cutting plane of view `{}` does not cut the part", view_name(cut.view))));
            }
            continue;
        }
        let camera = standard_camera(cut.view);
        let drawing = projection::drawing(&scene, &sketches, &camera).map_err(|why| bad(CMD, why))?;
        let (right, up, _) = camera.basis();
        let to_view = |p: Vec3| -> Vec2 { Vec2::new((p - camera.target).dot(right), (p - camera.target).dot(up)) };
        let view_segments: Vec<(Vec2, Vec2)> = segments.iter().map(|[a, b]| (to_view(*a), to_view(*b))).collect();
        let tol = 1e-6 * bbox_diagonal(&result, cut.view).max(1.0);
        let loops = sections::chain_loops(&view_segments, tol);
        clipped.push((cut.plane, scene, drawing, loops));
    }
    // Apply the cuts view by view: all break-outs of one view share its cutting plane, so the
    // clipped drawing and its loops are computed once and each freehand boundary clips it.
    for target in result.iter_mut() {
        let view_cuts: Vec<&ViewCut> = cuts.iter().take(sections::MAX_SECTIONS).filter(|cut| cut.view == target.view).collect();
        let Some(first) = view_cuts.first() else { continue };
        let Some((_, _clipped_scene, clipped_drawing, loops)) = clipped.iter().find(|(plane, ..)| *plane == first.plane) else { continue };
        let segments = drawing_segments(clipped_drawing, false)?;
        let (cut_visible, cut_tangent, cut_bbox) = (segments.visible, segments.tangent, segments.bbox);
        let outline: Vec<Seg2mm> = loops
            .iter()
            .flat_map(|loop_| {
                let mut segs: Vec<Seg2mm> = loop_
                    .windows(2)
                    .filter_map(|pair| match pair {
                        [a, b] => Some(Seg2mm { a: *a, b: *b }),
                        _ => None,
                    })
                    .collect();
                if let (Some(first), Some(last)) = (loop_.first(), loop_.last())
                    && first.dist(*last) > 1e-9
                {
                    segs.push(Seg2mm { a: *last, b: *first });
                }
                segs
            })
            .collect();
        let boundaries: Vec<Vec<Vec2>> = view_cuts.iter().filter_map(|cut| cut.boundary.as_ref().map(Boundary::points)).collect();
        let tol = 1e-6 * bbox_diagonal(std::slice::from_ref(target), target.view).max(1.0);
        if boundaries.is_empty() {
            // A full section: no hidden lines, the clipped visible edges plus the cut outline.
            target.visible = cut_visible;
            target.visible.extend(outline);
            target.hidden.clear();
            target.tangent = cut_tangent;
            target.bbox = [
                Vec2::new(target.bbox[0].x.min(cut_bbox[0].x), target.bbox[0].y.min(cut_bbox[0].y)),
                Vec2::new(target.bbox[1].x.max(cut_bbox[1].x), target.bbox[1].y.max(cut_bbox[1].y)),
            ];
        } else {
            // Outside every break-out the ordinary view stays; inside one, the clipped view's
            // visible edges and the cut outline replace it. No hidden line survives inside.
            let mut outside_visible = std::mem::take(&mut target.visible);
            let mut outside_hidden = std::mem::take(&mut target.hidden);
            let mut outside_tangent = std::mem::take(&mut target.tangent);
            for boundary in &boundaries {
                outside_visible = sections::split_by_boundary(&outside_visible, boundary, false, tol);
                outside_hidden = sections::split_by_boundary(&outside_hidden, boundary, false, tol);
                outside_tangent = sections::split_by_boundary(&outside_tangent, boundary, false, tol);
            }
            let mut inside = Vec::new();
            for boundary in &boundaries {
                inside.extend(sections::split_by_boundary(&cut_visible, boundary, true, tol));
                inside.extend(sections::split_by_boundary(&outline, boundary, true, tol));
            }
            target.visible = outside_visible;
            target.visible.extend(inside);
            target.hidden = outside_hidden;
            target.tangent = outside_tangent;
        }
        target.section = Some(ViewSection {
            full: boundaries.is_empty(),
            loops: loops.clone(),
            boundaries,
            label: view_cuts.iter().find_map(|cut| cut.label),
            plane: Some(first.plane),
            from: view_cuts.iter().find_map(|cut| cut.from),
        });
    }
    // A full section's mark belongs in the sheet's other view (the end view of a turned part).
    let marks: Vec<(StandardView, SectionMark)> = cuts
        .iter()
        .take(sections::MAX_SECTIONS)
        .filter_map(|cut| {
            let label = cut.label?;
            // The mark belongs in the view the line was picked in; without one (an explicit
            // plane, or an auto section), the sheet's other view.
            let end = cut.from.map(|from| from.view).or_else(|| views.iter().copied().find(|view| *view != cut.view))?;
            let projected = result.iter().find(|v| v.view == end)?;
            sections::section_mark(cut.plane, projected, label).map(|mark| (end, mark))
        })
        .collect();
    for (view, mark) in marks {
        if let Some(target) = result.iter_mut().find(|v| v.view == view) {
            target.section_marks.push(mark);
        }
    }
    // Analytic curves last: a sectioned view takes them from its clipped scene, the others from
    // the ordinary one, so the visible/hidden split of the HLR and of the cut is kept.
    if opts.curves {
        for target in result.iter_mut() {
            let source = target
                .section
                .as_ref()
                .and_then(|section| section.plane)
                .and_then(|plane| clipped.iter().find(|(clipped, ..)| *clipped == plane))
                .map_or(&scene, |(_, clipped, ..)| clipped);
            let camera = target.camera;
            attach_curves(target, source, &camera, opts.tolerance);
        }
    } else {
        for target in result.iter_mut() {
            target.visible_plain = target.visible.clone();
            target.hidden_plain = target.hidden.clone();
        }
    }
    Ok(result)
}

/// The diagonal of a view's bounding box, for the loop-chaining tolerance.
fn bbox_diagonal(views: &[ProjectedView], view: StandardView) -> f64 {
    views.iter().find(|v| v.view == view).map_or(1.0, |v| v.bbox[1].dist(v.bbox[0]))
}
