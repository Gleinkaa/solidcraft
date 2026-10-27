//! A small vector PDF writer: one page of black polylines and standard-font text at true scale.
//!
//! The file is written from the PDF 1.4 structure itself — objects, a cross-reference table and a
//! trailer — so no PDF library is involved. Geometry is given in millimetres and the content
//! stream starts with a `cm` that scales millimetres to points (`72 / 25.4`), so a 100 % print is
//! at true scale: a line of 100 mm is 100 mm on paper.
//!
//! Lines are stroked black with round caps and joins; a dash array makes a dashed line, and a
//! closed path can be filled (arrowheads). Text uses the standard Helvetica and Helvetica-Bold
//! fonts, which every reader has, with `/WinAnsiEncoding`; the writer maps Unicode to those bytes
//! itself (`ä ö ü Ä Ö Ü ß ° ± Ø ø × µ ²` and the rest of Windows-1252) and writes any other
//! character as `?`.
//!
//! Anchoring uses Helvetica's AFM advance widths — the published integer widths in units of
//! 1/1000 em for bytes 32…255, which reproduce the standard metrics to within 2 %. The text box
//! is the cap-height box: `height_mm` is the cap height (the DXF convention used by the sheet
//! code), so the font size is `height_mm / 0.718`. A rotation turns the box about `at`.
//!
//! The content stream is not compressed: the only deflate in `solidcraft-io` sits inside `zip`,
//! which exposes no deflate API, so a `/FlateDecode` filter would need a new dependency. Pages of
//! a drawing are small text and line streams; the file stays readable without one.

use thiserror::Error;

/// Largest side of a page, in millimetres.
const MAX_PAGE_MM: f64 = 2000.0;
/// Widest stroke a path may have, in millimetres.
const MAX_STROKE_MM: f64 = 10.0;
/// Most items one page may hold.
const MAX_ITEMS: usize = 1_000_000;
/// Most entries in one dash array.
const MAX_DASHES: usize = 64;
/// Most characters in one text string.
const MAX_TEXT_CHARS: usize = 1_000_000;
/// Helvetica's cap height as a fraction of the em size (Adobe AFM `CapHeight 718`).
const CAP_HEIGHT: f64 = 0.718;
/// PDF points per millimetre: 72 points per inch, 25.4 millimetres per inch.
const PT_PER_MM: f64 = 72.0 / 25.4;

/// A single page in millimetres, with its drawing items in page coordinates: the origin is the
/// bottom-left corner and y points up.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfPage {
    pub width_mm: f64,
    pub height_mm: f64,
    pub items: Vec<PdfItem>,
    pub title: String,
}

/// One drawing item on a page.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfItem {
    /// Open or closed polyline in mm, origin bottom-left of the page, y up.
    Path {
        points: Vec<[f64; 2]>,
        closed: bool,
        width_mm: f64,
        /// Dash and gap lengths in mm, repeated; empty for a solid line.
        dash_mm: Vec<f64>,
        /// Fill a closed path instead of only stroking it (arrowheads).
        fill: bool,
    },
    /// An ellipse or an arc of one, drawn as cubic Béziers (`c` operators), never as chords:
    /// the point at parameter `t` is `c + major·cos t + perp(major)·ratio·sin t`. A circle is
    /// `ratio` 1; a whole ellipse runs from 0 to 2π.
    EllipseArc {
        c: [f64; 2],
        major: [f64; 2],
        ratio: f64,
        t0: f64,
        t1: f64,
        width_mm: f64,
        /// Dash and gap lengths in mm, repeated; empty for a solid line.
        dash_mm: Vec<f64>,
    },
    /// Single-line text; `anchor` is where `at` sits on the text box.
    Text {
        at: [f64; 2],
        /// Cap height in mm, as DXF TEXT measures it.
        height_mm: f64,
        /// Counter-clockwise rotation in degrees about `at`.
        angle_deg: f64,
        text: String,
        anchor: TextAnchor,
        bold: bool,
    },
}

/// Which point of a text box `at` is: the box spans the text's advance width and its cap height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAnchor {
    BottomLeft,
    BottomCenter,
    BottomRight,
    MiddleLeft,
    Center,
    MiddleRight,
    TopLeft,
    TopCenter,
    TopRight,
}

/// Why a page could not be written.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum PdfError {
    #[error("page {width_mm} mm × {height_mm} mm is outside 0 < side ≤ 2000 mm")]
    PageSize { width_mm: f64, height_mm: f64 },
    #[error("{count} drawing items exceed the limit of 1 000 000")]
    TooManyItems { count: usize },
    #[error("item {index}: {message}")]
    Item { index: usize, message: String },
}

/// Writes `page` as a one-page PDF 1.4 document.
pub fn write_pdf(page: &PdfPage) -> Result<Vec<u8>, PdfError> {
    validate(page)?;
    let content = content_stream(page);
    let media = (page.width_mm * PT_PER_MM, page.height_mm * PT_PER_MM);

    // Objects in file order: 1 catalog, 2 page tree, 3 page, 4 content, 5–6 fonts, 7 info.
    let mut objects: Vec<Vec<u8>> = Vec::with_capacity(7);
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>\n".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>\n".to_vec());
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>\n",
            decimal(media.0, 2),
            decimal(media.1, 2)
        )
        .into_bytes(),
    );
    let mut stream = Vec::with_capacity(content.len() + 64);
    stream.extend_from_slice(format!("<< /Length {} >>\nstream\n", content.len()).as_bytes());
    stream.extend_from_slice(&content);
    stream.extend_from_slice(b"endstream\n");
    objects.push(stream);
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\n".to_vec());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>\n".to_vec());
    let mut info = Vec::new();
    info.extend_from_slice(b"<< /Title (");
    push_pdf_string(&mut info, &winansi_bytes(&page.title));
    info.extend_from_slice(b") /Producer (SolidCraft) >>\n");
    objects.push(info);

    let mut out = Vec::with_capacity(content.len() + 1024);
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"endobj\n");
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        // A cross-reference entry is exactly 20 bytes.
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 7 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1).as_bytes());
    Ok(out)
}

/// Checks everything the writer indexes or turns into a PDF number.
fn validate(page: &PdfPage) -> Result<(), PdfError> {
    let (w, h) = (page.width_mm, page.height_mm);
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 && w <= MAX_PAGE_MM && h <= MAX_PAGE_MM) {
        return Err(PdfError::PageSize { width_mm: w, height_mm: h });
    }
    if page.items.len() > MAX_ITEMS {
        return Err(PdfError::TooManyItems { count: page.items.len() });
    }
    for (index, item) in page.items.iter().enumerate() {
        match item {
            PdfItem::Path { points, width_mm, dash_mm, .. } => {
                check_stroke(index, *width_mm, dash_mm)?;
                for point in points {
                    if !(point[0].is_finite() && point[1].is_finite()) {
                        return Err(item_error(index, format!("point ({}, {}) is not finite", point[0], point[1])));
                    }
                }
            }
            PdfItem::EllipseArc { c, major, ratio, t0, t1, width_mm, dash_mm } => {
                if !c.iter().chain(major).all(|v| v.is_finite()) {
                    return Err(item_error(index, "an ellipse arc has a non-finite centre or axis".to_string()));
                }
                if !(major[0] * major[0] + major[1] * major[1] > 0.0) {
                    return Err(item_error(index, "an ellipse arc has a zero major axis".to_string()));
                }
                if !(ratio.is_finite() && *ratio > 0.0 && *ratio <= 1.0) {
                    return Err(item_error(index, format!("ellipse ratio {ratio} is outside 0 < ratio ≤ 1")));
                }
                if !(t0.is_finite() && t1.is_finite() && *t1 >= *t0 && (*t1 - *t0) <= std::f64::consts::TAU + 1e-9) {
                    return Err(item_error(index, "ellipse arc parameters are not a finite sweep from t0 to t1".to_string()));
                }
                check_stroke(index, *width_mm, dash_mm)?;
            }
            PdfItem::Text { at, height_mm, angle_deg, text, .. } => {
                if !(at[0].is_finite() && at[1].is_finite()) {
                    return Err(item_error(index, format!("text position ({}, {}) is not finite", at[0], at[1])));
                }
                if !(height_mm.is_finite() && *height_mm > 0.0 && *height_mm <= MAX_PAGE_MM) {
                    return Err(item_error(index, format!("text height {height_mm} mm is outside 0 < height ≤ 2000 mm")));
                }
                if !angle_deg.is_finite() {
                    return Err(item_error(index, format!("text angle {angle_deg}° is not finite")));
                }
                if text.chars().count() > MAX_TEXT_CHARS {
                    return Err(item_error(index, format!("text is longer than {MAX_TEXT_CHARS} characters")));
                }
            }
        }
    }
    Ok(())
}

fn item_error(index: usize, message: String) -> PdfError {
    PdfError::Item { index, message }
}

/// The stroke width and dash array both stroked item kinds share.
fn check_stroke(index: usize, width_mm: f64, dash_mm: &[f64]) -> Result<(), PdfError> {
    if !(width_mm.is_finite() && width_mm > 0.0 && width_mm <= MAX_STROKE_MM) {
        return Err(item_error(index, format!("stroke width {width_mm} mm is outside 0 < width ≤ 10 mm")));
    }
    if dash_mm.len() > MAX_DASHES {
        return Err(item_error(index, format!("{} dash entries exceed the limit of {MAX_DASHES}", dash_mm.len())));
    }
    let mut total = 0.0;
    for dash in dash_mm {
        if !(dash.is_finite() && *dash >= 0.0) {
            return Err(item_error(index, format!("dash length {dash} mm is not a finite non-negative number")));
        }
        total += dash;
    }
    if !dash_mm.is_empty() && !(total.is_finite() && total > 0.0) {
        return Err(item_error(index, "the dash array adds up to zero".to_string()));
    }
    Ok(())
}

/// The page's content stream: a millimetre-to-point `cm`, then each item.
fn content_stream(page: &PdfPage) -> Vec<u8> {
    let mut out = Content { bytes: Vec::new() };
    out.text("q\n");
    out.number(PT_PER_MM, 8);
    out.text(" 0 0 ");
    out.number(PT_PER_MM, 8);
    out.text(" 0 0 cm\n1 J 1 j 0 G\n");
    for item in &page.items {
        match item {
            PdfItem::Path { points, closed, width_mm, dash_mm, fill } => {
                out.path(points, *closed, *width_mm, dash_mm, *fill);
            }
            PdfItem::EllipseArc { c, major, ratio, t0, t1, width_mm, dash_mm } => {
                out.ellipse_arc(*c, *major, *ratio, *t0, *t1, *width_mm, dash_mm);
            }
            PdfItem::Text { at, height_mm, angle_deg, text, anchor, bold } => {
                out.text_item(*at, *height_mm, *angle_deg, text, *anchor, *bold);
            }
        }
    }
    out.text("Q\n");
    out.bytes
}

/// A content stream under construction.
struct Content {
    bytes: Vec<u8>,
}

impl Content {
    fn text(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    /// A PDF real number: fixed-point, no exponent, no trailing zeros.
    fn number(&mut self, value: f64, decimals: usize) {
        let number = decimal(value, decimals);
        self.text(&number);
    }

    fn point(&mut self, p: [f64; 2]) {
        self.number(p[0], 4);
        self.text(" ");
        self.number(p[1], 4);
    }

    fn path(&mut self, points: &[[f64; 2]], closed: bool, width_mm: f64, dash_mm: &[f64], fill: bool) {
        let Some(first) = points.first() else { return };
        self.number(width_mm, 4);
        self.text(" w\n[ ");
        for dash in dash_mm {
            self.number(*dash, 4);
            self.text(" ");
        }
        self.text("] 0 d\n");
        self.point(*first);
        self.text(" m\n");
        if points.len() == 1 {
            // A zero-length segment with the round cap paints a dot.
            self.point(*first);
            self.text(" l\n");
        }
        for point in points.iter().skip(1) {
            self.point(*point);
            self.text(" l\n");
        }
        if closed {
            self.text("h\n");
        }
        self.text(if fill && closed {
            "f\n"
        } else if closed {
            "s\n"
        } else {
            "S\n"
        });
    }

    /// An elliptical arc as cubic Béziers: each span of at most 45° is one `c` operator with the
    /// exact circle factor `4/3·tan(Δ/4)`. The stroke and dash settings are those of a path.
    fn ellipse_arc(&mut self, c: [f64; 2], major: [f64; 2], ratio: f64, t0: f64, t1: f64, width_mm: f64, dash_mm: &[f64]) {
        let minor = [-major[1] * ratio, major[0] * ratio];
        let point = |t: f64| {
            let (sin, cos) = t.sin_cos();
            [c[0] + major[0] * cos + minor[0] * sin, c[1] + major[1] * cos + minor[1] * sin]
        };
        let derivative = |t: f64| {
            let (sin, cos) = t.sin_cos();
            [-major[0] * sin + minor[0] * cos, -major[1] * sin + minor[1] * cos]
        };
        self.number(width_mm, 4);
        self.text(" w\n[ ");
        for dash in dash_mm {
            self.number(*dash, 4);
            self.text(" ");
        }
        self.text("] 0 d\n");
        let sweep = t1 - t0;
        // At most 45° per Bézier: a full ellipse is eight curves and a circle stays round.
        let spans = (sweep / std::f64::consts::FRAC_PI_4).ceil().clamp(1.0, 16.0) as usize;
        let step = sweep / spans as f64;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        self.point(point(t0));
        self.text(" m\n");
        for i in 0..spans {
            let (a, b) = (t0 + step * i as f64, t0 + step * (i + 1) as f64);
            let (p0, p3) = (point(a), point(b));
            let (d0, d3) = (derivative(a), derivative(b));
            let p1 = [p0[0] + d0[0] * k, p0[1] + d0[1] * k];
            let p2 = [p3[0] - d3[0] * k, p3[1] - d3[1] * k];
            self.point(p1);
            self.text(" ");
            self.point(p2);
            self.text(" ");
            self.point(p3);
            self.text(" c\n");
        }
        self.text("S\n");
    }

    fn text_item(&mut self, at: [f64; 2], height_mm: f64, angle_deg: f64, text: &str, anchor: TextAnchor, bold: bool) {
        let bytes = winansi_bytes(text);
        let font_size = height_mm / CAP_HEIGHT;
        let widths = if bold { &HELVETICA_BOLD_WIDTHS } else { &HELVETICA_WIDTHS };
        let advances: f64 = bytes.iter().map(|byte| advance(widths, *byte)).sum();
        let width = font_size * advances / 1000.0;
        let (dx, dy) = anchor_offset(anchor, width, height_mm);
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let tx = at[0] + cos * dx - sin * dy;
        let ty = at[1] + sin * dx + cos * dy;
        self.text("BT\n/");
        self.text(if bold { "F2 " } else { "F1 " });
        self.number(font_size, 4);
        self.text(" Tf\n");
        self.number(cos, 6);
        self.text(" ");
        self.number(sin, 6);
        self.text(" ");
        self.number(-sin, 6);
        self.text(" ");
        self.number(cos, 6);
        self.text(" ");
        self.number(tx, 4);
        self.text(" ");
        self.number(ty, 4);
        self.text(" Tm\n(");
        push_pdf_string(&mut self.bytes, &bytes);
        self.text(") Tj\nET\n");
    }
}

/// The offset from `at` to the text's drawing origin, in unrotated text space.
fn anchor_offset(anchor: TextAnchor, width: f64, height: f64) -> (f64, f64) {
    let x = match anchor {
        TextAnchor::BottomLeft | TextAnchor::MiddleLeft | TextAnchor::TopLeft => 0.0,
        TextAnchor::BottomCenter | TextAnchor::Center | TextAnchor::TopCenter => -width / 2.0,
        TextAnchor::BottomRight | TextAnchor::MiddleRight | TextAnchor::TopRight => -width,
    };
    let y = match anchor {
        TextAnchor::BottomLeft | TextAnchor::BottomCenter | TextAnchor::BottomRight => 0.0,
        TextAnchor::MiddleLeft | TextAnchor::Center | TextAnchor::MiddleRight => -height / 2.0,
        TextAnchor::TopLeft | TextAnchor::TopCenter | TextAnchor::TopRight => -height,
    };
    (x, y)
}

/// A real number with `decimals` places, trailing zeros removed, never in exponent notation.
fn decimal(value: f64, decimals: usize) -> String {
    let mut s = format!("{value:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.push('0');
        }
    }
    if s == "-0.0" { "0.0".to_string() } else { s }
}

/// Encodes `text` as WinAnsi (Windows-1252, the PDF `/WinAnsiEncoding`), one byte per character;
/// characters outside it, including control characters, become `?`.
fn winansi_bytes(text: &str) -> Vec<u8> {
    text.chars().map(winansi_byte).collect()
}

/// Width in millimetres of `text` set at `height_mm` cap height, with the writer's Helvetica
/// metrics and its WinAnsi mapping. Callers that lay text out (the drawing sheet's title block)
/// measure with this, so a string that fits their cell also fits the page.
pub fn text_width_mm(text: &str, height_mm: f64) -> f64 {
    let advances: f64 = winansi_bytes(text).iter().map(|byte| advance(&HELVETICA_WIDTHS, *byte)).sum();
    height_mm / CAP_HEIGHT * advances / 1000.0
}

fn winansi_byte(c: char) -> u8 {
    match c {
        '\u{20}'..='\u{7e}' => c as u8,
        '\u{00a0}'..='\u{00ff}' => c as u8,
        '\u{20ac}' => 0x80,
        '\u{201a}' => 0x82,
        '\u{0192}' => 0x83,
        '\u{201e}' => 0x84,
        '\u{2026}' => 0x85,
        '\u{2020}' => 0x86,
        '\u{2021}' => 0x87,
        '\u{02c6}' => 0x88,
        '\u{2030}' => 0x89,
        '\u{0160}' => 0x8a,
        '\u{2039}' => 0x8b,
        '\u{0152}' => 0x8c,
        '\u{017d}' => 0x8e,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201c}' => 0x93,
        '\u{201d}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '\u{02dc}' => 0x98,
        '\u{2122}' => 0x99,
        '\u{0161}' => 0x9a,
        '\u{203a}' => 0x9b,
        '\u{0153}' => 0x9c,
        '\u{017e}' => 0x9e,
        '\u{0178}' => 0x9f,
        _ => b'?',
    }
}

/// Appends `bytes` as a PDF literal string body: `(`, `)` and `\` are escaped.
fn push_pdf_string(out: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            _ => out.push(byte),
        }
    }
}

fn advance(widths: &[u16; 224], byte: u8) -> f64 {
    widths.get((byte as usize).wrapping_sub(32)).copied().unwrap_or(0) as f64
}

/// Helvetica advance widths (AFM, units of 1/1000 em) for WinAnsi bytes 32…255. From Adobe's
/// published standard metrics; the values are the AFM's integer widths, within 2 % of the font.
const HELVETICA_WIDTHS: [u16; 224] = [
    // 32
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 48
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 64
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 80
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 96
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 112
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 0, // 128
    556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, // 144
    0, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667, // 160
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, // 176
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611, // 192
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, // 208
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, // 224
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, // 240
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

/// Helvetica-Bold advance widths (AFM, units of 1/1000 em) for WinAnsi bytes 32…255.
const HELVETICA_BOLD_WIDTHS: [u16; 224] = [
    // 32
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, // 48
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, // 64
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, // 80
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, // 96
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // 112
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, 0, // 128
    556, 0, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, // 144
    0, 278, 278, 500, 500, 350, 556, 1000, 333, 1000, 556, 333, 944, 0, 500, 667, // 160
    278, 333, 556, 556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333, // 176
    400, 584, 333, 333, 333, 611, 556, 278, 333, 333, 365, 556, 834, 834, 834, 611, // 192
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, // 208
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, // 224
    556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278, 278, // 240
    611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556,
];

#[cfg(test)]
#[path = "pdf_out_tests.rs"]
mod tests;
