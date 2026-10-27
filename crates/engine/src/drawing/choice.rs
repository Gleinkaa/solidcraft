//! Automatic view choice and the sheet plan.
//!
//! The chooser runs in the order the brief fixes: an explicit `views` parameter is used as given;
//! a turned part gets its longitudinal view and its end view; a flat part its plan view and one
//! edge view; anything else keeps front, top and right, less any view whose drawing repeats one
//! already chosen (a cube shows one view, not three identical ones).

use std::collections::HashMap;

use solidcraft_geom::{Aabb3, Vec2, Vec3};
use solidcraft_render::StandardView;

use crate::Result;
use crate::Session;
use crate::drawing::autodim::{self, AutoDimensionSource, Undimensioned};
use crate::drawing::centre;
use crate::drawing::dims::Annotations;
use crate::drawing::sections::{self, SectionMode};
use crate::drawing::sheet::{NotesBlock, Sheet, TitleBlock, build_sheet_with};
use crate::drawing::{ProjectedView, TangentEdges, ViewOptions, project_views, project_views_cut, standard_camera};

/// The three views the sheet places when nothing else decides it, in this order.
pub const PRIMARY_VIEWS: [StandardView; 3] = [StandardView::Front, StandardView::Top, StandardView::Right];

/// Why the sheet's views are what they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewChoice {
    /// The `views` parameter said so.
    #[default]
    Explicit,
    /// A turned part: a longitudinal view and the end view.
    Turned,
    /// A flat part: the plan view and one edge view.
    Flat,
    /// Front, top and right, less repeated views.
    General,
}

impl ViewChoice {
    /// The word the commands report (`"turned"`, `"flat"`, `"general"`, `"explicit"`).
    pub fn name(self) -> &'static str {
        match self {
            ViewChoice::Explicit => "explicit",
            ViewChoice::Turned => "turned",
            ViewChoice::Flat => "flat",
            ViewChoice::General => "general",
        }
    }

    /// The English catalog id of the one-line reason the UI shows beside the views.
    pub fn message_id(self) -> &'static str {
        match self {
            ViewChoice::Explicit => "drawing.choice.explicit",
            ViewChoice::Turned => "drawing.choice.turned",
            ViewChoice::Flat => "drawing.choice.flat",
            ViewChoice::General => "drawing.choice.general",
        }
    }
}

/// Two drawings must agree within this (sheet mm) to count as the same view.
pub const SAME_VIEW_TOL_MM: f64 = 0.05;

/// The chosen views and why.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSelection {
    pub views: Vec<StandardView>,
    pub choice: ViewChoice,
}

/// A laid-out sheet with the views it was built from and the reason for them.
#[derive(Clone, Debug)]
pub struct SheetPlan {
    pub sheet: Sheet,
    pub choice: ViewChoice,
    /// The projected views the sheet was laid out from, kept so a title or scale edit can lay the
    /// sheet out again without projecting.
    pub projected: Vec<ProjectedView>,
    /// Features the automatic completeness pass found but could not reference.
    pub undimensioned: Vec<Undimensioned>,
    /// Conditions the automatic pass noticed (`skew_axis`), for the UI to report.
    pub warnings: Vec<autodim::Warning>,
    /// The cuts this plan applied (empty when the sheet has no sections).
    pub cuts: Vec<sections::ViewCut>,
}

/// Project `requested` (or the three primary views when it is `None`) and choose the sheet's
/// views. The projection options carry the curve tolerance; hidden edges are always projected, so
/// the duplicate test sees the same drawing the sheet would.
pub fn plan_views(s: &Session, requested: Option<&[StandardView]>, tolerance: f64) -> Result<(ViewSelection, Vec<ProjectedView>)> {
    plan_views_with(s, requested, tolerance, TangentEdges::Omit)
}

/// [`plan_views`] with the sheet's tangent-edge rule.
pub fn plan_views_with(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    tangent: TangentEdges,
) -> Result<(ViewSelection, Vec<ProjectedView>)> {
    let options = ViewOptions { tolerance, hidden: true, tangent, curves: true };
    match requested {
        Some(views) => {
            let projected = project_views(s, views, &options)?;
            Ok((ViewSelection { views: views.to_vec(), choice: ViewChoice::Explicit }, projected))
        }
        None => {
            let candidates = project_views(s, &PRIMARY_VIEWS, &options)?;
            let selection = choose_views(s, &candidates);
            let chosen = selection.views.iter().filter_map(|view| candidates.iter().find(|p| p.view == *view).cloned()).collect();
            Ok((selection, chosen))
        }
    }
}

/// Project and lay out a sheet. `requested` is the explicit `views` parameter (`None`: automatic
/// choice); `scale` is `None` for the largest ISO 5455 scale that fits. Sections are automatic.
pub fn plan_sheet(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
) -> Result<SheetPlan> {
    plan_sheet_with_sections(s, requested, tolerance, title, notes_block, scale, &Annotations::default(), &SectionMode::Auto)
}

/// Project and lay out a sheet with explicit dimensions, notes and the section mode. The
/// annotations' views are checked against the views the sheet actually shows.
pub fn plan_sheet_with(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    annotations: &Annotations,
) -> Result<SheetPlan> {
    plan_sheet_with_sections(s, requested, tolerance, title, notes_block, scale, annotations, &SectionMode::Auto)
}

/// [`plan_sheet_with`] with the section mode the caller asks for.
pub fn plan_sheet_with_sections(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    annotations: &Annotations,
    sections: &SectionMode,
) -> Result<SheetPlan> {
    plan_sheet_with_sections_tangent(s, requested, tolerance, title, notes_block, scale, annotations, sections, TangentEdges::Omit)
}

/// [`plan_sheet_with_sections`] with the sheet's tangent-edge rule (`omit` by default, `thin`
/// when the drawing metadata asks for light edges).
#[allow(clippy::too_many_arguments)]
pub fn plan_sheet_with_sections_tangent(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    annotations: &Annotations,
    sections: &SectionMode,
    tangent: TangentEdges,
) -> Result<SheetPlan> {
    let (selection, projected) = plan_views_with(s, requested, tolerance, tangent)?;
    let (projected, cuts) = cut_views(s, &selection, projected, tolerance, sections, tangent)?;
    crate::drawing::dims::validate(annotations, &selection.views)?;
    let sheet = build_sheet_with(&projected, title, notes_block, scale, annotations)?;
    Ok(SheetPlan { sheet, choice: selection.choice, projected, undimensioned: Vec::new(), warnings: Vec::new(), cuts })
}

/// Replace the cut views with their sectioned drawings. `None` and an empty auto plan leave the
/// projection untouched; an explicit cut on a view the sheet does not show is an error.
fn cut_views(
    s: &Session,
    selection: &ViewSelection,
    projected: Vec<ProjectedView>,
    tolerance: f64,
    mode: &SectionMode,
    tangent: TangentEdges,
) -> Result<(Vec<ProjectedView>, Vec<sections::ViewCut>)> {
    if let SectionMode::Explicit(specs) = mode {
        for spec in specs {
            if let Some(view) = spec.named_view()
                && !selection.views.contains(&view)
            {
                return Err(crate::params::bad(
                    "drawing.sheet",
                    format!("a section is on view `{}`, which is not on the sheet", crate::drawing::view_name(view)),
                ));
            }
            if let Some(view) = spec.line_view()
                && !selection.views.contains(&view)
            {
                return Err(crate::params::bad(
                    "drawing.sheet",
                    format!("a section's line is in view `{}`, which is not on the sheet", crate::drawing::view_name(view)),
                ));
            }
        }
    }
    let cuts = sections::plan_cuts(s, selection, &projected, mode)?;
    if let SectionMode::Explicit(specs) = mode
        && cuts.len() != specs.len()
    {
        return Err(crate::params::bad("drawing.sheet", "a section is not valid"));
    }
    if cuts.is_empty() {
        return Ok((projected, cuts));
    }
    let views: Vec<StandardView> = projected.iter().map(|v| v.view).collect();
    let cut = project_views_cut(s, &views, &ViewOptions { tolerance, hidden: true, tangent, curves: true }, &cuts)?;
    Ok((cut, cuts))
}

/// Project, choose the views and lay out the sheet with automatic dimensions and size notes
/// plus any explicit `extra` annotations. Returns the sheet and the traceability list.
///
/// The layout runs three times because automatic dimensions need the chosen scale (their offsets
/// are sheet millimetres) while the scale choice needs the real dimension bands: first with
/// row-naming offsets to pick the scale, then with the real stacks, then with the notes placed
/// against the finished dimension lines. No projection is repeated.
pub fn plan_sheet_auto(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    extra: &Annotations,
) -> Result<(SheetPlan, Vec<AutoDimensionSource>)> {
    plan_sheet_auto_sections(s, requested, tolerance, title, notes_block, scale, extra, &SectionMode::Auto)
}

/// [`plan_sheet_auto`] with the section mode the caller asks for. The sections are applied to
/// the projected views *before* the automatic dimensions are planned, so a dimension that used a
/// hidden edge snaps to the visible cut edge instead.
pub fn plan_sheet_auto_sections(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    extra: &Annotations,
    sections: &SectionMode,
) -> Result<(SheetPlan, Vec<AutoDimensionSource>)> {
    plan_sheet_auto_sections_tangent(s, requested, tolerance, title, notes_block, scale, extra, sections, TangentEdges::Omit)
}

/// [`plan_sheet_auto_sections`] with the sheet's tangent-edge rule.
#[allow(clippy::too_many_arguments)]
pub fn plan_sheet_auto_sections_tangent(
    s: &Session,
    requested: Option<&[StandardView]>,
    tolerance: f64,
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    extra: &Annotations,
    sections: &SectionMode,
    tangent: TangentEdges,
) -> Result<(SheetPlan, Vec<AutoDimensionSource>)> {
    let (selection, projected) = plan_views_with(s, requested, tolerance, tangent)?;
    let (projected, cuts) = cut_views(s, &selection, projected, tolerance, sections, tangent)?;
    let (mut plan, sources) = plan_sheet_auto_reuse(s, selection.choice, &selection.views, &projected, title, notes_block, scale, extra)?;
    plan.cuts = cuts;
    Ok((plan, sources))
}

/// [`plan_sheet_auto`] from views that were already projected (a title or scale edit).
pub fn plan_sheet_auto_reuse(
    s: &Session,
    choice: ViewChoice,
    views: &[StandardView],
    projected: &[ProjectedView],
    title: &TitleBlock,
    notes_block: &NotesBlock,
    scale: Option<(u32, u32)>,
    extra: &Annotations,
) -> Result<(SheetPlan, Vec<AutoDimensionSource>)> {
    let selection = ViewSelection { views: views.to_vec(), choice };
    crate::drawing::dims::validate(extra, views)?;
    let automatic = autodim::plan(s, &selection, projected);

    // 1. The row-naming annotations give the band sizes the scale search needs.
    let mut provisional = automatic.provisional_annotations();
    provisional.dimensions.extend(extra.dimensions.iter().cloned());
    provisional.notes.extend(extra.notes.iter().cloned());
    let first = build_sheet_with(projected, title, notes_block, scale, &provisional)?;
    let factor = first.scale_factor();

    // 2. The real stacks at the chosen scale; the notes find their room against them. Placing a
    //    note changes the obstacle set of the dimension texts (a leader or a line extended under
    //    an outside value), so the note positions settle in a small fixed-point loop.
    let mut annotations = automatic.annotations_at(projected, factor);
    annotations.dimensions.extend(extra.dimensions.iter().cloned());
    annotations.notes.extend(extra.notes.iter().cloned());
    let mut auto_notes: Vec<crate::drawing::dims::Note> = Vec::new();
    let mut note_unplaced = Vec::new();
    let mut sheet = build_sheet_with(projected, title, notes_block, Some(first.scale), &annotations)?;
    for round in 0..4 {
        let (notes, unplaced) = automatic.notes_at(&sheet, factor, auto_notes.len(), projected);
        note_unplaced = unplaced;
        let stable = round > 0
            && notes.len() == auto_notes.len()
            && notes.iter().zip(&auto_notes).all(|(a, b)| a.text == b.text && a.at == b.at && a.leader == b.leader && a.target == b.target);
        auto_notes = notes;
        if stable {
            break;
        }
        annotations.notes.truncate(extra.notes.len());
        annotations.notes.extend(auto_notes.iter().cloned());
        sheet = build_sheet_with(projected, title, notes_block, Some(first.scale), &annotations)?;
    }

    // 3. The finished sheet with the settled notes.
    annotations.notes.truncate(extra.notes.len());
    annotations.notes.extend(auto_notes);
    sheet = build_sheet_with(projected, title, notes_block, Some(first.scale), &annotations)?;
    sheet.unplaced.extend(note_unplaced);
    let undimensioned = automatic.undimensioned();
    Ok((
        SheetPlan { sheet, choice, projected: projected.to_vec(), undimensioned, warnings: automatic.warnings(), cuts: Vec::new() },
        automatic.sources(),
    ))
}

/// Choose the views for `candidates`, the projections of [`PRIMARY_VIEWS`].
pub fn choose_views(s: &Session, candidates: &[ProjectedView]) -> ViewSelection {
    if let Some(axis) = centre::turned_axis(s) {
        let (longitudinal, end) = turned_views(centre::world_axis(axis.axis), candidates);
        return ViewSelection { views: vec![longitudinal, end], choice: ViewChoice::Turned };
    }
    if let Some((plan, edge)) = flat_views(model_bounds(s)) {
        return ViewSelection { views: vec![plan, edge], choice: ViewChoice::Flat };
    }
    let mut chosen: Vec<StandardView> = Vec::new();
    let mut chosen_drawings: Vec<&ProjectedView> = Vec::new();
    for view in PRIMARY_VIEWS {
        let Some(projected) = candidates.iter().find(|p| p.view == view) else { continue };
        if chosen_drawings.iter().any(|other| drawings_match(other, projected)) {
            continue;
        }
        chosen.push(view);
        chosen_drawings.push(projected);
    }
    ViewSelection { views: chosen, choice: ViewChoice::General }
}

/// The longitudinal and end views of a turned part whose axis is `axis` (a world axis).
///
/// The longitudinal view is one of the three primary views that shows the axis in the view plane,
/// preferring one where it lies horizontal; of those it is the one with the larger visible edge
/// length. The end view looks along the axis.
fn turned_views(axis: Vec3, candidates: &[ProjectedView]) -> (StandardView, StandardView) {
    let end = end_view(axis);
    let (sin1, _) = (std::f64::consts::PI / 180.0).sin_cos();
    let perpendicular: Vec<StandardView> = PRIMARY_VIEWS
        .iter()
        .copied()
        .filter(|view| {
            let (_, _, back) = standard_camera(*view).basis();
            *view != end && back.dot(axis).abs() <= sin1
        })
        .collect();
    let horizontal: Vec<StandardView> = perpendicular
        .iter()
        .copied()
        .filter(|view| {
            let (_, up, _) = standard_camera(*view).basis();
            up.dot(axis).abs() <= sin1
        })
        .collect();
    let pool = if horizontal.is_empty() { perpendicular } else { horizontal };
    let mut best: Option<(StandardView, f64)> = None;
    for view in pool {
        let length = candidates.iter().find(|p| p.view == view).map_or(0.0, visible_length);
        // Two views of a round part carry the same silhouette, and their ink differs by the
        // tessellation's seams and by a small feature seen from one side only. Only a clearly
        // longer visible drawing (10 %) picks the other view, so a shaft keeps its front view
        // instead of flipping to the top one because a cross hole shows there.
        let clearly_longer = best.is_some_and(|(_, longest)| length > longest * 1.1 + 0.5);
        if best.is_none() || clearly_longer {
            best = Some((view, length));
        }
    }
    (best.map_or(StandardView::Front, |(view, _)| view), end)
}

/// The standard view looking along the positive world axis `axis` (a world axis): right for X,
/// front for Y, top for Z.
pub fn end_view(axis: Vec3) -> StandardView {
    if axis.x.abs() >= axis.y.abs() && axis.x.abs() >= axis.z.abs() {
        StandardView::Right
    } else if axis.y.abs() >= axis.z.abs() {
        StandardView::Front
    } else {
        StandardView::Top
    }
}

/// The total length of a view's visible edges (its drawing's ink).
fn visible_length(view: &ProjectedView) -> f64 {
    view.visible.iter().map(|s| s.a.dist(s.b)).filter(|l| l.is_finite()).sum()
}

/// The plan view of a flat part (looking along its thin direction) and the edge view showing the
/// longer outline; `None` when the part is not flat.
fn flat_views(bounds: Aabb3) -> Option<(StandardView, StandardView)> {
    if bounds.is_empty() {
        return None;
    }
    let size = bounds.size();
    let extents = [size.x, size.y, size.z];
    if !extents.iter().all(|e| e.is_finite() && *e >= 0.0) {
        return None;
    }
    let mut sorted = extents;
    sorted.sort_by(f64::total_cmp);
    let (thin, middle) = (sorted[0], sorted[1]);
    // A plate: its thinnest extent is less than half the middle one, so a plan view and one edge
    // view carry every size and a third view would only repeat the edge. This includes the crank
    // (92 × 40 × 10) and the 36 × 25 × 10 test plate, whose arcs still read in the plan. A block
    // exactly half as thick as it is wide (40 × 20 × 10) keeps its three views.
    if !(thin > 0.0 && middle > 0.0 && thin < middle * 0.5 - 1e-9) {
        return None;
    }
    let thin_axis = if thin == size.x {
        0
    } else if thin == size.y {
        1
    } else {
        2
    };
    // The plan looks along the thin direction; the edge view runs along one of the other two
    // axes, and the one showing the longer outline is the one whose remaining extent is larger.
    let (plan, first, first_length, second, second_length) = match thin_axis {
        0 => (StandardView::Right, StandardView::Front, size.z, StandardView::Top, size.y),
        1 => (StandardView::Front, StandardView::Right, size.z, StandardView::Top, size.x),
        _ => (StandardView::Top, StandardView::Front, size.x, StandardView::Right, size.y),
    };
    let edge = if first_length >= second_length { first } else { second };
    Some((plan, edge))
}

/// The bounding box of every visible body, in world millimetres. The body's outline (its B-rep
/// curves) is used rather than the tessellation, whose chords would make a 40 mm width come out
/// as 39.99999 and a flat part miss the quarter rule.
fn model_bounds(s: &Session) -> Aabb3 {
    let st = s.world_state();
    st.bodies.iter().filter(|b| s.body_visible(&b.name)).fold(Aabb3::EMPTY, |bounds, body| bounds.union(&body.outline_bounds()))
}

/// The segments of a view's drawing: visible and hidden edges together.
fn drawing_segments(view: &ProjectedView) -> Vec<(Vec2, Vec2)> {
    view.visible.iter().chain(&view.hidden).map(|s| (s.a, s.b)).collect()
}

fn segment_bounds(segments: &[(Vec2, Vec2)]) -> (Vec2, Vec2) {
    segments.iter().fold((Vec2::new(f64::INFINITY, f64::INFINITY), Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY)), |(lo, hi), (a, b)| {
        (Vec2::new(lo.x.min(a.x).min(b.x), lo.y.min(a.y).min(b.y)), Vec2::new(hi.x.max(a.x).max(b.x), hi.y.max(a.y).max(b.y)))
    })
}

/// Whether two views draw the same thing within [`SAME_VIEW_TOL_MM`], compared after shifting both
/// to a common bounding-box origin and allowing a mirror about either axis.
pub fn drawings_match(a: &ProjectedView, b: &ProjectedView) -> bool {
    let (sa, sb) = (drawing_segments(a), drawing_segments(b));
    if sa.len() != sb.len() {
        return false;
    }
    if sa.is_empty() {
        return true;
    }
    let (loa, hia) = segment_bounds(&sa);
    let (lob, hib) = segment_bounds(&sb);
    let (span_a, span_b) = (hia - loa, hib - lob);
    if (span_a.x - span_b.x).abs() > SAME_VIEW_TOL_MM || (span_a.y - span_b.y).abs() > SAME_VIEW_TOL_MM {
        return false;
    }
    // Normalised, mirrored copies of b: a mirror keeps the spans, so the span test above covers
    // every variant at once.
    let (width, height) = (span_a.x.max(span_b.x), span_a.y.max(span_b.y));
    let normal = |segments: &[(Vec2, Vec2)], (lo, _hi): (Vec2, Vec2), mirror_x: bool, mirror_y: bool| -> Vec<(Vec2, Vec2)> {
        let map = |p: Vec2| {
            let x = p.x - lo.x;
            let y = p.y - lo.y;
            Vec2::new(if mirror_x { width - x } else { x }, if mirror_y { height - y } else { y })
        };
        segments.iter().map(|(p, q)| (map(*p), map(*q))).collect()
    };
    let target = normal(&sa, (loa, hia), false, false);
    for (mirror_x, mirror_y) in [(false, false), (true, false), (false, true), (true, true)] {
        if same_segments(&target, &normal(&sb, (lob, hib), mirror_x, mirror_y)) {
            return true;
        }
    }
    false
}

/// Whether `b` holds a matching segment for every segment of `a`, within [`SAME_VIEW_TOL_MM`] at
/// both ends (in either direction), each segment used once.
fn same_segments(a: &[(Vec2, Vec2)], b: &[(Vec2, Vec2)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let cell = 2.0 * SAME_VIEW_TOL_MM;
    let key = |p: Vec2| ((p.x / cell).floor() as i64, (p.y / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (index, (p, q)) in b.iter().enumerate() {
        grid.entry(key((*p + *q) * 0.5)).or_default().push(index);
    }
    let mut used = vec![false; b.len()];
    for (p, q) in a {
        let (kx, ky) = key((*p + *q) * 0.5);
        let mut found = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for &index in grid.get(&(kx + dx, ky + dy)).map(Vec::as_slice).unwrap_or(&[]) {
                    if used[index] {
                        continue;
                    }
                    let (r, s) = b[index];
                    let same = (p.dist(r) <= SAME_VIEW_TOL_MM && q.dist(s) <= SAME_VIEW_TOL_MM)
                        || (p.dist(s) <= SAME_VIEW_TOL_MM && q.dist(r) <= SAME_VIEW_TOL_MM);
                    if same {
                        found = Some(index);
                        break 'search;
                    }
                }
            }
        }
        match found {
            Some(index) => used[index] = true,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(view: StandardView, segments: &[(Vec2, Vec2)]) -> ProjectedView {
        ProjectedView {
            view,
            visible: segments.iter().map(|(a, b)| crate::drawing::Seg2mm { a: *a, b: *b }).collect(),
            hidden: Vec::new(),
            visible_plain: segments.iter().map(|(a, b)| crate::drawing::Seg2mm { a: *a, b: *b }).collect(),
            visible_curves: Vec::new(),
            hidden_plain: Vec::new(),
            hidden_curves: Vec::new(),
            tangent: Vec::new(),
            bbox: [Vec2::ZERO, Vec2::new(1.0, 1.0)],
            camera: standard_camera(view),
            centre: Vec::new(),
            section: None,
            section_marks: Vec::new(),
        }
    }

    #[test]
    fn a_mirrored_and_shifted_copy_of_a_drawing_matches() {
        let square = [(Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0)), (Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0))];
        let shifted_mirrored = [(Vec2::new(100.0, 50.0), Vec2::new(90.0, 50.0)), (Vec2::new(90.0, 50.0), Vec2::new(100.0, 60.0))];
        assert!(drawings_match(&view(StandardView::Front, &square), &view(StandardView::Right, &shifted_mirrored)));
        let different = [(Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0)), (Vec2::new(10.0, 0.0), Vec2::new(0.0, 20.0))];
        assert!(!drawings_match(&view(StandardView::Front, &square), &view(StandardView::Top, &different)));
        let moved = [(Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0)), (Vec2::new(10.1, 0.0), Vec2::new(0.0, 10.0))];
        assert!(!drawings_match(&view(StandardView::Front, &square), &view(StandardView::Top, &moved)), "0.1 mm is more than the tolerance");
    }
}
