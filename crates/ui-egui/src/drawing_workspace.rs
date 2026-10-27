//! The Drawing workspace: the active design's drawing sheet as a 2D page.
//!
//! Mirrors the Render workspace's lifecycle: entering saves the Design camera and shows the
//! page, leaving restores both, and a design switch leaves the workspace. The sheet's projected
//! views take seconds on real parts, so they are built on a worker (synchronously in the
//! browser) and a result for a design revision or document that has changed meanwhile is
//! dropped. The page draws the engine's [`Sheet`] primitives with the egui painter and takes all
//! camera input from [`crate::navigation::handle`] with orbit and picking off, like the flat
//! pattern.
//!
//! The title-block and notes values are saved with the design through `drawing.title.set`, which
//! writes them through `document.metadata.set` (one undo step), so closing and reopening a design
//! keeps them.

use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};
use solidcraft_engine::drawing::sections::{inside_polygon, section_letter};
use solidcraft_engine::drawing::sheet::{ISO_SCALES, format_scale};
use solidcraft_engine::drawing::{
    Annotations, Boundary, Layer, LineKind, MAX_EXTRA_NOTES, MAX_NOTE_CHARS, NotesBlock, Paper, PlacedView, Prim, ProjectedView, SECTION_END_MM,
    SectionLine, SectionMode, SectionSpec, Sheet, TangentEdges, TextAnchor, TitleBlock, ViewCentre, ViewChoice, ViewCut, ViewSelection,
    assigned_letters, build_sheet, derive_full_from, plan_sheet_auto_reuse, plan_sheet_auto_sections_tangent, plan_sheet_with_sections_tangent,
    rohmass, section_mode_value, sheet_to_dxf, sheet_to_pdf, standard_camera, stored_notes, stored_sections, stored_tangent_edges, stored_title,
    view_name,
};
use solidcraft_engine::geom::Vec2;
use solidcraft_engine::render::{Camera, StandardView};

use crate::SolveApp;
use crate::dialogs_sheet::FlatView;
use crate::theme::Tokens;
use crate::ui_kit::{Container, Edge, Surface};

#[cfg(test)]
#[path = "drawing_workspace_tests.rs"]
mod tests;

/// Width of the title-block side panel.
const PANEL_WIDTH: f32 = 285.0;
/// The page's ink on white paper, whatever the interface theme is.
const SHEET_INK: Color32 = Color32::from_rgb(28, 28, 28);
/// Dashed hidden edges on white paper.
const HIDDEN_INK: Color32 = Color32::from_rgb(96, 96, 96);
/// Paper fill.
const PAPER: Color32 = Color32::WHITE;

/// The sheet scale the page shows: chosen to fit, or one of the ISO 5455 values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    /// The largest ISO 5455 scale that fits the drawing area.
    #[default]
    Auto,
    /// A forced scale, `(numerator, denominator)`.
    Fixed(u32, u32),
}

impl Scale {
    /// What `build_sheet` takes: `None` chooses a scale that fits.
    pub fn value(self) -> Option<(u32, u32)> {
        match self {
            Scale::Auto => None,
            Scale::Fixed(n, d) => Some((n, d)),
        }
    }

    /// The scale as the control channel states it, or `auto`.
    pub fn text(self) -> String {
        match self {
            Scale::Auto => "auto".into(),
            Scale::Fixed(n, d) => format_scale((n, d)),
        }
    }

    /// The scale as the page's controls label it.
    pub fn label(self, app: &SolveApp) -> String {
        match self {
            Scale::Auto => app.tr("drawing.auto").to_string(),
            Scale::Fixed(n, d) => format_scale((n, d)),
        }
    }
}

/// One title-block field the side panel edits: its catalog id and the [`TitleBlock`] field.
pub const TITLE_FIELDS: [(&str, &str); 14] = [
    ("drawing.benennung", "benennung"),
    ("drawing.zeichnungsnummer", "zeichnungsnummer"),
    ("drawing.werkstoff", "werkstoff"),
    ("drawing.allgemeintoleranz", "allgemeintoleranz"),
    ("drawing.gezeichnet", "gezeichnet"),
    ("drawing.gezeichnet_datum", "datum"),
    ("drawing.geprueft", "geprueft"),
    ("drawing.geprueft_datum", "geprueft_datum"),
    ("drawing.aend", "aend"),
    ("drawing.aend_datum", "aend_datum"),
    ("drawing.firma", "firma"),
    ("drawing.rohmass", "rohmass"),
    ("drawing.dokumentart", "dokumentart"),
    ("drawing.blatt", "blatt"),
];

/// What the Section tool places: a full section from a cutting line, or a break-out window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SectionToolMode {
    #[default]
    Full,
    Breakout,
}

impl SectionToolMode {
    /// The word the control state reports (`"full"`, `"breakout"`).
    pub fn name(self) -> &'static str {
        match self {
            SectionToolMode::Full => "full",
            SectionToolMode::Breakout => "breakout",
        }
    }

    /// The English catalog id of the mode's label.
    pub fn message_id(self) -> &'static str {
        match self {
            SectionToolMode::Full => "drawing.section.full",
            SectionToolMode::Breakout => "drawing.section.breakout",
        }
    }
}

/// Where a new section's gesture is in its click / drag sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GestureStage {
    /// The pointer is down at the first point (a drag may follow).
    FirstPress,
    /// A click set the first point; the next click sets the second.
    Armed,
    /// The second click is down; its release completes the gesture.
    SecondPress,
    /// A drag handle is being moved.
    Handle,
}

/// A section gesture in progress, in the picked view's own millimetres.
#[derive(Clone, Debug, PartialEq)]
struct SectionGesture {
    view: StandardView,
    mode: SectionToolMode,
    stage: GestureStage,
    anchor: Vec2,
    current: Vec2,
    /// Where the press happened on screen, to tell a click from a drag.
    press_at: Pos2,
    /// The handle drag's entry and handle, when this gesture edits an existing section.
    edit: Option<(char, SectionHandle)>,
}

/// One handle of a selected section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SectionHandle {
    LineA,
    LineB,
    BreakoutCentre,
    BreakoutRadius,
}

impl SectionHandle {
    /// The handle's published name suffix (`a`, `b`, `c`, `r`).
    fn suffix(self) -> &'static str {
        match self {
            SectionHandle::LineA => "a",
            SectionHandle::LineB => "b",
            SectionHandle::BreakoutCentre => "c",
            SectionHandle::BreakoutRadius => "r",
        }
    }
}

/// The per-document Drawing state: the sheet and what it was made from.
#[derive(Clone, Debug)]
pub struct ViewState {
    /// The finished sheet the page paints.
    pub sheet: Option<Sheet>,
    /// The title-block values; edited here and saved with the design.
    pub title: TitleBlock,
    /// The notes above the title block.
    pub notes: NotesBlock,
    /// The extra notes as one text per line (the panel's multiline editor).
    pub extra_text: String,
    /// A title or notes value changed and still has to be written to the design.
    pub title_dirty: bool,
    /// Why the sheet has the views it has (the engine's automatic choice, or an explicit list).
    pub choice: ViewChoice,
    /// The chosen scale (`Auto`: the largest one that fits).
    pub scale: Scale,
    /// Read the dimensions and size notes from the B-rep (`dimensions: auto`).
    pub auto_dimensions: bool,
    /// The sections spec stored with the design (D9.2); the page and the exports use it.
    pub sections: SectionMode,
    /// The cuts of the last build, for materialising the automatic sections on the first edit.
    pub(crate) cuts: Vec<ViewCut>,
    /// The Section tool is on.
    pub section_tool: bool,
    /// What the next click places.
    pub section_mode: SectionToolMode,
    /// The view the next cut is drawn in.
    pub picked_view: Option<StandardView>,
    /// The section whose handles are shown, by its letter.
    pub selected_section: Option<char>,
    /// The gesture in progress.
    gesture: Option<SectionGesture>,
    /// The page's own 2D camera: x right, y up, millimetres, like the flat pattern's.
    pub cam: FlatView,
    /// The sheet has to be (re)built.
    pub dirty: bool,
    /// Why the last build failed, when it did.
    pub error: Option<String>,
    /// The projected views, kept so a title or scale edit rebuilds only the sheet.
    pub(crate) projected: Option<Vec<ProjectedView>>,
    /// The design revision the projection was made from.
    pub(crate) revision: Option<u64>,
    /// How many successful projections this editor has installed (proof that a title edit does
    /// not project again).
    pub(crate) projections: usize,
    /// The free screen area the page was last fitted to (the layout's drawable area).
    fitted_area: Option<Rect>,
    /// The user panned or zoomed since the last fit, so a window or panel resize keeps their view.
    moved: bool,
    /// The title was given the design's defaults on the first entry.
    initialised: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            sheet: None,
            title: TitleBlock::default(),
            notes: NotesBlock::default(),
            extra_text: String::new(),
            title_dirty: false,
            choice: ViewChoice::default(),
            scale: Scale::Auto,
            auto_dimensions: true,
            sections: SectionMode::Auto,
            cuts: Vec::new(),
            section_tool: false,
            section_mode: SectionToolMode::Full,
            picked_view: None,
            selected_section: None,
            gesture: None,
            cam: FlatView::default(),
            dirty: false,
            error: None,
            projected: None,
            revision: None,
            projections: 0,
            fitted_area: None,
            moved: false,
            initialised: false,
        }
    }
}

/// A projection and sheet build running on a worker.
struct Build {
    done: Receiver<Result<(Vec<ProjectedView>, Sheet, ViewChoice, Vec<ViewCut>), String>>,
    /// The document allocation the build was started for.
    doc: usize,
    /// The design revision at the start; a newer one makes the result stale.
    revision: u64,
    /// The title, notes, scale and dimension mode the worker laid out with.
    title: TitleBlock,
    notes: NotesBlock,
    scale: Scale,
    auto_dimensions: bool,
}

/// The file format an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportKind {
    Pdf,
    Dxf,
}

impl ExportKind {
    fn extension(self) -> &'static str {
        match self {
            ExportKind::Pdf => "pdf",
            ExportKind::Dxf => "dxf",
        }
    }
}

/// An export waiting for the sheet build that will make it current.
struct PendingExport {
    path: String,
    kind: ExportKind,
}

/// The Drawing workspace's app-wide state.
#[derive(Default)]
pub struct State {
    /// The active design's drawing editor (swapped with the design tab).
    pub view: ViewState,
    /// The background build for the active design.
    build: Option<Build>,
    /// An export that waits for a fresh sheet: the built sheet, never a UI-thread projection.
    pending_export: Option<PendingExport>,
    /// The Design camera and perspective to put back on leaving.
    design_camera: Option<(Camera, bool)>,
}

/// Whether the Drawing workspace is the page the window shows.
pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == "DRAWING"
}

/// Whether a sheet build is under way.
pub fn building(app: &SolveApp) -> bool {
    app.drawing_workspace.build.is_some()
}

fn doc_key(app: &SolveApp) -> usize {
    std::sync::Arc::as_ptr(&app.session.doc) as usize
}

/// Enter or leave the Drawing workspace. Entering refuses while a modelling operation runs,
/// saving the Design camera and starting the sheet build; leaving restores the camera and tab.
pub fn enter(app: &mut SolveApp, on: bool) -> Result<(), String> {
    if on {
        if app.session.active_sketch.is_some() || app.tool.is_some() || app.dialog.is_some() || app.file_load.busy() {
            return Err("finish the current modelling operation before entering Drawing".into());
        }
        if !active(app) {
            app.drawing_workspace.design_camera = Some((app.cam, app.ui.perspective));
            app.ui.tab = "DRAWING".into();
            app.cancel_view_animation();
            prepare(app);
        } else if !building(app) {
            // Asking for Drawing again refreshes a sheet whose design moved on, or whose stored
            // section spec changed while another workspace was shown (an undo is metadata only).
            let stored = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
            let stale = {
                let view = &app.drawing_workspace.view;
                view.dirty || view.revision != Some(app.session.revision) || view.sections != stored
            };
            if stale {
                if app.drawing_workspace.view.sections != stored {
                    app.drawing_workspace.view.sections = stored;
                }
                start_build(app);
            }
        }
    } else {
        if let Some((camera, perspective)) = app.drawing_workspace.design_camera.take() {
            app.cam = camera;
            app.ui.up_axis = camera.up;
            app.ui.perspective = perspective;
        }
        app.ui.tab = "MODEL".into();
        app.drawing_workspace.build = None;
        app.drawing_workspace.pending_export = None;
        app.cancel_view_animation();
    }
    Ok(())
}

/// Initialise the editor for this design and build the sheet when it is missing or stale.
fn prepare(app: &mut SolveApp) {
    let view = &mut app.drawing_workspace.view;
    if !view.initialised {
        view.title = stored_title(&app.session.doc, rohmass(&app.session));
        view.notes = stored_notes(&app.session.doc);
        view.extra_text = view.notes.extra_notes.join("\n");
        view.sections = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
        view.initialised = true;
    }
    view.cam.fitted = false;
    let stored = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
    let spec_changed = view.sections != stored;
    view.sections = stored;
    let stale = view.revision != Some(app.session.revision);
    if view.dirty || view.sheet.is_none() || stale || spec_changed {
        start_build(app);
    }
}

/// Project the views and lay out the sheet off the UI thread. In the browser, where there are
/// no threads, the work runs here.
pub fn start_build(app: &mut SolveApp) {
    app.drawing_workspace.build = None;
    // The stored spec is the one source of truth: a build always reads it from the design.
    let stored = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
    let view = &mut app.drawing_workspace.view;
    view.sections = stored;
    if view.selected_section.is_some_and(|letter| !view.sections.letters().contains(&letter)) {
        view.selected_section = None;
    }
    view.dirty = true;
    view.error = None;
    let doc = doc_key(app);
    let revision = app.session.revision;
    let title = app.drawing_workspace.view.title.clone();
    let notes = app.drawing_workspace.view.notes.clone();
    let scale = app.drawing_workspace.view.scale;
    let auto_dimensions = app.drawing_workspace.view.auto_dimensions;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let session = app.session.scratch();
        let (worker_title, worker_notes) = (title.clone(), notes.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("drawing-sheet".into()).spawn(move || {
            let outcome = build(&session, &worker_title, &worker_notes, scale, auto_dimensions);
            let _ = tx.send(outcome);
        });
        if spawned.is_ok() {
            app.drawing_workspace.build = Some(Build { done: rx, doc, revision, title, notes, scale, auto_dimensions });
            return;
        }
        // No thread to build on: build here, as the browser does.
    }
    let outcome = build(&app.session, &title, &notes, scale, auto_dimensions);
    finish_build(app, doc, revision, &title, &notes, scale, auto_dimensions, outcome);
}

/// Choose the views the sheet should show (the engine's automatic choice), project them and lay
/// the sheet out from `title` and `scale`.
fn build(
    session: &solidcraft_engine::Session,
    title: &TitleBlock,
    notes: &NotesBlock,
    scale: Scale,
    auto_dimensions: bool,
) -> Result<(Vec<ProjectedView>, Sheet, ViewChoice, Vec<ViewCut>), String> {
    let sections = stored_sections(&session.doc).unwrap_or(SectionMode::Auto);
    let tangent = stored_tangent_edges(&session.doc).unwrap_or(TangentEdges::Omit);
    let plan = if auto_dimensions {
        plan_sheet_auto_sections_tangent(session, None, 0.01, title, notes, scale.value(), &Annotations::default(), &sections, tangent)
            .map(|(plan, _)| plan)
    } else {
        plan_sheet_with_sections_tangent(session, None, 0.01, title, notes, scale.value(), &Annotations::default(), &sections, tangent)
    }
    .map_err(|e| e.to_string())?;
    Ok((plan.projected, plan.sheet, plan.choice, plan.cuts))
}

/// Lay the sheet out again from cached views (a title, scale or dimension-mode edit).
fn sheet_from(
    session: &solidcraft_engine::Session,
    choice: ViewChoice,
    projected: &[ProjectedView],
    title: &TitleBlock,
    notes: &NotesBlock,
    scale: Scale,
    auto_dimensions: bool,
) -> Result<Sheet, String> {
    if auto_dimensions {
        let views: Vec<_> = projected.iter().map(|p| p.view).collect();
        plan_sheet_auto_reuse(session, choice, &views, projected, title, notes, scale.value(), &Annotations::default())
            .map(|(plan, _)| plan.sheet)
            .map_err(|e| e.to_string())
    } else {
        build_sheet(projected, title, notes, scale.value()).map_err(|e| e.to_string())
    }
}

/// Take up a finished build when its design, revision, title and scale are still current.
fn finish_build(
    app: &mut SolveApp,
    doc: usize,
    revision: u64,
    title: &TitleBlock,
    notes: &NotesBlock,
    scale: Scale,
    auto_dimensions: bool,
    outcome: Result<(Vec<ProjectedView>, Sheet, ViewChoice, Vec<ViewCut>), String>,
) {
    if doc != doc_key(app) || revision != app.session.revision {
        // A stale result (the design changed, or another document is shown) is dropped. An
        // export waiting for a current sheet asks for a build of its own; one for another
        // document is not written at all.
        if doc != doc_key(app) {
            app.drawing_workspace.pending_export = None;
        } else {
            continue_export(app);
        }
        return;
    }
    // The stored spec changed while the build ran (an undo, or another client): the cuts of
    // this result are stale, so start over with the current spec.
    let stored = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
    if app.drawing_workspace.view.sections != stored {
        app.drawing_workspace.view.sections = stored;
        start_build(app);
        return;
    }
    match outcome {
        Ok((projected, sheet, choice, cuts)) => {
            let view = &app.drawing_workspace.view;
            let same = view.title == *title && view.notes == *notes && view.scale == scale && view.auto_dimensions == auto_dimensions;
            // The worker laid out the sheet with the values it started with; if one changed
            // meanwhile, lay it out again here from the (expensive) projection.
            let sheet = if same {
                Ok(sheet)
            } else {
                let (title, notes, scale, auto) = (view.title.clone(), view.notes.clone(), view.scale, view.auto_dimensions);
                sheet_from(&app.session, choice, &projected, &title, &notes, scale, auto)
            };
            let view = &mut app.drawing_workspace.view;
            view.projected = Some(projected);
            view.choice = choice;
            view.cuts = cuts;
            view.revision = Some(revision);
            view.projections = view.projections.saturating_add(1);
            view.dirty = false;
            match sheet {
                Ok(sheet) => {
                    view.sheet = Some(sheet);
                    view.error = None;
                }
                Err(e) => view.error = Some(e),
            }
        }
        Err(e) => {
            let view = &mut app.drawing_workspace.view;
            view.dirty = false;
            view.error = Some(e);
        }
    }
    // An export waiting for this build writes the sheet that is now on screen; a failed sheet
    // reports its error rather than trying to build again.
    if app.drawing_workspace.pending_export.is_some() {
        if let Some(error) = app.drawing_workspace.view.error.clone() {
            app.drawing_workspace.pending_export = None;
            app.set_status(format!("export failed: {error}"), true);
        } else {
            write_pending_export(app);
        }
    }
}

/// Take up a finished background build, if one is waiting.
pub(crate) fn poll_build(app: &mut SolveApp) {
    let Some(build) = app.drawing_workspace.build.as_ref() else { return };
    let outcome = match build.done.try_recv() {
        Ok(outcome) => outcome,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
            app.drawing_workspace.build = None;
            app.drawing_workspace.pending_export = None;
            app.drawing_workspace.view.dirty = false;
            app.drawing_workspace.view.error = Some("the sheet builder stopped".into());
            return;
        }
    };
    let Some(build) = app.drawing_workspace.build.take() else { return };
    finish_build(app, build.doc, build.revision, &build.title, &build.notes, build.scale, build.auto_dimensions, outcome);
}

/// Per-frame work: poll a running build and keep drawing while it runs. Undo and redo restore
/// the stored section spec without a revision change (metadata is not geometry), so the spec is
/// compared every frame and the page rebuilds when it differs.
pub fn tick(app: &mut SolveApp, ctx: &egui::Context) {
    if active(app) {
        let stored = stored_sections(&app.session.doc).unwrap_or(SectionMode::Auto);
        if app.drawing_workspace.view.sections != stored {
            app.drawing_workspace.view.sections = stored;
            app.drawing_workspace.view.gesture = None;
            if app.drawing_workspace.view.selected_section.is_some_and(|letter| !app.drawing_workspace.view.sections.letters().contains(&letter)) {
                app.drawing_workspace.view.selected_section = None;
            }
            if app.drawing_workspace.view.sheet.is_some() {
                // A build in flight used the old spec: cancel it and project the new one.
                start_build(app);
            }
        }
    }
    if app.drawing_workspace.build.is_none() {
        return;
    }
    poll_build(app);
    if app.drawing_workspace.build.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// Rebuild only the sheet from the cached views (a title-block, notes or scale edit); never
/// projects. The values are written to the design when the panel's editors lose focus.
pub fn title_changed(app: &mut SolveApp) {
    let view = &mut app.drawing_workspace.view;
    view.dirty = true;
    view.title_dirty = true;
    if view.projected.is_none() {
        return;
    }
    let result = {
        let view = &app.drawing_workspace.view;
        view.projected
            .as_deref()
            .map(|projected| sheet_from(&app.session, view.choice, projected, &view.title, &view.notes, view.scale, view.auto_dimensions))
    };
    let view = &mut app.drawing_workspace.view;
    match result {
        Some(Ok(sheet)) => {
            view.sheet = Some(sheet);
            view.dirty = false;
            view.error = None;
        }
        Some(Err(e)) => view.error = Some(e),
        None => {}
    }
}

/// Write the panel's title and notes values into the design through `drawing.title.set` (one
/// undo step), once the user has finished editing. A running sheet build is left alone; the next
/// focus loss writes the values.
pub fn save_title(app: &mut SolveApp) {
    let view = &app.drawing_workspace.view;
    if !view.title_dirty || building(app) {
        return;
    }
    let params = json!({
        "benennung": view.title.benennung,
        "zeichnungsnummer": view.title.zeichnungsnummer,
        "werkstoff": view.title.werkstoff,
        "allgemeintoleranz": view.title.allgemeintoleranz,
        "gezeichnet": view.title.gezeichnet,
        "datum": view.title.datum,
        "geprueft": view.title.geprueft,
        "geprueft_datum": view.title.geprueft_datum,
        "aend": view.title.aend,
        "aend_datum": view.title.aend_datum,
        "firma": view.title.firma,
        "rohmass": view.title.rohmass,
        "dokumentart": view.title.dokumentart,
        "blatt": view.title.blatt,
        "notes_block": {
            "enabled": view.notes.enabled,
            "ra": view.notes.ra,
            "edge_note": view.notes.edge_note,
            "extra_notes": view.notes.extra_notes,
        },
    });
    match app.run("drawing.title.set", params) {
        Ok(_) => {
            let revision = app.session.revision;
            let view = &mut app.drawing_workspace.view;
            view.title_dirty = false;
            // The metadata write is not a geometry change: the sheet on screen is current.
            view.revision = Some(revision);
        }
        Err(e) => app.drawing_workspace.view.error = Some(e),
    }
}

/// Choose a scale and lay the sheet out again (no projection).
pub fn set_scale(app: &mut SolveApp, scale: Scale) {
    app.drawing_workspace.view.scale = scale;
    title_changed(app);
}

/// The tangent-edge rule the page was built with, as the design stores it.
pub fn tangent_edges_of(app: &SolveApp) -> TangentEdges {
    stored_tangent_edges(&app.session.doc).unwrap_or(TangentEdges::Omit)
}

/// Store the tangent-edge rule (one undo step through `drawing.tangent_edges.set`) and rebuild
/// the page: tangent edges are dropped or drawn during projection, so the views are re-projected.
pub fn set_tangent_edges(app: &mut SolveApp, mode: TangentEdges) {
    match app.run("drawing.tangent_edges.set", json!({ "tangent_edges": mode.name() })) {
        Ok(_) => start_build(app),
        Err(e) => app.set_status(format!("tangent edges: {e}"), true),
    }
}

// ---------------------------------------------------------------- D9.2: the Section tool

/// Store a sections spec with the design (one undo step through `drawing.sections.set`) and
/// rebuild the page. The command validates the value before anything is written.
pub fn set_section_spec(app: &mut SolveApp, mode: SectionMode) {
    let value = section_mode_value(&mode);
    match app.run("drawing.sections.set", json!({ "sections": value })) {
        Ok(_) => {
            let view = &mut app.drawing_workspace.view;
            view.sections = mode;
            view.gesture = None;
            if view.selected_section.is_some_and(|letter| !view.sections.letters().contains(&letter)) {
                view.selected_section = None;
            }
            start_build(app);
        }
        Err(e) => app.set_status(format!("section: {e}"), true),
    }
}

/// The selection the sheet's views make, for deriving a line section's view.
fn view_selection(view: &ViewState) -> Option<ViewSelection> {
    let sheet = view.sheet.as_ref()?;
    Some(ViewSelection { views: sheet.views.iter().map(|v| v.view).collect(), choice: view.choice })
}

/// The line a full section is drawn or edited from: the stored line, or the trace of an explicit
/// plane in the sheet's other view. The line is in the picked view's millimetres.
fn line_of_spec(spec: &SectionSpec, sheet: &Sheet) -> Option<SectionLine> {
    match spec {
        SectionSpec::FullFrom { from, .. } => Some(*from),
        SectionSpec::Full { view, plane, .. } => line_for_plane(*view, *plane, sheet),
        SectionSpec::Breakout { .. } => None,
    }
}

/// The trace of `plane` in a view of the sheet other than the sectioned one, extended past the
/// view's outline by the 6 mm section ends. The engine draws the same line in that view.
fn line_for_plane(sectioned: StandardView, plane: solidcraft_engine::drawing::CuttingPlane, sheet: &Sheet) -> Option<SectionLine> {
    let scale = sheet.scale_factor();
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    for placed in &sheet.views {
        if placed.view == sectioned {
            continue;
        }
        let camera = standard_camera(placed.view);
        let (right, up, back) = camera.basis();
        let trace = plane.normal.cross(back);
        if trace.len() < 1e-6 {
            continue;
        }
        let dir = Vec2::new(trace.dot(right), trace.dot(up)).normalized()?;
        let centre = Vec2::new((plane.point - camera.target).dot(right), (plane.point - camera.target).dot(up));
        let lo = (placed.bbox[0] - placed.origin) * (1.0 / scale);
        let hi = (placed.bbox[1] - placed.origin) * (1.0 / scale);
        let half = [lo, hi, Vec2::new(lo.x, hi.y), Vec2::new(hi.x, lo.y)].iter().map(|p| (*p - centre).dot(dir).abs()).fold(0.0, f64::max);
        if half.is_finite() && half > 0.0 {
            let reach = half + SECTION_END_MM / scale;
            return Some(SectionLine { view: placed.view, line: [centre - dir * reach, centre + dir * reach] });
        }
    }
    None
}

/// The sectioned view a spec names (the derived one for a line section).
fn sectioned_view_of(spec: &SectionSpec, view: &ViewState) -> Option<StandardView> {
    match spec {
        SectionSpec::Full { view, .. } => Some(*view),
        SectionSpec::Breakout { view, .. } => Some(*view),
        SectionSpec::FullFrom { from, .. } => {
            view_selection(view).and_then(|selection| derive_full_from(&selection, from).ok().map(|(view, _)| view))
        }
    }
}

/// The first free section letter (A…Z); `None` when all 26 are in use.
fn next_section_letter(used: &[char]) -> Option<char> {
    (0..26).map(section_letter).find(|letter| !used.contains(letter))
}

/// The explicit entries that represent the last automatic build, with letters. A full section
/// keeps its drawn letter and becomes a line section when the sheet has another view to pick the
/// line in; a break-out's freehand window becomes an `around` circle of its smaller half axis
/// (the explicit grammar has circles only).
fn materialise_auto(view: &ViewState) -> Vec<SectionSpec> {
    let mut used: Vec<char> = Vec::new();
    let mut out = Vec::new();
    let Some(sheet) = view.sheet.as_ref() else { return out };
    for cut in &view.cuts {
        let Some(letter) = cut.label.and_then(|letter| (!used.contains(&letter)).then_some(letter)).or_else(|| next_section_letter(&used)) else {
            break;
        };
        used.push(letter);
        if let Some(boundary) = &cut.boundary {
            out.push(SectionSpec::Breakout {
                view: cut.view,
                around: [boundary.centre.x, boundary.centre.y, boundary.rx.min(boundary.ry)],
                letter: Some(letter),
            });
        } else if let Some(from) = line_for_plane(cut.view, cut.plane, sheet) {
            out.push(SectionSpec::FullFrom { from, letter: Some(letter) });
        } else {
            out.push(SectionSpec::Full { view: cut.view, plane: cut.plane, letter: Some(letter) });
        }
    }
    out
}

/// The explicit list a new entry is added to. The first edit while the spec is `auto` writes the
/// automatic sections out as explicit entries first, so the user's auto sections do not vanish;
/// after `none` the list starts empty.
fn explicit_base(view: &ViewState) -> Vec<SectionSpec> {
    match &view.sections {
        SectionMode::Auto => materialise_auto(view),
        SectionMode::None => Vec::new(),
        SectionMode::Explicit(specs) => specs.clone(),
    }
}

/// The view's placed rectangle on the page, in screen points, and its view millimetres' bounding
/// box. `None` when the view has no drawn geometry.
fn placed_screen_rect(map: PageMap, placed: &PlacedView, scale: f64) -> Option<Rect> {
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let rect = Rect::from_two_pos(map.to(placed.bbox[0].x, placed.bbox[0].y), map.to(placed.bbox[1].x, placed.bbox[1].y));
    (rect.is_finite() && rect.width() > 1.0 && rect.height() > 1.0).then_some(rect)
}

/// Screen points → sheet mm → the placed view's own millimetres. The one transform the page,
/// the spec and the exports share; a view mm value does not depend on the sheet scale.
fn screen_to_view(map: PageMap, placed: &PlacedView, scale: f64, at: Pos2) -> Vec2 {
    let (x, y) = map.to_sheet(at);
    Vec2::new((x - placed.origin.x) / scale, (y - placed.origin.y) / scale)
}

/// View millimetres → screen points.
fn view_to_screen(map: PageMap, placed: &PlacedView, scale: f64, p: Vec2) -> Pos2 {
    map.to(placed.origin.x + p.x * scale, placed.origin.y + p.y * scale)
}

/// Snap a picked point to the view's centre lines and circle centres within `tolerance` view mm.
fn snap_to_centres(point: Vec2, centres: &[ViewCentre], tolerance: f64) -> Vec2 {
    if !(tolerance.is_finite() && tolerance > 0.0) {
        return point;
    }
    // A circle centre is a point feature and gets the first say; a centre line only catches a
    // point that is not already near a centre.
    let mut best = point;
    let mut best_distance = tolerance;
    for centre in centres {
        if let ViewCentre::Cross { c, .. } = centre {
            let distance = c.dist(point);
            if distance < best_distance {
                best = *c;
                best_distance = distance;
            }
        }
    }
    if best_distance < tolerance {
        return best;
    }
    for centre in centres {
        match centre {
            ViewCentre::Line { a, b } => {
                let d = *b - *a;
                let len2 = d.len2();
                if len2 > 1e-18 {
                    let t = ((point - *a).dot(d) / len2).clamp(0.0, 1.0);
                    let foot = *a + d * t;
                    let distance = foot.dist(point);
                    if distance < best_distance {
                        best = foot;
                        best_distance = distance;
                    }
                }
            }
            ViewCentre::Cross { c, .. } => {
                for candidate in [Vec2::new(c.x, point.y), Vec2::new(point.x, c.y)] {
                    let distance = candidate.dist(point);
                    if distance < best_distance {
                        best = candidate;
                        best_distance = distance;
                    }
                }
            }
        }
    }
    best
}

/// Snap a nearly horizontal or vertical line's second point (within 3°) onto the axis through
/// the first one.
fn snap_direction(anchor: Vec2, current: Vec2) -> Vec2 {
    let d = current - anchor;
    if !(d.len() > 1e-9) {
        return current;
    }
    let limit = 3.0_f64.to_radians().tan();
    let horizontal = d.y.abs() <= d.x.abs() * limit;
    let vertical = d.x.abs() <= d.y.abs() * limit;
    if horizontal && !vertical {
        Vec2::new(current.x, anchor.y)
    } else if vertical && !horizontal {
        Vec2::new(anchor.x, current.y)
    } else {
        current
    }
}

/// Extend the line `a`→`b` through `bbox` (view mm) by `margin` on both ends: the cutting line
/// runs through the whole view outline plus the 6 mm section ends.
fn extend_line(a: Vec2, b: Vec2, bbox: [Vec2; 2], margin: f64) -> [Vec2; 2] {
    let d = b - a;
    if !(d.len() > 1e-9) || !(margin.is_finite() && margin > 0.0) {
        return [a, b];
    }
    let lo = bbox[0] - Vec2::new(margin, margin);
    let hi = bbox[1] + Vec2::new(margin, margin);
    if !(lo.x.is_finite() && lo.y.is_finite() && hi.x.is_finite() && hi.y.is_finite()) || lo.x >= hi.x || lo.y >= hi.y {
        return [a, b];
    }
    let mut t0 = f64::NEG_INFINITY;
    let mut t1 = f64::INFINITY;
    for (p, d, lo, hi) in [(a.x, d.x, lo.x, hi.x), (a.y, d.y, lo.y, hi.y)] {
        if d.abs() <= 1e-12 {
            if p < lo || p > hi {
                return [a, b];
            }
        } else {
            let (ta, tb) = ((lo - p) / d, (hi - p) / d);
            t0 = t0.max(ta.min(tb));
            t1 = t1.min(ta.max(tb));
        }
    }
    if t0.is_finite() && t1.is_finite() && t1 > t0 { [a + d * t0, a + d * t1] } else { [a, b] }
}

/// The view's bounding box in its own millimetres.
fn view_bbox(placed: &PlacedView, scale: f64) -> [Vec2; 2] {
    [(placed.bbox[0] - placed.origin) * (1.0 / scale), (placed.bbox[1] - placed.origin) * (1.0 / scale)]
}

/// The freehand window of a break-out spec, in its view's millimetres.
fn breakout_window(around: [f64; 3]) -> Vec<Vec2> {
    Boundary::circle(around[0], around[1], around[2]).points()
}

/// The page position of one selected section's handles.
#[derive(Clone, Copy)]
struct HandleAt {
    handle: SectionHandle,
    view: StandardView,
    at: Pos2,
}

/// The handles a selected section shows: the line's two ends, or the break-out's centre and
/// radius. Sheet millimetres → screen through the same page map as the drawing.
fn handles_of(spec: &SectionSpec, map: PageMap, sheet: &Sheet) -> Vec<HandleAt> {
    let scale = sheet.scale_factor();
    let placed = |view: StandardView| sheet.views.iter().find(|p| p.view == view);
    match spec {
        SectionSpec::Breakout { view, around, .. } => {
            let Some(placed) = placed(*view) else { return Vec::new() };
            let centre = Vec2::new(around[0], around[1]);
            vec![
                HandleAt { handle: SectionHandle::BreakoutCentre, view: *view, at: view_to_screen(map, placed, scale, centre) },
                HandleAt {
                    handle: SectionHandle::BreakoutRadius,
                    view: *view,
                    at: view_to_screen(map, placed, scale, centre + Vec2::new(around[2], 0.0)),
                },
            ]
        }
        _ => {
            let Some(line) = line_of_spec(spec, sheet) else { return Vec::new() };
            let Some(placed) = placed(line.view) else { return Vec::new() };
            vec![
                HandleAt { handle: SectionHandle::LineA, view: line.view, at: view_to_screen(map, placed, scale, line.line[0]) },
                HandleAt { handle: SectionHandle::LineB, view: line.view, at: view_to_screen(map, placed, scale, line.line[1]) },
            ]
        }
    }
}

/// The written points of a selected section's line or window, in screen points.
fn outline_of(spec: &SectionSpec, map: PageMap, sheet: &Sheet) -> Vec<Pos2> {
    let scale = sheet.scale_factor();
    match spec {
        SectionSpec::Breakout { view, around, .. } => {
            let Some(placed) = sheet.views.iter().find(|p| p.view == *view) else { return Vec::new() };
            breakout_window(*around).into_iter().map(|p| view_to_screen(map, placed, scale, p)).collect()
        }
        _ => {
            let Some(line) = line_of_spec(spec, sheet) else { return Vec::new() };
            let Some(placed) = sheet.views.iter().find(|p| p.view == line.view) else { return Vec::new() };
            vec![view_to_screen(map, placed, scale, line.line[0]), view_to_screen(map, placed, scale, line.line[1])]
        }
    }
}

/// The selected section's spec and its assigned letter.
fn selected_spec(view: &ViewState) -> Option<(char, SectionSpec)> {
    let letter = view.selected_section?;
    let SectionMode::Explicit(specs) = &view.sections else { return None };
    let letters = assigned_letters(specs).ok()?;
    let index = letters.iter().position(|l| *l == letter)?;
    Some((letter, specs.get(index)?.clone()))
}

/// The spec of an explicit entry by its assigned letter.
fn selected_spec_for(view: &ViewState, letter: char) -> Option<SectionSpec> {
    let index = spec_index(view, letter)?;
    let SectionMode::Explicit(specs) = &view.sections else { return None };
    specs.get(index).cloned()
}

/// The section an entry's letter identifies, by the engine's assignment (explicit or A, B, C).
fn spec_index(view: &ViewState, letter: char) -> Option<usize> {
    let SectionMode::Explicit(specs) = &view.sections else { return None };
    assigned_letters(specs).ok()?.iter().position(|l| *l == letter)
}

/// Delete the section `letter` (one undo step). Exactly one entry goes.
pub fn delete_section(app: &mut SolveApp, letter: char) {
    let SectionMode::Explicit(mut specs) = app.drawing_workspace.view.sections.clone() else { return };
    let Some(index) = spec_index(&app.drawing_workspace.view, letter) else { return };
    specs.remove(index);
    app.drawing_workspace.view.selected_section = None;
    set_section_spec(app, SectionMode::Explicit(specs));
}

/// A placed view of the current sheet.
fn placed(sheet: &Sheet, view: StandardView) -> Option<&PlacedView> {
    sheet.views.iter().find(|p| p.view == view)
}

/// The projected view (for its centre lines) by view.
fn projected_view(view: &ViewState, name: StandardView) -> Option<&ProjectedView> {
    view.projected.as_ref()?.iter().find(|p| p.view == name)
}

/// The letters the current spec uses, including the automatic ones the first edit materialises.
fn gesture_letters(view: &ViewState) -> Vec<char> {
    match &view.sections {
        SectionMode::Explicit(specs) => assigned_letters(specs).unwrap_or_default(),
        SectionMode::Auto => (0..view.cuts.len()).map(section_letter).collect(),
        SectionMode::None => Vec::new(),
    }
}

/// The letter the preview line carries: the next free one.
fn preview_letter(view: &ViewState) -> char {
    next_section_letter(&gesture_letters(view)).unwrap_or('A')
}

/// A gesture's point in view millimetres: screen → sheet → view, then the view's centre-line and
/// circle-centre snap (2 mm on paper), then horizontal/vertical (3°) when the gesture is a line.
fn gesture_point(
    view: &ViewState,
    name: StandardView,
    map: PageMap,
    sheet: &Sheet,
    pos: Pos2,
    anchor: Option<Vec2>,
    directional: bool,
) -> Option<Vec2> {
    let scale = sheet.scale_factor();
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let placed = placed(sheet, name)?;
    let mut point = screen_to_view(map, placed, scale, pos);
    if let Some(projected) = projected_view(view, name) {
        point = snap_to_centres(point, &projected.centre, 2.0 / scale);
    }
    if directional && let Some(anchor) = anchor {
        point = snap_direction(anchor, point);
    }
    point.is_finite().then_some(point)
}

/// Whether a gesture's pointer snaps horizontally/vertically.
fn gesture_is_directional(gesture: &SectionGesture) -> bool {
    match gesture.edit {
        Some((_, SectionHandle::LineA | SectionHandle::LineB)) => true,
        Some(_) => false,
        None => gesture.mode == SectionToolMode::Full,
    }
}

/// Write a finished new-section gesture: the line's derived plane is checked first (a cut no view
/// looks along reports the engine's error and writes nothing), then the entry is appended to the
/// explicit list — materialising the automatic sections on the first edit while the spec is auto.
fn complete_gesture(app: &mut SolveApp, gesture: SectionGesture) {
    let outcome: Result<Vec<SectionSpec>, String> = (|| {
        let view = &app.drawing_workspace.view;
        let sheet = view.sheet.as_ref().ok_or("there is no sheet to place a section on")?;
        let scale = sheet.scale_factor();
        let placed = placed(sheet, gesture.view).ok_or("the picked view is not on the sheet")?;
        let mut specs = explicit_base(view);
        let used = assigned_letters(&specs).unwrap_or_default();
        let letter = next_section_letter(&used).ok_or("all 26 section letters are in use")?;
        match gesture.mode {
            SectionToolMode::Full => {
                let bbox = view_bbox(placed, scale);
                let [a, b] = extend_line(gesture.anchor, gesture.current, bbox, SECTION_END_MM / scale);
                if a.dist(b) <= 1e-6 {
                    return Err("the cutting line has no length".into());
                }
                let from = SectionLine { view: gesture.view, line: [a, b] };
                let selection = view_selection(view).ok_or("there is no sheet to place a section on")?;
                derive_full_from(&selection, &from).map_err(|e| e.to_string())?;
                specs.push(SectionSpec::FullFrom { from, letter: Some(letter) });
            }
            SectionToolMode::Breakout => {
                let radius = gesture.anchor.dist(gesture.current);
                if !(radius.is_finite() && radius > 0.05) {
                    return Err("the break-out radius is too small".into());
                }
                specs.push(SectionSpec::Breakout { view: gesture.view, around: [gesture.anchor.x, gesture.anchor.y, radius], letter: Some(letter) });
            }
        }
        Ok(specs)
    })();
    match outcome {
        Ok(specs) => set_section_spec(app, SectionMode::Explicit(specs)),
        Err(error) => app.set_status(format!("section: {error}"), true),
    }
}

/// Rewrite the entry a handle drag edits, one write per release.
fn rewrite_handle(app: &mut SolveApp, gesture: SectionGesture) {
    let Some((letter, handle)) = gesture.edit else { return };
    let outcome: Result<Option<Vec<SectionSpec>>, String> = (|| {
        let view = &app.drawing_workspace.view;
        let sheet = view.sheet.as_ref().ok_or("there is no sheet")?;
        let Some(index) = spec_index(view, letter) else { return Err(format!("section `{letter}` is gone")) };
        let SectionMode::Explicit(mut specs) = view.sections.clone() else { return Err("there are no explicit sections".into()) };
        let spec = specs.get(index).cloned().ok_or_else(|| format!("section `{letter}` is gone"))?;
        let current = gesture.current;
        if !current.is_finite() {
            return Ok(None);
        }
        let changed = match (&spec, handle) {
            (SectionSpec::FullFrom { .. } | SectionSpec::Full { .. }, SectionHandle::LineA | SectionHandle::LineB) => {
                let Some(mut line) = line_of_spec(&spec, sheet) else { return Ok(None) };
                let other = match handle {
                    SectionHandle::LineA => line.line[1],
                    _ => line.line[0],
                };
                if current.dist(other) <= 1e-6 {
                    return Ok(None);
                }
                match handle {
                    SectionHandle::LineA => line.line[0] = current,
                    _ => line.line[1] = current,
                }
                SectionSpec::FullFrom { from: line, letter: Some(letter) }
            }
            (SectionSpec::Breakout { view, around, .. }, SectionHandle::BreakoutCentre) => {
                SectionSpec::Breakout { view: *view, around: [current.x, current.y, around[2]], letter: Some(letter) }
            }
            (SectionSpec::Breakout { view, around, .. }, SectionHandle::BreakoutRadius) => {
                let radius = Vec2::new(around[0], around[1]).dist(current);
                if !(radius.is_finite() && radius > 0.05) {
                    return Ok(None);
                }
                SectionSpec::Breakout { view: *view, around: [around[0], around[1], radius], letter: Some(letter) }
            }
            _ => return Ok(None),
        };
        specs[index] = changed;
        Ok(Some(specs))
    })();
    match outcome {
        Ok(Some(specs)) => {
            set_section_spec(app, SectionMode::Explicit(specs));
            app.drawing_workspace.view.selected_section = Some(letter);
        }
        Ok(None) => {}
        Err(error) => app.set_status(format!("section: {error}"), true),
    }
}

/// The point of a screen segment nearest `p`, for hit tests (screen points).
fn screen_segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f64 {
    let d = b - a;
    let len2 = d.length_sq();
    if !(len2 > 1e-12) {
        return f64::from(p.distance(a));
    }
    let t = (f64::from((p - a).dot(d)) / f64::from(len2)).clamp(0.0, 1.0) as f32;
    f64::from(p.distance(a + d * t))
}

/// The selected section's handle under `pos` (10 px reach), with the anchor a line drag snaps to.
fn hit_handle(view: &ViewState, map: PageMap, sheet: &Sheet, pos: Pos2) -> Option<(SectionHandle, StandardView, Vec2)> {
    let (_letter, spec) = selected_spec(view)?;
    let line = line_of_spec(&spec, sheet);
    let mut best: Option<(f64, SectionHandle, StandardView, Vec2)> = None;
    for handle in handles_of(&spec, map, sheet) {
        let distance = f64::from(handle.at.distance(pos));
        if distance > 10.0 {
            continue;
        }
        let anchor = match handle.handle {
            SectionHandle::LineA => line.map(|l| l.line[1]),
            SectionHandle::LineB => line.map(|l| l.line[0]),
            SectionHandle::BreakoutCentre => match spec {
                SectionSpec::Breakout { around, .. } => Some(Vec2::new(around[0], around[1])),
                _ => None,
            },
            SectionHandle::BreakoutRadius => match spec {
                SectionSpec::Breakout { around, .. } => Some(Vec2::new(around[0], around[1])),
                _ => None,
            },
        };
        let Some(anchor) = anchor else { continue };
        if best.is_none_or(|(best_distance, ..)| distance < best_distance) {
            best = Some((distance, handle.handle, handle.view, anchor));
        }
    }
    best.map(|(_, handle, view, anchor)| (handle, view, anchor))
}

/// A section line or break-out window under `pos` (5 px reach), with the view it belongs to.
fn hit_section(view: &ViewState, map: PageMap, sheet: &Sheet, pos: Pos2) -> Option<(char, StandardView)> {
    let SectionMode::Explicit(specs) = &view.sections else { return None };
    let letters = assigned_letters(specs).ok()?;
    let scale = sheet.scale_factor();
    for (spec, letter) in specs.iter().zip(letters) {
        match spec {
            SectionSpec::Breakout { view: breakout, around, .. } => {
                let Some(placed) = placed(sheet, *breakout) else { continue };
                let window = breakout_window(*around);
                let point = screen_to_view(map, placed, scale, pos);
                let near_edge = window.windows(2).any(|pair| {
                    let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else { return false };
                    point_segment_distance(point, *a, *b) <= 5.0 / scale
                });
                if inside_polygon(point, &window) || near_edge {
                    return Some((letter, *breakout));
                }
            }
            _ => {
                let Some(line) = line_of_spec(spec, sheet) else { continue };
                let Some(placed) = placed(sheet, line.view) else { continue };
                let a = view_to_screen(map, placed, scale, line.line[0]);
                let b = view_to_screen(map, placed, scale, line.line[1]);
                if screen_segment_distance(pos, a, b) <= 5.0 {
                    return Some((letter, line.view));
                }
            }
        }
    }
    None
}

/// The point → segment distance in view millimetres.
fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let d = b - a;
    let len2 = d.len2();
    if !(len2 > 1e-18) {
        return p.dist(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.dist(a + d * t)
}

/// The view under `pos`, last drawn first (popups aside, the sheet's views do not overlap).
fn view_under(sheet: &Sheet, map: PageMap, pos: Pos2) -> Option<StandardView> {
    let scale = sheet.scale_factor();
    sheet.views.iter().rev().find(|placed| placed_screen_rect(map, placed, scale).is_some_and(|rect| rect.contains(pos))).map(|placed| placed.view)
}

/// Publish every placed view's page rect and its centre/edge handles, and draw the tool's
/// overlay: the hovered view's outline, the selected section's handles and the live preview.
fn paint_section_overlay(app: &SolveApp, painter: &egui::Painter, map: PageMap, page: Rect, sheet: &Sheet, pointer: Option<Pos2>) {
    let view = &app.drawing_workspace.view;
    let scale = sheet.scale_factor();
    let t = Tokens::get();
    for placed in &sheet.views {
        let Some(rect) = placed_screen_rect(map, placed, scale) else { continue };
        let name = format!("drawing:view:{}", view_name(placed.view));
        crate::scenario::publish_rect(&name, rect);
        crate::scenario::publish_handle(&name, rect.center());
        crate::scenario::publish_handle(&format!("{name}:center"), rect.center());
        crate::scenario::publish_handle(&format!("{name}:left"), rect.left_center());
        crate::scenario::publish_handle(&format!("{name}:right"), rect.right_center());
        crate::scenario::publish_handle(&format!("{name}:top"), rect.center_top());
        crate::scenario::publish_handle(&format!("{name}:bottom"), rect.center_bottom());
        // The view's centre crosses (circle centres): the centre and an arm end, so a scenario
        // can pick a cut through a feature and drag a break-out radius from it.
        if let Some(projected) = projected_view(view, placed.view) {
            let bbox = view_bbox(placed, scale);
            let diagonal = bbox[0].dist(bbox[1]);
            let mut cross_index = 0usize;
            let mut line_index = 0usize;
            let mut hole_index = 0usize;
            for centre in &projected.centre {
                match *centre {
                    ViewCentre::Cross { c, radius } => {
                        let at = view_to_screen(map, placed, scale, c);
                        let arm = radius + 3.0;
                        crate::scenario::publish_handle(&format!("{name}:centre"), at);
                        crate::scenario::publish_handle(&format!("{name}:centre:{cross_index}"), at);
                        crate::scenario::publish_handle(
                            &format!("{name}:centre:{cross_index}:x"),
                            view_to_screen(map, placed, scale, c + Vec2::new(arm, 0.0)),
                        );
                        crate::scenario::publish_handle(
                            &format!("{name}:centre:{cross_index}:y"),
                            view_to_screen(map, placed, scale, c + Vec2::new(0.0, arm)),
                        );
                        cross_index += 1;
                    }
                    ViewCentre::Line { a, b } => {
                        let mid = (a + b) * 0.5;
                        let at = view_to_screen(map, placed, scale, mid);
                        crate::scenario::publish_handle(&format!("{name}:centre_line:{line_index}"), at);
                        crate::scenario::publish_handle(&format!("{name}:centre_line:{line_index}:a"), view_to_screen(map, placed, scale, a));
                        crate::scenario::publish_handle(&format!("{name}:centre_line:{line_index}:b"), view_to_screen(map, placed, scale, b));
                        // A short centre line is a cross hole's own axis: its midpoint is the
                        // hole's centre, and `:out` is a point a little way from it.
                        if a.dist(b) <= 0.25 * diagonal {
                            crate::scenario::publish_handle(&format!("{name}:hole:{hole_index}"), at);
                            crate::scenario::publish_handle(&format!("{name}:hole:{hole_index}:out"), at + vec2(18.0, 0.0));
                            hole_index += 1;
                        }
                        line_index += 1;
                    }
                }
            }
        }
        if view.section_tool && pointer.is_some_and(|p| rect.contains(p) && !crate::shell::covered(app, p)) {
            painter.rect_stroke(rect.expand(2.0), 2.0, Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
        }
    }
    if let Some((letter, spec)) = selected_spec(view) {
        let outline = outline_of(&spec, map, sheet);
        if matches!(spec, SectionSpec::Breakout { .. }) {
            if outline.len() >= 2 {
                let mut closed = outline.clone();
                if let Some(first) = closed.first().copied() {
                    closed.push(first);
                }
                painter.add(egui::Shape::line(closed, Stroke::new(1.5, t.accent)));
            }
        } else if let [a, b] = outline.as_slice() {
            painter.line_segment([*a, *b], Stroke::new(1.5, t.accent));
        }
        for handle in handles_of(&spec, map, sheet) {
            crate::scenario::publish_handle(&format!("drawing:section:handle:{letter}:{}", handle.handle.suffix()), handle.at);
            let r = Rect::from_center_size(handle.at, vec2(9.0, 9.0));
            match handle.handle {
                SectionHandle::LineA | SectionHandle::LineB => {
                    painter.rect_filled(r, 1.0, t.accent);
                    painter.rect_stroke(r, 1.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Inside);
                }
                _ => {
                    painter.circle_filled(handle.at, 5.0, t.accent);
                    painter.circle_stroke(handle.at, 5.0, Stroke::new(1.0, Color32::WHITE));
                }
            }
        }
    }
    let _ = page;
    if let Some(gesture) = &view.gesture {
        paint_gesture(painter, map, sheet, view, gesture, &t);
    }
}

/// The live preview of a gesture: the section line in the section-line style with the next free
/// letter, or the freehand break-out window.
fn paint_gesture(painter: &egui::Painter, map: PageMap, sheet: &Sheet, view: &ViewState, gesture: &SectionGesture, t: &Tokens) {
    let scale = sheet.scale_factor();
    let Some(placed) = placed(sheet, gesture.view) else { return };
    if let Some((letter, handle)) = gesture.edit {
        let Some((_, spec)) = (|| {
            let SectionMode::Explicit(specs) = &view.sections else { return None };
            let index = spec_index(view, letter)?;
            Some((letter, specs.get(index)?.clone()))
        })() else {
            return;
        };
        match handle {
            SectionHandle::LineA | SectionHandle::LineB => {
                let Some(mut line) = line_of_spec(&spec, sheet) else { return };
                if handle == SectionHandle::LineA {
                    line.line[0] = gesture.current;
                } else {
                    line.line[1] = gesture.current;
                }
                let a = view_to_screen(map, placed, scale, line.line[0]);
                let b = view_to_screen(map, placed, scale, line.line[1]);
                painter.line_segment([a, b], Stroke::new(1.5, t.accent));
                painter.circle_filled(a, 5.0, t.accent);
                painter.circle_filled(b, 5.0, t.accent);
            }
            SectionHandle::BreakoutCentre | SectionHandle::BreakoutRadius => {
                let SectionSpec::Breakout { around, .. } = spec else { return };
                let centre = Vec2::new(around[0], around[1]);
                let centre = if handle == SectionHandle::BreakoutCentre { gesture.current } else { centre };
                let radius = if handle == SectionHandle::BreakoutRadius { centre.dist(gesture.current) } else { around[2] };
                paint_window(painter, map, placed, scale, centre, radius, t);
            }
        }
        return;
    }
    match gesture.mode {
        SectionToolMode::Full => {
            let bbox = view_bbox(placed, scale);
            let [a, b] = extend_line(gesture.anchor, gesture.current, bbox, SECTION_END_MM / scale);
            let pa = view_to_screen(map, placed, scale, a);
            let pb = view_to_screen(map, placed, scale, b);
            let stroke = Stroke::new(1.5, t.accent);
            let dash = (1.5 * scale).max(2.0) as f32;
            let gap = (1.0 * scale).max(1.5) as f32;
            painter.extend(egui::Shape::dashed_line(&[pa, pb], stroke, dash, gap));
            if pa.distance(pb) > 1.0 {
                let dir = (pb - pa).normalized();
                for (p, sign) in [(pa, -1.0), (pb, 1.0)] {
                    let n = vec2(-dir.y, dir.x) * 6.0;
                    painter.line_segment([p - n, p + n], Stroke::new(2.0, t.accent));
                    let letter = preview_letter(view).to_string();
                    let at = p + vec2(0.0, if sign > 0.0 { 12.0 } else { -4.0 });
                    painter.text(at, Align2::CENTER_BOTTOM, letter, FontId::proportional(14.0), SHEET_INK);
                }
            }
        }
        SectionToolMode::Breakout => {
            let radius = gesture.anchor.dist(gesture.current);
            if radius > 0.01 {
                paint_window(painter, map, placed, scale, gesture.anchor, radius, t);
            }
        }
    }
}

/// A break-out window in the freehand style.
fn paint_window(painter: &egui::Painter, map: PageMap, placed: &PlacedView, scale: f64, centre: Vec2, radius: f64, t: &Tokens) {
    if !(radius.is_finite() && radius > 0.0) {
        return;
    }
    let points: Vec<Pos2> =
        Boundary::circle(centre.x, centre.y, radius).points().into_iter().map(|p| view_to_screen(map, placed, scale, p)).collect();
    if points.len() < 2 {
        return;
    }
    let mut closed = points.clone();
    if let Some(first) = closed.first().copied() {
        closed.push(first);
    }
    painter.add(egui::Shape::line(closed, Stroke::new(1.5, t.accent)));
}

/// Read this frame's pointer and keyboard input and drive the Section tool with it.
fn process_section_input(app: &mut SolveApp, ui: &egui::Ui, rect: Rect, map: PageMap) {
    let (pointer, pressed, released) = ui.ctx().input(|i| (i.pointer.hover_pos(), i.pointer.primary_pressed(), i.pointer.primary_released()));
    let keyboard_free = !ui.ctx().egui_wants_keyboard_input();
    if keyboard_free {
        let (escape, delete) =
            ui.ctx().input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)));
        if delete && let Some(letter) = app.drawing_workspace.view.selected_section {
            delete_section(app, letter);
            return;
        }
        if escape {
            let view = &mut app.drawing_workspace.view;
            if view.gesture.is_some() {
                view.gesture = None;
            } else if view.section_tool {
                view.section_tool = false;
                view.picked_view = None;
                view.selected_section = None;
            } else {
                view.selected_section = None;
            }
            return;
        }
    }
    let on_page = pointer.is_some_and(|p| rect.contains(p) && !crate::shell::covered(app, p));
    let sheet_present = app.drawing_workspace.view.sheet.is_some();
    if pressed && on_page && sheet_present {
        let pos = pointer.unwrap_or(rect.center());
        // Hits are computed from one immutable look at the sheet.
        let (handle_hit, section_hit, view_hit) = {
            let view = &app.drawing_workspace.view;
            let Some(sheet) = view.sheet.as_ref() else { return };
            (
                hit_handle(view, map, sheet, pos),
                if view.section_tool { hit_section(view, map, sheet, pos) } else { None },
                if view.section_tool { view_under(sheet, map, pos) } else { None },
            )
        };
        let tool = app.drawing_workspace.view.section_tool;
        if let Some((handle, handle_view, anchor)) = handle_hit {
            let current = app.drawing_workspace.view.selected_section.and_then(|_letter| {
                let view = &app.drawing_workspace.view;
                let sheet = view.sheet.as_ref()?;
                gesture_point(view, handle_view, map, sheet, pos, Some(anchor), matches!(handle, SectionHandle::LineA | SectionHandle::LineB))
            });
            if let (Some(letter), Some(current)) = (app.drawing_workspace.view.selected_section, current) {
                app.drawing_workspace.view.gesture = Some(SectionGesture {
                    view: handle_view,
                    mode: app.drawing_workspace.view.section_mode,
                    stage: GestureStage::Handle,
                    anchor,
                    current,
                    press_at: pos,
                    edit: Some((letter, handle)),
                });
            }
        } else if let Some((letter, section_view)) = section_hit {
            let view = &mut app.drawing_workspace.view;
            view.selected_section = Some(letter);
            view.picked_view = Some(section_view);
        } else if let Some(name) = view_hit {
            let view = &mut app.drawing_workspace.view;
            if view.picked_view != Some(name) {
                view.picked_view = Some(name);
                view.selected_section = None;
            } else if let Some(gesture) = view.gesture.as_mut()
                && gesture.stage == GestureStage::Armed
                && gesture.edit.is_none()
                && gesture.view == name
            {
                gesture.stage = GestureStage::SecondPress;
                gesture.press_at = pos;
            } else if let Some(sheet) = view.sheet.as_ref()
                && let Some(anchor) = gesture_point(view, name, map, sheet, pos, None, false)
            {
                view.gesture = Some(SectionGesture {
                    view: name,
                    mode: view.section_mode,
                    stage: GestureStage::FirstPress,
                    anchor,
                    current: anchor,
                    press_at: pos,
                    edit: None,
                });
                view.selected_section = None;
            }
        } else if !tool {
            app.drawing_workspace.view.selected_section = None;
        }
    }
    // The preview follows the pointer, with the view's snap.
    if pointer.is_some() && !crate::shell::covered(app, pointer.unwrap_or(rect.center())) {
        let pos = pointer.unwrap_or(rect.center());
        let current = {
            let view = &app.drawing_workspace.view;
            view.gesture.as_ref().and_then(|gesture| {
                let sheet = view.sheet.as_ref()?;
                let directional = gesture_is_directional(gesture);
                gesture_point(view, gesture.view, map, sheet, pos, Some(gesture.anchor), directional)
            })
        };
        if let (Some(current), Some(gesture)) = (current, app.drawing_workspace.view.gesture.as_mut()) {
            gesture.current = current;
        }
    }
    if released && let Some(gesture) = app.drawing_workspace.view.gesture.take() {
        let pointer = pointer.unwrap_or(gesture.press_at);
        match gesture.stage {
            GestureStage::FirstPress => {
                if pointer.distance(gesture.press_at) > 3.0 {
                    complete_gesture(app, gesture);
                } else {
                    let mut armed = gesture;
                    armed.stage = GestureStage::Armed;
                    armed.current = armed.anchor;
                    app.drawing_workspace.view.gesture = Some(armed);
                }
            }
            GestureStage::SecondPress => complete_gesture(app, gesture),
            GestureStage::Handle => rewrite_handle(app, gesture),
            GestureStage::Armed => app.drawing_workspace.view.gesture = Some(gesture),
        }
    }
}

/// A new or opened design: the Drawing editor starts over, as the Render editor does.
pub fn reset_document(app: &mut SolveApp) {
    if active(app) {
        let _ = enter(app, false);
    }
    app.drawing_workspace = State::default();
}

/// Forget a build belonging to a design that is no longer the one in the window.
pub(crate) fn cancel_build(app: &mut SolveApp) {
    app.drawing_workspace.build = None;
    app.drawing_workspace.pending_export = None;
}

/// The read-only state the control channel reports for the orchestrator.
pub fn control_state(app: &SolveApp) -> Value {
    let view = &app.drawing_workspace.view;
    let views: Vec<&str> = view.projected.as_ref().map(|projected| projected.iter().map(|v| view_name(v.view)).collect()).unwrap_or_default();
    let letters: Vec<String> = view.sections.letters().iter().map(char::to_string).collect();
    let labels: Vec<String> =
        view.sheet.as_ref().map(|sheet| sheet.sections.iter().filter_map(|s| s.label).map(|l| l.to_string()).collect()).unwrap_or_default();
    let hatch_lines: usize = view.sheet.as_ref().map_or(0, |sheet| sheet.sections.iter().map(|s| s.hatch.len()).sum());
    json!({
        "active": active(app),
        "building": building(app),
        "scale": view.scale.text(),
        "view_choice": view.choice.name(),
        "auto_dimensions": view.auto_dimensions,
        "dimension_count": view.sheet.as_ref().map_or(0, |sheet| sheet.dimensions.len()),
        "note_count": view.sheet.as_ref().map_or(0, |sheet| sheet.notes.len()),
        "unplaced": view.sheet.as_ref().map(|sheet| sheet.unplaced.clone()).unwrap_or_default(),
        "notes_block": {
            "enabled": view.notes.enabled,
            "ra": view.notes.ra,
            "edge_note": view.notes.edge_note,
            "extra_notes": view.notes.extra_notes,
        },
        "title_dirty": view.title_dirty,
        "views": views,
        "view_count": views.len(),
        "tangent_edges": tangent_edges_of(app).name(),
        "tangent_source": stored_tangent_edges(&app.session.doc).map_or("default", |_| "stored"),
        "section_mode": view.sections.name(),
        "section_source": stored_sections(&app.session.doc).map_or("default", |_| "stored"),
        "sections": section_mode_value(&view.sections),
        "section_letters": letters,
        "section_labels": labels,
        "section_count": view.sheet.as_ref().map_or(0, |sheet| sheet.sections.len()),
        "section_hatch_lines": hatch_lines,
        "section_tool": {
            "active": view.section_tool,
            "mode": view.section_mode.name(),
            "picked_view": view.picked_view.map(view_name),
            "selected": view.selected_section.map(|letter| letter.to_string()),
        },
        "section_gesture": view.gesture.as_ref().map(|gesture| json!({
            "stage": match gesture.stage {
                GestureStage::FirstPress => "first_press",
                GestureStage::Armed => "armed",
                GestureStage::SecondPress => "second_press",
                GestureStage::Handle => "handle",
            },
            "view": view_name(gesture.view),
            "mode": gesture.mode.name(),
            "anchor": [gesture.anchor.x, gesture.anchor.y],
            "current": [gesture.current.x, gesture.current.y],
        })),
        "error": view.error,
        "dirty": view.dirty,
        "revision": view.revision,
        "projections": view.projections,
        "paper": view.sheet.as_ref().map(|sheet| sheet.paper.name()),
    })
}

/// The side panel beside the page: the title-block fields.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    Container::new(ui.ctx(), Surface::Panel)
        .edge("sc_drawing_settings", Edge::Right)
        .exact_size(PANEL_WIDTH)
        .resizable(false)
        .frame(Container::new(ui.ctx(), Surface::Panel).edge_frame(Tokens::get().panel).inner_margin(10))
        .show(ui, |ui| {
            crate::ui_kit::scope(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(app.tr("ui.drawing"));
                    ui.label(app.tr("drawing.title_block"));
                    ui.separator();
                    let mut changed = false;
                    let mut focused = false;
                    for (label, field) in TITLE_FIELDS {
                        ui.label(app.tr(label));
                        let id = ui.id().with(("drawing_title", field));
                        let title = &mut app.drawing_workspace.view.title;
                        let value = match field {
                            "benennung" => &mut title.benennung,
                            "zeichnungsnummer" => &mut title.zeichnungsnummer,
                            "werkstoff" => &mut title.werkstoff,
                            "allgemeintoleranz" => &mut title.allgemeintoleranz,
                            "gezeichnet" => &mut title.gezeichnet,
                            "datum" => &mut title.datum,
                            "geprueft" => &mut title.geprueft,
                            "geprueft_datum" => &mut title.geprueft_datum,
                            "aend" => &mut title.aend,
                            "aend_datum" => &mut title.aend_datum,
                            "firma" => &mut title.firma,
                            "rohmass" => &mut title.rohmass,
                            "dokumentart" => &mut title.dokumentart,
                            _ => &mut title.blatt,
                        };
                        let response = crate::ui_kit::bounded_cell_editor(ui, id, value, "", false, MAX_NOTE_CHARS);
                        changed |= response.changed();
                        focused |= response.has_focus();
                    }
                    ui.separator();
                    ui.label(app.tr("drawing.notes_block"));
                    {
                        let enabled_label = app.tr("drawing.notes_enabled").to_string();
                        let ra_label = app.tr("drawing.ra").to_string();
                        let edge_label = app.tr("drawing.edge_note").to_string();
                        let extra_label = app.tr("drawing.extra_notes").to_string();
                        let view = &mut app.drawing_workspace.view;
                        let notes = &mut view.notes;
                        let enabled = crate::ui_kit::checkbox(ui, &enabled_label, &mut notes.enabled);
                        changed |= enabled.changed();
                        focused |= enabled.has_focus();
                        ui.label(ra_label);
                        let id = ui.id().with(("drawing_notes", "ra"));
                        let response = crate::ui_kit::bounded_cell_editor(ui, id, &mut notes.ra, "", false, 32);
                        changed |= response.changed();
                        focused |= response.has_focus();
                        ui.label(edge_label);
                        let id = ui.id().with(("drawing_notes", "edge_note"));
                        let response = crate::ui_kit::bounded_cell_editor(ui, id, &mut notes.edge_note, "", false, MAX_NOTE_CHARS);
                        changed |= response.changed();
                        focused |= response.has_focus();
                        ui.label(extra_label);
                        let id = ui.id().with(("drawing_notes", "extra"));
                        let response = ui
                            .add(egui::TextEdit::multiline(&mut view.extra_text).id(id).desired_rows(3).char_limit(MAX_NOTE_CHARS * MAX_EXTRA_NOTES));
                        changed |= response.changed();
                        focused |= response.has_focus();
                        if response.changed() {
                            notes.extra_notes = view
                                .extra_text
                                .lines()
                                .map(str::trim)
                                .filter(|line| !line.is_empty())
                                .take(MAX_EXTRA_NOTES)
                                .map(|line| line.chars().take(MAX_NOTE_CHARS).collect())
                                .collect();
                        }
                    }
                    if changed {
                        title_changed(app);
                    }
                    if !changed && !focused {
                        // Editors lost focus: store the values with the design (one undo step).
                        save_title(app);
                    }
                    ui.separator();
                    ui.label(app.tr("drawing.sections"));
                    let rows: Vec<(char, String, String)> = {
                        let view_state = &app.drawing_workspace.view;
                        match &view_state.sections {
                            SectionMode::Explicit(specs) => {
                                let letters = assigned_letters(specs).unwrap_or_default();
                                specs
                                    .iter()
                                    .zip(letters)
                                    .map(|(spec, letter)| {
                                        let kind = match spec {
                                            SectionSpec::Full { .. } | SectionSpec::FullFrom { .. } => app.tr("drawing.section.full").to_string(),
                                            SectionSpec::Breakout { .. } => app.tr("drawing.section.breakout").to_string(),
                                        };
                                        let view = sectioned_view_of(spec, view_state)
                                            .or(match spec {
                                                SectionSpec::FullFrom { from, .. } => Some(from.view),
                                                _ => None,
                                            })
                                            .map(view_name)
                                            .unwrap_or("-");
                                        (letter, kind, view.to_string())
                                    })
                                    .collect()
                            }
                            _ => Vec::new(),
                        }
                    };
                    let mut clicked_row = None;
                    let mut delete_letter = None;
                    for (letter, kind, view) in &rows {
                        ui.horizontal(|ui| {
                            let selected = app.drawing_workspace.view.selected_section == Some(*letter);
                            let text = format!("{letter}  {kind}  {view}");
                            let response = ui.selectable_label(selected, text);
                            crate::scenario::publish_target(&format!("drawing:section:item:{letter}"), response.rect);
                            if response.clicked() {
                                clicked_row = Some(*letter);
                            }
                            let delete = ui.small_button("×").on_hover_text(app.tr("drawing.section.delete"));
                            crate::scenario::publish_target(&format!("drawing:section:delete:{letter}"), delete.rect);
                            if delete.clicked() {
                                delete_letter = Some(*letter);
                            }
                        });
                    }
                    if rows.is_empty() {
                        let hint = if matches!(app.drawing_workspace.view.sections, SectionMode::None) {
                            app.tr("drawing.section.none_hint")
                        } else {
                            app.tr("drawing.section.auto_hint")
                        };
                        ui.small(hint);
                    }
                    if let Some(letter) = clicked_row {
                        let picked = {
                            let view_state = &app.drawing_workspace.view;
                            selected_spec_for(view_state, letter).map(|spec| match spec {
                                SectionSpec::FullFrom { from, .. } => from.view,
                                SectionSpec::Breakout { view, .. } | SectionSpec::Full { view, .. } => view,
                            })
                        };
                        let view_state = &mut app.drawing_workspace.view;
                        view_state.selected_section = Some(letter);
                        if let Some(picked) = picked {
                            view_state.picked_view = Some(picked);
                        }
                    }
                    if let Some(letter) = delete_letter {
                        delete_section(app, letter);
                    }
                    if let Some(error) = &app.drawing_workspace.view.error {
                        ui.colored_label(Tokens::get().error, error);
                    }
                    ui.small(app.tr("drawing.saved_with_design"));
                    ui.separator();
                    ui.small(format!("{}: {}", app.tr("ui.scale"), app.drawing_workspace.view.scale.label(app)));
                    let views: Vec<&str> = app
                        .drawing_workspace
                        .view
                        .sheet
                        .as_ref()
                        .map(|sheet| sheet.views.iter().map(|v| view_name(v.view)).collect())
                        .unwrap_or_default();
                    let why = app.tr(app.drawing_workspace.view.choice.message_id());
                    if !views.is_empty() {
                        ui.small(format!("{}: {} ({})", app.tr("drawing.views"), views.join(", "), why));
                    }
                    if let Some(sheet) = &app.drawing_workspace.view.sheet {
                        ui.small(format!(
                            "{}: {}  {}: {}",
                            app.tr("drawing.dimensions"),
                            sheet.dimensions.len(),
                            app.tr("drawing.notes"),
                            sheet.notes.len()
                        ));
                        for text in &sheet.unplaced {
                            ui.colored_label(Tokens::get().error, format!("{}: {text}", app.tr("drawing.unplaced")));
                        }
                    }
                });
            });
        });
}

/// The DRAWING ribbon: Sheet (Update Views, Scale), Export (PDF, DXF) and Close.
pub fn ribbon(app: &mut SolveApp, ui: &mut egui::Ui, area: Rect) {
    let painter = ui.painter().clone();
    let labels = app.preferences.ribbon_labels;
    let top = area.top() + 2.0;
    let button_height = if labels { 60.0 } else { 36.0 };
    let width = 86.0;
    let mut x = area.left() + 8.0;
    let mut picked = None;

    let sheet_start = x;
    let update_label = app.tr("drawing.update_views").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "update", &update_label, labels);
    crate::scenario::publish_target("drawing:update_views", response.rect);
    let update = response.clicked();
    x += width + 2.0;

    let scale_label = app.drawing_workspace.view.scale.label(app);
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "scale", &scale_label, labels);
    crate::scenario::publish_target("drawing:scale", response.rect);
    let current = app.drawing_workspace.view.scale;
    Container::new(ui.ctx(), Surface::Popup).menu(&response).show(|ui| {
        ui.set_min_width(150.0);
        let auto = crate::ui_kit::list_picker(ui, "drawing_scale:auto", app.tr("drawing.auto"), current == Scale::Auto);
        // A `drawing:*` handle for every scale item, so an agent script can name the value the
        // way the ribbon reads (`drawing:scale:1_2`); the generic `dialog:list:` name stays.
        crate::scenario::publish_target("drawing:scale:auto", auto.rect);
        if auto.clicked() {
            picked = Some(Scale::Auto);
            ui.close();
        }
        for (n, d) in ISO_SCALES {
            let key = format!("drawing_scale:{n}_{d}");
            let response = crate::ui_kit::list_picker(ui, &key, &format_scale((n, d)), current == Scale::Fixed(n, d));
            crate::scenario::publish_target(&format!("drawing:scale:{n}_{d}"), response.rect);
            if response.clicked() {
                picked = Some(Scale::Fixed(n, d));
                ui.close();
            }
        }
    });
    x += width + 2.0;

    let dimensions_label = format!(
        "{}: {}",
        app.tr("drawing.dimensions"),
        if app.drawing_workspace.view.auto_dimensions { app.tr("drawing.auto") } else { app.tr("drawing.off") }
    );
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "dimension", &dimensions_label, labels);
    crate::scenario::publish_target("drawing:dimensions", response.rect);
    let dimensions_toggle = response.clicked();
    x += width + 2.0;

    // Tangent (G1) edges: omitted (the ISO default) or thin light edges on their own layer.
    let tangent = tangent_edges_of(app);
    let tangent_label = format!("{}: {}", app.tr("drawing.tangent_edges"), tangent.name());
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "edge_display", &tangent_label, labels);
    crate::scenario::publish_target("drawing:tangent_edges", response.rect);
    let tangent_toggle = response.clicked();
    x += width + 10.0;
    ribbon_rule(&painter, x - 5.0, top, button_height);
    ribbon_heading(&painter, area, sheet_start, x - 10.0, app.tr("drawing.sheet"));

    // Section (D9.2): the tool, its two modes, and the two spec resets.
    let section_start = x;
    let section_label = app.tr("drawing.section").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "section", &section_label, labels);
    crate::scenario::publish_target("drawing:section", response.rect);
    let tool_toggle = response.clicked();
    x += width + 2.0;
    let full_label = app.tr("drawing.section.full").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "section_full", &full_label, labels);
    crate::scenario::publish_target("drawing:section:full", response.rect);
    let full_mode = response.clicked();
    x += width + 2.0;
    let breakout_label = app.tr("drawing.section.breakout").to_string();
    let response =
        ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "section_breakout", &breakout_label, labels);
    crate::scenario::publish_target("drawing:section:breakout", response.rect);
    let breakout_mode = response.clicked();
    x += width + 2.0;
    let auto_sections_label = app.tr("drawing.section.auto").to_string();
    let response =
        ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "section_auto", &auto_sections_label, labels);
    crate::scenario::publish_target("drawing:section:auto", response.rect);
    let auto_sections = response.clicked();
    x += width + 2.0;
    let no_sections_label = app.tr("drawing.section.none").to_string();
    let response =
        ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "section_none", &no_sections_label, labels);
    crate::scenario::publish_target("drawing:section:none", response.rect);
    let no_sections = response.clicked();
    x += width + 10.0;
    ribbon_rule(&painter, x - 5.0, top, button_height);
    ribbon_heading(&painter, area, section_start, x - 10.0, app.tr("drawing.section"));

    let export_start = x;
    let pdf_label = app.tr("drawing.export_pdf").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "export", &pdf_label, labels);
    crate::scenario::publish_target("drawing:export_pdf", response.rect);
    let export_pdf = response.clicked();
    x += width + 2.0;
    let dxf_label = app.tr("drawing.export_dxf").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "dxf", &dxf_label, labels);
    crate::scenario::publish_target("drawing:export_dxf", response.rect);
    let export_dxf = response.clicked();
    x += width + 10.0;
    ribbon_rule(&painter, x - 5.0, top, button_height);
    ribbon_heading(&painter, area, export_start, x - 10.0, app.tr("cmd.file_export"));

    let close_start = x;
    let close_label = app.tr("ui.close").to_string();
    let response = ribbon_button(ui, &painter, Rect::from_min_size(pos2(x, top), vec2(width, button_height)), "close", &close_label, labels);
    crate::scenario::publish_target("drawing:close", response.rect);
    let close = response.clicked();
    x += width;
    ribbon_heading(&painter, area, close_start, x, app.tr("drawing.close"));

    if update && !building(app) {
        start_build(app);
    }
    if let Some(scale) = picked {
        set_scale(app, scale);
    }
    if dimensions_toggle && !building(app) {
        app.drawing_workspace.view.auto_dimensions = !app.drawing_workspace.view.auto_dimensions;
        start_build(app);
    }
    if tangent_toggle && !building(app) {
        set_tangent_edges(app, if tangent == TangentEdges::Thin { TangentEdges::Omit } else { TangentEdges::Thin });
    }
    if tool_toggle {
        let view = &mut app.drawing_workspace.view;
        view.section_tool = !view.section_tool;
        if !view.section_tool {
            view.gesture = None;
            view.picked_view = None;
            view.selected_section = None;
        }
    }
    if full_mode {
        app.drawing_workspace.view.section_tool = true;
        app.drawing_workspace.view.section_mode = SectionToolMode::Full;
    }
    if breakout_mode {
        app.drawing_workspace.view.section_tool = true;
        app.drawing_workspace.view.section_mode = SectionToolMode::Breakout;
    }
    if auto_sections {
        set_section_spec(app, SectionMode::Auto);
    }
    if no_sections {
        set_section_spec(app, SectionMode::None);
    }
    if export_pdf {
        export(app, "pdf");
    }
    if export_dxf {
        export(app, "dxf");
    }
    if close && let Err(e) = enter(app, false) {
        app.set_status(e, true);
    }
}

fn ribbon_rule(painter: &egui::Painter, x: f32, top: f32, height: f32) {
    let t = Tokens::get();
    painter.line_segment([pos2(x, top + 4.0), pos2(x, top + height - 8.0)], Stroke::new(1.0, t.border));
}

fn ribbon_heading(painter: &egui::Painter, area: Rect, left: f32, right: f32, label: &str) {
    let t = Tokens::get();
    painter.text(pos2((left + right) * 0.5, area.bottom() - 4.0), Align2::CENTER_BOTTOM, label, FontId::proportional(t.font_caption), t.text_dim);
}

fn ribbon_button(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, icon: &str, label: &str, labels: bool) -> egui::Response {
    let t = Tokens::get();
    let response = ui.interact(rect, ui.id().with(("drawing_ribbon", icon)), Sense::click());
    if response.hovered() {
        painter.rect_filled(rect, t.radius_sm, t.hover);
    }
    let icon_at = if labels { rect.top() + 20.0 } else { rect.center().y - 4.0 };
    crate::icons::paint_icon(painter, Rect::from_center_size(pos2(rect.center().x, icon_at), vec2(24.0, 24.0)), icon, 1.0);
    if labels {
        painter.text(pos2(rect.center().x, rect.top() + 38.0), Align2::CENTER_TOP, label, FontId::proportional(t.font_caption), t.text);
    }
    response.on_hover_text(label.to_string())
}

/// The page: the sheet as a flat 2D drawing, panned and zoomed by the shared navigation.
pub fn show(app: &mut SolveApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    app.viewport.rect = Some(rect);
    crate::viewport::refresh_layout(app, ui.ctx().pixels_per_point());
    let response = ui.interact(rect, ui.id().with("drawing_page"), Sense::click_and_drag());
    // The shared camera input drives the page's own camera; the Design camera stays as it was.
    // While the Section tool owns a primary press on the page, navigation keeps its hands off it.
    let (pointer, primary_busy) =
        ui.ctx().input(|i| (i.pointer.hover_pos(), i.pointer.primary_down() || i.pointer.primary_pressed() || i.pointer.primary_released()));
    let before = app.drawing_workspace.view.cam.cam;
    let design = std::mem::replace(&mut app.cam, before);
    let tool_owns = primary_busy && app.drawing_workspace.view.gesture.is_some()
        || (primary_busy
            && pointer.is_some_and(|p| rect.contains(p) && !crate::shell::covered(app, p))
            && (app.drawing_workspace.view.section_tool || app.drawing_workspace.view.selected_section.is_some()));
    if tool_owns {
        app.viewport.mouse = None;
    } else {
        crate::navigation::handle(app, ui, rect, &response, crate::navigation::NavCaps { orbit: false, pick: false });
    }
    let page_camera = std::mem::replace(&mut app.cam, design);
    let view = &mut app.drawing_workspace.view;
    view.cam.cam = page_camera;
    // A pan or zoom since the last fit belongs to the user: a resize must not undo it.
    if page_camera != before {
        view.moved = true;
    }
    app.viewport.mouse = None;

    let t = Tokens::get();
    let painter = ui.painter().clone();
    // A grey page canvas in either theme; the paper sits on it with a faint shadow.
    painter.rect_filled(rect, 0.0, t.panel.lerp_to_gamma(t.text_dim, 0.12));
    let paper_mm = app.drawing_workspace.view.sheet.as_ref().map_or(Paper::A4, |sheet| sheet.paper).landscape_mm();
    fit_page(app, rect, paper_mm);
    let camera = app.drawing_workspace.view.cam.cam;
    let Some(map) = PageMap::new(rect, camera) else { return };
    let scale = map.scale;
    let to = |x: f64, y: f64| map.to(x, y);
    let paper = Rect::from_two_pos(map.to(0.0, 0.0), map.to(paper_mm.0, paper_mm.1));
    painter.rect_filled(paper.translate(vec2(2.0, 3.0)), 0.0, Color32::from_black_alpha(40));
    painter.rect_filled(paper, 0.0, PAPER);
    painter.rect_stroke(paper, 0.0, Stroke::new(1.0, Color32::from_gray(190)), egui::StrokeKind::Inside);
    crate::scenario::publish_rect("drawing:paper", paper);
    // The page is a handle too: `ui.handles` lists it beside the ribbon's `drawing:*` names.
    crate::scenario::publish_target("drawing:paper", paper);

    let page = painter.with_clip_rect(paper.intersect(rect));
    if let Some(sheet) = &app.drawing_workspace.view.sheet {
        for prim in &sheet.prims {
            paint_prim(&page, prim, &to, scale);
        }
    }
    if let Some(sheet) = &app.drawing_workspace.view.sheet {
        paint_section_overlay(app, &page, map, paper, sheet, pointer);
    }
    process_section_input(app, ui, rect, map);
    let stale = {
        let view = &app.drawing_workspace.view;
        view.dirty || view.revision != Some(app.session.revision)
    };
    if building(app) {
        page.text(paper.center(), Align2::CENTER_CENTER, app.tr("drawing.building"), FontId::proportional(t.font_heading), Color32::from_gray(70));
    } else if let Some(error) = &app.drawing_workspace.view.error {
        page.text(paper.center(), Align2::CENTER_CENTER, error, FontId::proportional(t.font_body), Color32::from_rgb(150, 30, 30));
    } else if stale {
        page.text(
            paper.center_top() + vec2(0.0, 6.0),
            Align2::CENTER_TOP,
            app.tr("drawing.stale"),
            FontId::proportional(t.font_body),
            Color32::from_rgb(150, 90, 20),
        );
    }
}

/// Frame the paper in the area the shell's floating cards and side panels leave free, with the
/// layout's 16 px margin, whenever a fit was asked for or the free area changed while the user
/// has not taken the camera over.
fn fit_page(app: &mut SolveApp, rect: Rect, paper_mm: (f64, f64)) {
    let Some(area) = free_area(app, rect) else { return };
    let view = &mut app.drawing_workspace.view;
    let area_changed = view.fitted_area.is_none_or(|old| !same_rect(old, area));
    if view.cam.fitted && !(area_changed && !view.moved) {
        return;
    }
    view.cam.fit(rect, area, (0.0, 0.0), paper_mm);
    view.cam.fitted = true;
    view.fitted_area = Some(area);
    view.moved = false;
}

/// The free part of the page: the central rect minus the shell panels and the floating cards,
/// with the layout's own margin. This is the one area Design's fit uses (`Layout::drawable`),
/// not a second notion of "free".
fn free_area(app: &SolveApp, rect: Rect) -> Option<Rect> {
    let layout = app.viewport.layout.as_ref().filter(|layout| layout.viewport == rect)?;
    let area = layout.drawable.intersect(rect);
    (area.is_finite() && area.is_positive()).then_some(area)
}

/// Two layout rectangles, ignoring sub-pixel churn from the panel layout.
fn same_rect(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 0.25 && (a.max - b.max).length() < 0.25
}

/// Paper millimetres (origin bottom-left, y up) to screen points, the way the flat pattern maps
/// them: the page camera's target sits at the centre of the view and y is flipped for the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PageMap {
    center: Pos2,
    target_x: f64,
    target_y: f64,
    /// Screen points per paper millimetre.
    scale: f64,
}

impl PageMap {
    fn new(rect: Rect, camera: Camera) -> Option<PageMap> {
        let scale = f64::from(rect.height()) / (2.0 * camera.half_height());
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        Some(PageMap { center: rect.center(), target_x: camera.target.x, target_y: camera.target.y, scale })
    }

    fn to(self, x: f64, y: f64) -> Pos2 {
        pos2(
            (f64::from(self.center.x) + (x - self.target_x) * self.scale) as f32,
            (f64::from(self.center.y) - (y - self.target_y) * self.scale) as f32,
        )
    }

    /// Screen points → paper (sheet) millimetres, the inverse of [`PageMap::to`].
    fn to_sheet(self, at: Pos2) -> (f64, f64) {
        let x = self.target_x + (f64::from(at.x) - f64::from(self.center.x)) / self.scale;
        let y = self.target_y - (f64::from(at.y) - f64::from(self.center.y)) / self.scale;
        (x, y)
    }
}

/// One sheet primitive, in paper millimetres, on the page.
fn paint_prim(painter: &egui::Painter, prim: &Prim, to: &impl Fn(f64, f64) -> Pos2, scale: f64) {
    match prim {
        Prim::Line { a, b, layer } => paint_line(painter, *layer, to(a.x, a.y), to(b.x, b.y), scale),
        Prim::Polyline { points, closed, layer } => {
            let path: Vec<Pos2> = points.iter().map(|p| to(p.x, p.y)).collect();
            paint_path(painter, *layer, path, *closed, scale);
        }
        Prim::Arc { c, r, a0, a1, layer } => {
            let steps = (((a1 - a0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
            let path: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let t = a0 + (a1 - a0) * i as f64 / steps as f64;
                    to(c.x + r * t.cos(), c.y + r * t.sin())
                })
                .collect();
            paint_path(painter, *layer, path, false, scale);
        }
        Prim::Circle { c, r, layer } => {
            let path: Vec<Pos2> = (0..=32)
                .map(|i| {
                    let t = std::f64::consts::TAU * i as f64 / 32.0;
                    to(c.x + r * t.cos(), c.y + r * t.sin())
                })
                .collect();
            paint_path(painter, *layer, path, true, scale);
        }
        Prim::Ellipse { c, major, ratio, t0, t1, layer } => {
            let minor = major.perp() * *ratio;
            let steps = (((t1 - t0).abs() / 10.0_f64.to_radians()).ceil() as usize).clamp(2, 72);
            let path: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let t = t0 + (t1 - t0) * i as f64 / steps as f64;
                    to(c.x + major.x * t.cos() + minor.x * t.sin(), c.y + major.y * t.cos() + minor.y * t.sin())
                })
                .collect();
            paint_path(painter, *layer, path, false, scale);
        }
        Prim::Filled { points, layer } => {
            if points.len() < 3 {
                return;
            }
            let path: Vec<Pos2> = points.iter().map(|p| to(p.x, p.y)).collect();
            painter.add(egui::Shape::convex_polygon(path, ink(*layer), Stroke::NONE));
        }
        Prim::Text { at, height, angle_deg, text, anchor, layer } => {
            let size = (height * scale).max(1.0) as f32;
            let color = ink(*layer);
            let font = FontId::proportional(size);
            let align = match anchor {
                TextAnchor::Left => Align2::LEFT_BOTTOM,
                TextAnchor::Center => Align2::CENTER_BOTTOM,
                TextAnchor::Right => Align2::RIGHT_BOTTOM,
            };
            let at = to(at.x, at.y);
            if angle_deg.abs() < 0.01 {
                painter.text(at, align, text, font, color);
            } else {
                let galley = painter.layout_no_wrap(text.clone(), font, color);
                let corner = align.pos_in_rect(&galley.rect).to_vec2();
                // `with_angle_and_anchor` rotates around the galley corner, so start from
                // `at - corner`: the rotated anchor then lands exactly on `at`. A world CCW
                // angle is a negative clockwise angle in screen coordinates (y down).
                let shape = egui::epaint::TextShape::new(at - corner, galley, color).with_angle_and_anchor(-(angle_deg.to_radians() as f32), align);
                painter.add(shape);
            }
        }
    }
}

/// Paint a path on a layer: continuous, or with the layer's own dash pattern.
fn paint_path(painter: &egui::Painter, layer: Layer, mut path: Vec<Pos2>, closed: bool, scale: f64) {
    if path.len() < 2 {
        return;
    }
    if closed
        && let (Some(first), Some(last)) = (path.first().copied(), path.last().copied())
        && first.distance(last) > 0.01
    {
        path.push(first);
    }
    let stroke = Stroke::new(px_width(layer, scale), ink(layer));
    match layer.line_kind() {
        LineKind::Continuous => {
            painter.add(egui::Shape::line(path, stroke));
        }
        LineKind::Dashed { dash_mm, gap_mm } | LineKind::DashDot { dash_mm, gap_mm, .. } => {
            let dash = (dash_mm * scale).max(1.0) as f32;
            let gap = (gap_mm * scale).max(0.5) as f32;
            painter.extend(egui::Shape::dashed_line(&path, stroke, dash, gap));
        }
    }
}

fn paint_line(painter: &egui::Painter, layer: Layer, a: Pos2, b: Pos2, scale: f64) {
    let stroke = Stroke::new(px_width(layer, scale), ink(layer));
    match layer.line_kind() {
        LineKind::Continuous => {
            painter.line_segment([a, b], stroke);
        }
        LineKind::Dashed { dash_mm, gap_mm } | LineKind::DashDot { dash_mm, gap_mm, .. } => {
            let dash = (dash_mm * scale).max(1.0) as f32;
            let gap = (gap_mm * scale).max(0.5) as f32;
            painter.extend(egui::Shape::dashed_line(&[a, b], stroke, dash, gap));
        }
    }
}

/// The prim's pen width in mm × pixels per mm, never thinner than one pixel.
fn px_width(layer: Layer, scale: f64) -> f32 {
    (layer.width_mm() * scale).max(1.0) as f32
}

fn ink(layer: Layer) -> Color32 {
    match layer {
        Layer::Hidden | Layer::Thin | Layer::Tangent => HIDDEN_INK,
        _ => SHEET_INK,
    }
}

/// Open the app's save dialog and export the sheet on screen with the current title block and
/// scale. The page's built [`Sheet`] is written as it is; a sheet that is missing or older than
/// the design is rebuilt on the worker first, never on this thread.
fn export(app: &mut SolveApp, format: &str) {
    let kind = if format == "pdf" { ExportKind::Pdf } else { ExportKind::Dxf };
    let name = app.session.doc.name.trim();
    let name = if name.is_empty() { "drawing" } else { name };
    let name = name.replace(['/', '\\'], "_");
    let extension = kind.extension();
    app.pick_save(&format!("{name}.{extension}"), &[extension], move |app, path| {
        if let Some(path) = path {
            request_export(app, path, kind);
        }
    });
}

/// Ask for the given file to be written from the current sheet, or from the sheet the next
/// background build produces when the design has moved on.
fn request_export(app: &mut SolveApp, path: String, kind: ExportKind) {
    app.drawing_workspace.pending_export = Some(PendingExport { path, kind });
    continue_export(app);
}

/// Write a waiting export when a current sheet is on screen; otherwise start the worker build
/// that makes it current. A running build is taken up by [`finish_build`].
fn continue_export(app: &mut SolveApp) {
    if app.drawing_workspace.pending_export.is_none() || app.drawing_workspace.build.is_some() {
        return;
    }
    let current = {
        let view = &app.drawing_workspace.view;
        view.sheet.is_some() && view.revision == Some(app.session.revision)
    };
    if current {
        write_pending_export(app);
    } else {
        start_build(app);
    }
}

/// Write the waiting export from the sheet that is on screen, exactly as the page paints it.
fn write_pending_export(app: &mut SolveApp) {
    let Some(pending) = app.drawing_workspace.pending_export.take() else { return };
    let outcome = match app.drawing_workspace.view.sheet.as_ref() {
        Some(sheet) => write_sheet(sheet, &pending.path, pending.kind),
        None => Err("there is no sheet to export".to_string()),
    };
    match outcome {
        Ok(()) => {
            let message = crate::i18n::Locale::for_language(&app.preferences.language).format("ui.saved_to", &[("path", &pending.path)]);
            app.set_status(message, false);
        }
        Err(e) => app.set_status(format!("export failed: {e}"), true),
    }
}

/// Serialise the sheet with the engine's writers (the same bytes `drawing.export` writes), with
/// no projection and no document change.
fn write_sheet(sheet: &Sheet, path: &str, kind: ExportKind) -> Result<(), String> {
    match kind {
        ExportKind::Dxf => {
            let text = sheet_to_dxf(sheet).map_err(|e| e.to_string())?;
            solidcraft_engine::io::vfs::write(path, text.as_bytes()).map_err(|e| format!("{path}: {e}"))
        }
        ExportKind::Pdf => {
            let page = sheet_to_pdf(sheet).map_err(|e| e.to_string())?;
            let bytes = solidcraft_engine::io::write_pdf(&page).map_err(|e| e.to_string())?;
            solidcraft_engine::io::vfs::write(path, &bytes).map_err(|e| format!("{path}: {e}"))
        }
    }
}
