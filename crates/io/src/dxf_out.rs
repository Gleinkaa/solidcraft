//! The flat pattern's DXF drawing, as AutoCAD 2000 (`AC1015`), the oldest release that has the
//! ELLIPSE and SPLINE entities.
//!
//! Curves stay exact: a circle is a CIRCLE, a circular arc an ARC, a whole ellipse one ELLIPSE
//! and a conic arc of weight below 1 an elliptical ELLIPSE arc; Béziers (TrueType glyph
//! outlines, spline pieces) and the other conics are SPLINEs of their own degree. The file holds
//! what the entities refer to and what release 2000 needs to read them: the version, the
//! handle seed, the units and the extents; the linetype, layer and block-record tables; the
//! model and paper space blocks; handles, owners and the subclass markers of each entity; the
//! root dictionary. Layers carry their ISO 128-2 lineweight in group 370 (a standard value in
//! hundredths of a millimetre) and the header turns lineweight display on (`$LWDISPLAY`), so a
//! viewer draws the same thick and thin lines as the PDF. Entities take their layer's width
//! (`370 = −1`, BYLAYER). TEXT states where on its baseline its point sits — left (halign 0),
//! centred (72 = 1) or right (72 = 2, with the alignment point in group 11) — at width factor 1,
//! so a viewer's own font metrics cannot move a left-aligned note off its margin or leader.
//! Layers can be dashed and a drawing can carry TEXT (for annotations). Group codes follow
//! Autodesk's public DXF reference.

use std::f64::consts::TAU;
use std::fmt::Write as _;

use solidcraft_geom::{Loop2, MAX_BEZIER_DEGREE, Seg2, Vec2};

use crate::{IoError, Result};

/// A layer's linetype, defined in the file's LTYPE table so a viewer needs nothing else.
///
/// The patterns are absolute millimetres (`$LTSCALE` is 1): a sheet is drawn in millimetres at
/// 1:1 on paper, so the dashes of the DXF and of the PDF have the same length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DxfLinetype {
    #[default]
    Continuous,
    /// AutoCAD's DASHED: a dash of 12.7 mm and a gap of 6.35 mm.
    Dashed,
    /// ISO 128-2 type 02 at d = 0.25 mm (the thin line of the 0.5/0.25 group): dash 3 mm and
    /// gap 0.75 mm (12·d, 3·d). The sheet draws hidden edges with exactly this pattern.
    Hidden,
    /// ISO 128-2 type 04 for centre lines at d = 0.25 mm: long dash 6 mm, gap 0.75 mm,
    /// dot 0.125 mm, gap 0.75 mm (24·d, 3·d, 0.5·d, 3·d). A zero dash is a dot in DXF.
    Center,
    /// ISO 128-2 type 04 for the section line's thin trace (a long-dash-dot line); its thick
    /// 6 mm ends are separate continuous entities on the `SECTION` layer.
    Section,
}

impl DxfLinetype {
    fn name(self) -> &'static str {
        match self {
            DxfLinetype::Continuous => "Continuous",
            DxfLinetype::Dashed => "DASHED",
            DxfLinetype::Hidden => "HIDDEN",
            DxfLinetype::Center => "CENTER",
            DxfLinetype::Section => "ISO_SECTION",
        }
    }

    /// The pattern's dash lengths in millimetres: positive a dash, negative a gap, zero a dot.
    fn pattern(self) -> &'static [f64] {
        match self {
            DxfLinetype::Continuous => &[],
            DxfLinetype::Dashed => &[12.7, -6.35],
            DxfLinetype::Hidden => &[3.0, -0.75],
            DxfLinetype::Center | DxfLinetype::Section => &[6.0, -0.75, 0.0, -0.75],
        }
    }

    /// The description written into the LTYPE record.
    fn description(self) -> &'static str {
        match self {
            DxfLinetype::Continuous => "Solid line",
            DxfLinetype::Dashed => "Dashed __ __ __ __ __ __ __ __ __ __",
            DxfLinetype::Hidden => "ISO 128-2 type 02 hidden __ __ __ __",
            DxfLinetype::Center => "ISO 128-2 type 04 centre __ . __ . __",
            DxfLinetype::Section => "ISO 128-2 type 04 section line __ . __ . __",
        }
    }
}

/// A DXF lineweight in hundredths of a millimetre: one of AutoCAD's standard values, or the
/// inherited `BYLAYER` (−1) / `BYDEFAULT` (−3). AutoCAD 2000 (AC1015) introduced lineweights, so
/// an AC1015 file can carry the sheet's ISO 128-2 line group as layer data instead of leaving
/// every layer at `BYDEFAULT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DxfLineweight(i16);

impl DxfLineweight {
    /// AutoCAD's standard lineweights, hundredths of a millimetre: 0.00 … 2.11 mm. Only these may
    /// appear as a layer's group 370 (the reference calls every other value invalid).
    pub const STANDARD: [i16; 24] = [0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200, 211];
    /// The entity's or layer's lineweight comes from its layer (entity group 370 value −1).
    pub const BY_LAYER: DxfLineweight = DxfLineweight(-1);
    /// The layer uses the drawing's default lineweight (layer group 370 value −3).
    pub const BY_DEFAULT: DxfLineweight = DxfLineweight(-3);
    /// 0.25 mm: the thin line of the ISO 128-2 0.5/0.25 line group, the sheet's ordinary width.
    pub const THIN: DxfLineweight = DxfLineweight(25);

    /// The standard lineweight nearest to `width_mm` (a zero or non-finite width is 0.00 mm).
    pub fn from_mm(width_mm: f64) -> DxfLineweight {
        if !width_mm.is_finite() || width_mm <= 0.0 {
            return DxfLineweight(0);
        }
        let target = width_mm * 100.0;
        let nearest =
            Self::STANDARD.iter().copied().min_by(|a, b| (target - f64::from(*a)).abs().total_cmp(&(target - f64::from(*b)).abs())).unwrap_or(0);
        DxfLineweight(nearest)
    }

    /// The value written into group 370, in hundredths of a millimetre.
    pub fn hundredths(self) -> i16 {
        self.0
    }
}

/// A layer: its name, AutoCAD colour index (1 red, 3 green, 5 blue, 7 white or black), linetype
/// and lineweight.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfLayer {
    pub name: String,
    pub color: i16,
    pub linetype: DxfLinetype,
    /// The ISO 128-2 line width of everything on the layer, in hundredths of a millimetre.
    pub lineweight: DxfLineweight,
}

impl DxfLayer {
    /// A continuous layer, thin (0.25 mm) until the caller sets the sheet's own pen.
    pub fn new(name: impl Into<String>, color: i16) -> DxfLayer {
        DxfLayer { name: name.into(), color, linetype: DxfLinetype::Continuous, lineweight: DxfLineweight::THIN }
    }

    /// The same layer with a dashed line.
    pub fn dashed(self) -> DxfLayer {
        DxfLayer { linetype: DxfLinetype::Dashed, ..self }
    }

    /// The same layer with this lineweight.
    pub fn with_lineweight(self, lineweight: DxfLineweight) -> DxfLayer {
        DxfLayer { lineweight, ..self }
    }
}

/// Which point of a [`DxfShape::Text`]'s baseline `at` is: its left end, its middle or its right
/// end. The writer maps this to AutoCAD's horizontal alignment (group 72); the vertical alignment
/// (group 73) is always 0, the baseline, which is where the sheet's PDF text sits too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DxfTextAnchor {
    Left,
    #[default]
    Center,
    Right,
}

/// The geometry of one entity (millimetres).
#[derive(Clone, Debug, PartialEq)]
pub enum DxfShape {
    Line(Vec2, Vec2),
    /// Connected straight segments, with continuous linetype generation through the vertices.
    Polyline {
        points: Vec<Vec2>,
    },
    Circle {
        c: Vec2,
        r: f64,
    },
    /// Counter-clockwise from `a0` to `a1` (degrees).
    Arc {
        c: Vec2,
        r: f64,
        a0: f64,
        a1: f64,
    },
    /// An ellipse or an arc of one, counter-clockwise from parameter `t0` to `t1` (radians):
    /// the point at `t` is `c + major·cos t + perp(major)·ratio·sin t`, `major` being the
    /// vector from the centre to the end of the major axis and `ratio` the minor radius over
    /// the major one (0 < ratio ≤ 1). A whole ellipse runs from 0 to 2π.
    Ellipse {
        c: Vec2,
        major: Vec2,
        ratio: f64,
        t0: f64,
        t1: f64,
    },
    /// A clamped B-spline of `degree` over `ctrl`, rational when `weights` are given.
    Spline {
        degree: u8,
        knots: Vec<f64>,
        ctrl: Vec<Vec2>,
        weights: Option<Vec<f64>>,
    },
    /// One line of text on `at`, turned `angle_deg` counter-clockwise. `anchor` says where on the
    /// string's baseline `at` sits: the left end, the middle (the historical default) or the right
    /// end. `%%d`, `%%p` and `%%c` are AutoCAD's degree, plus-minus and diameter signs; other
    /// letters outside ASCII through U+FFFF are written as `\U+XXXX`. Characters beyond U+FFFF
    /// are refused by [`write_dxf_drawing`], never substituted with `?`.
    Text {
        at: Vec2,
        height: f64,
        angle_deg: f64,
        text: String,
        anchor: DxfTextAnchor,
    },
    /// A closed filled polygon of three or four corners (an arrowhead, a note dot piece). Written
    /// as a SOLID, the entity every DXF viewer fills; more corners are split into triangles by
    /// the caller.
    Solid {
        points: Vec<Vec2>,
    },
}

/// An entity on a layer.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfEntity {
    pub layer: String,
    pub shape: DxfShape,
}

/// The dimension type of a [`DxfDimension`] (group 70, without the anonymous-block bit 32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DxfDimKind {
    /// A rotated linear dimension (`AcDbAlignedDimension` + `AcDbRotatedDimension`).
    Rotated,
    /// A diametric dimension (`AcDbDiametricDimension`).
    Diametric,
    /// A radial dimension (`AcDbRadialDimension`).
    Radial,
}

impl DxfDimKind {
    fn dimtype(self) -> i32 {
        match self {
            DxfDimKind::Rotated => 0,
            DxfDimKind::Diametric => 3,
            DxfDimKind::Radial => 4,
        }
    }
}

/// A real `DIMENSION` entity with its anonymous block. The block holds exactly the geometry the
/// sheet drew (lines, filled SOLID arrowheads, TEXT), so a viewer shows our rendering instead of
/// recomputing the dimension.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfDimension {
    pub layer: String,
    pub kind: DxfDimKind,
    /// The anonymous block name (`*D1`), also written into the block-record table.
    pub block_name: String,
    /// Group 10: the dimension line's definition point (a rotated dimension) or the first
    /// diameter point.
    pub def_point: Vec2,
    /// Group 11: the text's midpoint.
    pub text_midpoint: Vec2,
    /// Group 13: the first extension-line origin.
    pub def_point2: Option<Vec2>,
    /// Group 14: the second extension-line origin.
    pub def_point3: Option<Vec2>,
    /// Group 15: the opposite diameter point, or the point on the arc of a radial dimension.
    pub def_point4: Option<Vec2>,
    /// Group 50: the rotation angle of a rotated dimension (degrees).
    pub angle_deg: f64,
    /// Group 42: the measured model value.
    pub measurement: f64,
    /// Group 1: the text override (`Ø10 h6`), when the text is more than the plain measurement.
    pub text_override: Option<String>,
    /// Group 3: the dimension style the entity names.
    pub style: String,
    /// The geometry inside the anonymous block, in sheet millimetres.
    pub block: Vec<DxfEntity>,
}

/// A standalone `LEADER` with its vertices (a note's leader line).
#[derive(Clone, Debug, PartialEq)]
pub struct DxfLeader {
    pub layer: String,
    /// The dimension style the leader names.
    pub style: String,
    pub points: Vec<Vec2>,
    /// Group 71: 1 when the leader carries an arrowhead.
    pub arrowhead: bool,
}

/// A real `HATCH` entity: an ISO 128-50 hatching pattern over closed boundary paths, the first
/// one the outer boundary and the rest islands (bores, recesses). The pattern is one of AutoCAD's
/// predefined patterns (`ANSI31`), so the file needs no pattern table; `scale` turns its nominal
/// 3.175 mm line spacing into the sheet's 3 mm (or 2 mm) spacing.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfHatch {
    pub layer: String,
    /// The predefined pattern name (`ANSI31`).
    pub pattern: String,
    /// The pattern's line spacing at `scale` 1 is 3.175 mm for ANSI31.
    pub scale: f64,
    /// The pattern's rotation added to its own 45° (degrees; 0 keeps the 45° direction).
    pub angle_deg: f64,
    /// Closed boundary paths in sheet millimetres: the outer loop first, then the islands.
    pub loops: Vec<Vec<Vec2>>,
}

impl DxfHatch {
    /// The nominal line spacing of ANSI31 at scale 1 (mm).
    pub const ANSI31_SPACING_MM: f64 = 3.175;
}

/// The entity of one profile segment, exactly: a line, an arc (a circle when it is a whole
/// turn), an elliptical arc for a conic of weight below 1, a spline for any other conic, and for a
/// Bézier a spline of its own degree over all its control points.
pub fn seg_shape(s: &Seg2) -> DxfShape {
    match *s {
        Seg2::Line { a, b } => DxfShape::Line(a, b),
        Seg2::Arc { center, radius, start, sweep } => {
            if (sweep.abs() - TAU).abs() < 1e-9 {
                return DxfShape::Circle { c: center, r: radius };
            }
            let (a0, a1) = if sweep >= 0.0 { (start, start + sweep) } else { (start + sweep, start) };
            DxfShape::Arc { c: center, r: radius, a0: a0.to_degrees().rem_euclid(360.0), a1: a1.to_degrees().rem_euclid(360.0) }
        }
        Seg2::Conic { a, apex, b, w } => match conic_ellipse(a, apex, b, w) {
            Some(e) => DxfShape::Ellipse { c: e.c, major: e.major, ratio: e.ratio, t0: wrap(e.t0), t1: wrap(e.t1) },
            None => bezier_spline(&[a, apex, b], ((w - 1.0).abs() > 1e-12).then(|| vec![1.0, w, 1.0])),
        },
        Seg2::Cubic { p0, p1, p2, p3 } => bezier_spline(&[p0, p1, p2, p3], None),
        Seg2::Bezier { n, p } => bezier_spline(p.get(..=(n as usize).clamp(1, MAX_BEZIER_DEGREE)).unwrap_or(&p), None),
    }
}

/// The entities of a closed loop: one ELLIPSE when it is a whole ellipse of conic arcs, else
/// one entity for each segment.
pub fn loop_shapes(lp: &Loop2) -> Vec<DxfShape> {
    let arcs: Option<Vec<EllipseArc>> =
        lp.segs.iter().map(|s| if let Seg2::Conic { a, apex, b, w } = *s { conic_ellipse(a, apex, b, w) } else { None }).collect();
    if let Some(arcs) = arcs
        && let Some(first) = arcs.first()
        && arcs.iter().all(|e| e.same_ellipse(first))
        && (arcs.iter().map(|e| e.t1 - e.t0).sum::<f64>() - TAU).abs() < 1e-7
    {
        return vec![DxfShape::Ellipse { c: first.c, major: first.major, ratio: first.ratio, t0: 0.0, t1: TAU }];
    }
    lp.segs.iter().map(seg_shape).collect()
}

/// A parameter reduced to [0, 2π); one within 1e-12 of a full turn is 0.
fn wrap(t: f64) -> f64 {
    let r = t.rem_euclid(TAU);
    if r > TAU - 1e-12 { 0.0 } else { r }
}

/// An elliptical arc as DXF describes it (the parameters are not yet reduced to 0…2π).
#[derive(Clone, Copy, Debug)]
struct EllipseArc {
    c: Vec2,
    major: Vec2,
    ratio: f64,
    t0: f64,
    t1: f64,
}

impl EllipseArc {
    fn same_ellipse(&self, o: &EllipseArc) -> bool {
        let tol = 1e-7 * self.major.len().max(1.0);
        let parallel = self.major.cross(o.major).abs() <= tol * self.major.len();
        self.c.dist(o.c) <= tol && parallel && (self.major.len() - o.major.len()).abs() <= tol && (self.ratio - o.ratio).abs() <= 1e-9
    }
}

/// The ellipse a rational quadratic Bézier of weight `w` below 1 (end weights 1) lies on. The
/// arc is `c + u·cos θ + v·sin θ` for θ from −α to α, `cos α = w`, with the conjugate
/// semi-diameters `u` (from the centre towards the arc's middle) and `v` (towards its end).
/// With `m` the middle of the chord, `apex − m = u·(1 − w²)/w` and `m − c = w·u`, so
/// `c = m − (apex − m)·w²/(1 − w²)`, and `v = (b − m)/sin α`. The principal axes are where
/// |u cos t + v sin t| is largest and smallest, tan 2t = 2 u·v / (u² − v²).
fn conic_ellipse(a: Vec2, apex: Vec2, b: Vec2, w: f64) -> Option<EllipseArc> {
    // Within 1e-6 of 1 the centre is hundreds of metres away: a rational spline is exact too.
    if !(w > 1e-9 && w < 1.0 - 1e-6) {
        return None;
    }
    let m = (a + b) * 0.5;
    let c = m - (apex - m) * (w * w / (1.0 - w * w));
    let alpha = w.acos();
    let (u, v) = ((m - c) / w, (b - m) / alpha.sin());
    if !(c.is_finite() && u.is_finite() && v.is_finite()) || u.cross(v).abs() < 1e-18 {
        return None;
    }
    let t = 0.5 * (2.0 * u.dot(v)).atan2(u.dot(u) - v.dot(v));
    let major = u * t.cos() + v * t.sin();
    let minor = v * t.cos() - u * t.sin();
    if !(major.len() > 1e-12) {
        return None;
    }
    let ratio = (minor.len() / major.len()).clamp(1e-12, 1.0);
    // DXF runs counter-clockwise with the minor axis a quarter turn ahead of the major one. If
    // `minor` lies on the other side the arc, which runs a → b with increasing θ, is clockwise.
    let (t0, t1) = if major.cross(minor) > 0.0 { (-alpha - t, alpha - t) } else { (t - alpha, t + alpha) };
    Some(EllipseArc { c, major, ratio, t0, t1 })
}

/// A Bézier over `pts` (weights `weights`) as a clamped spline of degree `pts.len() − 1`.
fn bezier_spline(pts: &[Vec2], weights: Option<Vec<f64>>) -> DxfShape {
    let degree = pts.len().saturating_sub(1).max(1);
    let knots = std::iter::repeat_n(0.0, degree + 1).chain(std::iter::repeat_n(1.0, degree + 1)).collect();
    DxfShape::Spline { degree: degree as u8, knots, ctrl: pts.to_vec(), weights }
}

impl DxfShape {
    /// Points on the shape (or its control polygon) for the drawing's extents.
    fn sample(&self, out: &mut Vec<Vec2>) {
        match self {
            DxfShape::Line(a, b) => out.extend([*a, *b]),
            DxfShape::Polyline { points } => out.extend(points.iter().copied()),
            DxfShape::Circle { c, r } => out.extend([*c - Vec2::new(*r, *r), *c + Vec2::new(*r, *r)]),
            DxfShape::Arc { c, r, a0, a1 } => {
                let sweep = (a1 - a0).rem_euclid(360.0);
                out.extend((0..=64).map(|i| *c + Vec2::from_angle((a0 + sweep * i as f64 / 64.0).to_radians()) * *r));
            }
            DxfShape::Ellipse { c, major, ratio, t0, t1 } => {
                let (minor, sweep) = (major.perp() * *ratio, if (t1 - t0 - TAU).abs() < 1e-12 { TAU } else { (t1 - t0).rem_euclid(TAU) });
                out.extend((0..=128).map(|i| {
                    let t = t0 + sweep * i as f64 / 128.0;
                    *c + *major * t.cos() + minor * t.sin()
                }));
            }
            // A spline lies within the hull of its control points.
            DxfShape::Spline { ctrl, .. } => out.extend(ctrl.iter().copied()),
            DxfShape::Solid { points } => out.extend(points.iter().copied()),
            // Where a viewer puts the letters depends on its font: 0.8 heights for each.
            DxfShape::Text { at, height, angle_deg, text, anchor } => {
                let (along, up) = (Vec2::from_angle(angle_deg.to_radians()), Vec2::from_angle(angle_deg.to_radians()).perp());
                let width = 0.8 * height * text.chars().count() as f64;
                let (from, to) = match anchor {
                    DxfTextAnchor::Left => (0.0, width),
                    DxfTextAnchor::Center => (-width * 0.5, width * 0.5),
                    DxfTextAnchor::Right => (-width, 0.0),
                };
                out.extend([from, to].map(|s| *at + along * s));
                out.extend([from, to].map(|s| *at + along * s + up * *height));
            }
        }
    }
}

/// Group codes and handles, in file order.
struct Out {
    s: String,
    next: u64,
}

impl Out {
    fn code(&mut self, c: i32, v: impl std::fmt::Display) {
        let _ = write!(self.s, "{c:>3}\n{v}\n");
    }
    /// A real number: up to twelve decimals, at least one.
    fn num(&mut self, c: i32, v: f64) {
        let text = format!("{:.12}", if v.is_finite() { v } else { 0.0 });
        let text = text.trim_end_matches('0');
        let text = if text.ends_with('.') { format!("{text}0") } else { text.to_string() };
        self.code(c, if text == "-0.0" { "0.0" } else { text.as_str() });
    }
    /// A point in the plane at height 0: codes `c`, `c + 10`, `c + 20`.
    fn point(&mut self, c: i32, p: Vec2) {
        self.num(c, p.x);
        self.num(c + 10, p.y);
        self.num(c + 20, 0.0);
    }
    fn handle(&mut self) -> String {
        let h = format!("{:X}", self.next);
        self.next += 1;
        h
    }
    /// The start of a symbol table; returns its handle.
    fn table(&mut self, name: &str, entries: usize) -> String {
        let h = self.handle();
        self.code(0, "TABLE");
        self.code(2, name);
        self.code(5, &h);
        self.code(330, 0);
        self.code(100, "AcDbSymbolTable");
        self.code(70, entries);
        h
    }
    /// The start of a symbol table record; returns its handle.
    fn record(&mut self, kind: &str, subclass: &str, table: &str) -> String {
        let h = self.handle();
        self.code(0, kind);
        self.code(5, &h);
        self.code(330, table);
        self.code(100, "AcDbSymbolTableRecord");
        self.code(100, subclass);
        h
    }
    /// A BLOCK and its ENDBLK, empty, owned by the block record `owner`.
    fn block(&mut self, owner: &str, name: &str, paper: bool) {
        for (kind, sub) in [("BLOCK", "AcDbBlockBegin"), ("ENDBLK", "AcDbBlockEnd")] {
            let h = self.handle();
            self.code(0, kind);
            self.code(5, h);
            self.code(330, owner);
            self.code(100, "AcDbEntity");
            if paper {
                self.code(67, 1);
            }
            self.code(8, "0");
            self.code(100, sub);
            if kind == "BLOCK" {
                self.code(2, name);
                self.code(70, 0);
                self.point(10, Vec2::ZERO);
                self.code(3, name);
                self.code(1, "");
            }
        }
    }
}

/// A text on one line, as a TEXT entity holds it: at most 250 characters, control characters as
/// spaces, letters outside ASCII through U+FFFF as `\U+XXXX`. Refuse characters beyond
/// U+FFFF anywhere in the source, including past the existing 250-character output limit.
/// AC1015 uses CIF escapes, not UTF-8; surrogate-pair escapes are not portable across readers.
fn text_value(s: &str) -> Result<String> {
    if let Some((i, c)) = s.chars().enumerate().find(|(_, c)| u32::from(*c) > 0xffff) {
        return Err(IoError::Invalid(format!(
            "character {c:?} (U+{:04X}) at position {} cannot be written as DXF AC1015 text; only characters through U+FFFF are supported",
            u32::from(c),
            i + 1
        )));
    }
    Ok(s.chars()
        .take(250)
        .map(|c| match c {
            c if c.is_ascii_control() => " ".to_string(),
            c if c.is_ascii() => c.to_string(),
            c => format!("\\U+{:04X}", u32::from(c)),
        })
        .collect())
}

/// A DXF drawing (AutoCAD 2000, millimetres) of `entities`. `layers` are the layers they are on
/// (layer `0` is always there). A TEXT character beyond U+FFFF returns an error naming the
/// character, its position, the entity and its layer; no drawing is returned on failure.
pub fn write_dxf_drawing(layers: &[DxfLayer], entities: &[DxfEntity]) -> Result<String> {
    write_dxf_drawing_with(layers, entities, &[], &[])
}

/// The dimension style every `DIMENSION` and `LEADER` we write names: SolidCraft's ISO 129-1
/// style (2.5 mm closed filled arrows, 3.5 mm text above the line, no extension-line gap, 2 mm
/// extension overshoot, 1 mm text gap, decimal comma, two decimals, trailing zeros suppressed).
pub const DIM_STYLE: &str = "SolidCraft ISO";

/// A DXF drawing (AutoCAD 2000, millimetres) of `entities`, plus real `DIMENSION` entities with
/// their anonymous `*D<n>` blocks and `LEADER` entities. The blocks hold exactly the geometry the
/// caller drew, so a viewer shows the sheet's own rendering.
pub fn write_dxf_drawing_with(layers: &[DxfLayer], entities: &[DxfEntity], dimensions: &[DxfDimension], leaders: &[DxfLeader]) -> Result<String> {
    write_dxf_drawing_full(layers, entities, dimensions, leaders, &[])
}

/// [`write_dxf_drawing_with`] plus real `HATCH` entities (ISO 128-50 section hatching).
pub fn write_dxf_drawing_full(
    layers: &[DxfLayer],
    entities: &[DxfEntity],
    dimensions: &[DxfDimension],
    leaders: &[DxfLeader],
    hatches: &[DxfHatch],
) -> Result<String> {
    write_dxf_drawing_layout(layers, entities, &[], dimensions, leaders, hatches, None)
}

/// A paper-space layout: a named sheet of `paper_mm` with one `VIEWPORT` at 1:1 that shows the
/// whole model-space drawing. The model-space coordinates are the sheet's millimetres, so the
/// view's centre and height are the paper's own.
#[derive(Clone, Debug, PartialEq)]
pub struct DxfLayout {
    /// The layout (tab) name, e.g. `Blatt 1`.
    pub name: String,
    /// Paper width and height in millimetres, landscape.
    pub paper_mm: (f64, f64),
    /// When set, the paper-space entities (frame, title block, notes) become one named block
    /// and the layout carries one `INSERT` of it — the way a drawing sheet's furniture is
    /// normally organised, and one entity instead of the hundreds it is made of.
    pub furniture_block: Option<String>,
}

impl DxfLayout {
    /// The layout of an ISO A4 landscape sheet, named as a German drawing sheet names it.
    pub fn a4_landscape() -> DxfLayout {
        DxfLayout { name: "Blatt 1".to_string(), paper_mm: (297.0, 210.0), furniture_block: None }
    }
}

/// A DXF drawing with model-space `entities`, paper-space `paper_entities` (the frame, title block
/// and notes when the caller puts them there) and, when `layout` is given, a named paper-space
/// layout with one `VIEWPORT` at 1:1 that shows the whole model space.
///
/// Dimensions, leaders and hatches always live in model space: they are part of the drawing, not
/// of the sheet furniture.
pub fn write_dxf_drawing_layout(
    layers: &[DxfLayer],
    entities: &[DxfEntity],
    paper_entities: &[DxfEntity],
    dimensions: &[DxfDimension],
    leaders: &[DxfLeader],
    hatches: &[DxfHatch],
    layout: Option<&DxfLayout>,
) -> Result<String> {
    let mut pts = Vec::new();
    for e in entities.iter().chain(paper_entities) {
        e.shape.sample(&mut pts);
    }
    for h in hatches {
        pts.extend(h.loops.iter().flatten().copied());
    }
    for d in dimensions {
        pts.extend([d.def_point, d.text_midpoint]);
        for e in &d.block {
            e.shape.sample(&mut pts);
        }
    }
    for l in leaders {
        pts.extend(l.points.iter().copied());
    }
    pts.retain(|p| p.is_finite());
    let (lo, hi) = pts
        .iter()
        .fold(None, |acc: Option<(Vec2, Vec2)>, p| {
            Some(acc.map_or((*p, *p), |(lo, hi)| (Vec2::new(lo.x.min(p.x), lo.y.min(p.y)), Vec2::new(hi.x.max(p.x), hi.y.max(p.y)))))
        })
        .unwrap_or((Vec2::ZERO, Vec2::ZERO));

    let mut layer_list = vec![DxfLayer::new("0", 7)];
    for l in layers {
        if !layer_list.iter().any(|x| x.name == l.name) {
            layer_list.push(l.clone());
        }
    }
    // The furniture block reference sits on its own title-block layer.
    if layout.is_some_and(|layout| layout.furniture_block.is_some()) && !layer_list.iter().any(|l| l.name == "TITLE_BLOCK") {
        layer_list.push(DxfLayer::new("TITLE_BLOCK", 7));
    }
    // The layout's viewport is on its own viewport layer; declare it too, so every layer an
    // entity names exists in the table and carries a lineweight.
    if layout.is_some() && !layer_list.iter().any(|l| l.name == "VIEWPORT") {
        layer_list.push(DxfLayer::new("VIEWPORT", 7));
    }

    // Everything but the header, which needs the handle seed.
    let mut o = Out { s: String::new(), next: 0x20 };
    let (model, paper) = (o.handle(), o.handle());
    let root = o.handle();
    // One block record for every anonymous dimension block.
    let dimension_records: Vec<String> = dimensions.iter().map(|_| o.handle()).collect();
    // The paper-space layout: its dictionary, the two LAYOUT objects and the one viewport.
    let layout_handles = layout.map(|_| (o.handle(), o.handle(), o.handle()));
    let viewport = layout.map(|_| o.handle());
    let furniture_record = layout.and_then(|layout| layout.furniture_block.as_ref()).map(|_| o.handle());

    o.code(0, "SECTION");
    o.code(2, "TABLES");
    // The three every drawing has, then every ISO pattern a layer names. The patterns are
    // absolute millimetres (`$LTSCALE` 1), so a dashed line has the same dash length on paper as
    // in the PDF; an entity stays continuous geometry and takes its pattern from its layer.
    let linetypes: Vec<DxfLinetype> = [DxfLinetype::Dashed, DxfLinetype::Hidden, DxfLinetype::Center, DxfLinetype::Section]
        .into_iter()
        .filter(|kind| layer_list.iter().any(|l| l.linetype == *kind))
        .collect();
    let t = o.table("LTYPE", 3 + linetypes.len());
    for (name, description, pattern) in [("ByBlock", "", &[][..]), ("ByLayer", "", &[][..]), ("Continuous", "Solid line", &[][..])]
        .into_iter()
        .chain(linetypes.iter().map(|kind| (kind.name(), kind.description(), kind.pattern())))
    {
        o.record("LTYPE", "AcDbLinetypeTableRecord", &t);
        o.code(2, name);
        o.code(70, 0);
        o.code(3, description);
        o.code(72, 65);
        o.code(73, pattern.len());
        o.num(40, pattern.iter().map(|d| d.abs()).sum());
        for d in pattern {
            o.num(49, *d);
            o.code(74, 0);
        }
    }
    o.code(0, "ENDTAB");
    let t = o.table("LAYER", layer_list.len());
    for l in &layer_list {
        o.record("LAYER", "AcDbLayerTableRecord", &t);
        // Non-ASCII names (DIMENSION) use AutoCAD's \U+XXXX escape, as LibreDWG and ODA write
        // them; the drawing checker and CAD readers decode it.
        o.code(2, text_value(&l.name)?);
        o.code(70, 0);
        o.code(62, l.color);
        o.code(6, l.linetype.name());
        // Group 370: the ISO 128-2 line width of everything on the layer, in hundredths of a
        // millimetre (a standard AutoCAD value, never BYDEFAULT for a sheet layer).
        o.code(370, l.lineweight.hundredths());
    }
    o.code(0, "ENDTAB");
    // The one text style, which a TEXT entity uses when it names none.
    let t = o.table("STYLE", 1);
    let style_handle = o.record("STYLE", "AcDbTextStyleTableRecord", &t);
    o.code(2, "Standard");
    o.code(70, 0);
    o.num(40, 0.0);
    o.num(41, 1.0);
    o.num(50, 0.0);
    o.code(71, 0);
    o.num(42, 2.5);
    o.code(3, "txt");
    o.code(4, "");
    o.code(0, "ENDTAB");
    // One dimension style: the house ISO 129-1 settings. DIMTAD 1 puts the text above the line;
    // DIMEXO 0 leaves extension lines without a gap; DIMDSEP 44 is the decimal comma; DIMLUNIT 2
    // is decimal, DIMDEC 2 two decimals and DIMZIN 8 suppresses trailing zeros.
    let t = o.table("DIMSTYLE", 1);
    o.code(100, "AcDbDimStyleTable");
    let ds = o.handle();
    o.code(0, "DIMSTYLE");
    o.code(105, &ds);
    o.code(330, &t);
    o.code(100, "AcDbSymbolTableRecord");
    o.code(100, "AcDbDimStyleTableRecord");
    o.code(2, DIM_STYLE);
    o.code(70, 0);
    o.num(40, 1.0);
    o.num(41, 2.5);
    o.num(42, 0.0);
    o.num(44, 2.0);
    o.num(140, 3.5);
    o.num(142, 0.0);
    o.num(147, 1.0);
    o.code(77, 1);
    o.code(73, 0);
    o.code(74, 0);
    o.code(78, 8);
    o.code(271, 2);
    o.code(277, 2);
    o.code(278, 44);
    o.code(340, &style_handle);
    o.code(371, -2);
    o.code(372, -2);
    o.code(0, "ENDTAB");
    let t = o.table("BLOCK_RECORD", 2 + dimensions.len() + usize::from(furniture_record.is_some()));
    for (h, name) in [(&model, "*Model_Space"), (&paper, "*Paper_Space")] {
        o.code(0, "BLOCK_RECORD");
        o.code(5, h);
        o.code(330, &t);
        o.code(100, "AcDbSymbolTableRecord");
        o.code(100, "AcDbBlockTableRecord");
        o.code(2, name);
    }
    for (record, dimension) in dimension_records.iter().zip(dimensions) {
        o.code(0, "BLOCK_RECORD");
        o.code(5, record);
        o.code(330, &t);
        o.code(100, "AcDbSymbolTableRecord");
        o.code(100, "AcDbBlockTableRecord");
        o.code(2, &dimension.block_name);
    }
    let furniture_name = layout.and_then(|layout| layout.furniture_block.as_ref());
    if let (Some(record), Some(name)) = (&furniture_record, furniture_name) {
        o.code(0, "BLOCK_RECORD");
        o.code(5, record);
        o.code(330, &t);
        o.code(100, "AcDbSymbolTableRecord");
        o.code(100, "AcDbBlockTableRecord");
        o.code(2, text_value(name)?);
    }
    o.code(0, "ENDTAB");
    o.code(0, "ENDSEC");

    o.code(0, "SECTION");
    o.code(2, "BLOCKS");
    o.block(&model, "*Model_Space", false);
    // The sheet furniture as one block, inserted in paper space below.
    if let (Some(record), Some(name)) = (&furniture_record, furniture_name) {
        let h = o.handle();
        o.code(0, "BLOCK");
        o.code(5, h);
        o.code(330, record);
        o.code(100, "AcDbEntity");
        o.code(8, "0");
        o.code(100, "AcDbBlockBegin");
        o.code(2, text_value(name)?);
        o.code(70, 0);
        o.point(10, Vec2::ZERO);
        o.code(3, text_value(name)?);
        o.code(1, "");
        for (index, e) in paper_entities.iter().enumerate() {
            write_entity(&mut o, record, false, e, index)?;
        }
        let h = o.handle();
        o.code(0, "ENDBLK");
        o.code(5, h);
        o.code(330, record);
        o.code(100, "AcDbEntity");
        o.code(8, "0");
        o.code(100, "AcDbBlockEnd");
    }
    // The paper-space block holds the sheet furniture (or one reference to it) and the one
    // viewport, so a reader that lists the layout sees both.
    {
        let h = o.handle();
        o.code(0, "BLOCK");
        o.code(5, h);
        o.code(330, &paper);
        o.code(100, "AcDbEntity");
        o.code(67, 1);
        o.code(8, "0");
        o.code(100, "AcDbBlockBegin");
        o.code(2, "*Paper_Space");
        o.code(70, 0);
        o.point(10, Vec2::ZERO);
        o.code(3, "*Paper_Space");
        o.code(1, "");
        match furniture_name {
            Some(name) => write_insert(&mut o, &paper, name)?,
            None => {
                for (index, e) in paper_entities.iter().enumerate() {
                    write_entity(&mut o, &paper, true, e, index)?;
                }
            }
        }
        if let (Some(layout), Some(handle)) = (layout, viewport.as_deref()) {
            write_viewport(&mut o, &paper, handle, layout);
        }
        let h = o.handle();
        o.code(0, "ENDBLK");
        o.code(5, h);
        o.code(330, &paper);
        o.code(100, "AcDbEntity");
        o.code(67, 1);
        o.code(8, "0");
        o.code(100, "AcDbBlockEnd");
    }
    for (record, dimension) in dimension_records.iter().zip(dimensions) {
        let h = o.handle();
        o.code(0, "BLOCK");
        o.code(5, h);
        o.code(330, record);
        o.code(100, "AcDbEntity");
        o.code(8, "0");
        o.code(100, "AcDbBlockBegin");
        o.code(2, &dimension.block_name);
        o.code(70, 0);
        o.point(10, Vec2::ZERO);
        o.code(3, &dimension.block_name);
        o.code(1, "");
        for (index, e) in dimension.block.iter().enumerate() {
            write_entity(&mut o, record, false, e, index)?;
        }
        let h = o.handle();
        o.code(0, "ENDBLK");
        o.code(5, h);
        o.code(330, record);
        o.code(100, "AcDbEntity");
        o.code(8, "0");
        o.code(100, "AcDbBlockEnd");
    }
    o.code(0, "ENDSEC");

    o.code(0, "SECTION");
    o.code(2, "ENTITIES");
    for (index, e) in entities.iter().enumerate() {
        write_entity(&mut o, &model, false, e, index)?;
    }
    for dimension in dimensions {
        write_dimension(&mut o, &model, dimension)?;
    }
    for leader in leaders {
        write_leader(&mut o, &model, leader)?;
    }
    for hatch in hatches {
        write_hatch(&mut o, &model, hatch)?;
    }
    o.code(0, "ENDSEC");

    o.code(0, "SECTION");
    o.code(2, "OBJECTS");
    match (layout, &layout_handles) {
        (Some(layout), Some((dict, model_layout, paper_layout))) => {
            // The root dictionary names the layout dictionary; it lists both the model layout
            // and the named paper-space layout.
            o.code(0, "DICTIONARY");
            o.code(5, root.as_str());
            o.code(330, 0);
            o.code(100, "AcDbDictionary");
            o.code(281, 1);
            o.code(3, "ACAD_LAYOUT");
            o.code(350, dict);
            o.code(0, "DICTIONARY");
            o.code(5, dict);
            o.code(330, root.as_str());
            o.code(100, "AcDbDictionary");
            o.code(281, 1);
            o.code(3, "Model");
            o.code(350, model_layout);
            o.code(3, text_value(&layout.name)?);
            o.code(350, paper_layout);
            write_layout(&mut o, dict, model_layout, "Model", 0, &model, None, layout.paper_mm)?;
            write_layout(&mut o, dict, paper_layout, &layout.name, 1, &paper, viewport.as_deref(), layout.paper_mm)?;
        }
        _ => {
            o.code(0, "DICTIONARY");
            o.code(5, root.as_str());
            o.code(330, 0);
            o.code(100, "AcDbDictionary");
            o.code(281, 1);
        }
    }
    o.code(0, "ENDSEC");
    o.code(0, "EOF");

    let mut head = Out { s: String::new(), next: 0 };
    head.code(0, "SECTION");
    head.code(2, "HEADER");
    head.code(9, "$ACADVER");
    head.code(1, "AC1015");
    head.code(9, "$HANDSEED");
    head.code(5, format!("{:X}", o.next));
    head.code(9, "$INSUNITS");
    head.code(70, 4);
    head.code(9, "$MEASUREMENT");
    head.code(70, 1);
    // The sheet is drawn in millimetres at 1:1 on paper, so the LTYPE patterns are absolute
    // millimetres and need no scaling (the PDF uses the same lengths).
    head.code(9, "$LTSCALE");
    head.num(40, 1.0);
    head.code(9, "$PSLTSCALE");
    head.code(70, 1);
    // Lineweight display on: a viewer that honours the header draws the layers' group 370 widths
    // (the sheet's ISO 128-2 thick/thin lines) instead of one uniform hairline.
    head.code(9, "$LWDISPLAY");
    head.code(290, 1);
    head.code(9, "$EXTMIN");
    head.point(10, lo);
    head.code(9, "$EXTMAX");
    head.point(10, hi);
    head.code(0, "ENDSEC");
    Ok(head.s + &o.s)
}

/// The paper-space reference to the sheet-furniture block.
fn write_insert(o: &mut Out, owner: &str, name: &str) -> Result<()> {
    let h = o.handle();
    o.code(0, "INSERT");
    o.code(5, h);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    o.code(67, 1);
    o.code(8, "TITLE_BLOCK");
    // The reference takes its width from the title-block layer, like every other entity.
    o.code(370, -1);
    o.code(100, "AcDbBlockReference");
    o.code(2, text_value(name)?);
    o.point(10, Vec2::ZERO);
    o.num(41, 1.0);
    o.num(42, 1.0);
    o.num(43, 1.0);
    o.num(50, 0.0);
    Ok(())
}

/// One entity with an owner (the model space, the paper space or an anonymous dimension block).
fn write_entity(o: &mut Out, owner: &str, paper: bool, e: &DxfEntity, index: usize) -> Result<()> {
    let kind = match &e.shape {
        DxfShape::Line(..) => "LINE",
        DxfShape::Polyline { .. } => "LWPOLYLINE",
        DxfShape::Circle { .. } => "CIRCLE",
        DxfShape::Arc { .. } => "ARC",
        DxfShape::Ellipse { .. } => "ELLIPSE",
        DxfShape::Spline { .. } => "SPLINE",
        DxfShape::Text { .. } => "TEXT",
        DxfShape::Solid { .. } => "SOLID",
    };
    let h = o.handle();
    o.code(0, kind);
    o.code(5, h);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    if paper {
        o.code(67, 1);
    }
    o.code(8, if e.layer.trim().is_empty() { "0".to_string() } else { text_value(&e.layer)? });
    // Group 370 BYLAYER (−1): the entity takes the width of its layer, the sheet's line group.
    o.code(370, -1);
    match &e.shape {
        DxfShape::Polyline { points } => {
            o.code(100, "AcDbPolyline");
            o.code(90, points.len());
            // Autodesk DXF reference: bit 128 (PLINEGEN) continues the dash pattern.
            o.code(70, 128);
            for p in points {
                o.num(10, p.x);
                o.num(20, p.y);
            }
        }
        DxfShape::Line(a, b) => {
            o.code(100, "AcDbLine");
            o.point(10, *a);
            o.point(11, *b);
        }
        DxfShape::Circle { c, r } => {
            o.code(100, "AcDbCircle");
            o.point(10, *c);
            o.num(40, *r);
        }
        DxfShape::Arc { c, r, a0, a1 } => {
            o.code(100, "AcDbCircle");
            o.point(10, *c);
            o.num(40, *r);
            o.code(100, "AcDbArc");
            o.num(50, *a0);
            o.num(51, *a1);
        }
        DxfShape::Ellipse { c, major, ratio, t0, t1 } => {
            o.code(100, "AcDbEllipse");
            o.point(10, *c);
            o.point(11, *major);
            o.num(210, 0.0);
            o.num(220, 0.0);
            o.num(230, 1.0);
            o.num(40, *ratio);
            o.num(41, *t0);
            o.num(42, *t1);
        }
        DxfShape::Spline { degree, knots, ctrl, weights } => {
            o.code(100, "AcDbSpline");
            o.num(210, 0.0);
            o.num(220, 0.0);
            o.num(230, 1.0);
            // Flags: 4 rational, 8 planar.
            o.code(70, 8 + if weights.is_some() { 4 } else { 0 });
            o.code(71, degree);
            o.code(72, knots.len());
            o.code(73, ctrl.len());
            o.code(74, 0);
            // Knots, then weights, then control points: the order of the reference.
            for k in knots {
                o.num(40, *k);
            }
            for w in weights.iter().flatten() {
                o.num(41, *w);
            }
            for p in ctrl {
                o.point(10, *p);
            }
        }
        DxfShape::Text { at, height, angle_deg, text, anchor } => {
            o.code(100, "AcDbText");
            o.point(10, *at);
            o.num(40, *height);
            let value =
                text_value(text).map_err(|error| IoError::Invalid(format!("DXF TEXT entity {} on layer {:?}: {error}", index + 1, e.layer)))?;
            o.code(1, value);
            o.num(50, *angle_deg);
            // Group 41 is the width factor: 1.0, the text's natural width. The PDF sets its
            // Helvetica at its own advance widths, so no horizontal stretch belongs in the DXF.
            o.num(41, 1.0);
            // Group 72 is the horizontal alignment: 0 left, 1 centre, 2 right, with the baseline
            // (73 = 0) the vertical one. Left text names its start in group 10 alone; centred and
            // right texts name the alignment point in group 11, so a viewer that measures the
            // string with its own font still puts the anchor where the sheet put it.
            match anchor {
                DxfTextAnchor::Left => o.code(72, 0),
                DxfTextAnchor::Center => {
                    o.code(72, 1);
                    o.point(11, *at);
                }
                DxfTextAnchor::Right => {
                    o.code(72, 2);
                    o.point(11, *at);
                }
            }
            o.code(100, "AcDbText");
            o.code(73, 0);
        }
        DxfShape::Solid { points } => {
            o.code(100, "AcDbTrace");
            let p = |i: usize| points.get(i).copied().unwrap_or(Vec2::ZERO);
            // SOLID runs 10 → 11 → 13 → 12; a triangle repeats its third corner.
            let fourth = points.get(3).or_else(|| points.get(2)).copied().unwrap_or(Vec2::ZERO);
            o.point(10, p(0));
            o.point(11, p(1));
            o.point(12, p(2));
            o.point(13, fourth);
        }
    }
    Ok(())
}

/// A real `DIMENSION` entity with the anonymous block it references.
fn write_dimension(o: &mut Out, owner: &str, d: &DxfDimension) -> Result<()> {
    let h = o.handle();
    o.code(0, "DIMENSION");
    o.code(5, h);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    o.code(8, text_value(&d.layer)?);
    o.code(370, -1);
    o.code(100, "AcDbDimension");
    o.code(2, &d.block_name);
    o.point(10, d.def_point);
    o.point(11, d.text_midpoint);
    // Bit 32 marks the referenced block as an anonymous dimension block.
    o.code(70, d.kind.dimtype() + 32);
    o.code(71, 5);
    o.num(42, d.measurement);
    o.code(3, &d.style);
    if let Some(text) = &d.text_override {
        o.code(1, text_value(text)?);
    }
    match d.kind {
        DxfDimKind::Rotated => {
            o.code(100, "AcDbAlignedDimension");
            o.point(13, d.def_point2.unwrap_or(d.def_point));
            o.point(14, d.def_point3.unwrap_or(d.def_point));
            o.num(50, d.angle_deg);
            o.code(100, "AcDbRotatedDimension");
        }
        DxfDimKind::Diametric => {
            o.code(100, "AcDbDiametricDimension");
            o.point(15, d.def_point4.unwrap_or(d.def_point));
        }
        DxfDimKind::Radial => {
            o.code(100, "AcDbRadialDimension");
            o.point(15, d.def_point4.unwrap_or(d.def_point));
        }
    }
    Ok(())
}

/// A real `HATCH` entity with one polyline boundary path per loop (outer first, then islands).
fn write_hatch(o: &mut Out, owner: &str, hatch: &DxfHatch) -> Result<()> {
    let loops: Vec<&Vec<Vec2>> = hatch.loops.iter().filter(|loop_| loop_.len() >= 3).collect();
    let h = o.handle();
    o.code(0, "HATCH");
    o.code(5, h);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    o.code(8, text_value(&hatch.layer)?);
    o.code(370, -1);
    o.code(100, "AcDbHatch");
    o.point(10, Vec2::ZERO);
    o.num(210, 0.0);
    o.num(220, 0.0);
    o.num(230, 1.0);
    o.code(2, text_value(&hatch.pattern)?);
    // 70: 0 = pattern fill (1 would be solid); 71: not associative.
    o.code(70, 0);
    o.code(71, 0);
    o.code(91, loops.len());
    for loop_ in loops {
        // 92: external (1) | polyline (2); a polyline path carries its own vertices.
        o.code(92, 3);
        o.code(72, 0);
        o.code(73, 1);
        o.code(93, loop_.len());
        for p in loop_ {
            o.num(10, p.x);
            o.num(20, p.y);
        }
        o.code(97, 0);
    }
    // 75: 0 = nested (odd parity: islands stay free of hatching).
    o.code(75, 0);
    o.code(76, 1);
    o.num(52, hatch.angle_deg);
    o.num(41, hatch.scale);
    o.code(77, 0);
    // One predefined pattern line: ANSI31 is 45° with a 3.175 mm spacing at scale 1.
    o.code(78, 1);
    o.num(53, 45.0);
    o.num(43, 0.0);
    o.num(44, 0.0);
    o.num(45, -DxfHatch::ANSI31_SPACING_MM / std::f64::consts::SQRT_2);
    o.num(46, DxfHatch::ANSI31_SPACING_MM / std::f64::consts::SQRT_2);
    o.code(79, 0);
    o.code(98, 0);
    Ok(())
}

/// The one paper-space `VIEWPORT`: the whole page, showing the whole model space at 1:1. Model
/// coordinates are sheet millimetres, so a view height equal to the paper height and a view
/// centre at the paper's centre put every model-space millimetre on its paper millimetre.
fn write_viewport(o: &mut Out, owner: &str, handle: &str, layout: &DxfLayout) {
    let (w, h) = layout.paper_mm;
    let (w, h) = if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 { (w, h) } else { (297.0, 210.0) };
    let centre = Vec2::new(w * 0.5, h * 0.5);
    o.code(0, "VIEWPORT");
    o.code(5, handle);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    o.code(67, 1);
    // A layer of its own for viewports, so a reader can keep VIEWPORTs apart from the drawing.
    o.code(8, "VIEWPORT");
    o.code(370, -1);
    o.code(100, "AcDbViewport");
    o.point(10, centre);
    o.num(40, w);
    o.num(41, h);
    // Active viewport, id 1.
    o.code(68, 1);
    o.code(69, 1);
    // The model-space point at the middle of the view and the view height: 1:1 because the
    // height equals the paper height.
    o.point(12, centre);
    o.point(13, Vec2::ZERO);
    o.point(14, Vec2::new(10.0, 10.0));
    o.point(15, Vec2::new(10.0, 10.0));
    // Looking down +Z; target at the sheet's origin, so the view centre above is the model point.
    o.num(16, 0.0);
    o.num(26, 0.0);
    o.num(36, 1.0);
    o.num(17, 0.0);
    o.num(27, 0.0);
    o.num(37, 0.0);
    o.num(42, 50.0);
    o.num(43, 0.0);
    o.num(44, 0.0);
    o.num(45, h);
    o.num(50, 0.0);
    o.num(51, 0.0);
    o.code(72, 100);
    o.code(90, 0);
    o.code(1, "");
    o.code(281, 0);
    o.code(71, 0);
    o.code(74, 0);
    // UCS at the world origin, x and y along the paper axes.
    o.point(110, Vec2::ZERO);
    o.point(111, Vec2::new(1.0, 0.0));
    o.point(112, Vec2::new(0.0, 1.0));
    o.code(79, 0);
    o.num(146, 0.0);
}

/// A `LAYOUT` object: the plot settings for an ISO sheet and the layout's block record. The
/// paper layout lists its viewport (`331`), which is what AutoCAD and ezdxf read as its one
/// "sheet".
fn write_layout(
    o: &mut Out,
    dict: &str,
    handle: &str,
    name: &str,
    tab_order: i32,
    block: &str,
    viewport: Option<&str>,
    paper_mm: (f64, f64),
) -> Result<()> {
    let (w, h) = paper_mm;
    let (w, h) = if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 { (w, h) } else { (297.0, 210.0) };
    o.code(0, "LAYOUT");
    o.code(5, handle);
    o.code(330, dict);
    // AcDbPlotSettings: the printer, the ISO sheet and its millimetre margins.
    o.code(100, "AcDbPlotSettings");
    o.code(1, "");
    o.code(2, "DWG to PDF.pc3");
    o.code(4, format!("ISO_A4_({w:.2}_x_{h:.2}_MM)"));
    o.code(6, "");
    o.num(40, 0.0);
    o.num(41, 0.0);
    o.num(42, 0.0);
    o.num(43, 0.0);
    o.num(44, w);
    o.num(45, h);
    o.num(46, 0.0);
    o.num(47, 0.0);
    o.num(48, 0.0);
    o.num(49, 0.0);
    o.num(140, 0.0);
    o.num(141, 0.0);
    o.num(142, 1.0);
    o.num(143, 1.0);
    // 1:1 plot, millimetres, plot-with-plot-style.
    o.code(70, 688);
    o.code(72, 1);
    o.code(73, 0);
    o.code(74, 5);
    o.code(7, "");
    o.code(75, 16);
    o.code(76, 0);
    o.code(77, 2);
    o.code(78, 300);
    o.num(147, 1.0);
    o.num(148, 0.0);
    o.num(149, 0.0);
    // AcDbLayout.
    o.code(100, "AcDbLayout");
    o.code(1, text_value(name)?);
    o.code(70, 1);
    o.code(71, tab_order);
    o.point(10, Vec2::ZERO);
    o.point(11, Vec2::new(w, h));
    o.point(12, Vec2::ZERO);
    for code in [14, 24, 34] {
        o.num(code, 1e20);
    }
    for code in [15, 25, 35] {
        o.num(code, -1e20);
    }
    o.num(146, 0.0);
    o.point(13, Vec2::ZERO);
    o.num(16, 1.0);
    o.num(26, 0.0);
    o.num(36, 0.0);
    o.num(17, 0.0);
    o.num(27, 1.0);
    o.num(37, 0.0);
    match viewport {
        Some(viewport) => {
            o.code(76, 1);
            o.code(330, block);
            o.code(331, viewport);
        }
        None => {
            o.code(76, 0);
            o.code(330, block);
        }
    }
    Ok(())
}

/// A standalone `LEADER` (a note's leader line).
fn write_leader(o: &mut Out, owner: &str, leader: &DxfLeader) -> Result<()> {
    let h = o.handle();
    o.code(0, "LEADER");
    o.code(5, h);
    o.code(330, owner);
    o.code(100, "AcDbEntity");
    o.code(8, text_value(&leader.layer)?);
    o.code(370, -1);
    o.code(100, "AcDbLeader");
    o.code(3, &leader.style);
    o.code(71, i32::from(leader.arrowhead));
    o.code(72, 0);
    o.code(73, 3);
    o.code(76, leader.points.len());
    for p in &leader.points {
        o.point(10, *p);
    }
    o.num(210, 0.0);
    o.num(220, 0.0);
    o.num(230, 1.0);
    o.num(211, 1.0);
    o.num(221, 0.0);
    o.num(231, 0.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI};

    use super::*;
    use crate::{Geom2, read_dxf};

    /// The conic quarter of the ellipse `c ± u`, `c ± v` from `c + u` to `c + v`.
    fn quarter(c: Vec2, u: Vec2, v: Vec2) -> Seg2 {
        Seg2::Conic { a: c + u, apex: c + u + v, b: c + v, w: FRAC_1_SQRT_2 }
    }

    /// The ellipse of 50 × 24 round (50, 40): its four quarters, counter-clockwise from (75, 40).
    fn oval() -> [Seg2; 4] {
        let (c, u, v) = (Vec2::new(50.0, 40.0), Vec2::new(25.0, 0.0), Vec2::new(0.0, 12.0));
        [quarter(c, u, v), quarter(c, v, -u), quarter(c, -u, -v), quarter(c, -v, u)]
    }

    fn ellipse_point(e: &DxfShape, t: f64) -> Vec2 {
        let DxfShape::Ellipse { c, major, ratio, .. } = e else { panic!("{e:?}") };
        *c + *major * t.cos() + major.perp() * (*ratio * t.sin())
    }

    /// A DXF text as (group code, value) pairs.
    fn pairs(dxf: &str) -> Vec<(i32, String)> {
        let lines: Vec<&str> = dxf.lines().collect();
        lines.chunks(2).map(|c| (c[0].trim().parse().unwrap(), c.get(1).unwrap_or(&"").trim().to_string())).collect()
    }

    /// The entities of the ENTITIES section: their type and group codes.
    fn entities(dxf: &str) -> Vec<(String, Vec<(i32, String)>)> {
        let p = pairs(dxf);
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "ENTITIES")).unwrap();
        let mut out: Vec<(String, Vec<(i32, String)>)> = Vec::new();
        for (c, v) in &p[start + 1..] {
            match (*c, v.as_str()) {
                (0, "ENDSEC") => break,
                (0, kind) => out.push((kind.to_string(), Vec::new())),
                _ => out.last_mut().unwrap().1.push((*c, v.clone())),
            }
        }
        out
    }

    fn values(e: &(String, Vec<(i32, String)>), code: i32) -> Vec<f64> {
        e.1.iter().filter(|(c, _)| *c == code).map(|(_, v)| v.parse().unwrap()).collect()
    }

    fn near(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn a_conic_arc_below_weight_one_is_an_elliptical_arc() {
        // By hand: the first quarter of the 25 × 12 ellipse round (50, 40) runs from (75, 40)
        // over the corner (75, 52) to (50, 52) with weight cos 45°. Its centre is
        // m − (apex − m)·w²/(1 − w²) = (62.5, 46) − (12.5, 6)·1 = (50, 40); the semi-diameters
        // u = (m − c)/w and v = (b − m)/sin 45° are (25, 12) and (−25, 12) over √2, which are
        // equally long, so the principal axes are 45° from them: the major one is
        // (u − v)/√2 = (25, 0), the minor one (u + v)/√2 = (0, 12), the ratio 12/25 and, from
        // (75, 40) to (50, 52), the parameters 0 to π/2.
        let [first, second, ..] = oval();
        let DxfShape::Ellipse { c, major, ratio, t0, t1 } = seg_shape(&first) else { panic!("{:?}", seg_shape(&first)) };
        assert!(c.dist(Vec2::new(50.0, 40.0)) < 1e-9 && major.dist(Vec2::new(25.0, 0.0)) < 1e-9, "{c:?} {major:?}");
        assert!((ratio - 0.48).abs() < 1e-12 && t0.abs() < 1e-9 && (t1 - FRAC_PI_2).abs() < 1e-9, "{ratio} {t0} {t1}");
        // Whatever axis the second quarter is described by, its arc starts and ends at the
        // segment's ends and every point of it lies on the segment.
        let tilt = |v: Vec2| Vec2::from_angle(0.3) * v.x + Vec2::from_angle(0.3).perp() * v.y;
        let (centre, u, v) = (Vec2::new(5.0, -2.0), tilt(Vec2::new(15.0, 0.0)), tilt(Vec2::new(0.0, 8.0)));
        let general = quarter(centre, u, v).split_at(0.3).1;
        for seg in [first, second, quarter(centre, u, v), quarter(centre, v, -u).reversed(), general, general.reversed()] {
            let e = seg_shape(&seg);
            let DxfShape::Ellipse { t0, t1, .. } = e else { panic!("{e:?}") };
            let sweep = (t1 - t0).rem_euclid(TAU);
            for i in 0..=20 {
                let p = ellipse_point(&e, t0 + sweep * i as f64 / 20.0);
                assert!(seg.point_at(seg.closest_param(p)).dist(p) < 1e-9, "{seg:?} {p:?}");
            }
            let ends = [ellipse_point(&e, t0), ellipse_point(&e, t0 + sweep)];
            assert!(ends.iter().any(|p| p.dist(seg.start()) < 1e-9) && ends.iter().any(|p| p.dist(seg.end()) < 1e-9), "{ends:?} {seg:?}");
        }
    }

    #[test]
    fn four_quarters_are_one_ellipse() {
        let quarters = oval();
        let reversed = Loop2 { segs: quarters.iter().rev().map(Seg2::reversed).collect() };
        let shifted = Loop2 { segs: quarters.iter().cycle().skip(2).take(4).copied().collect() };
        for lp in [Loop2 { segs: quarters.to_vec() }, reversed, shifted] {
            let shapes = loop_shapes(&lp);
            let [DxfShape::Ellipse { c, major, ratio, t0, t1 }] = shapes.as_slice() else { panic!("{shapes:?}") };
            assert!(
                c.dist(Vec2::new(50.0, 40.0)) < 1e-9 && (major.len() - 25.0).abs() < 1e-9 && major.cross(Vec2::X).abs() < 1e-9,
                "{c:?} {major:?}"
            );
            assert!((ratio - 0.48).abs() < 1e-12 && *t0 == 0.0 && *t1 == TAU, "{ratio} {t0} {t1}");
        }
        // Three quarters and a line are pieces: three ELLIPSE arcs and a LINE.
        let (q, line) = (oval(), Seg2::Line { a: Vec2::new(50.0, 28.0), b: Vec2::new(50.0, 28.0) + Vec2::X });
        let open = loop_shapes(&Loop2 { segs: vec![q[0], q[1], q[2], line] });
        assert!(matches!(open.as_slice(), [DxfShape::Ellipse { t1, .. }, _, _, DxfShape::Line(..)] if (*t1 - FRAC_PI_2).abs() < 1e-9), "{open:?}");
        // A circle of arcs is the caller's business, but a whole turn is one CIRCLE.
        let turn = Seg2::Arc { center: Vec2::new(3.0, 4.0), radius: 2.0, start: -PI, sweep: -TAU };
        assert_eq!(seg_shape(&turn), DxfShape::Circle { c: Vec2::new(3.0, 4.0), r: 2.0 });
    }

    #[test]
    fn beziers_and_the_other_conics_are_splines() {
        let p = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 2.0), Vec2::new(3.0, 3.0), Vec2::new(4.0, 0.0)];
        let cubic = seg_shape(&Seg2::Cubic { p0: p[0], p1: p[1], p2: p[2], p3: p[3] });
        assert_eq!(cubic, DxfShape::Spline { degree: 3, knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0], ctrl: p.to_vec(), weights: None });
        // A quadratic (a glyph curve) is a parabola: degree 2, not rational. Weight 2 is a
        // hyperbola: rational. Weight 1 − 10⁻⁹ is no ellipse a reader could place: rational too.
        let conic = |w: f64| seg_shape(&Seg2::Conic { a: p[0], apex: p[1], b: p[3], w });
        let degree_2 = |s: &DxfShape, weights: Option<Vec<f64>>| {
            *s == DxfShape::Spline { degree: 2, knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0], ctrl: vec![p[0], p[1], p[3]], weights }
        };
        assert!(degree_2(&conic(1.0), None));
        assert!(degree_2(&conic(2.0), Some(vec![1.0, 2.0, 1.0])));
        assert!(degree_2(&conic(1.0 - 1e-9), Some(vec![1.0, 1.0 - 1e-9, 1.0])));
        // A quintic piece: six control points and twelve knots.
        let mut q = [Vec2::ZERO; solidcraft_geom::MAX_BEZIER_CTRL];
        for (i, s) in q.iter_mut().enumerate() {
            *s = Vec2::new(i as f64, (i * i) as f64);
        }
        let DxfShape::Spline { degree, knots, ctrl, weights } = seg_shape(&Seg2::Bezier { n: 5, p: q }) else { panic!() };
        assert_eq!((degree, knots.len(), ctrl.len(), weights), (5, 12, 6, None));
        assert!(knots[..6].iter().all(|k| *k == 0.0) && knots[6..].iter().all(|k| *k == 1.0) && ctrl[5] == q[5]);
    }

    #[test]
    fn a_bezier_is_a_spline_of_its_own_degree_over_all_its_control_points() {
        let mut q = [Vec2::ZERO; solidcraft_geom::MAX_BEZIER_CTRL];
        for (i, s) in q.iter_mut().enumerate() {
            *s = Vec2::new(i as f64, (i * i) as f64);
        }
        // Every degree a Bézier piece can have, up to a G4 blend's 9: n + 1 control points, all of
        // them, and 2(n + 1) clamped knots.
        for n in 4..=solidcraft_geom::MAX_BEZIER_DEGREE as u8 {
            let k = usize::from(n) + 1;
            let DxfShape::Spline { degree, knots, ctrl, weights } = seg_shape(&Seg2::Bezier { n, p: q }) else { panic!("degree {n}") };
            assert_eq!((degree, knots.len(), weights), (n, 2 * k, None), "degree {n}");
            assert_eq!(ctrl, q[..k], "degree {n}");
        }
        // A degree no piece can have (hostile data) is held to what the piece holds, never past it.
        let shape = |n: u8| seg_shape(&Seg2::Bezier { n, p: q });
        assert!(matches!(shape(0), DxfShape::Spline { degree: 1, ref ctrl, .. } if ctrl.len() == 2));
        assert!(matches!(shape(u8::MAX), DxfShape::Spline { degree: 9, ref ctrl, .. } if ctrl[..] == q[..]));
    }

    #[test]
    fn drawings_carry_version_units_extents_and_handles() {
        let ents = vec![
            DxfEntity { layer: "OUTLINE".into(), shape: DxfShape::Line(Vec2::new(-1.0, 2.0), Vec2::new(10.0, 2.0)) },
            DxfEntity { layer: "OUTLINE".into(), shape: DxfShape::Circle { c: Vec2::new(5.0, 5.0), r: 4.0 } },
            DxfEntity { layer: "CUT".into(), shape: DxfShape::Line(Vec2::ZERO, Vec2::X) },
        ];
        let dxf = write_dxf_drawing(&[DxfLayer::new("OUTLINE", 7), DxfLayer::new("CUT", 1)], &ents).unwrap();
        let p = pairs(&dxf);
        let after = |var: &str| &p[p.iter().position(|(c, v)| (*c, v.as_str()) == (9, var)).unwrap() + 1..];
        assert_eq!(after("$ACADVER")[0], (1, "AC1015".to_string()));
        assert_eq!(after("$INSUNITS")[0], (70, "4".to_string()));
        // The circle reaches (1, 1) to (9, 9), the first line starts at x = −1 and the last at y = 0.
        assert_eq!(&after("$EXTMIN")[..3], &[(10, "-1.0".to_string()), (20, "0.0".to_string()), (30, "0.0".to_string())]);
        assert_eq!(&after("$EXTMAX")[..2], &[(10, "10.0".to_string()), (20, "9.0".to_string())]);
        // Layer 0 and the two layers are in the LAYER table, each entity is on one of them,
        // and the entities belong to the model space block record.
        let layer_table = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "LAYER")).unwrap();
        let names: Vec<&str> = p[layer_table..]
            .iter()
            .take_while(|(c, v)| (*c, v.as_str()) != (0, "ENDTAB"))
            .filter(|(c, _)| *c == 2)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(names, ["LAYER", "0", "OUTLINE", "CUT"]);
        // (The first "*Model_Space" is the BLOCK_RECORD, whose handle is four pairs before its name.)
        let model_at = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "*Model_Space")).unwrap();
        let model = p[model_at - 4].1.clone();
        assert_eq!(p[model_at - 4].0, 5);
        for e in entities(&dxf) {
            assert!(e.1.contains(&(330, model.clone())) && e.1.contains(&(100, "AcDbEntity".to_string())), "{e:?}");
            assert!(names.contains(&e.1.iter().find(|(c, _)| *c == 8).unwrap().1.as_str()));
        }
        // Handles are unique and the seed is above all of them.
        let seed_at = p.iter().position(|(c, v)| (*c, v.as_str()) == (9, "$HANDSEED")).unwrap() + 1;
        let handles: Vec<u64> =
            p.iter().enumerate().filter(|(i, (c, _))| *i != seed_at && *c == 5).map(|(_, (_, v))| u64::from_str_radix(v, 16).unwrap()).collect();
        let mut sorted = handles.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), handles.len());
        assert!(handles.len() > ents.len() && handles.iter().all(|h| *h < u64::from_str_radix(&p[seed_at].1, 16).unwrap()));
        assert!(dxf.ends_with("  0\nEOF\n"));
        // No entities: a valid, empty drawing.
        assert_eq!(entities(&write_dxf_drawing(&[], &[]).unwrap()).len(), 0);
    }

    #[test]
    fn what_is_written_is_what_the_reader_gets_back() {
        let quarter_one = seg_shape(&oval()[0]);
        let cubic = [Vec2::new(0.0, 0.0), Vec2::new(5.0, 10.0), Vec2::new(15.0, 10.0), Vec2::new(20.0, 0.0)];
        let shapes = vec![
            DxfShape::Line(Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0)),
            DxfShape::Circle { c: Vec2::new(5.0, 5.0), r: 2.0 },
            DxfShape::Arc { c: Vec2::ZERO, r: 3.0, a0: 0.0, a1: 90.0 },
            loop_shapes(&Loop2 { segs: oval().to_vec() }).remove(0),
            quarter_one,
            seg_shape(&Seg2::Cubic { p0: cubic[0], p1: cubic[1], p2: cubic[2], p3: cubic[3] }),
        ];
        let ents: Vec<DxfEntity> = shapes.into_iter().map(|shape| DxfEntity { layer: "0".into(), shape }).collect();
        let dxf = write_dxf_drawing(&[], &ents).unwrap();
        let back = read_dxf(&dxf).unwrap();
        assert_eq!(back.len(), 6, "{back:?}");
        assert!(matches!(&back[0], Geom2::Line(a, b) if a.dist(Vec2::ZERO) < 1e-9 && b.dist(Vec2::new(10.0, 0.0)) < 1e-9));
        assert!(matches!(&back[1], Geom2::Circle(c, r) if c.dist(Vec2::new(5.0, 5.0)) < 1e-9 && (r - 2.0).abs() < 1e-9));
        assert!(
            matches!(&back[2], Geom2::Arc { c, a, b } if c.dist(Vec2::ZERO) < 1e-9 && a.dist(Vec2::new(3.0, 0.0)) < 1e-9 && b.dist(Vec2::new(0.0, 3.0)) < 1e-9)
        );
        // The whole ellipse: centre, end of the major axis, minor radius.
        assert!(
            matches!(&back[3], Geom2::Ellipse { c, major, minor } if c.dist(Vec2::new(50.0, 40.0)) < 1e-9 && major.dist(Vec2::new(75.0, 40.0)) < 1e-9 && (minor - 12.0).abs() < 1e-9),
            "{:?}",
            back[3]
        );
        // The reader makes an elliptical arc a spline through points of it: from (75, 40) to (50, 52).
        let Geom2::Spline { pts, control: false, .. } = &back[4] else { panic!("{:?}", back[4]) };
        let on_ellipse = |p: &Vec2| (((p.x - 50.0) / 25.0).powi(2) + ((p.y - 40.0) / 12.0).powi(2) - 1.0).abs() < 1e-9;
        assert!(
            pts.first().is_some_and(|p| p.dist(Vec2::new(75.0, 40.0)) < 1e-9) && pts.last().is_some_and(|p| p.dist(Vec2::new(50.0, 52.0)) < 1e-9)
        );
        assert!(pts.iter().all(on_ellipse));
        // The cubic: degree and control points.
        let Geom2::Spline { pts, control: true, degree: 3 } = &back[5] else { panic!("{:?}", back[5]) };
        assert!(pts.iter().zip(&cubic).all(|(a, b)| a.dist(*b) < 1e-9) && pts.len() == 4);
        // A degree-9 Bézier (a G4 blend) comes back at its own degree over all ten points.
        let mut q = [Vec2::ZERO; solidcraft_geom::MAX_BEZIER_CTRL];
        for (i, s) in q.iter_mut().enumerate() {
            *s = Vec2::new(i as f64, (i % 3) as f64);
        }
        let nine = write_dxf_drawing(&[], &[DxfEntity { layer: "0".into(), shape: seg_shape(&Seg2::Bezier { n: 9, p: q }) }]).unwrap();
        let back_nine = read_dxf(&nine).unwrap();
        assert!(
            matches!(&back_nine[..], [Geom2::Spline { pts, control: true, degree: 9 }] if pts.len() == 10 && pts.iter().zip(&q).all(|(a, b)| a.dist(*b) < 1e-9)),
            "{back_nine:?}"
        );

        // The numbers in the file itself, which the reader does not keep: the ellipse...
        let file = entities(&dxf);
        assert_eq!(file.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(), ["LINE", "CIRCLE", "ARC", "ELLIPSE", "ELLIPSE", "SPLINE"]);
        assert!(near(&values(&file[3], 10), &[50.0]) && near(&values(&file[3], 20), &[40.0]) && near(&values(&file[3], 11), &[25.0]));
        assert!(near(&values(&file[3], 21), &[0.0]) && near(&values(&file[3], 40), &[0.48]) && near(&values(&file[3], 41), &[0.0]));
        assert!(near(&values(&file[3], 42), &[TAU]) && near(&values(&file[3], 230), &[1.0]));
        // ...the elliptical arc from parameter 0 to π/2...
        assert!(near(&values(&file[4], 41), &[0.0]) && near(&values(&file[4], 42), &[FRAC_PI_2]) && near(&values(&file[4], 40), &[0.48]));
        // ...and the spline: planar, degree 3, eight knots, four control points.
        let spline = &file[5];
        assert_eq!(values(spline, 70), [8.0]);
        assert_eq!((values(spline, 71), values(spline, 72), values(spline, 73), values(spline, 74)), (vec![3.0], vec![8.0], vec![4.0], vec![0.0]));
        assert!(near(&values(spline, 40), &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]));
        assert!(near(&values(spline, 10), &[0.0, 5.0, 15.0, 20.0]) && near(&values(spline, 20), &[0.0, 10.0, 10.0, 0.0]));
        assert!(values(spline, 41).is_empty());
    }

    #[test]
    fn a_rational_spline_lists_knots_then_weights_then_control_points() {
        let hyperbola = seg_shape(&Seg2::Conic { a: Vec2::ZERO, apex: Vec2::new(1.0, 2.0), b: Vec2::new(3.0, 0.0), w: 2.0 });
        let dxf = write_dxf_drawing(&[], &[DxfEntity { layer: "0".into(), shape: hyperbola }]).unwrap();
        let e = &entities(&dxf)[0];
        assert_eq!(values(e, 70), [12.0]);
        assert!(near(&values(e, 41), &[1.0, 2.0, 1.0]) && near(&values(e, 40), &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]));
        let at = |code: i32| e.1.iter().position(|(c, _)| *c == code).unwrap();
        let last = |code: i32| e.1.iter().rposition(|(c, _)| *c == code).unwrap();
        assert!(last(40) < at(41) && last(41) < at(10), "{:?}", e.1);
    }

    /// The records of a symbol table, each as its group codes after the type.
    fn table_records(p: &[(i32, String)], table: &str) -> Vec<Vec<(i32, String)>> {
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, table)).unwrap();
        let mut out: Vec<Vec<(i32, String)>> = Vec::new();
        for (c, v) in &p[start + 1..] {
            match (*c, v.as_str()) {
                (0, "ENDTAB") => break,
                (0, _) => out.push(Vec::new()),
                _ => {
                    if let Some(record) = out.last_mut() {
                        record.push((*c, v.clone()));
                    }
                }
            }
        }
        out
    }

    fn field(record: &[(i32, String)], code: i32) -> &str {
        record.iter().find(|(c, _)| *c == code).map_or("", |(_, v)| v.as_str())
    }

    #[test]
    fn a_dashed_layer_names_its_linetype_and_the_table_defines_it() {
        let dxf = write_dxf_drawing(&[DxfLayer::new("BEND", 1).dashed(), DxfLayer::new("CUT", 4)], &[]).unwrap();
        let p = pairs(&dxf);
        // The linetypes of every drawing, then DASHED: a dash of 12.7 mm and a gap of 6.35 mm, 19.05 mm in all.
        let types = table_records(&p, "LTYPE");
        assert_eq!(types.iter().map(|r| field(r, 2)).collect::<Vec<_>>(), ["ByBlock", "ByLayer", "Continuous", "DASHED"]);
        let dashed = &types[3];
        let numbers = |code: i32| dashed.iter().filter(|(c, _)| *c == code).map(|(_, v)| v.parse::<f64>().unwrap()).collect::<Vec<_>>();
        assert!(near(&numbers(49), &[12.7, -6.35]) && near(&numbers(40), &[19.05]) && near(&numbers(73), &[2.0]) && near(&numbers(72), &[65.0]));
        // The table says how many records it holds.
        let at = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "LTYPE")).unwrap();
        assert_eq!(p[at..].iter().find(|(c, _)| *c == 70).map(|(_, v)| v.as_str()), Some("4"));
        // Each layer names its linetype.
        let layers = table_records(&p, "LAYER");
        assert_eq!(
            layers.iter().map(|r| (field(r, 2), field(r, 62), field(r, 6))).collect::<Vec<_>>(),
            [("0", "7", "Continuous"), ("BEND", "1", "DASHED"), ("CUT", "4", "Continuous")]
        );
    }

    #[test]
    fn a_center_layer_names_the_center_linetype_and_the_table_defines_its_elements() {
        let layer = DxfLayer { linetype: DxfLinetype::Center, ..DxfLayer::new("CENTER", 4) };
        let dxf =
            write_dxf_drawing(&[layer], &[DxfEntity { layer: "CENTER".into(), shape: DxfShape::Line(Vec2::ZERO, Vec2::new(10.0, 0.0)) }]).unwrap();
        let p = pairs(&dxf);
        let types = table_records(&p, "LTYPE");
        let center = types.iter().find(|r| field(r, 2) == "CENTER").expect("CENTER is defined when a layer names it");
        let numbers = |code: i32| center.iter().filter(|(c, _)| *c == code).map(|(_, v)| v.parse::<f64>().unwrap()).collect::<Vec<_>>();
        // ISO 128-2 type 04 at d = 0.25 mm: long dash 6 mm, gap 0.75 mm, dot 0.125 mm, gap 0.75 mm.
        assert!(near(&numbers(49), &[6.0, -0.75, 0.0, -0.75]), "{:?}", numbers(49));
        assert!(near(&numbers(40), &[7.5]) && near(&numbers(73), &[4.0]) && near(&numbers(72), &[65.0]));
        // The layer names the linetype, so every entity on it uses it.
        let layers = table_records(&p, "LAYER");
        let center_layer = layers.iter().find(|r| field(r, 2) == "CENTER").expect("the layer is in the table");
        assert_eq!(field(center_layer, 6), "CENTER");
        // A drawing without a centre layer carries no CENTER entry.
        let plain = write_dxf_drawing(&[DxfLayer::new("OUTLINE", 7)], &[]).unwrap();
        assert!(table_records(&pairs(&plain), "LTYPE").iter().all(|r| field(r, 2) != "CENTER"));
    }

    #[test]
    fn hidden_polylines_continue_short_dashes_across_chords_and_round_trip() {
        let points = vec![Vec2::ZERO, Vec2::new(1., 0.), Vec2::new(2., 1.), Vec2::new(3., 1.)];
        let layer = DxfLayer { linetype: DxfLinetype::Hidden, ..DxfLayer::new("HIDDEN", 8) };
        let text = write_dxf_drawing(&[layer], &[DxfEntity { layer: "HIDDEN".into(), shape: DxfShape::Polyline { points } }]).unwrap();
        let records = entities(&text);
        assert_eq!(records[0].0, "LWPOLYLINE");
        assert_eq!(field(&records[0].1, 70), "128");
        assert_eq!(field(&records[0].1, 90), "4");
        let types = table_records(&pairs(&text), "LTYPE");
        let hidden = types.iter().find(|r| field(r, 2) == "HIDDEN").unwrap();
        // ISO 128-2 type 02 in the sheet's thin 0.5/0.25 group: 12·d = 3 mm, 3·d = 0.75 mm.
        assert_eq!(hidden.iter().filter(|(c, _)| *c == 49).map(|(_, v)| v.parse::<f64>().unwrap()).collect::<Vec<_>>(), [3., -0.75]);
        assert_eq!(read_dxf(&text).unwrap().len(), 3);
    }

    /// The blocks of the BLOCKS section, by name, each as its entity records' (code, value) pairs.
    fn named_blocks(p: &[(i32, String)]) -> std::collections::BTreeMap<String, Vec<(i32, String)>> {
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "BLOCKS")).unwrap();
        let mut out = std::collections::BTreeMap::new();
        let mut current: Option<(String, Vec<(i32, String)>)> = None;
        for (c, v) in &p[start + 1..] {
            if *c == 0 && v == "ENDSEC" {
                break;
            }
            if *c == 0 && v == "BLOCK" {
                current = Some((String::new(), Vec::new()));
            } else if *c == 0 && v == "ENDBLK" {
                if let Some((name, block)) = current.take() {
                    out.insert(name, block);
                }
            } else if let Some((name, block)) = current.as_mut() {
                if name.is_empty() && *c == 2 {
                    *name = v.clone();
                } else if *c == 0 {
                    block.push((*c, v.clone()));
                }
            }
        }
        out
    }

    fn solid(points: &[Vec2]) -> DxfEntity {
        DxfEntity { layer: "0".into(), shape: DxfShape::Solid { points: points.to_vec() } }
    }

    fn sample_dimensions() -> (Vec<DxfDimension>, Vec<DxfLeader>) {
        let dims = vec![
            DxfDimension {
                layer: "DIMENSION".into(),
                kind: DxfDimKind::Rotated,
                block_name: "*D1".into(),
                def_point: Vec2::new(0.0, -20.0),
                text_midpoint: Vec2::new(15.0, -17.25),
                def_point2: Some(Vec2::new(0.0, -5.0)),
                def_point3: Some(Vec2::new(30.0, -5.0)),
                def_point4: None,
                angle_deg: 0.0,
                measurement: 30.0,
                text_override: None,
                style: DIM_STYLE.into(),
                block: vec![
                    DxfEntity { layer: "0".into(), shape: DxfShape::Line(Vec2::new(0.0, -5.0), Vec2::new(0.0, -22.0)) },
                    DxfEntity { layer: "0".into(), shape: DxfShape::Line(Vec2::new(30.0, -5.0), Vec2::new(30.0, -22.0)) },
                    DxfEntity { layer: "0".into(), shape: DxfShape::Line(Vec2::new(2.5, -20.0), Vec2::new(27.5, -20.0)) },
                    solid(&[Vec2::new(2.5, -20.0), Vec2::new(0.0, -19.33), Vec2::new(0.0, -20.67)]),
                    solid(&[Vec2::new(27.5, -20.0), Vec2::new(30.0, -19.33), Vec2::new(30.0, -20.67)]),
                    DxfEntity {
                        layer: "0".into(),
                        shape: DxfShape::Text {
                            at: Vec2::new(15.0, -17.25),
                            height: 3.5,
                            angle_deg: 0.0,
                            text: "30".into(),
                            anchor: DxfTextAnchor::Center,
                        },
                    },
                ],
            },
            DxfDimension {
                layer: "DIMENSION".into(),
                kind: DxfDimKind::Diametric,
                block_name: "*D2".into(),
                def_point: Vec2::new(15.0, -5.0),
                text_midpoint: Vec2::new(14.0, 14.0),
                def_point2: Some(Vec2::new(15.0, -5.0)),
                def_point3: Some(Vec2::new(15.0, 5.0)),
                def_point4: Some(Vec2::new(15.0, 5.0)),
                angle_deg: 90.0,
                measurement: 10.0,
                text_override: Some("\u{00d8}10 h6".into()),
                style: DIM_STYLE.into(),
                block: vec![
                    DxfEntity { layer: "0".into(), shape: DxfShape::Line(Vec2::new(15.0, -5.0), Vec2::new(15.0, 5.0)) },
                    solid(&[Vec2::new(15.0, 5.0), Vec2::new(14.33, 2.5), Vec2::new(15.67, 2.5)]),
                    solid(&[Vec2::new(15.0, -5.0), Vec2::new(14.33, -2.5), Vec2::new(15.67, -2.5)]),
                    DxfEntity {
                        layer: "0".into(),
                        shape: DxfShape::Text {
                            at: Vec2::new(14.0, 14.0),
                            height: 3.5,
                            angle_deg: 90.0,
                            text: "\u{00d8}10 h6".into(),
                            anchor: DxfTextAnchor::Center,
                        },
                    },
                ],
            },
        ];
        let leaders = vec![DxfLeader {
            layer: "DIMENSION".into(),
            style: DIM_STYLE.into(),
            points: vec![Vec2::new(120.0, 170.0), Vec2::new(100.0, 120.0)],
            arrowhead: true,
        }];
        (dims, leaders)
    }

    #[test]
    fn dimension_entities_reference_blocks_and_the_dimstyle_is_defined() {
        let (dims, leaders) = sample_dimensions();
        let arrow = solid(&[Vec2::new(0.0, 0.0), Vec2::new(2.5, 0.67), Vec2::new(2.5, -0.67)]);
        let dxf = write_dxf_drawing_with(&[DxfLayer::new("DIMENSION", 7)], &[arrow], &dims, &leaders).unwrap();
        let p = pairs(&dxf);
        // DIMSTYLE: one SolidCraft ISO style with the house values. Non-ASCII names are escaped.
        let styles = table_records(&p, "DIMSTYLE");
        assert_eq!(styles.len(), 1);
        let style = &styles[0];
        assert_eq!(field(style, 2), "SolidCraft ISO");
        let number = |code: i32| field(style, code).parse::<f64>().unwrap_or(f64::NAN);
        assert!((number(41) - 2.5).abs() < 1e-9, "DIMASZ");
        assert!((number(140) - 3.5).abs() < 1e-9, "DIMTXT");
        assert!((number(44) - 2.0).abs() < 1e-9, "DIMEXE");
        assert!((number(42) - 0.0).abs() < 1e-9, "DIMEXO");
        assert!((number(147) - 1.0).abs() < 1e-9, "DIMGAP");
        assert_eq!(field(style, 77), "1", "DIMTAD");
        assert_eq!(field(style, 271), "2", "DIMDEC");
        assert_eq!(field(style, 277), "2", "DIMLUNIT");
        assert_eq!(field(style, 278), "44", "DIMDSEP is the comma");
        assert_eq!(field(style, 78), "8", "DIMZIN suppresses trailing zeros");
        // N DIMENSION entities with their anonymous blocks, and the block records.
        let ents = entities(&dxf);
        let dimensions: Vec<_> = ents.iter().filter(|e| e.0 == "DIMENSION").collect();
        assert_eq!(dimensions.len(), 2);
        assert_eq!(dimensions[0].1.iter().find(|(c, _)| *c == 2).map(|(_, v)| v.as_str()), Some("*D1"));
        assert_eq!(dimensions[1].1.iter().find(|(c, _)| *c == 2).map(|(_, v)| v.as_str()), Some("*D2"));
        assert_eq!(field(&dimensions[0].1, 70), "32", "rotated, anonymous block");
        assert_eq!(field(&dimensions[1].1, 70), "35", "diametric, anonymous block");
        assert!((field(&dimensions[0].1, 42).parse::<f64>().unwrap_or(0.0) - 30.0).abs() < 1e-9);
        assert_eq!(field(&dimensions[1].1, 1), "\\U+00D810 h6", "the diameter override is escaped");
        let records = table_records(&p, "BLOCK_RECORD");
        assert!(records.iter().any(|r| field(r, 2) == "*D1") && records.iter().any(|r| field(r, 2) == "*D2"));
        // The blocks hold the drawn geometry: lines, two SOLIDs and a text each.
        let blocks = named_blocks(&p);
        for (name, solids) in [("*D1", 2usize), ("*D2", 2usize)] {
            let block = blocks.get(name).unwrap_or_else(|| panic!("{name} is missing: {:?}", blocks.keys().collect::<Vec<_>>()));
            let counts = block.iter().filter(|(c, _)| *c == 0).fold(std::collections::BTreeMap::new(), |mut m, (_, kind)| {
                *m.entry(kind.clone()).or_insert(0usize) += 1;
                m
            });
            assert_eq!(counts.get("SOLID").copied().unwrap_or(0), solids, "{name}: {counts:?}");
            assert!(counts.get("LINE").copied().unwrap_or(0) >= 1, "{name}: {counts:?}");
            assert_eq!(counts.get("TEXT").copied().unwrap_or(0), 1, "{name}: {counts:?}");
        }
        // The SOLID entity codes and the leader.
        let solids_raw: Vec<_> = ents.iter().filter(|e| e.0 == "SOLID").collect();
        assert_eq!(solids_raw.len(), 1, "the model-space arrowhead");
        assert!(solids_raw[0].1.contains(&(100, "AcDbTrace".to_string())));
        assert_eq!(values(solids_raw[0], 13), [2.5], "SOLID repeats its third corner for a triangle");
        let leaders_raw: Vec<_> = ents.iter().filter(|e| e.0 == "LEADER").collect();
        assert_eq!(leaders_raw.len(), 1);
        assert_eq!(field(&leaders_raw[0].1, 76), "2", "two leader vertices");
        assert_eq!(values(leaders_raw[0], 10), [120.0, 100.0]);
    }

    #[test]
    fn a_hatch_entity_carries_its_pattern_and_closed_boundary_paths() {
        let hatch = DxfHatch {
            layer: "HATCH".into(),
            pattern: "ANSI31".into(),
            scale: 3.0 / DxfHatch::ANSI31_SPACING_MM,
            angle_deg: 0.0,
            loops: vec![
                vec![Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)],
                vec![Vec2::new(3.0, 3.0), Vec2::new(6.0, 3.0), Vec2::new(6.0, 6.0), Vec2::new(3.0, 6.0)],
            ],
        };
        let dxf = write_dxf_drawing_full(&[DxfLayer::new("HATCH", 4)], &[], &[], &[], &[hatch]).unwrap();
        let ents = entities(&dxf);
        let [hatch] = ents.as_slice() else { panic!("{ents:?}") };
        assert_eq!(hatch.0, "HATCH");
        assert_eq!(field(&hatch.1, 8), "HATCH");
        assert_eq!(field(&hatch.1, 2), "ANSI31");
        assert_eq!(field(&hatch.1, 91), "2", "two boundary paths");
        assert_eq!(field(&hatch.1, 75), "0", "nested fill: islands stay free");
        assert_eq!(field(&hatch.1, 78), "1", "one predefined pattern line");
        assert!((field(&hatch.1, 41).parse::<f64>().unwrap_or(0.0) - 3.0 / 3.175).abs() < 1e-9);
        let paths: Vec<&(i32, String)> = hatch.1.iter().filter(|(c, _)| *c == 92).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|(_, v)| v == "3"), "external polyline paths");
        let counts: Vec<&(i32, String)> = hatch.1.iter().filter(|(c, _)| *c == 93).collect();
        assert_eq!(counts.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["4", "4"]);
    }

    #[test]
    fn a_leader_without_an_arrowhead_is_written_with_flag_zero() {
        let leader = DxfLeader { layer: "0".into(), style: DIM_STYLE.into(), points: vec![Vec2::ZERO, Vec2::X], arrowhead: false };
        let dxf = write_dxf_drawing_with(&[], &[], &[], &[leader]).unwrap();
        let e = entities(&dxf);
        assert_eq!(e.len(), 1);
        assert_eq!(field(&e[0].1, 71), "0");
    }

    /// Every block of the BLOCKS section with all of its group codes (the block's own name,
    /// handle and owner excluded).
    fn blocks_with_pairs(p: &[(i32, String)]) -> std::collections::BTreeMap<String, Vec<(i32, String)>> {
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "BLOCKS")).unwrap();
        let mut out = std::collections::BTreeMap::new();
        let mut current: Option<(String, Vec<(i32, String)>)> = None;
        for (c, v) in &p[start + 1..] {
            if *c == 0 && v == "ENDSEC" {
                break;
            }
            if *c == 0 && v == "BLOCK" {
                current = Some((String::new(), Vec::new()));
            } else if *c == 0 && v == "ENDBLK" {
                if let Some((name, block)) = current.take() {
                    out.insert(name, block);
                }
            } else if let Some((name, block)) = current.as_mut() {
                if name.is_empty() {
                    if *c == 2 {
                        *name = v.clone();
                    }
                } else {
                    block.push((*c, v.clone()));
                }
            }
        }
        out
    }

    /// The (code, value) pairs of one object in the OBJECTS section, by its group 1 name or
    /// group 0 type.
    fn objects(p: &[(i32, String)]) -> Vec<Vec<(i32, String)>> {
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "OBJECTS")).unwrap();
        let mut out: Vec<Vec<(i32, String)>> = Vec::new();
        for (c, v) in &p[start + 1..] {
            if *c == 0 && v == "ENDSEC" {
                break;
            }
            if *c == 0 {
                out.push(Vec::new());
            }
            if let Some(object) = out.last_mut() {
                object.push((*c, v.clone()));
            }
        }
        out
    }

    #[test]
    fn iso_linetypes_and_the_ltscale_hold_the_sheet_patterns() {
        let layers = [
            DxfLayer { linetype: DxfLinetype::Hidden, ..DxfLayer::new("HIDDEN", 3) },
            DxfLayer { linetype: DxfLinetype::Center, ..DxfLayer::new("CENTER", 4) },
            DxfLayer { linetype: DxfLinetype::Section, ..DxfLayer::new("SECTION_LINE", 7) },
        ];
        let dxf = write_dxf_drawing(&layers, &[]).unwrap();
        let p = pairs(&dxf);
        let types = table_records(&p, "LTYPE");
        assert_eq!(types.iter().map(|r| field(r, 2)).collect::<Vec<_>>(), ["ByBlock", "ByLayer", "Continuous", "HIDDEN", "CENTER", "ISO_SECTION"]);
        let elements = |name: &str| {
            let record = types.iter().find(|r| field(r, 2) == name).unwrap();
            record.iter().filter(|(c, _)| *c == 49).map(|(_, v)| v.parse::<f64>().unwrap()).collect::<Vec<_>>()
        };
        assert_eq!(elements("HIDDEN"), [3.0, -0.75], "ISO 128-2 type 02 in the thin line group");
        assert_eq!(elements("CENTER"), [6.0, -0.75, 0.0, -0.75], "ISO 128-2 type 04");
        assert_eq!(elements("ISO_SECTION"), [6.0, -0.75, 0.0, -0.75], "the section line is type 04 too");
        // Layers name their linetypes; the entities need no pattern of their own.
        let records = table_records(&p, "LAYER");
        let by_name = |name: &str| records.iter().find(|r| field(r, 2) == name).map(|r| field(r, 6).to_string()).unwrap();
        assert_eq!((by_name("HIDDEN"), by_name("CENTER"), by_name("SECTION_LINE")), ("HIDDEN".into(), "CENTER".into(), "ISO_SECTION".into()));
        // $LTSCALE is 1: the absolute millimetre patterns are the drawn dash lengths.
        let scale = p.iter().position(|(c, v)| (*c, v.as_str()) == (9, "$LTSCALE")).unwrap() + 1;
        assert_eq!(p[scale], (40, "1.0".to_string()));
    }

    #[test]
    fn lineweights_snap_to_the_standard_enum() {
        // The sheet's ISO 128-2 widths are already standard values; a caller's off-value width
        // becomes the nearest one, and a hostile or zero width becomes 0.00 mm, never BYDEFAULT.
        for (mm, hundredths) in [(0.70, 70), (0.50, 50), (0.35, 35), (0.25, 25), (0.18, 18), (0.13, 13), (2.00, 200)] {
            let weight = DxfLineweight::from_mm(mm);
            assert_eq!(weight.hundredths(), hundredths, "{mm} mm");
            assert!(DxfLineweight::STANDARD.contains(&weight.hundredths()));
        }
        assert_eq!(DxfLineweight::from_mm(0.26).hundredths(), 25, "nearest standard value");
        assert_eq!(DxfLineweight::from_mm(0.0).hundredths(), 0);
        assert_eq!(DxfLineweight::from_mm(-1.0).hundredths(), 0);
        assert_eq!(DxfLineweight::from_mm(f64::NAN).hundredths(), 0);
        assert_eq!(DxfLineweight::from_mm(f64::INFINITY).hundredths(), 0);
        // The inherited values are not geometric widths but group 370 codes in their own right.
        assert_eq!((DxfLineweight::BY_LAYER.hundredths(), DxfLineweight::BY_DEFAULT.hundredths()), (-1, -3));
    }

    #[test]
    fn every_layer_carries_the_pdf_stroke_width_and_entities_stay_bylayer() {
        // The drawing sheet's PDF pen table (`PENS` in engine/src/drawing/sheet.rs) strokes these
        // layers with these widths. The DXF layer must name the same width as a standard group 370
        // value: 0.70 → 70, 0.50 → 50 and 0.25 → 25 hundredths of a millimetre. Lettering has no
        // stroke (it is filled), so its layer takes the thin 0.25 mm of the line group.
        let pens = [
            ("FRAME", 0.70, 70),
            ("VISIBLE", 0.50, 50),
            ("HIDDEN", 0.25, 25),
            ("THIN", 0.25, 25),
            ("CENTER", 0.25, 25),
            ("DIMENSION", 0.25, 25),
            ("HATCH", 0.25, 25),
            ("SECTION", 0.50, 50),
            ("SECTION_LINE", 0.25, 25),
            ("TANGENT", 0.25, 25),
            ("FREEHAND", 0.25, 25),
            ("TITLE", 0.25, 25),
            ("TEXT", 0.0, 25),
        ];
        let layers: Vec<DxfLayer> = pens
            .iter()
            .map(|(name, width, _)| {
                let lineweight = if *width > 0.0 { DxfLineweight::from_mm(*width) } else { DxfLineweight::THIN };
                DxfLayer::new(*name, 7).with_lineweight(lineweight)
            })
            .collect();
        let entity = |shape| DxfEntity { layer: "VISIBLE".into(), shape };
        let dxf = write_dxf_drawing(&layers, &[entity(DxfShape::Line(Vec2::ZERO, Vec2::X))]).unwrap();
        let p = pairs(&dxf);
        // Every layer in the table has a standard lineweight; a created layer (0) is thin and so
        // is an inherited layer without a pen, so no layer falls back to BYDEFAULT.
        let records = table_records(&p, "LAYER");
        for record in &records {
            let value: i16 = field(record, 370).parse().unwrap_or_else(|_| panic!("layer {:?} has no group 370", field(record, 2)));
            assert!(DxfLineweight::STANDARD.contains(&value), "layer {:?} has the non-standard lineweight {value}", field(record, 2));
        }
        assert_eq!(field(&records[0], 370), "25", "layer 0 is the thin width");
        for (name, width, expected) in pens {
            // The values the PDF uses are exactly the layer's lineweight, in hundredths of a mm.
            if width > 0.0 {
                assert_eq!(DxfLineweight::from_mm(width).hundredths(), expected, "{name}");
            }
            let record = records
                .iter()
                .find(|r| field(r, 2) == text_value(name).unwrap())
                .unwrap_or_else(|| panic!("layer {name} is missing (escaped as {:?})", text_value(name).unwrap()));
            assert_eq!(field(record, 370).parse::<i16>().unwrap_or(-999), expected, "layer {name}");
        }
        // The header turns lineweight display on ($LWDISPLAY is a boolean, group 290).
        let display = p.iter().position(|(c, v)| (*c, v.as_str()) == (9, "$LWDISPLAY")).expect("$LWDISPLAY");
        assert_eq!(p.get(display + 1).map(|(c, v)| (*c, v.as_str())), Some((290, "1")), "lineweights are shown");
        // Every entity takes its layer's width: group 370 is BYLAYER (−1), never a private value.
        for (kind, fields) in entities(&dxf) {
            assert_eq!(field(&fields, 370), "-1", "{kind} is not BYLAYER");
        }
    }

    #[test]
    fn a_paper_space_layout_holds_a_viewport_and_a_named_layout() {
        let paper = vec![
            DxfEntity { layer: "FRAME".into(), shape: DxfShape::Line(Vec2::ZERO, Vec2::new(297.0, 0.0)) },
            DxfEntity {
                layer: "TEXT".into(),
                shape: DxfShape::Text { at: Vec2::new(10.0, 10.0), height: 3.5, angle_deg: 0.0, text: "F".into(), anchor: DxfTextAnchor::Center },
            },
        ];
        let model = vec![DxfEntity { layer: "VISIBLE".into(), shape: DxfShape::Circle { c: Vec2::new(50.0, 50.0), r: 5.0 } }];
        let layout = DxfLayout::a4_landscape();
        let dxf = write_dxf_drawing_layout(&[DxfLayer::new("FRAME", 7)], &model, &paper, &[], &[], &[], Some(&layout)).unwrap();
        let p = pairs(&dxf);
        // The paper block owns the furniture and the one viewport; each paper entity says 67=1.
        let blocks = blocks_with_pairs(&p);
        let block = blocks.get("*Paper_Space").expect("the paper-space block");
        assert_eq!(block.iter().filter(|(c, v)| *c == 0 && (v == "LINE" || v == "VIEWPORT")).count(), 2, "{block:?}");
        let viewport = block.iter().enumerate().position(|(_, (c, v))| *c == 0 && v == "VIEWPORT").unwrap();
        let viewport_pairs = &block[viewport..];
        assert_eq!(field(viewport_pairs, 8), "VIEWPORT");
        assert_eq!(field(viewport_pairs, 67), "1");
        // The visible rectangle is the page and the view height is the paper height: scale 1:1.
        assert!(near(&values_raw(viewport_pairs, 40), &[297.0]) && near(&values_raw(viewport_pairs, 41), &[210.0]));
        assert!(near(&values_raw(viewport_pairs, 45), &[210.0]));
        // Paper entities in the block: every one is flagged and owned by its block record.
        let paper_record = {
            let records = table_records(&p, "BLOCK_RECORD");
            records.iter().find(|r| field(r, 2) == "*Paper_Space").map(|r| field(r, 5).to_string()).unwrap()
        };
        for (i, (c, v)) in block.iter().enumerate() {
            if *c == 0 && v == "LINE" {
                assert_eq!(field(&block[i..], 67), "1");
                assert_eq!(field(&block[i..], 330), paper_record);
            }
        }
        // Model space keeps only the drawing geometry.
        let model_entities = entities(&dxf);
        assert!(model_entities.iter().any(|e| e.0 == "CIRCLE"));
        assert_eq!(model_entities.iter().filter(|e| e.0 == "LINE").count(), 0, "the frame is not in model space");
        // The layout objects: Model and the named paper layout, the latter listing its viewport.
        let objects = objects(&p);
        let dicts: Vec<&Vec<(i32, String)>> = objects.iter().filter(|o| o.first().is_some_and(|(c, v)| *c == 0 && v == "DICTIONARY")).collect();
        assert_eq!(dicts.len(), 2, "root and layout dictionary");
        let layout_dict = dicts[1];
        let names: Vec<&str> = layout_dict.iter().filter(|(c, _)| *c == 3).map(|(_, v)| v.as_str()).collect();
        assert_eq!(names, ["Model", "Blatt 1"]);
        let layouts: Vec<&Vec<(i32, String)>> = objects.iter().filter(|o| o.first().is_some_and(|(c, v)| *c == 0 && v == "LAYOUT")).collect();
        assert_eq!(layouts.len(), 2);
        // A layout's own name is the second group 1 (the first is the page setup's).
        let layout_name = |l: &&Vec<(i32, String)>| l.iter().rev().find(|(c, _)| *c == 1).map(|(_, v)| v.clone()).unwrap_or_default();
        let paper_layout = layouts.iter().find(|l| layout_name(l) == "Blatt 1").expect("the named layout");
        assert_eq!(layout_name(&layouts[0]), "Model");
        let at = paper_layout.iter().position(|(c, v)| *c == 100 && v == "AcDbLayout").expect("the layout half");
        let layout_part = &paper_layout[at..];
        assert_eq!(field(layout_part, 76), "1");
        assert_eq!(field(layout_part, 330), paper_record);
        assert_eq!(field(layout_part, 331), p[viewport_handle_at(&p)].1.as_str());
    }

    #[test]
    fn a_furniture_block_is_one_insert_on_the_title_block_layer() {
        let paper = vec![
            DxfEntity { layer: "FRAME".into(), shape: DxfShape::Line(Vec2::ZERO, Vec2::new(297.0, 0.0)) },
            DxfEntity {
                layer: "TEXT".into(),
                shape: DxfShape::Text { at: Vec2::new(10.0, 10.0), height: 3.5, angle_deg: 0.0, text: "F".into(), anchor: DxfTextAnchor::Center },
            },
        ];
        let layout = DxfLayout { name: "Blatt 1".into(), paper_mm: (297.0, 210.0), furniture_block: Some("SolidCraft-Sheet".into()) };
        let dxf = write_dxf_drawing_layout(&[], &[], &paper, &[], &[], &[], Some(&layout)).unwrap();
        let p = pairs(&dxf);
        // The block record and the block hold the furniture.
        assert!(table_records(&p, "BLOCK_RECORD").iter().any(|r| field(r, 2) == "SolidCraft-Sheet"));
        let blocks = blocks_with_pairs(&p);
        let furniture = blocks.get("SolidCraft-Sheet").expect("the furniture block");
        assert!(furniture.iter().any(|(c, v)| *c == 0 && v == "LINE") && furniture.iter().any(|(c, v)| *c == 0 && v == "TEXT"));
        // One INSERT in paper space, on the title-block layer, flagged as paper space.
        let paper_block = blocks.get("*Paper_Space").expect("the paper block");
        let inserts: Vec<&(i32, String)> = paper_block.iter().filter(|(c, v)| *c == 0 && v == "INSERT").collect();
        assert_eq!(inserts.len(), 1, "{paper_block:?}");
        let at = paper_block.iter().position(|(c, v)| *c == 0 && v == "INSERT").unwrap();
        assert_eq!(field(&paper_block[at..], 2), "SolidCraft-Sheet");
        assert_eq!(field(&paper_block[at..], 8), "TITLE_BLOCK");
        assert_eq!(field(&paper_block[at..], 67), "1");
        // The layer exists, so the reference is not on layer 0.
        assert!(table_records(&p, "LAYER").iter().any(|r| field(r, 2) == "TITLE_BLOCK"));
        // The loose furniture entities are not also in the paper block.
        assert!(!paper_block.iter().any(|(c, v)| *c == 0 && v == "LINE"));
    }

    /// The handle of the viewport in the paper-space block.
    fn viewport_handle_at(p: &[(i32, String)]) -> usize {
        let start = p.iter().position(|(c, v)| (*c, v.as_str()) == (2, "BLOCKS")).unwrap();
        p[start..].iter().position(|(c, v)| *c == 0 && v == "VIEWPORT").map(|i| start + i + 1).unwrap()
    }

    fn values_raw(pairs: &[(i32, String)], code: i32) -> Vec<f64> {
        pairs.iter().filter(|(c, _)| *c == code).map(|(_, v)| v.parse().unwrap()).collect()
    }

    #[test]
    fn text_names_its_baseline_anchor_and_the_reader_gets_it_back() {
        let at = Vec2::new(30.0, 12.0);
        let note = |anchor| DxfShape::Text { at, height: 3.0, angle_deg: 90.0, text: "AB%%d\nC\u{b0}".into(), anchor };
        for (anchor, halign) in [(DxfTextAnchor::Left, 0.0), (DxfTextAnchor::Center, 1.0), (DxfTextAnchor::Right, 2.0)] {
            let dxf = write_dxf_drawing(&[DxfLayer::new("NOTES", 8)], &[DxfEntity { layer: "NOTES".into(), shape: note(anchor) }]).unwrap();
            let file = entities(&dxf);
            let [text] = file.as_slice() else { panic!("{file:?}") };
            assert_eq!(text.0, "TEXT");
            // One line (the newline is a space), the sign outside ASCII a code of its own, `%%d` AutoCAD's degree sign.
            assert_eq!(text.1.iter().find(|(c, _)| *c == 1).map(|(_, v)| v.as_str()), Some("AB%%d C\\U+00B0"));
            // 3 mm high, turned 90°, unstretched (width factor 1) and on the baseline.
            assert!(
                near(&values(text, 40), &[3.0])
                    && near(&values(text, 41), &[1.0])
                    && near(&values(text, 50), &[90.0])
                    && near(&values(text, 73), &[0.0])
            );
            assert!(near(&values(text, 72), &[halign]), "{anchor:?}: {:?}", text.1);
            // The insertion point is the anchor; centred and right text also name it as the
            // alignment point, so a viewer measuring with its own font still hits it.
            assert!(near(&values(text, 10), &[at.x]) && near(&values(text, 20), &[at.y]));
            if anchor == DxfTextAnchor::Left {
                assert!(values(text, 11).is_empty(), "left text aligns at its insertion point alone");
            } else {
                assert!(near(&values(text, 11), &[at.x]) && near(&values(text, 21), &[at.y]));
            }
            // Its style is in the STYLE table, and the layer it is on in the LAYER table.
            let p = pairs(&dxf);
            assert_eq!(table_records(&p, "STYLE").iter().map(|r| field(r, 2)).collect::<Vec<_>>(), ["Standard"]);
            assert!(text.1.contains(&(8, "NOTES".to_string())));
            // The reader finds the text where it was put, as high and as turned.
            let back = read_dxf(&dxf).unwrap();
            assert!(
                matches!(&back[..], [Geom2::Text { at: found, height, angle, .. }] if found.dist(at) < 1e-9 && (height - 3.0).abs() < 1e-9 && (angle - FRAC_PI_2).abs() < 1e-9),
                "{back:?}"
            );
        }
    }

    #[test]
    fn non_bmp_text_is_never_silently_replaced() {
        for c in ['𠀋', '𝑥', '😀', '\u{10000}', '\u{10ffff}'] {
            let shape = DxfShape::Text { at: Vec2::ZERO, height: 3.0, angle_deg: 0.0, text: format!("中{c}"), anchor: DxfTextAnchor::Center };
            let entities =
                [DxfEntity { layer: "OUTLINE".into(), shape: DxfShape::Line(Vec2::ZERO, Vec2::X) }, DxfEntity { layer: "NOTES".into(), shape }];
            let error = write_dxf_drawing(&[DxfLayer::new("NOTES", 8)], &entities).unwrap_err();
            assert!(matches!(error, IoError::Invalid(_)), "{error}");
            let message = error.to_string();
            assert!(message.contains("DXF TEXT entity 2 on layer \"NOTES\""), "{message}");
            assert!(message.contains(&format!("{c:?} (U+{:04X}) at position 2", u32::from(c))), "{message}");
            assert!(message.contains("only characters through U+FFFF are supported"), "{message}");
        }
    }

    #[test]
    fn non_bmp_text_past_the_output_limit_is_also_refused() {
        // Even a character beyond the existing output limit must not vanish unnoticed.
        for prefix in [249, 250, 10_000] {
            let shape = DxfShape::Text {
                at: Vec2::ZERO,
                height: 3.0,
                angle_deg: 0.0,
                text: format!("{}𠀋", "中".repeat(prefix)),
                anchor: DxfTextAnchor::Center,
            };
            let error = write_dxf_drawing(&[], &[DxfEntity { layer: "NOTES".into(), shape }]).unwrap_err().to_string();
            assert!(error.contains(&format!("U+2000B) at position {}", prefix + 1)), "{error}");
            assert!(error.len() < 300, "the error must not repeat unbounded input text");
        }
    }

    #[test]
    fn bmp_text_keeps_its_existing_encoding_through_ffff() {
        for (text, expected) in [
            ("ASCII %%d %%p %%c ° 中 \u{ffff}\n".into(), "ASCII %%d %%p %%c \\U+00B0 \\U+4E2D \\U+FFFF".into()),
            ("\u{ffff}".repeat(250), "\\U+FFFF".repeat(250)),
        ] {
            let shape = DxfShape::Text { at: Vec2::ZERO, height: 3.0, angle_deg: 0.0, text, anchor: DxfTextAnchor::Center };
            let dxf = write_dxf_drawing(&[], &[DxfEntity { layer: "0".into(), shape }]).unwrap();
            let file = entities(&dxf);
            let [entity] = file.as_slice() else { panic!("{file:?}") };
            assert_eq!(field(&entity.1, 1), expected);
            assert!(dxf.contains("AC1015"));
        }
    }

    #[test]
    fn the_extents_hold_the_text() {
        // Five letters of 3 mm, turned to read upwards: 0.8 × 3 × 5 = 12 mm long around y = 12, and
        // 3 mm to the left of the baseline at x = 30.
        let shape = DxfShape::Text { at: Vec2::new(30.0, 12.0), height: 3.0, angle_deg: 90.0, text: "ABCDE".into(), anchor: DxfTextAnchor::Center };
        let dxf = write_dxf_drawing(&[], &[DxfEntity { layer: "0".into(), shape }]).unwrap();
        let p = pairs(&dxf);
        let after = |var: &str| &p[p.iter().position(|(c, v)| (*c, v.as_str()) == (9, var)).unwrap() + 1..];
        assert_eq!(&after("$EXTMIN")[..2], &[(10, "27.0".to_string()), (20, "6.0".to_string())]);
        assert_eq!(&after("$EXTMAX")[..2], &[(10, "30.0".to_string()), (20, "18.0".to_string())]);
    }
}
