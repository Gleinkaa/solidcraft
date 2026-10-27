//! A drawing sheet as pure data: paper, frame, ISO 5457 zone grid, ISO 7200 title block, the
//! first-angle (ISO method 1) placement of projected views with a dimension band kept free
//! around each, and the views' centre lines — all in sheet millimetres with the origin at the
//! bottom-left corner and y up.
//!
//! Nothing here touches the file system. [`build_sheet`] turns [`ProjectedView`]s into the
//! [`Sheet`] a drawing page paints and a writer serialises; [`sheet_to_dxf`] is the DXF
//! serialisation (a string; the command writes it) and [`sheet_to_pdf`] the PDF one (a
//! [`PdfPage`] of the same prims).

use std::f64::consts::TAU;

use solidcraft_geom::Vec2;
use solidcraft_io::{DxfEntity, DxfLayer, DxfLinetype, DxfLineweight, DxfShape, PdfItem, PdfPage, TextAnchor as PdfTextAnchor, text_width_mm};
use solidcraft_render::StandardView;

use crate::Result;
use crate::drawing::ProjectedView;
use crate::drawing::centre::ViewCentre;
use crate::drawing::dims::{
    Annotations, DIM_BAND_MM as DIM_NO_ROW_BAND_MM, Dimension, DrawnAnnotations, DrawnDimension, DrawnNote, band_mm, draw_annotations, format_value,
};
use crate::drawing::sections::{self, CutRegion, SectionMark};
use crate::params::bad;

/// Width of the ISO 7200 title block (mm).
pub const TITLE_BLOCK_WIDTH_MM: f64 = 180.0;
/// Height of the neutral ISO 7200 title block this sheet draws (mm). D5 drew 51 mm; the D8
/// layout packs the same fields plus the D8 ones into 49 mm, with room below every label for
/// the lettering's descenders.
pub const TITLE_BLOCK_HEIGHT_MM: f64 = 49.0;
/// Lettering height of the notes block (mm), ISO 3098 type B like the rest of the sheet.
pub const NOTES_TEXT_HEIGHT_MM: f64 = 3.5;
/// Notes line pitch: 1.5 × the lettering height (mm).
pub const NOTES_LINE_PITCH_MM: f64 = 1.5 * NOTES_TEXT_HEIGHT_MM;
/// Most free extra notes a notes block carries.
pub const MAX_EXTRA_NOTES: usize = 5;
/// Most characters one note line may hold.
pub const MAX_NOTE_CHARS: usize = 200;
/// The namespaced design-property key the drawing's own title and note values live under.
pub const DRAWING_METADATA: &str = "app.solidcraft.drawing";
/// The default edge note: undefined-shape edges are broken to at most 0.2 mm (EN ISO 13715).
pub const DEFAULT_EDGE_NOTE: &str = "Werkst\u{00fc}ckkanten ISO 13715: -0,2";
/// The default surface roughness value, written with a decimal comma.
pub const DEFAULT_RA: &str = "3,2";
/// ISO 5457 frame line width (mm).
pub const FRAME_WIDTH_MM: f64 = 0.7;
/// Least clear space between two placed views on the sheet (mm): the two dimension bands plus a
/// millimetre of air.
pub const VIEW_GAP_MM: f64 = 25.0;
/// Least margin from the drawing area's boundary (mm) the views keep. The dimension band is
/// wider (12 mm), so in practice the band sets the clearance.
pub const DRAWING_MARGIN_MM: f64 = 10.0;
/// The dimension band reserved around a view without dimension rows (mm); D6/D7 put dimensions
/// there. A side with `rows` stacked dimension lines reserves [`crate::drawing::dims::band_mm`].
pub const DIM_BAND_MM: f64 = DIM_NO_ROW_BAND_MM;
/// How far a centre line reaches past the view's outline, and a centre cross past its circle (mm).
pub const CENTRE_OVERSHOOT_MM: f64 = 3.0;
/// ISO 5455 preferred scales, largest first.
pub const ISO_SCALES: [(u32, u32); 10] = [(10, 1), (5, 1), (2, 1), (1, 1), (1, 2), (1, 5), (1, 10), (1, 20), (1, 50), (1, 100)];
/// The ISO 3098 heights below a 7 mm Benennung, largest first. 2.5 mm is the smallest the
/// drawing checker accepts, so a value that still does not fit is shortened at that height.
const SMALL_TEXT_HEIGHTS: [f64; 3] = [5.0, 3.5, 2.5];
/// Space between a title-block text's box and its cell boundary (mm).
const CELL_PADDING_MM: f64 = 2.5;
/// Inset of a title-block label and value from the left edge of its cell (mm).
const CELL_INSET_MM: f64 = 2.5;
/// Air above the first notes line (mm).
const NOTES_TOP_PAD_MM: f64 = 1.0;
/// EN ISO 21920-1 symbol height `h`, the lettering height (mm).
const SYMBOL_H_MM: f64 = NOTES_TEXT_HEIGHT_MM;
/// EN ISO 21920-1 annex: the short leg's height, `H1 = 1.4·h` (mm).
const SYMBOL_H1_MM: f64 = 1.4 * SYMBOL_H_MM;
/// EN ISO 21920-1 annex: the long leg's height, `H2 = 2·H1` (mm).
const SYMBOL_H2_MM: f64 = 2.0 * SYMBOL_H1_MM;
/// The vertical room the surface-texture line needs: the symbol stands `H2` above its baseline,
/// plus the line's own descender air. Its line therefore advances further than a text line.
const SURFACE_LINE_ADVANCE_MM: f64 = SYMBOL_H2_MM + NOTES_LINE_PITCH_MM - NOTES_TEXT_HEIGHT_MM;

/// A paper size. Only A4 is built today; the enum carries the ISO 5457 sizes the sheet code
/// already computes the frame, zone grid and drawing area for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    A4,
    A3,
    A2,
    A1,
    A0,
}

impl Paper {
    /// The format name, as ISO 5457 and the checker spell it.
    pub const fn name(self) -> &'static str {
        match self {
            Paper::A4 => "A4",
            Paper::A3 => "A3",
            Paper::A2 => "A2",
            Paper::A1 => "A1",
            Paper::A0 => "A0",
        }
    }

    /// Width and height in portrait orientation (mm).
    pub const fn portrait_mm(self) -> (f64, f64) {
        match self {
            Paper::A4 => (210.0, 297.0),
            Paper::A3 => (297.0, 420.0),
            Paper::A2 => (420.0, 594.0),
            Paper::A1 => (594.0, 841.0),
            Paper::A0 => (841.0, 1189.0),
        }
    }

    /// Width and height in the landscape orientation a drawing sheet uses (mm).
    pub const fn landscape_mm(self) -> (f64, f64) {
        let (w, h) = self.portrait_mm();
        (h, w)
    }
}

/// The ISO 128 line kinds the sheet's layers carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineKind {
    Continuous,
    /// ISO 128-2 type 02: dash 12·d, gap 3·d, with d the line width.
    Dashed {
        dash_mm: f64,
        gap_mm: f64,
    },
    /// ISO 128-2 type 04: long dash 24·d, gap 3·d, dot 0.5·d, gap 3·d, with d the line width.
    DashDot {
        dash_mm: f64,
        gap_mm: f64,
        dot_mm: f64,
        dot_gap_mm: f64,
    },
}

/// A drawing layer: its ISO 128 line kind, nominal width and the DXF name it is written on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// ISO 5457 frame and centring marks: 0.7 mm.
    Frame,
    /// Visible outlines: 0.5 mm, continuous.
    Visible,
    /// Hidden edges: the thin width (0.25 mm) of the 0.5/0.25 line group, ISO 128-2 type 02
    /// dashed (12·d / 3·d, so 3.0 / 0.75 mm). LN-3 asks hidden edges to be thin.
    Hidden,
    /// Thin lines (dimensions, hatching): 0.25 mm.
    Thin,
    /// Centre lines and centre marks: ISO 128-2 type 04 (long-dash-dot) at the thin 0.25 mm,
    /// long dash 24·d = 6 mm, gap 3·d = 0.75 mm, dot 0.5·d = 0.125 mm, gap 0.75 mm. The DXF
    /// layer is `CENTER`, with the CENTER linetype.
    Center,
    /// Dimensions, leaders and their lettering: thin 0.25 mm continuous on DXF layer `DIMENSION`.
    Dimension,
    /// Section hatching (ISO 128-50): continuous thin 0.25 mm on DXF layer `HATCH`. The DXF
    /// writer emits these lines as real `HATCH` entities instead of one LINE per hatch stroke.
    Hatch,
    /// The section line's thick ends and its filled arrows: 0.5 mm on DXF layer `SECTION`.
    Section,
    /// The section line's thin trace: ISO 128-2 type 04 (long-dash-dot) at 0.25 mm on the DXF
    /// layer `SECTION_LINE`, whose LTYPE is `ISO_SECTION`. The thick ends stay separate entities
    /// on `SECTION`.
    SectionLine,
    /// Tangent (G1) edges when the sheet asks for them: thin 0.25 mm continuous on `TANGENT`.
    Tangent,
    /// A break-out's freehand boundary (ISO 128-2 type 01.1): thin 0.25 mm on `FREEHAND`.
    Freehand,
    /// Title-block outline. Thin like the other non-frame lines, so the sheet keeps one ISO
    /// 128-2 line group (the checker's LN-1 rejects a third drawing width).
    TitleBlock,
    /// Title-block inner rules: 0.25 mm.
    TitleBlockInner,
    /// Lettering.
    Text,
}

/// The pen one sheet layer is drawn with: the DXF layer name, colour and linetype, and the PDF
/// stroke width and ISO 128 line kind. Both writers read [`PENS`], so they cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    /// The DXF layer name (`FRAME`, `VISIBLE`, `HIDDEN`, `THIN`, `TITLE`, `TEXT`).
    pub dxf_name: &'static str,
    /// The DXF colour index (7 white/black, 3 green, 4 cyan).
    pub dxf_color: i16,
    /// The DXF linetype.
    pub dxf_linetype: DxfLinetype,
    /// The stroke width in mm; 0 for lettering, which is not stroked.
    pub width_mm: f64,
    /// The ISO 128 line kind the layer is drawn with.
    pub line_kind: LineKind,
}

/// The one layer → pen table: DXF layer name, colour and linetype, and PDF stroke width and dash.
/// The DXF and PDF writers both serialise the same [`Sheet::prims`] through it.
const PENS: &[(Layer, Pen)] = &[
    (
        Layer::Frame,
        Pen { dxf_name: "FRAME", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: FRAME_WIDTH_MM, line_kind: LineKind::Continuous },
    ),
    (Layer::Visible, Pen { dxf_name: "VISIBLE", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.5, line_kind: LineKind::Continuous }),
    (
        Layer::Hidden,
        // ISO 128-2 type 02 in the thin width of the 0.5/0.25 group: dash 12·d, gap 3·d.
        Pen {
            dxf_name: "HIDDEN",
            dxf_color: 3,
            dxf_linetype: DxfLinetype::Hidden,
            width_mm: 0.25,
            line_kind: LineKind::Dashed { dash_mm: 12.0 * 0.25, gap_mm: 3.0 * 0.25 },
        },
    ),
    (Layer::Thin, Pen { dxf_name: "THIN", dxf_color: 4, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous }),
    (
        Layer::Center,
        Pen {
            dxf_name: "CENTER",
            dxf_color: 4,
            dxf_linetype: DxfLinetype::Center,
            width_mm: 0.25,
            line_kind: LineKind::DashDot { dash_mm: 6.0, gap_mm: 0.75, dot_mm: 0.125, dot_gap_mm: 0.75 },
        },
    ),
    (
        Layer::Dimension,
        Pen { dxf_name: "DIMENSION", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (
        Layer::Hatch,
        Pen { dxf_name: "HATCH", dxf_color: 4, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (
        Layer::Section,
        Pen { dxf_name: "SECTION", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.5, line_kind: LineKind::Continuous },
    ),
    (
        Layer::SectionLine,
        Pen {
            dxf_name: "SECTION_LINE",
            dxf_color: 4,
            dxf_linetype: DxfLinetype::Section,
            width_mm: 0.25,
            line_kind: LineKind::DashDot { dash_mm: 6.0, gap_mm: 0.75, dot_mm: 0.125, dot_gap_mm: 0.75 },
        },
    ),
    (
        Layer::Tangent,
        Pen { dxf_name: "TANGENT", dxf_color: 4, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (
        Layer::Freehand,
        Pen { dxf_name: "FREEHAND", dxf_color: 4, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (
        Layer::TitleBlock,
        Pen { dxf_name: "TITLE", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (
        Layer::TitleBlockInner,
        Pen { dxf_name: "TITLE", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous },
    ),
    (Layer::Text, Pen { dxf_name: "TEXT", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.0, line_kind: LineKind::Continuous }),
];

/// A layer missing from [`PENS`] is drawn as a thin continuous line on DXF layer `0`, never a
/// panic. Every [`Layer`] has a row, so this is unreachable in practice.
const FALLBACK_PEN: Pen = Pen { dxf_name: "0", dxf_color: 7, dxf_linetype: DxfLinetype::Continuous, width_mm: 0.25, line_kind: LineKind::Continuous };

impl Layer {
    /// The pen of this layer, from the one table both writers use.
    pub fn pen(self) -> &'static Pen {
        PENS.iter().find(|(layer, _)| *layer == self).map_or(&FALLBACK_PEN, |(_, pen)| pen)
    }

    /// Nominal line width in mm.
    pub fn width_mm(self) -> f64 {
        self.pen().width_mm
    }

    /// The ISO 128 line kind.
    pub fn line_kind(self) -> LineKind {
        self.pen().line_kind
    }

    /// The DXF layer name (`FRAME`, `VISIBLE`, `HIDDEN`, `THIN`, `TITLE`, `TEXT`).
    pub fn dxf_name(self) -> &'static str {
        self.pen().dxf_name
    }
}

/// Where a text's reference point sits horizontally; the vertical reference is its baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAnchor {
    Left,
    Center,
    Right,
}

/// One 2D primitive on the sheet, in sheet millimetres (origin bottom-left, y up).
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Line {
        a: Vec2,
        b: Vec2,
        layer: Layer,
    },
    Polyline {
        points: Vec<Vec2>,
        closed: bool,
        layer: Layer,
    },
    /// A circular arc, counter-clockwise from `a0` to `a1` radians (a whole circle is `a1 = a0 + 2π`).
    Arc {
        c: Vec2,
        r: f64,
        a0: f64,
        a1: f64,
        layer: Layer,
    },
    Circle {
        c: Vec2,
        r: f64,
        layer: Layer,
    },
    /// An ellipse or an arc of one: the point at parameter `t` is
    /// `c + major·cos t + perp(major)·ratio·sin t`.
    Ellipse {
        c: Vec2,
        major: Vec2,
        ratio: f64,
        t0: f64,
        t1: f64,
        layer: Layer,
    },
    Text {
        at: Vec2,
        height: f64,
        angle_deg: f64,
        text: String,
        anchor: TextAnchor,
        layer: Layer,
    },
    /// A closed convex polygon painted filled (arrowheads, the note dot). The PDF writer fills
    /// it, the DXF writer emits SOLIDs and the page paints a convex polygon.
    Filled {
        points: Vec<Vec2>,
        layer: Layer,
    },
}

impl Prim {
    /// The layer the primitive is drawn on.
    pub fn layer(&self) -> Layer {
        match self {
            Prim::Line { layer, .. }
            | Prim::Polyline { layer, .. }
            | Prim::Arc { layer, .. }
            | Prim::Circle { layer, .. }
            | Prim::Ellipse { layer, .. }
            | Prim::Text { layer, .. }
            | Prim::Filled { layer, .. } => *layer,
        }
    }
}

/// Where one projected view sits on the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedView {
    /// The standard view that was projected.
    pub view: StandardView,
    /// Sheet-mm position of the view's projected origin (the projected world origin).
    pub origin: Vec2,
    /// The view's geometry on the sheet: `[[min x, min y], [max x, max y]]` in mm. Centre lines
    /// reaching past the outline are not part of it; see [`PlacedView::centre_lines`].
    pub bbox: [Vec2; 2],
    /// How many centre-line primitives this view draws (a centre cross counts its two arms).
    pub centre_lines: usize,
}

/// The data fields of the neutral ISO 7200 title block. Labels are drawn by [`build_sheet`];
/// these are the values, with defaults that the drawing checker accepts.
///
/// `gezeichnet` is the one creator field (D8 renamed D5's `erstellt`): the ISO 7200 "created
/// by" name, drawn as the `Gezeichnet` signature row. `geprueft` is the "approved by" name and
/// `aend` the revision index; an empty `geprueft`, `firma` or `aend` is drawn as `–`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleBlock {
    pub benennung: String,
    pub zeichnungsnummer: String,
    pub werkstoff: String,
    pub allgemeintoleranz: String,
    /// Created by (the `Gezeichnet` row).
    pub gezeichnet: String,
    /// The created-by date (the `Tag` column of the `Gezeichnet` row).
    pub datum: String,
    /// Approved by (the `Geprüft` row; empty: `–`).
    pub geprueft: String,
    pub geprueft_datum: String,
    /// Revision index (`–` for the first issue).
    pub aend: String,
    pub aend_datum: String,
    /// Legal owner (ISO 7200 `Firma`; empty: `–`).
    pub firma: String,
    /// Raw size, computed from the geometry unless overridden.
    pub rohmass: String,
    pub dokumentart: String,
    pub blatt: String,
}

/// The general notes above the title block (D8): the general-tolerance source, the projection
/// method, the edge note, the default surface texture and up to [`MAX_EXTRA_NOTES`] free lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotesBlock {
    /// Draw the block at all.
    pub enabled: bool,
    /// Default surface roughness, written with a decimal comma (`3,2`).
    pub ra: String,
    /// The edge note; empty leaves it out.
    pub edge_note: String,
    /// Free extra lines, top to bottom; empty lines are left out.
    pub extra_notes: Vec<String>,
}

impl Default for NotesBlock {
    fn default() -> Self {
        NotesBlock { enabled: true, ra: DEFAULT_RA.to_string(), edge_note: DEFAULT_EDGE_NOTE.to_string(), extra_notes: Vec::new() }
    }
}

impl TitleBlock {
    /// The defaults from a design name alone: the cleaned Benennung and drawing number, today's
    /// date and values the ISO 7200 checker accepts. `7-10.1_Countershaft_Pulley_Shaft` becomes
    /// Benennung `Countershaft Pulley Shaft` and Zeichnungsnummer `7-10.1` (the leading token is
    /// the drawing number, the underscores become spaces).
    pub fn for_design(design_name: &str) -> TitleBlock {
        let (benennung, zeichnungsnummer) = design_names(design_name);
        let today = today_dd_mm_yyyy();
        TitleBlock {
            benennung,
            zeichnungsnummer,
            // `n. b.` (nicht bestimmt) is honest and carries the alphanumeric the checker needs.
            werkstoff: "n. b.".to_string(),
            allgemeintoleranz: "ISO 2768-mK".to_string(),
            gezeichnet: "SolidCraft".to_string(),
            datum: today.clone(),
            geprueft: String::new(),
            geprueft_datum: String::new(),
            aend: String::new(),
            aend_datum: today,
            firma: String::new(),
            rohmass: String::new(),
            dokumentart: "Fertigungszeichnung".to_string(),
            blatt: "1/1".to_string(),
        }
    }

    /// The defaults for a design document: [`TitleBlock::for_design`] plus the design's own
    /// properties (description, part number, author, organization, revision) and the physical
    /// material of its bodies. `rohmass` is the computed default from the geometry (`None` when
    /// nothing is visible).
    pub fn for_document(doc: &solidcraft_doc::Document, rohmass: Option<String>) -> TitleBlock {
        let mut title = TitleBlock::for_design(&doc.name);
        if let Some(description) = doc.metadata.description() {
            title.benennung = clean_text(description);
        }
        if let Some(number) = doc.metadata.part_number() {
            title.zeichnungsnummer = clean_text(number);
        }
        title.werkstoff = design_material(doc).unwrap_or_else(|| "n. b.".to_string());
        if let Some(author) = doc.metadata.author() {
            title.gezeichnet = clean_text(author);
        }
        if let Some(organization) = doc.metadata.organization() {
            title.firma = clean_text(organization);
        }
        if let Some(revision) = doc.metadata.revision() {
            title.aend = clean_text(revision);
        }
        if let Some(rohmass) = rohmass {
            title.rohmass = rohmass;
        }
        title
    }
}

/// The text a field is drawn with: an empty approved-by, company, revision or raw size is the
/// ISO 7200 dash, never an empty cell.
pub fn shown(value: &str) -> &str {
    if value.trim().is_empty() { "\u{2014}" } else { value }
}

impl Default for TitleBlock {
    fn default() -> Self {
        TitleBlock::for_design("Untitled")
    }
}

/// Trimmed text with control characters replaced, as a title-block value.
fn clean_text(text: &str) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_NOTE_CHARS).collect();
    collapse_spaces(cleaned.trim())
}

/// Trimmed text with runs of whitespace collapsed to one space.
fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The Benennung and drawing number a design name gives: `_` becomes a space, and a leading
/// drawing-number token followed by `_`, `-` or a space goes into the drawing number:
/// `7-10.1_Countershaft_Pulley_Shaft` → (`Countershaft Pulley Shaft`, `7-10.1`). A name without
/// such a token (`shaft`, `AB 12-3 Welle`) is both.
pub fn design_names(design_name: &str) -> (String, String) {
    let name = design_name.trim();
    let name = if name.is_empty() { "Untitled" } else { name };
    match leading_token(name) {
        Some((token, rest)) => {
            let rest = collapse_spaces(&rest.replace('_', " "));
            let benennung = if rest.is_empty() { token.to_string() } else { rest };
            (benennung, token.to_string())
        }
        None => {
            let cleaned = collapse_spaces(&name.replace('_', " "));
            (cleaned.clone(), cleaned)
        }
    }
}

/// The leading drawing-number token of a design name and the rest: the name starts with a digit,
/// the token is `[0-9A-Za-z.-]`, and a separator (`_`, a space, or a `-` in front of a letter,
/// which is how `7-10.1-Countershaft…` differs from `2026-10-24_Part`) ends it.
fn leading_token(name: &str) -> Option<(&str, &str)> {
    if !name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let bytes = name.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let byte = bytes[end];
        let token_char = byte.is_ascii_alphanumeric()
            || byte == b'.'
            // A hyphen inside a number (`7-10.1`) is followed by a digit or a dot; one in front
            // of a letter separates the number from the name.
            || (byte == b'-' && bytes.get(end + 1).is_some_and(|next| next.is_ascii_digit() || *next == b'.'));
        if !token_char {
            break;
        }
        end += 1;
    }
    let separator = *bytes.get(end)?;
    if end == 0 || !(separator == b'_' || separator == b' ' || separator == b'-') {
        return None;
    }
    let rest = name.get(end + 1..)?;
    if rest.is_empty() {
        return None;
    }
    Some((name.get(..end)?, rest))
}

/// The physical material of the first body that has one and is not the default, as the material
/// library or the design spells it.
fn design_material(doc: &solidcraft_doc::Document) -> Option<String> {
    for material in doc.materials.values() {
        if material.trim().is_empty() || material.trim().eq_ignore_ascii_case("default") {
            continue;
        }
        if let Some(name) = doc.material_name(material) {
            return Some(name);
        }
    }
    None
}

/// The title block the design stores: [`TitleBlock::for_document`] with the drawing's own saved
/// values (under [`DRAWING_METADATA`]) on top.
pub fn stored_title(doc: &solidcraft_doc::Document, rohmass: Option<String>) -> TitleBlock {
    let mut title = TitleBlock::for_document(doc, rohmass);
    let Some(object) = stored_object(doc) else { return title };
    for (key, slot) in [
        ("werkstoff", &mut title.werkstoff),
        ("geprueft", &mut title.geprueft),
        ("geprueft_datum", &mut title.geprueft_datum),
        ("aend_datum", &mut title.aend_datum),
        ("rohmass", &mut title.rohmass),
        ("dokumentart", &mut title.dokumentart),
        ("allgemeintoleranz", &mut title.allgemeintoleranz),
        ("datum", &mut title.datum),
        ("blatt", &mut title.blatt),
    ] {
        if let Some(text) = stored_text(object, key) {
            *slot = text;
        }
    }
    title
}

/// The notes block the design stores, over [`NotesBlock::default`].
pub fn stored_notes(doc: &solidcraft_doc::Document) -> NotesBlock {
    let mut notes = NotesBlock::default();
    let Some(object) = stored_object(doc) else { return notes };
    if let Some(enabled) = object.get("enabled").and_then(serde_json::Value::as_bool) {
        notes.enabled = enabled;
    }
    // `ra` and `edge_note` may be deliberately empty: their line is then left out.
    if let Some(ra) = stored_field(object, "ra") {
        notes.ra = ra;
    }
    if let Some(edge) = stored_field(object, "edge_note") {
        notes.edge_note = edge;
    }
    if let Some(extra) = object.get("extra_notes").and_then(serde_json::Value::as_array) {
        notes.extra_notes = extra
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(MAX_EXTRA_NOTES)
            .map(|line| line.chars().take(MAX_NOTE_CHARS).collect())
            .collect();
    }
    notes
}

/// The sections spec the design stores (D9.2), or `None` when the design has none (or a corrupt
/// value: a hand-edited file must not stop the sheet, so the caller falls back to `auto`).
pub fn stored_sections(doc: &solidcraft_doc::Document) -> Option<sections::SectionMode> {
    let value = stored_object(doc)?.get("sections")?;
    if value.is_null() {
        return None;
    }
    sections::parse_value(Some(value), "drawing.sheet").ok()
}

/// The tangent-edge rule the design stores (`omit` or `thin`), or `None` when it has none (or a
/// corrupt value: the default `omit` applies).
pub fn stored_tangent_edges(doc: &solidcraft_doc::Document) -> Option<crate::drawing::TangentEdges> {
    let value = stored_object(doc)?.get("tangent_edges")?;
    value.as_str().and_then(crate::drawing::TangentEdges::parse)
}

/// The drawing's namespaced metadata object.
fn stored_object(doc: &solidcraft_doc::Document) -> Option<&serde_json::Map<String, serde_json::Value>> {
    doc.metadata.get(DRAWING_METADATA)?.as_object()
}

/// A stored text value, trimmed and capped like any title value; empty reads as absent.
pub fn stored_text(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    stored_field(object, key).filter(|text| !text.is_empty())
}

/// A stored text value including a deliberately empty one.
fn stored_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    object.get(key)?.as_str().map(|text| text.trim().chars().take(MAX_NOTE_CHARS).collect())
}

/// How the views are arranged on the sheet: first angle (ISO 5456-2 method 1), the pair laid out
/// side by side, or the pair stacked vertically. The scale search tries all of them, so a sheet
/// only drops a scale step when no arrangement fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    FirstAngle,
    SideBySide,
    VerticalStack,
}

impl Arrangement {
    /// The word the commands report.
    pub fn name(self) -> &'static str {
        match self {
            Arrangement::FirstAngle => "first-angle",
            Arrangement::SideBySide => "side-by-side",
            Arrangement::VerticalStack => "vertical-stack",
        }
    }
}

/// One drawn section: the cut regions (as the DXF `HATCH` boundary paths), the hatch strokes of
/// the PDF and the page, and the freehand boundary of a break-out. All in sheet millimetres.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnSection {
    pub view: StandardView,
    pub full: bool,
    pub label: Option<char>,
    /// One region per material piece: the outer loop first, then its islands.
    pub regions: Vec<CutRegion>,
    /// The hatch strokes as drawn (thin, 45°).
    pub hatch: Vec<(Vec2, Vec2)>,
    /// The hatch line spacing in paper millimetres (3 mm, or 2 mm for a narrow region).
    pub spacing_mm: f64,
    pub angle_deg: f64,
    /// The break-out's freehand boundaries in sheet millimetres (empty for a full section).
    pub boundaries: Vec<Vec<Vec2>>,
    /// The boxes the hatching is interrupted around (lettering, ISO 128-50), sheet mm.
    pub gaps: Vec<[Vec2; 4]>,
    /// The cutting plane the view was cut with.
    pub plane: Option<sections::CuttingPlane>,
    /// The picked line a full section was derived from (D9.2).
    pub from: Option<sections::SectionLine>,
}

/// One finished drawing sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub paper: Paper,
    /// Drawing scale as `(numerator, denominator)`: 1:2 is `(1, 2)`, 2:1 is `(2, 1)`.
    pub scale: (u32, u32),
    pub title: TitleBlock,
    /// The general notes above the title block, as drawn.
    pub notes_block: NotesBlock,
    pub views: Vec<PlacedView>,
    /// How the views were arranged.
    pub arrangement: Arrangement,
    pub prims: Vec<Prim>,
    /// How many of `prims` are the sheet furniture (frame, zone grid, title block and notes).
    /// The DXF writes them into the paper-space layout; the drawing views (the prims after this
    /// index) stay in model space, shown through the layout's 1:1 viewport.
    pub furniture_prims: usize,
    /// How many of `prims` are the sheet itself (frame, title block, views, centre lines);
    /// everything after that index is annotation geometry, serialised through the records below.
    pub base_prims: usize,
    pub dimensions: Vec<DrawnDimension>,
    pub notes: Vec<DrawnNote>,
    /// The sections and break-outs drawn on this sheet (D9).
    pub sections: Vec<DrawnSection>,
    /// Dimension texts that could not be placed without crossing a line; they are not drawn.
    pub unplaced: Vec<String>,
}

impl Sheet {
    /// The frame line rectangle `[[left, bottom], [right, top]]` in sheet mm.
    pub fn frame_rect(&self) -> [Vec2; 2] {
        frame_rect_for(self.paper)
    }

    /// The title-block rectangle `[[left, bottom], [right, top]]` in sheet mm.
    pub fn title_block_rect(&self) -> [Vec2; 2] {
        title_block_rect_for(self.paper)
    }

    /// The notes block's rectangle `[[left, bottom], [right, top]]` in sheet mm; both corners
    /// are equal (an empty rect at the title block's top edge) when the block is off.
    pub fn notes_block_rect(&self) -> [Vec2; 2] {
        notes_block_rect_for(self.paper, &self.title, &self.notes_block)
    }

    /// The scale as the title block states it (`"1:2"`).
    pub fn scale_text(&self) -> String {
        format_scale(self.scale)
    }

    /// The scale as a sheet/model factor (2:1 is 2.0).
    pub fn scale_factor(&self) -> f64 {
        scale_factor(self.scale)
    }
}

/// The frame of an ISO 5457 sheet: 20 mm at the left edge, 10 mm at the others.
pub fn frame_rect_for(paper: Paper) -> [Vec2; 2] {
    let (w, h) = paper.landscape_mm();
    [Vec2::new(20.0, 10.0), Vec2::new(w - 10.0, h - 10.0)]
}

/// The title block in the bottom-right corner: 180 mm wide, touching the frame at right and bottom.
pub fn title_block_rect_for(paper: Paper) -> [Vec2; 2] {
    let [f0, fr] = frame_rect_for(paper);
    [Vec2::new(fr.x - TITLE_BLOCK_WIDTH_MM, f0.y), Vec2::new(fr.x, f0.y + TITLE_BLOCK_HEIGHT_MM)]
}

/// One line of the notes block: plain lettering, or the default-surface line with its
/// EN ISO 21920-1 symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteLine {
    /// A plain text line.
    Text(String),
    /// The default-surface line: the symbol, `Ra <value>` on its extension, the bracketed basic
    /// symbol and the plain statement `Oberfläche Ra <value>`.
    Surface(String),
}

impl NoteLine {
    /// The text the line states (what the sheet checker reads).
    pub fn text(&self) -> &str {
        match self {
            NoteLine::Text(text) | NoteLine::Surface(text) => text,
        }
    }

    /// The vertical room the line takes, measured from its top to the next line's top (mm).
    fn advance_mm(&self) -> f64 {
        match self {
            NoteLine::Text(_) => NOTES_LINE_PITCH_MM,
            NoteLine::Surface(_) => SURFACE_LINE_ADVANCE_MM,
        }
    }

    /// The room above the baseline: a text line is `3.5` high, the surface line's symbol stands
    /// `H2` above its baseline.
    fn rise_mm(&self) -> f64 {
        match self {
            NoteLine::Text(_) => NOTES_TEXT_HEIGHT_MM,
            NoteLine::Surface(_) => SYMBOL_H2_MM,
        }
    }
}

/// The general-tolerance line: `Allgemeintoleranzen ISO 2768-m, K nach EN ISO 22081:2021` for the
/// default `ISO 2768-mK`. An ISO 2768 value without a geometric class drops the `K nach …` part;
/// anything that is not an ISO 2768 class at all is written as given.
pub fn general_tolerance_note(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let Some(classes) = iso_2768_classes(value) else { return value.to_string() };
    match classes.geometric {
        Some(geometric) => {
            format!("Allgemeintoleranzen ISO 2768-{}, {geometric} nach EN ISO 22081:2021", classes.linear)
        }
        None => format!("Allgemeintoleranzen ISO 2768-{}", classes.linear),
    }
}

/// The linear and geometric classes of an `ISO 2768` value (`mK`, `m`, `f-K`); `None` when the
/// value names another tolerance standard.
fn iso_2768_classes(value: &str) -> Option<Iso2768> {
    let head = value.get(..3)?;
    if !head.eq_ignore_ascii_case("iso") {
        return None;
    }
    let rest = value.get(3..)?.trim_start().strip_prefix("2768")?;
    let rest = rest.trim_start().trim_start_matches(['-', '\u{2013}']);
    let rest: String = rest.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    let mut chars = rest.chars();
    let linear = chars.next()?.to_ascii_lowercase();
    if !matches!(linear, 'm' | 'f' | 'c' | 'v') {
        return None;
    }
    let geometric = chars.next().map(|c| c.to_ascii_uppercase());
    if chars.next().is_some() || !geometric.as_ref().is_none_or(|g| matches!(g, 'K' | 'L' | 'H')) {
        return None;
    }
    Some(Iso2768 { linear, geometric })
}

/// The classes of an ISO 2768 general-tolerance value.
struct Iso2768 {
    linear: char,
    geometric: Option<char>,
}

/// The block's lines, top to bottom, empty ones left out.
pub fn notes_block_lines(title: &TitleBlock, notes: &NotesBlock) -> Vec<NoteLine> {
    if !notes.enabled {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for text in [general_tolerance_note(&title.allgemeintoleranz), "Projektionsmethode 1".to_string(), notes.edge_note.trim().to_string()] {
        if !text.is_empty() {
            lines.push(NoteLine::Text(text));
        }
    }
    let ra = notes.ra.trim();
    if !ra.is_empty() {
        lines.push(NoteLine::Surface(format!("Oberfl\u{00e4}che Ra {ra}")));
    }
    for extra in notes.extra_notes.iter().take(MAX_EXTRA_NOTES) {
        let text = extra.trim();
        if !text.is_empty() {
            lines.push(NoteLine::Text(text.chars().take(MAX_NOTE_CHARS).map(|c| if c.is_control() { ' ' } else { c }).collect()));
        }
    }
    lines
}

/// The notes block's height (mm): its lines' advance plus the air above the first one.
pub fn notes_block_height(title: &TitleBlock, notes: &NotesBlock) -> f64 {
    let lines = notes_block_lines(title, notes);
    if lines.is_empty() {
        return 0.0;
    }
    NOTES_TOP_PAD_MM + lines.iter().map(NoteLine::advance_mm).sum::<f64>()
}

/// The notes block's rectangle: left-aligned with the title block's left edge, directly above it,
/// as wide as the title block. An empty block is a zero-height rectangle at the block's top.
pub fn notes_block_rect_for(paper: Paper, title: &TitleBlock, notes: &NotesBlock) -> [Vec2; 2] {
    let [tb0, tb1] = title_block_rect_for(paper);
    [Vec2::new(tb0.x, tb1.y), Vec2::new(tb1.x, tb1.y + notes_block_height(title, notes))]
}

/// One title-block cell: its rectangle in sheet mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleCell {
    pub rect: [Vec2; 2],
    /// The text the cell is captioned with (`None` for the signature table's sub-cells, whose
    /// captions are drawn as their own cells).
    pub label: Option<&'static str>,
}

/// The title block's cells in sheet mm, in drawing order. The drawing code and the geometry
/// tests share this table, so a moved rule cannot leave an unchecked text.
///
/// Layout of the 180 × 49 mm block (x from the left edge, y from the bottom):
/// - top strip 37…49: Benennung (0…100), Werkstoff (100…140), Rohmaß (140…180);
/// - left column 0…48: Zeichnungsnummer (27…37), Maßstab (18…27), Allgemeintoleranz (9…18),
///   Ausgabedatum (0…9);
/// - signature table 48…180, 18…37: the `Erstellt durch` caption, the `Tag` and `Name` column
///   headers, and the `Gezeichnet`/`Geprüft` rows;
/// - bottom band 0…18 right of the left column: Firma (48…110, 9…18),
///   Dokumentart (110…180, 9…18), Änd. (48…90, 0…9), Blatt (90…120, 0…9),
///   Projektion (120…180, 0…9).
///
/// Every label sits 3 mm below its cell's top and every value 0.5 mm above its bottom, so a
/// label with a descender (`Allgemeintoleranz`, `Maßstab`, `Ausgabedatum`) still clears its
/// value by more than 0.5 mm.
pub fn title_block_cells() -> Vec<TitleCell> {
    let cell = |x0: f64, y0: f64, x1: f64, y1: f64, label: Option<&'static str>| TitleCell { rect: [Vec2::new(x0, y0), Vec2::new(x1, y1)], label };
    vec![
        cell(0.0, 37.0, 100.0, 49.0, Some("Benennung")),
        cell(100.0, 37.0, 140.0, 49.0, Some("Werkstoff")),
        cell(140.0, 37.0, 180.0, 49.0, Some("Rohma\u{00df}")),
        cell(0.0, 27.0, 48.0, 37.0, Some("Zeichnungsnummer")),
        cell(0.0, 18.0, 48.0, 27.0, Some("Ma\u{00df}stab")),
        cell(0.0, 9.0, 48.0, 18.0, Some("Allgemeintoleranz")),
        cell(0.0, 0.0, 48.0, 9.0, Some("Ausgabedatum")),
        cell(48.0, 32.0, 72.0, 37.0, Some("Erstellt durch")),
        cell(72.0, 32.0, 100.0, 37.0, Some("Tag")),
        cell(100.0, 32.0, 180.0, 37.0, Some("Name")),
        cell(48.0, 25.0, 72.0, 32.0, Some("Gezeichnet")),
        cell(72.0, 25.0, 100.0, 32.0, None),
        cell(100.0, 25.0, 180.0, 32.0, None),
        cell(48.0, 18.0, 72.0, 25.0, Some("Gepr\u{00fc}ft")),
        cell(72.0, 18.0, 100.0, 25.0, None),
        cell(100.0, 18.0, 180.0, 25.0, None),
        cell(48.0, 9.0, 110.0, 18.0, Some("Firma")),
        cell(110.0, 9.0, 180.0, 18.0, Some("Dokumentart")),
        cell(48.0, 0.0, 90.0, 9.0, Some("\u{00c4}nd.")),
        cell(90.0, 0.0, 120.0, 9.0, Some("Blatt")),
        cell(120.0, 0.0, 180.0, 9.0, Some("Projektionsmethode")),
    ]
}

/// `(n, d)` as `"n:d"`.
pub fn format_scale((n, d): (u32, u32)) -> String {
    format!("{n}:{d}")
}

/// Parse an ISO-style scale string (`"1:2"`, spaces allowed around the colon). Two positive
/// numbers, neither above 100 000.
pub fn parse_scale(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    if text.len() > 32 {
        return None;
    }
    let (n, d) = text.split_once(':')?;
    if d.contains(':') {
        return None;
    }
    let n: u32 = n.trim().parse().ok()?;
    let d: u32 = d.trim().parse().ok()?;
    (n > 0 && d > 0 && n <= 100_000 && d <= 100_000).then_some((n, d))
}

/// Today's date as `dd.mm.yyyy` (UTC), without a calendar crate.
pub fn today_dd_mm_yyyy() -> String {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    format!("{d:02}.{m:02}.{y:04}")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's public-domain
/// `civil_from_days`). Never panics: impossible month/day values fall back to 1.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// ISO 5455 scale factor: sheet length / model length.
fn scale_factor((n, d): (u32, u32)) -> f64 {
    f64::from(n) / f64::from(d)
}

fn finite_bbox(b: [Vec2; 2]) -> bool {
    b[0].x.is_finite() && b[0].y.is_finite() && b[1].x.is_finite() && b[1].y.is_finite() && b[1].x >= b[0].x && b[1].y >= b[0].y
}

/// A scaled view and its position relative to the front view's projected origin, before the
/// whole arrangement is moved into the drawing area.
struct ScaledView {
    view: StandardView,
    /// The projected view's bounding box on the sheet, scaled.
    lo: Vec2,
    hi: Vec2,
    /// The box the layout reserves: the geometry plus the centre lines that reach past it.
    layout_lo: Vec2,
    layout_hi: Vec2,
    /// The view's centre lines on the sheet, relative to its projected origin.
    centre: Vec<(Vec2, Vec2)>,
    /// The midpoint of an in-plane axis (a turned part's longitudinal view), sheet mm.
    axis_mid: Option<Vec2>,
    /// The centre of a round axis group seen end-on (a turned part's end view), sheet mm.
    cross_centre: Option<Vec2>,
    rel: Vec2,
}

/// The centre lines of one view on the sheet (mm, relative to the view's projected origin): each
/// in-plane axis over the view's outline plus [`CENTRE_OVERSHOOT_MM`] at both ends, each axis
/// along the view direction as the two arms of a centre cross reaching the same distance past the
/// largest circle of its group. Collinear arms that overlap (holes on a common line) share one
/// line.
pub fn centre_segments(view: &ProjectedView, s: f64) -> Vec<(Vec2, Vec2)> {
    let (lo, hi) = (view.bbox[0] * s, view.bbox[1] * s);
    let corners = [Vec2::new(lo.x, lo.y), Vec2::new(hi.x, lo.y), Vec2::new(hi.x, hi.y), Vec2::new(lo.x, hi.y)];
    let mut segments = Vec::new();
    for item in &view.centre {
        match *item {
            ViewCentre::Line { a, b } => {
                let (a, b) = (a * s, b * s);
                let Some(dir) = (b - a).normalized() else { continue };
                // The line runs over the axis extent and the outline, with the overshoot past both.
                let (mut t0, mut t1) = (0.0_f64, a.dist(b));
                for corner in corners {
                    let t = (corner - a).dot(dir);
                    t0 = t0.min(t);
                    t1 = t1.max(t);
                }
                segments.push((a + dir * (t0 - CENTRE_OVERSHOOT_MM), a + dir * (t1 + CENTRE_OVERSHOOT_MM)));
            }
            ViewCentre::Cross { c, radius } => {
                let centre = c * s;
                let arm = radius * s + CENTRE_OVERSHOOT_MM;
                if !(arm.is_finite() && arm > 0.0) {
                    continue;
                }
                segments.push((Vec2::new(centre.x - arm, centre.y), Vec2::new(centre.x + arm, centre.y)));
                segments.push((Vec2::new(centre.x, centre.y - arm), Vec2::new(centre.x, centre.y + arm)));
            }
        }
    }
    merge_collinear(segments)
}

/// Merge centre lines that lie on one line and overlap or touch (the arms of two crosses on a
/// common line, or a cross arm and a hole's own axis).
fn merge_collinear(mut segments: Vec<(Vec2, Vec2)>) -> Vec<(Vec2, Vec2)> {
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..segments.len() {
            for j in (i + 1)..segments.len() {
                if let Some(merged) = merge_two(segments[i], segments[j]) {
                    segments[i] = merged;
                    segments.remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    segments
}

/// One segment over both when they lie on one line and overlap or touch.
fn merge_two(a: (Vec2, Vec2), b: (Vec2, Vec2)) -> Option<(Vec2, Vec2)> {
    const TOL: f64 = 1e-7;
    let (da, db) = (a.1 - a.0, b.1 - b.0);
    let (la, lb) = (da.len(), db.len());
    if !(la > TOL && lb > TOL) {
        return None;
    }
    let (ua, ub) = (da * (1.0 / la), db * (1.0 / lb));
    if ua.cross(ub).abs() > 1e-9 {
        return None;
    }
    // Both ends of b on a's line?
    let off = |p: Vec2| {
        let w = p - a.0;
        (w - ua * w.dot(ua)).len()
    };
    if off(b.0) > TOL || off(b.1) > TOL {
        return None;
    }
    let (mut t0, mut t1) = (0.0_f64, la);
    for p in [b.0, b.1] {
        let t = (p - a.0).dot(ua);
        t0 = t0.min(t);
        t1 = t1.max(t);
    }
    if t0 > TOL || t1 < la - TOL {
        return None;
    }
    Some((a.0 + ua * t0, a.0 + ua * t1))
}

/// The front view's box for first-angle placement: the drawn front view, or a virtual one at the
/// projected origin whose width comes from a top or bottom view and whose height from a right or
/// left view. `None` when nothing shares either axis.
fn front_box(scaled: &[ScaledView]) -> (Vec2, Vec2) {
    if let Some(front) = scaled.iter().find(|v| v.view == StandardView::Front) {
        return (front.layout_lo, front.layout_hi);
    }
    let fits =
        |views: &[StandardView]| -> Option<(Vec2, Vec2)> { scaled.iter().find(|v| views.contains(&v.view)).map(|v| (v.layout_lo, v.layout_hi)) };
    let across = fits(&[StandardView::Top, StandardView::Bottom]).or_else(|| fits(&[StandardView::Right, StandardView::Left]));
    let up = fits(&[StandardView::Right, StandardView::Left]).or_else(|| fits(&[StandardView::Top, StandardView::Bottom]));
    let (mut lo, mut hi) = (Vec2::ZERO, Vec2::ZERO);
    if let Some((a, b)) = across {
        lo.x = a.x;
        hi.x = b.x;
    }
    if let Some((a, b)) = up {
        lo.y = a.y;
        hi.y = b.y;
    }
    (lo, hi)
}

/// The arrangements the scale search tries for a sheet with `count` views: for a pair all three
/// (first angle, side by side, stacked); the standard first-angle L for three or more.
pub fn arrangements_for(count: usize) -> &'static [Arrangement] {
    const PAIR: [Arrangement; 3] = [Arrangement::FirstAngle, Arrangement::SideBySide, Arrangement::VerticalStack];
    const L: [Arrangement; 1] = [Arrangement::FirstAngle];
    if count <= 2 { &PAIR } else { &L }
}

/// Place the views in `arrangement` and fit the arrangement (with its dimension bands) into the
/// drawing area at `scale`, returning the placed views. `bands` are the four sheet-side bands
/// (above, below, left, right) the dimensions need; `reserved_top` is the notes block's height,
/// which the drawing area excludes above the title block. `None` when the arrangement does not
/// fit.
fn place_views(
    views: &[ProjectedView],
    paper: Paper,
    scale: (u32, u32),
    bands: [f64; 4],
    reserved_top: f64,
    arrangement: Arrangement,
) -> Option<Vec<PlacedView>> {
    let s = scale_factor(scale);
    if !s.is_finite() || s <= 0.0 {
        return None;
    }
    if views.is_empty() {
        return Some(Vec::new());
    }
    let mut scaled: Vec<ScaledView> = Vec::with_capacity(views.len());
    for v in views {
        if !finite_bbox(v.bbox) {
            return None;
        }
        let lo = v.bbox[0] * s;
        let hi = v.bbox[1] * s;
        let centre = centre_segments(v, s);
        let mut layout_lo = lo;
        let mut layout_hi = hi;
        for (a, b) in &centre {
            layout_lo = Vec2::new(layout_lo.x.min(a.x).min(b.x), layout_lo.y.min(a.y).min(b.y));
            layout_hi = Vec2::new(layout_hi.x.max(a.x).max(b.x), layout_hi.y.max(a.y).max(b.y));
        }
        // A longitudinal view's axis midpoint and an end view's circle centre, for the pair
        // alignment of a turned part's two views (D9.1).
        let mut axis_mid = None;
        let mut cross_centre = None;
        for item in &v.centre {
            match *item {
                ViewCentre::Line { a, b } => axis_mid = Some((a + b) * (0.5 * s)),
                ViewCentre::Cross { c, .. } => cross_centre = Some(c * s),
            }
        }
        scaled.push(ScaledView { view: v.view, lo, hi, layout_lo, layout_hi, centre, axis_mid, cross_centre, rel: Vec2::ZERO });
    }
    match arrangement {
        Arrangement::FirstAngle => {
            // First angle (ISO method 1): the view from above goes below the front view, the view
            // from the right goes to its left, the view from the left to its right and the view
            // from below above it. Shared axes keep the projections lined up. Without a front
            // view the pair is placed around a virtual one, so a turned part's end view and
            // longitudinal view, or a flat part's plan and edge view, still sit the way method 1
            // puts them.
            let (flo, fhi) = front_box(&scaled);
            for v in &mut scaled {
                v.rel = match v.view {
                    StandardView::Front => Vec2::ZERO,
                    StandardView::Top => Vec2::new(0.0, flo.y - VIEW_GAP_MM - v.layout_hi.y),
                    StandardView::Bottom => Vec2::new(0.0, fhi.y + VIEW_GAP_MM - v.layout_lo.y),
                    StandardView::Right => Vec2::new(flo.x - VIEW_GAP_MM - v.layout_hi.x, 0.0),
                    StandardView::Left => Vec2::new(fhi.x + VIEW_GAP_MM - v.layout_lo.x, 0.0),
                    StandardView::Back | StandardView::Iso => Vec2::ZERO,
                };
            }
            // The rear and isometric views have no first-angle neighbour of their own; put them
            // beside the left view (rear) or the front view (iso), sharing the front's axis.
            let left_max_x = scaled.iter().find(|v| v.view == StandardView::Left).map(|v| v.rel.x + v.layout_hi.x);
            for v in &mut scaled {
                match v.view {
                    StandardView::Back | StandardView::Iso => {
                        let base = left_max_x.unwrap_or(fhi.x);
                        v.rel = Vec2::new(base + VIEW_GAP_MM - v.layout_lo.x, 0.0);
                    }
                    _ => {}
                }
            }
        }
        Arrangement::SideBySide => {
            // The principal (longitudinal or front) view first, every other view to its right on
            // the shared projected origin: a turned part's end view lines up with the axis, and
            // the pair gains width instead of height. A turned pair aligns on its feature, not on
            // the world origin: the end view's centre goes on the longitudinal view's axis
            // midpoint (D9.1: the hub axis on one line, not the section's base).
            let first = scaled.first().map(|v| (v.layout_lo, v.layout_hi))?;
            let base = first.1.x;
            let axis_mid = scaled.first().and_then(|v| v.axis_mid);
            for v in scaled.iter_mut().skip(1) {
                v.rel = Vec2::new(base + VIEW_GAP_MM - v.layout_lo.x, 0.0);
                if let (Some(axis), Some(centre)) = (axis_mid, v.cross_centre) {
                    v.rel.y = axis.y - centre.y;
                }
            }
        }
        Arrangement::VerticalStack => {
            // The principal view at the top, the second below it on the shared projected origin.
            let first = scaled.first().map(|v| (v.layout_lo, v.layout_hi))?;
            let base = first.0.y;
            for v in scaled.iter_mut().skip(1) {
                v.rel = Vec2::new(0.0, base - VIEW_GAP_MM - v.layout_hi.y);
            }
        }
    }
    // The arrangement's own bounding box (centre lines included: they are part of the view).
    let mut min = Vec2::new(f64::INFINITY, f64::INFINITY);
    let mut max = Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for v in &scaled {
        min = Vec2::new(min.x.min(v.rel.x + v.layout_lo.x), min.y.min(v.rel.y + v.layout_lo.y));
        max = Vec2::new(max.x.max(v.rel.x + v.layout_hi.x), max.y.max(v.rel.y + v.layout_hi.y));
    }
    if !(min.x.is_finite() && min.y.is_finite() && max.x.is_finite() && max.y.is_finite()) {
        return None;
    }
    let (w, h) = (max.x - min.x, max.y - min.y);
    let offset = fit_offset(paper, w, h, bands, reserved_top)?;
    let shift = offset - min;
    Some(
        scaled
            .into_iter()
            .map(|v| {
                let origin = v.rel + shift;
                PlacedView { view: v.view, origin, bbox: [origin + v.lo, origin + v.hi], centre_lines: v.centre.len() }
            })
            .collect(),
    )
}

/// The first arrangement that fits at `scale`, with the placed views.
fn place_in_any(
    views: &[ProjectedView],
    paper: Paper,
    scale: (u32, u32),
    bands: [f64; 4],
    reserved_top: f64,
) -> Option<(Vec<PlacedView>, Arrangement)> {
    for &arrangement in arrangements_for(views.len()) {
        if let Some(placed) = place_views(views, paper, scale, bands, reserved_top, arrangement) {
            return Some((placed, arrangement));
        }
    }
    None
}

/// The bottom-left point of the arrangement in the drawing area, or `None` when it does not fit.
///
/// The drawing area is the frame inside less the title block, and it is L-shaped: above the title
/// block the full width is free (less the notes block, whose height is not drawable), beside it
/// only the part left of the block. The arrangement is fitted with the dimensions' bands kept
/// free around it (the room D6/D7 put dimensions in), so the views themselves stay at least that
/// far from the frame — more than the old 10 mm margin; the band replaces that margin, it is not
/// added to it. No dimension keeps D5's [`DIM_BAND_MM`]; stacked rows use [`band_mm`]. The arm
/// that fits at this scale is used, and the scale search in [`build_sheet`] picks the largest
/// ISO 5455 scale any arm fits. The notes block above the title block is reserved, so no view or
/// dimension can land on it.
fn fit_offset(paper: Paper, w: f64, h: f64, bands: [f64; 4], reserved_top: f64) -> Option<Vec2> {
    if !(w.is_finite() && h.is_finite())
        || w < 0.0
        || h < 0.0
        || bands.iter().any(|b| !(b.is_finite() && *b >= 0.0))
        || !(reserved_top.is_finite() && reserved_top >= 0.0)
    {
        return None;
    }
    let [above, below, left, right] = bands;
    let [f0, f1] = frame_rect_for(paper);
    let [tb0, tb1] = title_block_rect_for(paper);
    let (bw, bh) = (w + left + right, h + above + below);
    /// The group's bottom-left in one arm: centred on the arm, then pulled back only as far as
    /// the dimension bands need to stay inside it.
    fn centred(arm: [Vec2; 2], size: Vec2, bands: [f64; 4]) -> Vec2 {
        let (arm_lo, arm_hi) = (arm[0], arm[1]);
        let centre = Vec2::new(arm_lo.x + (arm_hi.x - arm_lo.x - size.x) * 0.5, arm_lo.y + (arm_hi.y - arm_lo.y - size.y) * 0.5);
        let x_lo = arm_lo.x + bands[2];
        let x_hi = (arm_hi.x - bands[3] - size.x).max(x_lo);
        let y_lo = arm_lo.y + bands[0];
        let y_hi = (arm_hi.y - bands[1] - size.y).max(y_lo);
        Vec2::new(centre.x.max(x_lo).min(x_hi), centre.y.max(y_lo).min(y_hi))
    }
    let upper_bottom = tb1.y + reserved_top;
    let (upper_w, upper_h) = (f1.x - f0.x, f1.y - upper_bottom);
    if bw <= upper_w && bh <= upper_h {
        let arm = [Vec2::new(f0.x, upper_bottom), Vec2::new(f1.x, f1.y)];
        return Some(centred(arm, Vec2::new(w, h), bands));
    }
    let (left_w, left_h) = (tb0.x - f0.x, f1.y - f0.y);
    if bw <= left_w && bh <= left_h {
        let arm = [Vec2::new(f0.x, f0.y), Vec2::new(tb0.x, f1.y)];
        return Some(centred(arm, Vec2::new(w, h), bands));
    }
    None
}

/// The band each sheet side needs for the dimensions on it: the largest row count any view puts
/// on that side, turned into a band with [`band_mm`]. Sides without dimensions keep 12 mm.
fn annotation_bands(annotations: &Annotations) -> [f64; 4] {
    let mut views: Vec<StandardView> = Vec::new();
    for dim in &annotations.dimensions {
        if !views.contains(&dim.view) {
            views.push(dim.view);
        }
    }
    let mut rows = [0usize; 4];
    for view in views {
        let dims: Vec<Dimension> = annotations.dimensions.iter().filter(|d| d.view == view).cloned().collect();
        let per_view = crate::drawing::dims::side_rows(&dims);
        for (total, count) in rows.iter_mut().zip(per_view) {
            *total = (*total).max(count);
        }
    }
    [band_mm(rows[0]), band_mm(rows[1]), band_mm(rows[2]), band_mm(rows[3])]
}

/// Build the A4 landscape sheet for `views`: frame, centring marks, ISO 5457 zone grid, ISO 7200
/// title block, the notes block above it, the views in first-angle arrangement and their centre
/// lines. `scale` is `None` to choose the largest ISO 5455 scale whose views and dimension bands
/// fit the drawing area, or `Some((n, d))` to force one.
pub fn build_sheet(views: &[ProjectedView], title: &TitleBlock, notes: &NotesBlock, scale: Option<(u32, u32)>) -> Result<Sheet> {
    build_sheet_with(views, title, notes, scale, &Annotations::default())
}

/// Build the sheet with dimensions and leader notes. `scale` is `None` to choose the largest
/// ISO 5455 scale whose views and dimension bands fit, or `Some((n, d))` to force one.
pub fn build_sheet_with(
    views: &[ProjectedView],
    title: &TitleBlock,
    notes: &NotesBlock,
    scale: Option<(u32, u32)>,
    annotations: &Annotations,
) -> Result<Sheet> {
    if views.len() > 6 {
        return Err(bad(CMD, "a sheet takes at most the six standard views"));
    }
    for (i, v) in views.iter().enumerate() {
        if views[..i].iter().any(|o| o.view == v.view) {
            return Err(bad(CMD, format!("the view `{}` is given more than once", crate::drawing::view_name(v.view))));
        }
        if !finite_bbox(v.bbox) {
            return Err(bad(CMD, "a view's bounding box is not finite"));
        }
    }
    crate::drawing::dims::validate(annotations, &views.iter().map(|v| v.view).collect::<Vec<_>>())?;
    let paper = Paper::A4;
    let bands = annotation_bands(annotations);
    let reserved_top = notes_block_height(title, notes);
    let chosen = match scale {
        Some((n, d)) => {
            if n == 0 || d == 0 || n > 100_000 || d > 100_000 {
                return Err(bad(CMD, "scale must be two positive numbers n:m"));
            }
            if place_in_any(views, paper, (n, d), bands, reserved_top).is_none() {
                return Err(bad(CMD, format!("scale {n}:{d} does not fit the drawing area of an {paper} sheet", paper = paper.name())));
            }
            (n, d)
        }
        None if views.is_empty() => (1, 1),
        None => *ISO_SCALES
            .iter()
            .find(|s| place_in_any(views, paper, **s, bands, reserved_top).is_some())
            .ok_or_else(|| bad(CMD, format!("the views do not fit the drawing area of an {} sheet even at 1:100", paper.name())))?,
    };
    let (placed, arrangement) =
        place_in_any(views, paper, chosen, bands, reserved_top).ok_or_else(|| bad(CMD, "the views do not fit the drawing area"))?;
    let mut prims = Vec::new();
    frame_prims(paper, &mut prims);
    zone_prims(paper, &mut prims);
    title_block_prims(paper, title, &format_scale(chosen), &mut prims);
    notes_block_prims(paper, title, notes, &mut prims);
    let furniture_prims = prims.len();
    let s = scale_factor(chosen);
    let mut drawn_sections = Vec::new();
    for (view, at) in views.iter().zip(&placed) {
        // The analytic curves are the exact geometry; the chords they replace are not drawn.
        for seg in &view.visible_plain {
            prims.push(Prim::Line { a: at.origin + seg.a * s, b: at.origin + seg.b * s, layer: Layer::Visible });
        }
        for curve in &view.visible_curves {
            prims.push(curve_prim(*curve, at.origin, s, Layer::Visible));
        }
        for seg in &view.hidden_plain {
            prims.push(Prim::Line { a: at.origin + seg.a * s, b: at.origin + seg.b * s, layer: Layer::Hidden });
        }
        for curve in &view.hidden_curves {
            prims.push(curve_prim(*curve, at.origin, s, Layer::Hidden));
        }
        for seg in &view.tangent {
            prims.push(Prim::Line { a: at.origin + seg.a * s, b: at.origin + seg.b * s, layer: Layer::Tangent });
        }
        // The section's hatching goes under the centre lines and under every annotation.
        if let Some(section) = &view.section
            && let Some(drawn) = section_prims(&mut prims, section, at, view, s)
        {
            drawn_sections.push(drawn);
        }
        for (a, b) in centre_segments(view, s) {
            prims.push(Prim::Line { a: at.origin + a, b: at.origin + b, layer: Layer::Center });
        }
        for mark in &view.section_marks {
            section_mark_prims(&mut prims, mark, at, s);
        }
        if let Some(section) = &view.section
            && let Some(label) = section.label
        {
            // The A–A label below the section view, 5 mm lettering. Above the section the
            // diameter rows and their values now live (D9.1), and a label between those
            // extension lines would read as part of the nearest dimension.
            let bottom = at.bbox[0].y;
            prims.push(Prim::Text {
                at: Vec2::new((at.bbox[0].x + at.bbox[1].x) * 0.5, bottom - 3.0 - SECTION_LETTER_MM),
                height: SECTION_LETTER_MM,
                angle_deg: 0.0,
                text: format!("{label}\u{2013}{label}"),
                anchor: TextAnchor::Center,
                layer: Layer::Text,
            });
        }
    }
    let mut base_prims = prims.len();
    let drawn: DrawnAnnotations = draw_annotations(&mut prims, &placed, views, s, annotations)?;
    // The hatching is interrupted around every placed text (ISO 128-50), and an interrupted
    // stroke is what both writers paint; the DXF HATCH gets the same boxes as islands.
    if !drawn.text_boxes.is_empty() {
        let annotations_after = prims.split_off(base_prims);
        let mut rebuilt = Vec::with_capacity(prims.len());
        for prim in prims.drain(..) {
            match prim {
                Prim::Line { a, b, layer } if layer == Layer::Hatch => {
                    // Only the part of a stroke that really lies inside a text box is removed
                    // (a conservative box projection would cut strokes well beside the text).
                    let mut parts = vec![crate::drawing::Seg2mm { a, b }];
                    for corners in &drawn.text_boxes {
                        parts = sections::split_by_boundary(&parts, corners, false, 1e-6);
                        if parts.is_empty() {
                            break;
                        }
                    }
                    for part in parts {
                        if part.a.dist(part.b) > 0.05 {
                            rebuilt.push(Prim::Line { a: part.a, b: part.b, layer });
                        }
                    }
                }
                other => rebuilt.push(other),
            }
        }
        base_prims = rebuilt.len();
        prims = rebuilt;
        prims.extend(annotations_after);
        for section in &mut drawn_sections {
            if let Some(placed) = placed.iter().find(|p| p.view == section.view) {
                let lo = placed.bbox[0] - Vec2::new(5.0, 5.0);
                let hi = placed.bbox[1] + Vec2::new(5.0, 5.0);
                let inside = |p: Vec2| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y;
                for corners in &drawn.text_boxes {
                    if corners.iter().all(|p| inside(*p)) {
                        section.gaps.push(*corners);
                    }
                }
                section.hatch = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        Prim::Line { a, b, layer } if *layer == Layer::Hatch => {
                            let mid = (*a + *b) * 0.5;
                            inside(mid).then_some((*a, *b))
                        }
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    Ok(Sheet {
        paper,
        scale: chosen,
        title: title.clone(),
        notes_block: notes.clone(),
        views: placed,
        arrangement,
        prims,
        furniture_prims,
        base_prims,
        dimensions: drawn.dimensions,
        notes: drawn.notes,
        sections: drawn_sections,
        unplaced: drawn.unplaced,
    })
}

/// The `A` letters and label height of a section (mm).
pub const SECTION_LETTER_MM: f64 = 5.0;
/// The section line's thick end length past the outline (mm).
pub const SECTION_END_MM: f64 = 6.0;

/// The prims of one section, in sheet millimetres: hatching, the freehand boundary and (a full
/// section's) section mark. The cut outline itself is already part of the view's visible edges.
fn section_prims(prims: &mut Vec<Prim>, section: &sections::ViewSection, at: &PlacedView, view: &ProjectedView, s: f64) -> Option<DrawnSection> {
    if !(s.is_finite() && s > 0.0) {
        return None;
    }
    let to_sheet = |p: Vec2| at.origin + p * s;
    let loops: Vec<Vec<Vec2>> = section.loops.iter().map(|loop_| loop_.iter().map(|p| to_sheet(*p)).collect()).collect();
    let boundaries: Vec<Vec<Vec2>> = section.boundaries.iter().map(|b| b.iter().map(|p| to_sheet(*p)).collect::<Vec<Vec2>>()).collect();
    let cut_regions = sections::regions(&loops);
    let mut regions: Vec<CutRegion> = Vec::new();
    let mut hatch = Vec::new();
    let mut spacing = 0.0_f64;
    if boundaries.is_empty() {
        // A full section: the whole cut is hatched.
        for region in &cut_regions {
            let step = if sections::region_narrow(region, 10.0) { 2.0 } else { 3.0 };
            spacing = spacing.max(step);
            hatch.extend(sections::hatch_spans(region, step, 45.0, None));
            regions.push(region.clone());
        }
    } else {
        // Each break-out shows only the part of the cut inside its freehand curve: the region is
        // clipped to the boundary, so both the strokes and the DXF HATCH stop on it.
        for boundary in &boundaries {
            for region in sections::clip_regions(&cut_regions, boundary, 1e-6) {
                let step = if sections::region_narrow(&region, 10.0) { 2.0 } else { 3.0 };
                spacing = spacing.max(step);
                hatch.extend(sections::hatch_spans(&region, step, 45.0, None));
                regions.push(region);
            }
            if boundary.len() >= 3 {
                prims.push(Prim::Polyline { points: boundary.clone(), closed: true, layer: Layer::Freehand });
            }
        }
    }
    for (a, b) in &hatch {
        prims.push(Prim::Line { a: *a, b: *b, layer: Layer::Hatch });
    }
    let _ = view;
    Some(DrawnSection {
        view: at.view,
        full: section.full,
        label: section.label,
        regions,
        hatch,
        spacing_mm: if spacing > 0.0 { spacing } else { 3.0 },
        angle_deg: 45.0,
        boundaries,
        gaps: Vec::new(),
        plane: section.plane,
        from: section.from,
    })
}

/// A full section's mark in the end view: the thin long-dash-dot trace on `CENTER` (the centre-line
/// layer, CENTER linetype), its thick 6 mm ends on `SECTION`, the 2.5 mm
/// filled arrows pointing in the viewing direction, and the letter at each arrow.
fn section_mark_prims(prims: &mut Vec<Prim>, mark: &SectionMark, at: &PlacedView, s: f64) {
    let centre = at.origin + mark.centre * s;
    let dir = mark.dir;
    let arrow = mark.arrow;
    let half = mark.half * s;
    let thin = half + 2.0;
    // The thin trace is ISO 128-2 type 04 on its own `SECTION_LINE` layer; the 6 mm thick ends
    // (and their arrows) stay separate continuous entities on `SECTION`.
    prims.push(Prim::Line { a: centre - dir * thin, b: centre + dir * thin, layer: Layer::SectionLine });
    for sign in [-1.0_f64, 1.0] {
        let outline = centre + dir * (sign * half);
        let end = centre + dir * (sign * (half + SECTION_END_MM));
        prims.push(Prim::Line { a: outline, b: end, layer: Layer::Section });
        let tip = end;
        let back = tip - arrow * 2.5;
        let n = arrow.perp() * (2.5 * 15.0_f64.to_radians().tan());
        prims.push(Prim::Filled { points: vec![tip, back + n, back - n], layer: Layer::Section });
        prims.push(Prim::Text {
            at: tip + arrow * 2.6 + dir * (sign * 1.2),
            height: SECTION_LETTER_MM,
            angle_deg: 0.0,
            text: mark.label.to_string(),
            anchor: TextAnchor::Center,
            layer: Layer::Text,
        });
    }
}

/// The frame and the four centring marks at the side midpoints, reaching from the page edge to
/// the frame.
fn frame_prims(paper: Paper, prims: &mut Vec<Prim>) {
    let (w, h) = paper.landscape_mm();
    let [f0, f1] = frame_rect_for(paper);
    prims.push(Prim::Polyline {
        points: vec![Vec2::new(f0.x, f0.y), Vec2::new(f1.x, f0.y), Vec2::new(f1.x, f1.y), Vec2::new(f0.x, f1.y)],
        closed: true,
        layer: Layer::Frame,
    });
    for (a, b) in [
        (Vec2::new(0.0, h * 0.5), Vec2::new(f0.x, h * 0.5)),
        (Vec2::new(w, h * 0.5), Vec2::new(f1.x, h * 0.5)),
        (Vec2::new(w * 0.5, 0.0), Vec2::new(w * 0.5, f0.y)),
        (Vec2::new(w * 0.5, h), Vec2::new(w * 0.5, f1.y)),
    ] {
        prims.push(Prim::Line { a, b, layer: Layer::Frame });
    }
}

/// The ISO 5457 zone grid: numerals 1… along the top, letters A… down the right side, outside
/// the frame. A4 carries them there only (ISO 5457), which is what the drawing checker expects.
fn zone_prims(paper: Paper, prims: &mut Vec<Prim>) {
    let (w, h) = paper.landscape_mm();
    let [f0, f1] = frame_rect_for(paper);
    let numbers = ((w / 50.0).round() as usize).max(2);
    let letters = ((h / 50.0).round() as usize).max(2);
    let (fw, fh) = (f1.x - f0.x, f1.y - f0.y);
    for i in 0..numbers {
        let x = f0.x + fw * (i as f64 + 0.5) / numbers as f64;
        prims.push(Prim::Text {
            at: Vec2::new(x, f1.y + 3.0),
            height: 3.5,
            angle_deg: 0.0,
            text: (i + 1).to_string(),
            anchor: TextAnchor::Center,
            layer: Layer::Text,
        });
    }
    for i in 0..letters {
        let y = f1.y - fh * (i as f64 + 0.5) / letters as f64;
        prims.push(Prim::Text {
            at: Vec2::new(f1.x + 5.0, y - 1.75),
            height: 3.5,
            angle_deg: 0.0,
            text: char::from(b'A'.saturating_add(u8::try_from(i).unwrap_or(0))).to_string(),
            anchor: TextAnchor::Center,
            layer: Layer::Text,
        });
    }
}

/// A text primitive on the lettering layer.
fn text(prims: &mut Vec<Prim>, x: f64, y: f64, height: f64, value: &str, anchor: TextAnchor) {
    prims.push(Prim::Text { at: Vec2::new(x, y), height, angle_deg: 0.0, text: value.to_string(), anchor, layer: Layer::Text });
}

/// A label with its value below it in one title-block cell (2.5 mm inset, values fitted to the
/// cell's width within the ISO 3098 heights).
fn field(prims: &mut Vec<Prim>, cell: [Vec2; 2], label: &str, value: &str, value_height: f64) {
    field_above(prims, cell, label, value, value_height, 1.0);
}

/// [`field`] with the value's own distance from the cell's bottom: a taller value in the top
/// strip needs more, so its descender stays inside its cell and off the rule below.
fn field_above(prims: &mut Vec<Prim>, cell: [Vec2; 2], label: &str, value: &str, value_height: f64, bottom: f64) {
    let [lo, hi] = cell;
    text(prims, lo.x + CELL_INSET_MM, hi.y - 3.0, 2.5, label, TextAnchor::Left);
    let (value, height) = fit_value(value, (hi.x - lo.x - CELL_INSET_MM - CELL_PADDING_MM).max(1.0), value_height);
    text(prims, lo.x + CELL_INSET_MM, lo.y + bottom, height, &value, TextAnchor::Left);
}

/// A value without a label in its cell (the signature table's date and name columns).
fn value_only(prims: &mut Vec<Prim>, cell: [Vec2; 2], value: &str, height: f64) {
    let [lo, hi] = cell;
    let (value, height) = fit_value(value, (hi.x - lo.x - CELL_INSET_MM - CELL_PADDING_MM).max(1.0), height);
    text(prims, lo.x + CELL_INSET_MM, lo.y + 1.0, height, &value, TextAnchor::Left);
}

/// A caption without a value (the signature table's headers and row captions).
fn caption(prims: &mut Vec<Prim>, cell: [Vec2; 2], label: &str) {
    let [lo, hi] = cell;
    text(prims, lo.x + CELL_INSET_MM, hi.y - 3.0, 2.5, label, TextAnchor::Left);
}

/// The neutral ISO 7200 title block, 180 × 56 mm in the bottom-right corner.
fn title_block_prims(paper: Paper, title: &TitleBlock, scale_text: &str, prims: &mut Vec<Prim>) {
    let [tb0, tb1] = title_block_rect_for(paper);
    let (x0, y0, x1, y1) = (tb0.x, tb0.y, tb1.x, tb1.y);
    // The block shares the frame's bottom and right lines; only its own two sides are drawn.
    for (a, b) in [(Vec2::new(x0, y0), Vec2::new(x0, y1)), (Vec2::new(x0, y1), Vec2::new(x1, y1))] {
        prims.push(Prim::Line { a, b, layer: Layer::TitleBlock });
    }
    let inner = Layer::TitleBlockInner;
    // Rules: the strip's bottom and the signature band's bottom across the block; the left
    // column's splits; the signature table's header and row rules; the bottom band's splits.
    let rules = [
        ((0.0, 37.0), (180.0, 37.0)),
        ((0.0, 18.0), (180.0, 18.0)),
        ((0.0, 27.0), (48.0, 27.0)),
        ((0.0, 9.0), (48.0, 9.0)),
        ((48.0, 32.0), (180.0, 32.0)),
        ((48.0, 25.0), (180.0, 25.0)),
        ((48.0, 9.0), (180.0, 9.0)),
        ((48.0, 0.0), (48.0, 49.0)),
        ((100.0, 37.0), (100.0, 49.0)),
        ((140.0, 37.0), (140.0, 49.0)),
        ((72.0, 18.0), (72.0, 37.0)),
        ((100.0, 18.0), (100.0, 37.0)),
        ((110.0, 9.0), (110.0, 18.0)),
        ((90.0, 0.0), (90.0, 9.0)),
        ((120.0, 0.0), (120.0, 9.0)),
    ];
    for ((ax, ay), (bx, by)) in rules {
        prims.push(Prim::Line { a: Vec2::new(x0 + ax, y0 + ay), b: Vec2::new(x0 + bx, y0 + by), layer: inner });
    }
    let cells = title_block_cells();
    let at = |index: usize| -> [Vec2; 2] {
        let [lo, hi] = cells.get(index).map_or([Vec2::ZERO, Vec2::ZERO], |cell| cell.rect);
        [Vec2::new(x0 + lo.x, y0 + lo.y), Vec2::new(x0 + hi.x, y0 + hi.y)]
    };
    field_above(prims, at(0), "Benennung", &title.benennung, 5.0, 1.5);
    field_above(prims, at(1), "Werkstoff", &title.werkstoff, 3.5, 1.5);
    field_above(prims, at(2), "Rohma\u{00df}", shown(&title.rohmass), 3.5, 1.5);
    field(prims, at(3), "Zeichnungsnummer", &title.zeichnungsnummer, 5.0);
    field(prims, at(4), "Ma\u{00df}stab", scale_text, 3.5);
    field(prims, at(5), "Allgemeintoleranz", &title.allgemeintoleranz, 3.5);
    field(prims, at(6), "Ausgabedatum", &title.datum, 3.5);
    // Signature table: the ISO 7200 created-by caption over the Gezeichnet/Geprüft rows, whose
    // name and date sit in the Tag and Name columns (the reference sheet's layout).
    caption(prims, at(7), "Erstellt durch");
    caption(prims, at(8), "Tag");
    caption(prims, at(9), "Name");
    caption(prims, at(10), "Gezeichnet");
    value_only(prims, at(11), shown(&title.datum), 3.5);
    value_only(prims, at(12), shown(&title.gezeichnet), 3.5);
    caption(prims, at(13), "Gepr\u{00fc}ft");
    value_only(prims, at(14), shown(&title.geprueft_datum), 3.5);
    value_only(prims, at(15), shown(&title.geprueft), 3.5);
    field(prims, at(16), "Firma", shown(&title.firma), 3.5);
    field(prims, at(17), "Dokumentart", &title.dokumentart, 3.5);
    // Änd.: the revision index and its date on one line.
    let [lo, hi] = at(18);
    text(prims, lo.x + CELL_INSET_MM, hi.y - 3.0, 2.5, "\u{00c4}nd.", TextAnchor::Left);
    let (index, height) = fit_value(shown(&title.aend), 12.0, 3.5);
    text(prims, lo.x + CELL_INSET_MM, lo.y + 1.0, height, &index, TextAnchor::Left);
    let (date, date_height) = fit_value(shown(&title.aend_datum), (hi.x - lo.x - 18.5 - CELL_PADDING_MM).max(1.0), 3.5);
    text(prims, lo.x + 18.5, lo.y + 1.0, date_height, &date, TextAnchor::Left);
    field(prims, at(19), "Blatt", &title.blatt, 3.5);
    caption(prims, at(20), "Projektionsmethode");
    projection_symbol(prims, at(20));
}

/// The ISO 5456-2 first-angle projection symbol (truncated cone, end view on the right) inside
/// its cell, below the `Projektionsmethode` caption.
fn projection_symbol(prims: &mut Vec<Prim>, [lo, hi]: [Vec2; 2]) {
    let ymid = lo.y + 2.8;
    let xa = lo.x + 7.0;
    let xb = xa + 8.0;
    let (half_a, half_b) = (1.4, 2.0);
    prims.push(Prim::Polyline {
        points: vec![Vec2::new(xa, ymid - half_a), Vec2::new(xa, ymid + half_a), Vec2::new(xb, ymid + half_b), Vec2::new(xb, ymid - half_b)],
        closed: true,
        layer: Layer::TitleBlockInner,
    });
    let cx = xb + 7.0;
    for r in [half_b, 0.8] {
        prims.push(Prim::Circle { c: Vec2::new(cx, ymid), r, layer: Layer::TitleBlockInner });
    }
    // Centre lines are thin (ISO 128-2) and make the symbol read as a projection symbol.
    let (cx0, cx1) = (xa - 4.0, hi.x - 2.0);
    prims.push(Prim::Line { a: Vec2::new(cx0, ymid), b: Vec2::new(cx1, ymid), layer: Layer::Thin });
    prims.push(Prim::Line { a: Vec2::new(cx, ymid - half_b - 0.5), b: Vec2::new(cx, ymid + half_b + 0.5), layer: Layer::Thin });
}

/// The notes block above the title block: 3.5 mm ISO 3098 type B lettering at a 1.5 × line
/// pitch, left-aligned with the title block's left edge, on the same font as the rest.
fn notes_block_prims(paper: Paper, title: &TitleBlock, notes: &NotesBlock, prims: &mut Vec<Prim>) {
    let [tb0, tb1] = title_block_rect_for(paper);
    let lines = notes_block_lines(title, notes);
    if lines.is_empty() {
        return;
    }
    let mut top = tb1.y + notes_block_height(title, notes) - NOTES_TOP_PAD_MM;
    for line in lines {
        let baseline = top - line.rise_mm();
        match &line {
            NoteLine::Text(line_text) => text(prims, tb0.x, baseline, NOTES_TEXT_HEIGHT_MM, line_text, TextAnchor::Left),
            NoteLine::Surface(line_text) => surface_texture_note(prims, tb0.x, baseline, line_text, notes.ra.trim()),
        }
        top -= line.advance_mm();
    }
}

/// The default-surface line with the EN ISO 21920-1 symbol.
///
/// EN ISO 21920-1:2022, clause 4.2 and Annex A: the basic graphical symbol is the two legs of
/// unequal length at 60° to the surface, the short leg `H1 = 1.4·h` and the long leg
/// `H2 = 2·H1`, with `h` the lettering height (3.5 mm here, so H1 = 4.9 and H2 = 9.8 mm). The
/// material-removal symbol adds the horizontal bar; this sheet draws it as the 21920 "long tick"
/// from the short leg's top across the long leg into the horizontal extension that carries the
/// value (the ISO 1302 short bar is not used). The value keeps the decimal comma, and the
/// bracketed basic symbol after it says that all other surfaces are as stated. The line closes
/// with the plain statement `Oberfläche Ra <value>` so every reader and the sheet checker find
/// the default surface as text too.
fn surface_texture_note(prims: &mut Vec<Prim>, x: f64, baseline: f64, plain: &str, ra: &str) {
    let one_deg = std::f64::consts::PI / 180.0;
    let dx1 = SYMBOL_H1_MM / (60.0 * one_deg).tan();
    let dx2 = SYMBOL_H2_MM / (60.0 * one_deg).tan();
    let vertex = Vec2::new(x + dx1 + 0.5, baseline);
    // The basic symbol: short leg at 60° to the surface, long leg at 60° the other side.
    prims.push(Prim::Line { a: vertex, b: Vec2::new(vertex.x - dx1, vertex.y + SYMBOL_H1_MM), layer: Layer::Thin });
    prims.push(Prim::Line { a: vertex, b: Vec2::new(vertex.x + dx2, vertex.y + SYMBOL_H2_MM), layer: Layer::Thin });
    // The material-removal bar and its extension: one long horizontal tick at height H1.
    let value = format!("Ra {ra}");
    let value_width = solidcraft_io::text_width_mm(&value, NOTES_TEXT_HEIGHT_MM);
    // The value starts right of the long leg's top (dx2 at H2), so the 60° leg never touches the
    // lettering (D9: ≥ 0.5 mm), and it sits under the horizontal extension as the norm figure has
    // it. The bar is drawn over the whole value.
    let value_x = (vertex.x + dx1 + 2.0).max(vertex.x + dx2 + 0.5);
    let bar_end = value_x + value_width + 1.5;
    prims.push(Prim::Line {
        a: Vec2::new(vertex.x - dx1, vertex.y + SYMBOL_H1_MM),
        b: Vec2::new(bar_end, vertex.y + SYMBOL_H1_MM),
        layer: Layer::Thin,
    });
    text(prims, value_x, vertex.y + SYMBOL_H1_MM + 0.4, NOTES_TEXT_HEIGHT_MM, &value, TextAnchor::Left);
    // In brackets, the basic symbol again: all other surfaces as stated.
    let bracket_x = bar_end + 2.0;
    text(prims, bracket_x, baseline, NOTES_TEXT_HEIGHT_MM, "(", TextAnchor::Left);
    let basic = Vec2::new(bracket_x + 2.5 + dx1, baseline);
    prims.push(Prim::Line { a: basic, b: Vec2::new(basic.x - dx1, basic.y + SYMBOL_H1_MM), layer: Layer::Thin });
    prims.push(Prim::Line { a: basic, b: Vec2::new(basic.x + dx2, basic.y + SYMBOL_H2_MM), layer: Layer::Thin });
    let close = basic.x + dx2 + 1.0;
    text(prims, close, baseline, NOTES_TEXT_HEIGHT_MM, ")", TextAnchor::Left);
    text(prims, close + 3.0, baseline, NOTES_TEXT_HEIGHT_MM, plain, TextAnchor::Left);
}

/// One analytic view curve as a sheet primitive, placed at `origin` and scaled by `s`.
fn curve_prim(curve: crate::drawing::Curve2mm, origin: Vec2, s: f64, layer: Layer) -> Prim {
    match curve {
        crate::drawing::Curve2mm::Arc { c, r, a0, a1 } => {
            let (c, r) = (origin + c * s, r * s);
            if (a1 - a0 - TAU).abs() < 1e-6 { Prim::Circle { c, r, layer } } else { Prim::Arc { c, r, a0, a1, layer } }
        }
        crate::drawing::Curve2mm::Ellipse { c, major, ratio, t0, t1 } => Prim::Ellipse { c: origin + c * s, major: major * s, ratio, t0, t1, layer },
    }
}

/// A title-block value at the largest ISO 3098 height that fits `max_width`: the nominal height,
/// then the smaller series heights down to 2.5 mm. Widths are the PDF writer's Helvetica
/// metrics, so what fits here also fits the page. Only below 2.5 mm is the text shortened with
/// an ellipsis.
fn fit_value(text: &str, max_width: f64, nominal_height: f64) -> (String, f64) {
    let cleaned: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(400).collect();
    let cleaned = cleaned.trim_end();
    if cleaned.is_empty() {
        return ("\u{2014}".to_string(), nominal_height);
    }
    let finite = max_width.is_finite() && max_width > 0.0;
    let candidates = std::iter::once(nominal_height).chain(SMALL_TEXT_HEIGHTS.iter().copied().filter(|h| *h < nominal_height - 1e-9));
    for height in candidates {
        if finite && text_width_mm(cleaned, height) <= max_width {
            return (cleaned.to_string(), height);
        }
    }
    let height = nominal_height.min(2.5);
    (truncate_to_width(cleaned, max_width, height), height)
}

/// `text` shortened with an ellipsis so it is no wider than `max_width` at `height`.
fn truncate_to_width(text: &str, max_width: f64, height: f64) -> String {
    const ELLIPSIS: char = '\u{2026}';
    let fits = |s: &str| max_width.is_finite() && max_width > 0.0 && text_width_mm(s, height) <= max_width;
    let shortest: String = std::iter::once(ELLIPSIS).collect();
    if !fits(&shortest) {
        return shortest;
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (0..chars.len()).rev() {
        let candidate: String = chars.iter().take(keep).copied().chain(std::iter::once(ELLIPSIS)).collect();
        if fits(&candidate) {
            return candidate;
        }
    }
    shortest
}

/// The DXF entities of one prim, on `layer`. A filled polygon becomes one SOLID (three or four
/// corners) or a fan of triangles; every DXF viewer fills a SOLID.
fn prim_entities(prim: &Prim, layer: &str) -> Vec<DxfEntity> {
    let entity = |shape| DxfEntity { layer: layer.to_string(), shape };
    match prim {
        Prim::Line { a, b, .. } => vec![entity(DxfShape::Line(*a, *b))],
        Prim::Polyline { points, closed, .. } => {
            if points.is_empty() {
                return Vec::new();
            }
            let mut points = points.clone();
            if *closed && points.last().is_some_and(|last| last.dist(points[0]) > 1e-12) {
                points.push(points[0]);
            }
            vec![entity(DxfShape::Polyline { points })]
        }
        Prim::Arc { c, r, a0, a1, .. } => {
            if (a1 - a0 - TAU).abs() < 1e-6 {
                return vec![entity(DxfShape::Circle { c: *c, r: *r })];
            }
            vec![entity(DxfShape::Arc { c: *c, r: *r, a0: a0.to_degrees().rem_euclid(360.0), a1: a1.to_degrees().rem_euclid(360.0) })]
        }
        Prim::Circle { c, r, .. } => vec![entity(DxfShape::Circle { c: *c, r: *r })],
        Prim::Ellipse { c, major, ratio, t0, t1, .. } => {
            vec![entity(DxfShape::Ellipse { c: *c, major: *major, ratio: *ratio, t0: *t0, t1: *t1 })]
        }
        Prim::Text { at, height, angle_deg, text, anchor, .. } => {
            // The PDF's text origin IS the DXF anchor: left text names it as its insertion point
            // (halign 0), centred and right text as the alignment point (groups 72/11). No width
            // estimate shifts it, so a DXF viewer's own font cannot move the text away from the
            // leader or notes margin the PDF aligned it with.
            let anchor = match anchor {
                TextAnchor::Left => solidcraft_io::DxfTextAnchor::Left,
                TextAnchor::Center => solidcraft_io::DxfTextAnchor::Center,
                TextAnchor::Right => solidcraft_io::DxfTextAnchor::Right,
            };
            vec![entity(DxfShape::Text { at: *at, height: *height, angle_deg: *angle_deg, text: text.clone(), anchor })]
        }
        Prim::Filled { points, .. } => {
            if points.len() < 3 {
                return Vec::new();
            }
            if points.len() <= 4 {
                return vec![entity(DxfShape::Solid { points: points.clone() })];
            }
            let Some(first) = points.first().copied() else { return Vec::new() };
            points
                .windows(2)
                .skip(1)
                .filter_map(|pair| match pair {
                    [b, c] => Some(entity(DxfShape::Solid { points: vec![first, *b, *c] })),
                    _ => None,
                })
                .collect()
        }
    }
}

/// Serialise the sheet as an AutoCAD 2000 DXF in sheet millimetres (`$INSUNITS` 4, model space
/// = sheet mm). Dimensions become real `DIMENSION` entities with anonymous blocks holding the
/// prims the PDF paints; notes become `LEADER` + `TEXT`. Text the DXF cannot hold (a character
/// beyond U+FFFF) is an error, never a silently changed drawing.
pub fn sheet_to_dxf(sheet: &Sheet) -> Result<String> {
    // The frame, zone grid, title block and notes are the sheet furniture: they go into the
    // paper-space layout. The views, sections and centre lines are the drawing and stay in model
    // space, which the layout's 1:1 viewport shows on the paper.
    let mut paper_entities = Vec::new();
    for prim in sheet.prims.iter().take(sheet.furniture_prims) {
        paper_entities.extend(prim_entities(prim, prim.layer().dxf_name()));
    }
    let mut entities = Vec::new();
    for prim in sheet.prims.iter().take(sheet.base_prims).skip(sheet.furniture_prims) {
        // Hatching is written as real HATCH entities below, not as one LINE per stroke.
        if prim.layer() == Layer::Hatch {
            continue;
        }
        entities.extend(prim_entities(prim, prim.layer().dxf_name()));
    }
    let hatches: Vec<solidcraft_io::DxfHatch> = sheet
        .sections
        .iter()
        .flat_map(|section| {
            section.regions.iter().map(|region| {
                // A text box inside the cut material is an island: the hatch is interrupted
                // there, as the PDF's split strokes are.
                let gaps: Vec<Vec<Vec2>> = section
                    .gaps
                    .iter()
                    .filter(|corners| {
                        let centre = corners.iter().fold(Vec2::ZERO, |sum, p| sum + *p) * 0.25;
                        sections::inside_polygon(centre, &region.outer)
                            && !region.islands.iter().any(|island| sections::inside_polygon(centre, island))
                    })
                    .map(|corners| corners.to_vec())
                    .collect();
                solidcraft_io::DxfHatch {
                    layer: Layer::Hatch.dxf_name().to_string(),
                    pattern: "ANSI31".to_string(),
                    // ANSI31's nominal spacing is 3.175 mm: the scale gives the sheet's 3 mm (or 2).
                    scale: section.spacing_mm / solidcraft_io::DxfHatch::ANSI31_SPACING_MM,
                    angle_deg: 0.0,
                    loops: std::iter::once(region.outer.clone()).chain(region.islands.iter().cloned()).chain(gaps).collect(),
                }
            })
        })
        .collect();
    let mut dimensions = Vec::new();
    for (index, drawn) in sheet.dimensions.iter().enumerate() {
        let block_name = format!("*D{}", index + 1);
        let block = drawn.prims.iter().flat_map(|prim| prim_entities(prim, "0")).collect();
        let default_text = format_value(drawn.measurement);
        let text_override = (drawn.text != default_text).then(|| drawn.text.clone());
        dimensions.push(solidcraft_io::DxfDimension {
            layer: Layer::Dimension.dxf_name().to_string(),
            kind: match drawn.kind {
                crate::drawing::dims::DimensionKind::Rotated => solidcraft_io::DxfDimKind::Rotated,
                crate::drawing::dims::DimensionKind::Diametric => solidcraft_io::DxfDimKind::Diametric,
                crate::drawing::dims::DimensionKind::Radial => solidcraft_io::DxfDimKind::Radial,
            },
            block_name,
            def_point: drawn.def_point,
            text_midpoint: drawn.text_midpoint,
            def_point2: Some(drawn.def_point2),
            def_point3: Some(drawn.def_point3),
            def_point4: drawn.opposite,
            angle_deg: drawn.angle_deg,
            measurement: drawn.measurement,
            text_override,
            style: solidcraft_io::DIM_STYLE.to_string(),
            block,
        });
    }
    let mut leaders = Vec::new();
    for note in &sheet.notes {
        let layer = Layer::Dimension.dxf_name();
        if let Some((a, b)) = note.reference {
            entities.extend(prim_entities(&Prim::Line { a, b, layer: Layer::Dimension }, layer));
        }
        let mut points: Vec<Vec2> = Vec::new();
        for (a, b) in &note.leader {
            if points.last().is_none_or(|p| p.dist(*a) > 1e-9) {
                points.push(*a);
            }
            points.push(*b);
        }
        leaders.push(solidcraft_io::DxfLeader {
            layer: layer.to_string(),
            style: solidcraft_io::DIM_STYLE.to_string(),
            points,
            arrowhead: note.terminator.len() <= 4,
        });
        if !note.terminator.is_empty() {
            entities.extend(prim_entities(&Prim::Filled { points: note.terminator.clone(), layer: Layer::Dimension }, layer));
        }
        entities.extend(prim_entities(&note.text, layer));
    }
    let mut layers: Vec<DxfLayer> = Vec::new();
    for (_, pen) in PENS {
        if !layers.iter().any(|l| l.name == pen.dxf_name) {
            // The DXF layer carries the same ISO 128-2 width the PDF strokes with. Lettering is
            // filled by the font (a 0 mm pen), so its layer takes the thin 0.25 mm of the group.
            let lineweight = if pen.width_mm > 0.0 { DxfLineweight::from_mm(pen.width_mm) } else { DxfLineweight::THIN };
            layers.push(DxfLayer { name: pen.dxf_name.to_string(), color: pen.dxf_color, linetype: pen.dxf_linetype, lineweight });
        }
    }
    let (w, h) = sheet.paper.landscape_mm();
    let layout = solidcraft_io::DxfLayout {
        name: "Blatt 1".to_string(),
        paper_mm: (w, h),
        // The frame, zone grid, title block and notes are one block reference in paper space:
        // the drawing's own entities stay what the file is about.
        furniture_block: Some("SolidCraft-Sheet".to_string()),
    };
    Ok(solidcraft_io::write_dxf_drawing_layout(&layers, &entities, &paper_entities, &dimensions, &leaders, &hatches, Some(&layout))?)
}

/// The PDF dash array for a line kind and a painted length, in mm; empty for a continuous line.
///
/// Hidden dashes are the plain ISO 128-2 type 02 array. A centre line's long-dash-dot array is
/// adjusted so that the line begins and ends on a long dash and not mid-dot: with `n` gap-dot-gap
/// runs between `n + 1` dashes, the first (and last) dash is `(length − n·(gap + dot + gap)) /
/// (n + 1)`, which is the nominal 6 mm whenever the length allows it.
fn pdf_dash(kind: LineKind, length_mm: f64) -> Vec<f64> {
    match kind {
        LineKind::Continuous => Vec::new(),
        LineKind::Dashed { dash_mm, gap_mm } => vec![dash_mm, gap_mm],
        LineKind::DashDot { dash_mm, gap_mm, dot_mm, dot_gap_mm } => {
            let rest = gap_mm + dot_mm + dot_gap_mm;
            let period = dash_mm + rest;
            if !(length_mm.is_finite() && length_mm > 0.0 && period > 0.0 && rest >= 0.0) {
                return vec![dash_mm, gap_mm, dot_mm, dot_gap_mm];
            }
            let runs = ((length_mm - dash_mm) / period).floor().max(0.0);
            let first = (length_mm - runs * rest) / (runs + 1.0);
            if !(first.is_finite() && first > 0.0) {
                return vec![dash_mm, gap_mm, dot_mm, dot_gap_mm];
            }
            vec![first, gap_mm, dot_mm, dot_gap_mm]
        }
    }
}

/// The length of the path through `points` (closing it when `closed`).
fn path_length(points: &[Vec2], closed: bool) -> f64 {
    let mut length: f64 = points.windows(2).map(|pair| pair[0].dist(pair[1])).sum();
    if closed
        && points.len() > 2
        && let (Some(first), Some(last)) = (points.first(), points.last())
    {
        length += last.dist(*first);
    }
    length
}

/// The PDF text anchor for a sheet text's baseline anchor.
fn pdf_anchor(anchor: TextAnchor) -> PdfTextAnchor {
    match anchor {
        TextAnchor::Left => PdfTextAnchor::BottomLeft,
        TextAnchor::Center => PdfTextAnchor::BottomCenter,
        TextAnchor::Right => PdfTextAnchor::BottomRight,
    }
}

/// Serialise the sheet as one PDF page in sheet millimetres: the page is the paper size, the
/// origin its bottom-left corner, and every primitive is drawn through its layer's [`Pen`] —
/// the same prims, layer table, widths, dashes and text anchors the DXF writer uses.
pub fn sheet_to_pdf(sheet: &Sheet) -> Result<PdfPage> {
    let (width_mm, height_mm) = sheet.paper.landscape_mm();
    if !(width_mm.is_finite() && height_mm.is_finite() && width_mm > 0.0 && height_mm > 0.0) {
        return Err(bad(CMD, "the paper size is not a finite positive size"));
    }
    let finite = |p: &Vec2| p.x.is_finite() && p.y.is_finite();
    let mut items = Vec::with_capacity(sheet.prims.len());
    for prim in &sheet.prims {
        match prim {
            Prim::Line { a, b, layer } => {
                if !(finite(a) && finite(b)) {
                    return Err(bad(CMD, "a line endpoint is not finite"));
                }
                let pen = layer.pen();
                items.push(PdfItem::Path {
                    points: vec![[a.x, a.y], [b.x, b.y]],
                    closed: false,
                    width_mm: pen.width_mm,
                    dash_mm: pdf_dash(pen.line_kind, a.dist(*b)),
                    fill: false,
                });
            }
            Prim::Polyline { points, closed, layer } => {
                if points.iter().any(|p| !finite(p)) {
                    return Err(bad(CMD, "a polyline point is not finite"));
                }
                // An empty polyline is no geometry: the DXF writer skips it too.
                if points.is_empty() {
                    continue;
                }
                let pen = layer.pen();
                items.push(PdfItem::Path {
                    points: points.iter().map(|p| [p.x, p.y]).collect(),
                    closed: *closed,
                    width_mm: pen.width_mm,
                    dash_mm: pdf_dash(pen.line_kind, path_length(points, *closed)),
                    fill: false,
                });
            }
            Prim::Arc { c, r, a0, a1, layer } => {
                if !(finite(c) && r.is_finite() && *r > 0.0 && a0.is_finite() && a1.is_finite() && a1 > a0) {
                    return Err(bad(CMD, "an arc is not finite or has no extent"));
                }
                let pen = layer.pen();
                items.push(PdfItem::EllipseArc {
                    c: [c.x, c.y],
                    major: [*r, 0.0],
                    ratio: 1.0,
                    t0: *a0,
                    t1: *a1,
                    width_mm: pen.width_mm,
                    dash_mm: pdf_dash(pen.line_kind, r * (a1 - a0)),
                });
            }
            Prim::Circle { c, r, layer } => {
                if !(finite(c) && r.is_finite() && *r > 0.0) {
                    return Err(bad(CMD, "a circle is not finite or has no radius"));
                }
                let pen = layer.pen();
                items.push(PdfItem::EllipseArc {
                    c: [c.x, c.y],
                    major: [*r, 0.0],
                    ratio: 1.0,
                    t0: 0.0,
                    t1: TAU,
                    width_mm: pen.width_mm,
                    dash_mm: pdf_dash(pen.line_kind, TAU * r),
                });
            }
            Prim::Ellipse { c, major, ratio, t0, t1, layer } => {
                let (a, b) = (major.len(), major.len() * ratio);
                if !(finite(c)
                    && major.is_finite()
                    && a > 0.0
                    && ratio.is_finite()
                    && *ratio > 0.0
                    && *ratio <= 1.0
                    && t0.is_finite()
                    && t1.is_finite())
                {
                    return Err(bad(CMD, "an ellipse arc is not finite"));
                }
                let pen = layer.pen();
                let h = if a + b > 0.0 { ((a - b) / (a + b)).powi(2) } else { 0.0 };
                let perimeter = std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
                items.push(PdfItem::EllipseArc {
                    c: [c.x, c.y],
                    major: [major.x, major.y],
                    ratio: *ratio,
                    t0: *t0,
                    t1: *t1,
                    width_mm: pen.width_mm,
                    dash_mm: pdf_dash(pen.line_kind, perimeter * ((t1 - t0).abs() / TAU)),
                });
            }
            Prim::Filled { points, layer } => {
                if points.iter().any(|p| !finite(p)) {
                    return Err(bad(CMD, "a filled polygon point is not finite"));
                }
                if points.len() < 3 {
                    continue;
                }
                let pen = layer.pen();
                items.push(PdfItem::Path {
                    points: points.iter().map(|p| [p.x, p.y]).collect(),
                    closed: true,
                    width_mm: pen.width_mm.max(0.01),
                    dash_mm: Vec::new(),
                    fill: true,
                });
            }
            Prim::Text { at, height, angle_deg, text, anchor, .. } => {
                if !finite(at) {
                    return Err(bad(CMD, "a text position is not finite"));
                }
                if !(height.is_finite() && *height > 0.0) {
                    return Err(bad(CMD, "a text height is not a finite positive length"));
                }
                if !angle_deg.is_finite() {
                    return Err(bad(CMD, "a text angle is not finite"));
                }
                items.push(PdfItem::Text {
                    at: [at.x, at.y],
                    height_mm: *height,
                    angle_deg: *angle_deg,
                    text: text.clone(),
                    anchor: pdf_anchor(*anchor),
                    bold: false,
                });
            }
        }
    }
    Ok(PdfPage { width_mm, height_mm, items, title: format!("{} ({})", sheet.title.benennung, sheet.scale_text()) })
}

/// The command id used in errors from this module.
const CMD: &str = "drawing.sheet";

#[cfg(test)]
#[path = "sheet_tests.rs"]
mod tests;
