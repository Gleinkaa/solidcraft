//! Centre lines and centre marks, read off the B-rep's exact surfaces.
//!
//! A round face whose points cover at least half a turn around its axis is a hole, bore, shaft,
//! boss or slot end, and its axis belongs on the sheet. Fillets and rounds below half a turn are
//! left out. Faces on one axis line are grouped, so a stepped shaft has one centre line along its
//! whole length and not one per diameter. Per view, an axis in the view plane becomes a line
//! reaching 3 mm past the view's outline, an axis along the view direction a centre cross
//! reaching 3 mm past the largest circle of its group, and an axis at any other angle nothing.
//!
//! Everything here is read-only and panics on nothing: an unreadable surface simply carries no
//! axis.

use std::f64::consts::PI;

use solidcraft_geom::{Aabb3, UnitVec3, Vec2, Vec3};
use solidcraft_kernel::FaceSurface;
use solidcraft_render::StandardView;

use crate::Session;
use crate::cmd::measure_shape::{Shape, coverage};
use crate::drawing::standard_camera;

/// One degree in radians: the tolerance for "the axis lies in the view plane" and "the axis points
/// along the view direction".
const ONE_DEG: f64 = PI / 180.0;
/// Faces are grouped when their axes are parallel within this angle (radians), 1e-6 rad.
const PARALLEL_TOL: f64 = 1e-6;
/// Faces are grouped when the distance between their axis lines is below this fraction of the
/// part size.
const AXIS_DISTANCE_FRACTION: f64 = 1e-4;
/// A round face counts from half a turn of coverage on (holes, bores, shafts, bosses, slot ends).
const HALF_TURN: f64 = PI;
/// Half a turn exactly, with the last bits of the tessellation's angles: a half cylinder is a
/// slot end and must count, a third of a turn must not.
const HALF_TURN_TOL: f64 = 1e-6;
/// The share of the round-face area a turned part's common axis must carry.
pub const TURNED_AREA_SHARE: f64 = 0.8;

/// An axis line: a point on it, its direction, the extent of the round faces on it (`lo..hi` along
/// the direction from `origin`, in model millimetres) and the largest radius around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub origin: Vec3,
    pub axis: UnitVec3,
    pub lo: f64,
    pub hi: f64,
    pub max_radius: f64,
}

impl Axis {
    /// The point `t` along the axis.
    pub fn point_at(&self, t: f64) -> Vec3 {
        self.origin + self.axis * t
    }
}

/// How one axis shows in one view, in the view's own 2D millimetres (x right, y up, the projected
/// world origin at 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewCentre {
    /// The axis lies in the view plane: the projected axis extent. The sheet draws it over the
    /// view's outline plus 3 mm at each end.
    Line { a: Vec2, b: Vec2 },
    /// The axis points at the viewer: a centre cross through `c` whose arms reach 3 mm past a
    /// circle of `radius` (the largest of the axis group).
    Cross { c: Vec2, radius: f64 },
}

/// One B-rep face with its exact surface, the mesh points and triangle normals that lie on it,
/// and its area. D7 reads material sides from the normals; the centre-line code reads axes.
#[derive(Clone, Debug)]
pub(crate) struct SurfaceFace {
    /// The B-rep face index (the first face of a smooth surface patch).
    pub(crate) face: usize,
    pub(crate) surface: FaceSurface,
    /// Mesh points of the triangles, three per triangle.
    pub(crate) points: Vec<Vec3>,
    /// Outward triangle normals (unit), one per triangle, in world coordinates.
    pub(crate) normals: Vec<Vec3>,
    pub(crate) area: f64,
}

impl SurfaceFace {
    /// The angle around its axis the face's points cover (0 for a plane or a sphere).
    pub(crate) fn coverage(&self) -> f64 {
        match self.surface {
            FaceSurface::Cylinder { origin, axis, radius } => match crate::cmd::measure_shape::cylinder(origin, axis, radius, &self.points) {
                Some(Shape::Round { angle, .. }) => angle,
                _ => 0.0,
            },
            FaceSurface::Cone { apex, axis, .. } => coverage(apex, axis, &self.points),
            FaceSurface::Torus { center, axis, .. } => coverage(center, axis, &self.points),
            FaceSurface::Plane { .. } | FaceSurface::Sphere { .. } => 0.0,
        }
    }

    /// Area-weighted mean of the outward normal's radial component about the axis line through
    /// `origin` along `axis`: positive when the material lies inside (an external surface),
    /// negative when it lies outside (a bore).
    pub(crate) fn radial_side(&self, origin: Vec3, axis: UnitVec3) -> f64 {
        let mut sum = 0.0;
        for (index, normal) in self.normals.iter().enumerate() {
            let base = index * 3;
            let (Some(a), Some(b), Some(c)) = (self.points.get(base), self.points.get(base + 1), self.points.get(base + 2)) else {
                continue;
            };
            let centroid = (*a + *b + *c) * (1.0 / 3.0);
            let d = centroid - origin;
            let radial = d - axis * d.dot(axis);
            let Some(unit) = radial.normalized() else { continue };
            let area = (*b - *a).cross(*c - *a).len() * 0.5;
            if area.is_finite() {
                sum += area * normal.dot(unit);
            }
        }
        sum
    }

    /// The extent of the face along `axis` from `origin`.
    pub(crate) fn extent(&self, origin: Vec3, axis: UnitVec3) -> Option<(f64, f64)> {
        extent(origin, axis, &self.points)
    }
}

/// One round face of a body, as the centre-line code needs it.
#[derive(Clone, Debug)]
struct RoundFace {
    surface: FaceSurface,
    points: Vec<Vec3>,
    area: f64,
}

impl RoundFace {
    /// The angle around its axis the face's points cover (0 for a plane or a sphere).
    fn coverage(&self) -> f64 {
        match self.surface {
            FaceSurface::Cylinder { origin, axis, radius } => match crate::cmd::measure_shape::cylinder(origin, axis, radius, &self.points) {
                Some(Shape::Round { angle, .. }) => angle,
                _ => 0.0,
            },
            FaceSurface::Cone { apex, axis, .. } => coverage(apex, axis, &self.points),
            FaceSurface::Torus { center, axis, .. } => coverage(center, axis, &self.points),
            FaceSurface::Plane { .. } | FaceSurface::Sphere { .. } => 0.0,
        }
    }

    /// The face's line, whatever part of the turn it covers: a cylinder's, cone's or torus's own
    /// axis with the face's extent along it. A sphere has no single axis.
    fn line(&self) -> Option<Axis> {
        match self.surface {
            FaceSurface::Cylinder { origin, axis, radius } => {
                let Some(Shape::Round { center, lo, hi, .. }) = crate::cmd::measure_shape::cylinder(origin, axis, radius, &self.points) else {
                    return None;
                };
                Some(Axis { origin: center, axis, lo, hi, max_radius: radius })
            }
            FaceSurface::Cone { apex, axis, .. } => {
                let (lo, hi) = extent(apex, axis, &self.points)?;
                Some(Axis { origin: apex, axis, lo, hi, max_radius: max_radial(apex, axis, &self.points) })
            }
            FaceSurface::Torus { center, axis, .. } => {
                let (lo, hi) = extent(center, axis, &self.points)?;
                Some(Axis { origin: center, axis, lo, hi, max_radius: max_radial(center, axis, &self.points) })
            }
            FaceSurface::Plane { .. } | FaceSurface::Sphere { .. } => None,
        }
    }

    /// The face's own axis, if it covers at least half a turn around it.
    fn axis(&self) -> Option<Axis> {
        (self.coverage() >= HALF_TURN - HALF_TURN_TOL).then(|| self.line()).flatten()
    }

    /// The point a sphere (which has no axis of its own) is centred on, for the turned-part test.
    fn sphere_centre(&self) -> Option<Vec3> {
        match self.surface {
            FaceSurface::Sphere { center, .. } => Some(center),
            _ => None,
        }
    }
}

/// The extent of `points` along `axis` from `origin`.
fn extent(origin: Vec3, axis: UnitVec3, points: &[Vec3]) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in points {
        let t = (*p - origin).dot(axis);
        if t.is_finite() {
            lo = lo.min(t);
            hi = hi.max(t);
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// The largest distance of `points` from the axis line through `origin`.
fn max_radial(origin: Vec3, axis: UnitVec3, points: &[Vec3]) -> f64 {
    points
        .iter()
        .map(|p| {
            let d = *p - origin;
            (d - axis * d.dot(axis)).len()
        })
        .filter(|r| r.is_finite())
        .fold(0.0, f64::max)
}

/// The distance between the axis lines `(p1, d1)` and `(p2, d2)`, when they are parallel.
fn line_distance(p1: Vec3, d1: UnitVec3, p2: Vec3) -> f64 {
    let w = p2 - p1;
    (w - d1 * w.dot(d1)).len()
}

/// Whether two axis directions are parallel within [`PARALLEL_TOL`] radians.
fn parallel(a: UnitVec3, b: UnitVec3) -> bool {
    a.cross(b).len() <= PARALLEL_TOL
}

/// Move a surface with the rigid placement `m` of a placed body (`identity`: it is not moved).
fn moved(surface: FaceSurface, m: &solidcraft_doc::Mat, identity: bool) -> Option<FaceSurface> {
    if identity {
        return Some(surface);
    }
    let point = |p: Vec3| solidcraft_doc::apply_point(m, p);
    let direction = |d: UnitVec3| UnitVec3::new(solidcraft_doc::apply_vector(m, d.vec()));
    Some(match surface {
        FaceSurface::Plane { normal, point: at } => FaceSurface::Plane { normal: direction(normal)?, point: point(at) },
        FaceSurface::Cylinder { origin, axis, radius } => FaceSurface::Cylinder { origin: point(origin), axis: direction(axis)?, radius },
        FaceSurface::Cone { apex, axis, half_angle } => FaceSurface::Cone { apex: point(apex), axis: direction(axis)?, half_angle },
        FaceSurface::Sphere { center, radius } => FaceSurface::Sphere { center: point(center), radius },
        FaceSurface::Torus { center, axis, major, minor } => FaceSurface::Torus { center: point(center), axis: direction(axis)?, major, minor },
    })
}

/// Every B-rep face of the visible bodies, in world coordinates, with its exact surface, mesh
/// points, outward triangle normals and area. Smooth surface pieces joined by hidden seams share
/// one entry (see [`Mesh::surface_patch`]).
pub(crate) fn surface_faces(s: &Session) -> Vec<SurfaceFace> {
    let st = s.world_state();
    let mut out = Vec::new();
    for body in st.bodies.iter().filter(|b| s.body_visible(&b.name)) {
        if body.body.is_mesh() {
            continue;
        }
        let (mesh, placement) = body.mesh_in_place();
        let identity = solidcraft_doc::is_identity(&placement);
        let face_count = mesh.tri_face.iter().map(|f| *f as usize + 1).max().unwrap_or(0);
        if face_count == 0 {
            continue;
        }
        let mut points: Vec<Vec<Vec3>> = vec![Vec::new(); face_count];
        let mut normals: Vec<Vec<Vec3>> = vec![Vec::new(); face_count];
        let mut areas = vec![0.0_f64; face_count];
        for (triangle, face) in mesh.triangles.iter().zip(&mesh.tri_face) {
            let Some([a, b, c]) = mesh.tri(triangle) else { continue };
            let Some(slot) = points.get_mut(*face as usize) else { continue };
            let cross = (b - a).cross(c - a);
            let area = cross.len() * 0.5;
            if area.is_finite() {
                if let Some(total) = areas.get_mut(*face as usize) {
                    *total += area;
                }
                slot.extend([a, b, c]);
            }
            if let Some(slot) = normals.get_mut(*face as usize)
                && let Some(unit) = cross.normalized()
            {
                slot.push(unit);
            }
        }
        // A smooth surface is often several B-rep faces: a revolve is cut into three pieces and
        // a drilled hole into two halves. The mesh joins them across its hidden seams, so the
        // patch is one face for coverage, area and material side; taking the pieces alone would
        // never reach half a turn.
        let mut visited = vec![false; face_count];
        for index in 0..face_count {
            if visited.get(index).copied().unwrap_or(true) {
                continue;
            }
            let patch = mesh.surface_patch(index);
            let mut pts: Vec<Vec3> = Vec::new();
            let mut nrm: Vec<Vec3> = Vec::new();
            let mut area = 0.0;
            for face in &patch {
                if let Some(points) = points.get(*face) {
                    pts.extend(points.iter().copied());
                }
                if let Some(normals) = normals.get(*face) {
                    nrm.extend(normals.iter().copied());
                }
                if let Some(piece) = areas.get(*face) {
                    area += piece;
                }
                if let Some(seen) = visited.get_mut(*face) {
                    *seen = true;
                }
            }
            let Some(surface) = solidcraft_kernel::face_surface(&body.body, index) else { continue };
            let Some(surface) = moved(surface, &placement, identity) else { continue };
            // The points are in the definition frame (mesh_in_place), the surface now in world.
            if !identity {
                pts = pts.iter().map(|p| solidcraft_doc::apply_point(&placement, *p)).collect();
                nrm = nrm.iter().filter_map(|n| UnitVec3::new(solidcraft_doc::apply_vector(&placement, *n)).map(|u| u.vec())).collect();
            }
            out.push(SurfaceFace { face: index, surface, points: pts, normals: nrm, area });
        }
    }
    out
}

/// Every round face of the visible bodies, in world coordinates, with its mesh points and area.
fn round_faces(s: &Session) -> Vec<RoundFace> {
    surface_faces(s)
        .into_iter()
        .filter(|f| !matches!(f.surface, FaceSurface::Plane { .. }))
        .map(|f| RoundFace { surface: f.surface, points: f.points, area: f.area })
        .collect()
}

/// The model size (bounding-box diagonal of the visible bodies), the grouping tolerance's basis.
fn model_size(s: &Session) -> f64 {
    let st = s.world_state();
    let mut bounds = Aabb3::EMPTY;
    for body in st.bodies.iter().filter(|b| s.body_visible(&b.name)) {
        bounds = bounds.union(&body.outline_bounds());
    }
    let diagonal = bounds.diagonal();
    if diagonal.is_finite() && diagonal > 0.0 { diagonal } else { 1.0 }
}

/// Group the faces' axes: parallel within [`PARALLEL_TOL`], their lines closer than `tolerance`.
fn group(faces: &[RoundFace], tolerance: f64) -> Vec<Axis> {
    let mut groups: Vec<Axis> = Vec::new();
    for face in faces {
        let Some(axis) = face.axis() else { continue };
        match groups.iter_mut().find(|g| parallel(g.axis, axis.axis) && line_distance(g.origin, g.axis, axis.origin) <= tolerance) {
            Some(group) => {
                let (a, b) = (axis.point_at(axis.lo), axis.point_at(axis.hi));
                for p in [a, b] {
                    let t = (p - group.origin).dot(group.axis);
                    group.lo = group.lo.min(t);
                    group.hi = group.hi.max(t);
                }
                group.max_radius = group.max_radius.max(axis.max_radius);
            }
            None => groups.push(axis),
        }
    }
    groups
}

/// Every axis of a round face covering at least half a turn, grouped by axis line.
///
/// Parallel axes within 1e-6 rad whose lines are closer than 1e-4 of the part size are one axis;
/// its extent is the union of the faces' extents and its radius the largest of theirs.
pub fn axes(s: &Session) -> Vec<Axis> {
    group(&round_faces(s), AXIS_DISTANCE_FRACTION * model_size(s))
}

/// The common axis of the round faces when at least [`TURNED_AREA_SHARE`] of their area lies on
/// it and it is parallel to a world axis: a turned part (shaft, disc, flywheel).
pub fn turned_axis(s: &Session) -> Option<Axis> {
    let faces = round_faces(s);
    let total: f64 = faces.iter().map(|f| f.area).sum();
    if !(total > 0.0) {
        return None;
    }
    let tolerance = AXIS_DISTANCE_FRACTION * model_size(s);
    let mut best: Option<(f64, Axis)> = None;
    for candidate in faces.iter().filter_map(RoundFace::axis) {
        let on_axis: f64 = faces
            .iter()
            .filter(|face| match (face.line(), face.sphere_centre()) {
                (Some(line), _) => parallel(line.axis, candidate.axis) && line_distance(candidate.origin, candidate.axis, line.origin) <= tolerance,
                (None, Some(centre)) => line_distance(candidate.origin, candidate.axis, centre) <= tolerance,
                (None, None) => false,
            })
            .map(|face| face.area)
            .sum();
        if best.as_ref().is_none_or(|(area, _)| on_axis > *area) {
            best = Some((on_axis, candidate));
        }
    }
    let (area, axis) = best?;
    if area < TURNED_AREA_SHARE * total || !world_parallel(axis.axis) {
        return None;
    }
    // The whole group's extent, not only the candidate face's.
    group(&faces, tolerance)
        .into_iter()
        .find(|g| parallel(g.axis, axis.axis) && line_distance(g.origin, g.axis, axis.origin) <= tolerance)
        .or(Some(axis))
}

/// Whether `axis` is parallel to one of the world axes within 1e-6 rad.
pub fn world_parallel(axis: UnitVec3) -> bool {
    [Vec3::X, Vec3::Y, Vec3::Z].iter().any(|w| axis.cross(*w).len() <= PARALLEL_TOL)
}

/// The positive world axis `axis` is parallel to ([`world_parallel`]).
pub fn world_axis(axis: UnitVec3) -> Vec3 {
    [Vec3::X, Vec3::Y, Vec3::Z].into_iter().find(|w| axis.cross(*w).len() <= PARALLEL_TOL).unwrap_or_else(|| {
        if axis.dot(Vec3::X).abs() >= axis.dot(Vec3::Y).abs() && axis.dot(Vec3::X).abs() >= axis.dot(Vec3::Z).abs() {
            Vec3::X
        } else if axis.dot(Vec3::Y).abs() >= axis.dot(Vec3::Z).abs() {
            Vec3::Y
        } else {
            Vec3::Z
        }
    })
}

/// The centre lines of the axes in one standard view, in the view's 2D millimetres.
pub fn view_centres(axes: &[Axis], view: StandardView) -> Vec<ViewCentre> {
    let camera = standard_camera(view);
    let (right, up, back) = camera.basis();
    let project = |p: Vec3| Vec2::new(p.dot(right), p.dot(up));
    let (sin1, cos1) = ONE_DEG.sin_cos();
    let mut out = Vec::new();
    for axis in axes {
        let along = axis.axis.dot(back);
        if along.abs() <= sin1 {
            out.push(ViewCentre::Line { a: project(axis.point_at(axis.lo)), b: project(axis.point_at(axis.hi)) });
        } else if along.abs() >= cos1 {
            out.push(ViewCentre::Cross { c: project(axis.origin), radius: axis.max_radius });
        }
    }
    out
}

#[cfg(test)]
#[path = "centre_tests.rs"]
mod tests;
