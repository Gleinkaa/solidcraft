//! Export-only mesh instances. Keep one geometry buffer per unambiguous definition, while
//! retaining the current placed body's edge choices (tangent classification can vary after
//! placement). Never modify the document, fuse parts, or share across separate exports.
use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use solidcraft_doc::{IDENTITY, Mat, ModelBody, mat_inverse, mat_mul};
use solidcraft_geom::{Aabb3, Mesh, Vec3};
use solidcraft_kernel::{Body, ContinuityTolerances, EdgeCircle, KernelError, SeamVerdict, map_with_local_pools};

use crate::drawing::TangentEdges;

use super::{CMD, progress::Stage};
use crate::params::bad;
use crate::{Result, Session};

#[cfg(not(target_arch = "wasm32"))]
pub(super) const WORKERS: usize = 8;
#[cfg(target_arch = "wasm32")]
pub(super) const WORKERS: usize = 1;
const MESH_BYTES: usize = 512 * 1024 * 1024;
const MAX_BODIES: usize = 100_000;

pub(crate) struct Placement {
    pub mesh: usize,
    pub transform: Mat,
    pub visibility: Vec<bool>,
}
pub(crate) struct Scene {
    pub meshes: Vec<Arc<Mesh>>,
    pub placements: Vec<Placement>,
    pub triangles: usize,
    pub bytes: usize,
    /// Per mesh, per edge: the exact circle the edge lies on, in the mesh's own coordinates.
    /// Empty when the caller did not ask for analytic curves.
    pub curves: Vec<Vec<Option<EdgeCircle>>>,
    /// Per mesh, per edge: the edge is a tangent (G1) seam.
    pub tangent: Vec<Vec<bool>>,
    /// A cutting plane the scene was clipped with: `(point, normal)`, the normal pointing towards
    /// the viewer. Edges lying in the plane are the cut boundary, which the drawing emits itself,
    /// so the hidden-line projection must not also draw them as mesh creases.
    pub cut: Option<(Vec3, Vec3)>,
}

struct Group {
    first: usize,
    inverse: Mat,
}
struct Item<'a> {
    body: &'a ModelBody,
    group: usize,
    keep: bool,
    transform: Mat,
}
struct Prepared {
    mesh: Option<Arc<Mesh>>,
    visibility: Vec<bool>,
    triangles: usize,
    circles: Vec<Option<EdgeCircle>>,
    tangent: Vec<bool>,
}

/// The edges whose two faces meet tangentially (G1): ISO 128's tangent edges. A measured seam
/// with a normal angle within half a degree is tangent, whether or not its curvature also
/// matches. Bodies with more edges than the cap keep the display flags' behaviour only.
fn tangent_edges(body: &Body, mesh: &Mesh) -> Vec<bool> {
    const MAX_EDGES_MEASURED: usize = 400;
    let mut flags = vec![false; mesh.edges.len()];
    if body.is_mesh() || mesh.edges.len() > MAX_EDGES_MEASURED {
        return flags;
    }
    let tolerances = ContinuityTolerances { gap_mm: 0.001, angle_deg: 0.5, curvature_per_mm: 0.01 };
    let Ok(report) = solidcraft_kernel::seam_diagnostics(body, &[], 5, tolerances) else { return flags };
    for seam in report.seams {
        if matches!(seam.verdict, SeamVerdict::Smooth | SeamVerdict::CurvatureJump)
            && let Some(flag) = flags.get_mut(seam.edge)
        {
            *flag = true;
        }
    }
    flags
}

/// The exact circle of every edge that is one. Only edges whose display polyline turns (more than
/// two points) are asked, unless the body is small enough to ask every edge: a short arc that
/// happens to need two points is then found too.
fn edge_circles(body: &Body, mesh: &Mesh) -> Vec<Option<EdgeCircle>> {
    const MAX_EDGES_ALL: usize = 64;
    let mut circles = vec![None; mesh.edges.len()];
    if body.is_mesh() {
        return circles;
    }
    let every = mesh.edges.len() <= MAX_EDGES_ALL;
    for (index, edge) in mesh.edges.iter().enumerate() {
        if (every || edge.len() > 2)
            && let Some(slot) = circles.get_mut(index)
        {
            *slot = solidcraft_kernel::edge_circle(body, index);
        }
    }
    circles
}

pub(super) fn claim(counter: &AtomicUsize, amount: usize, limit: usize) -> bool {
    let mut old = counter.load(Ordering::Relaxed);
    loop {
        let Some(next) = old.checked_add(amount).filter(|n| *n <= limit) else { return false };
        match counter.compare_exchange_weak(old, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(current) => old = current,
        }
    }
}

pub(super) fn buffer_bytes(m: &Mesh) -> usize {
    m.positions.capacity() * size_of::<Vec3>()
        + m.normals.capacity() * size_of::<Vec3>()
        + m.triangles.capacity() * size_of::<[u32; 3]>()
        + m.tri_face.capacity() * size_of::<u32>()
        + m.edges.capacity() * size_of::<Vec<Vec3>>()
        + m.edges.iter().map(|e| e.capacity() * size_of::<Vec3>()).sum::<usize>()
        + m.seams.capacity()
        + m.smooth_edges.capacity()
        + m.sharp_edges.capacity()
        + m.edge_faces.capacity() * size_of::<Vec<u32>>()
        + m.edge_faces.iter().map(|e| e.capacity() * size_of::<u32>()).sum::<usize>()
}

fn rigid(m: &Mat) -> bool {
    if m.iter().flatten().any(|v| !v.is_finite()) || m.iter().zip([0., 0., 0., 1.]).any(|(c, w)| (c[3] - w).abs() > 1e-10) {
        return false;
    }
    let axes = [Vec3::new(m[0][0], m[0][1], m[0][2]), Vec3::new(m[1][0], m[1][1], m[1][2]), Vec3::new(m[2][0], m[2][1], m[2][2])];
    let [x, y, z] = axes;
    (x.len2() - 1.).abs() < 1e-9
        && (y.len2() - 1.).abs() < 1e-9
        && (z.len2() - 1.).abs() < 1e-9
        && x.dot(y).abs() < 1e-9
        && x.dot(z).abs() < 1e-9
        && y.dot(z).abs() < 1e-9
        && (x.cross(y).dot(z) - 1.).abs() < 1e-9
}

pub(crate) fn prepare(s: &Session, tolerance: f64, tangent_mode: TangentEdges, curves: bool) -> Result<Scene> {
    let world = s.world_state();
    let state = s.model.state();
    let mut definitions = BTreeMap::new();
    for body in &state.bodies {
        *definitions.entry((body.component, body.definition_name(), body.feature)).or_insert(0usize) += 1;
    }
    let mut by_definition = BTreeMap::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut items = Vec::new();
    for (index, body) in world.bodies.iter().enumerate().filter(|(_, b)| s.body_visible(&b.name)) {
        if items.len() >= MAX_BODIES {
            return Err(bad(CMD, "too many placed bodies to export"));
        }
        let key = (body.component, body.definition_name(), body.feature);
        let unambiguous = definitions.get(&key) == Some(&1);
        let reused = unambiguous
            .then(|| by_definition.get(&key).copied())
            .flatten()
            .and_then(|g: usize| groups.get(g).map(|group| (g, mat_mul(&body.source_transform, &group.inverse))))
            .filter(|(_, m)| rigid(m));
        let (group, transform) = if let Some(reused) = reused {
            reused
        } else {
            let g = groups.len();
            groups.push(Group { first: index, inverse: mat_inverse(&body.source_transform).unwrap_or(IDENTITY) });
            if unambiguous && mat_inverse(&body.source_transform).is_some() {
                by_definition.entry(key).or_insert(g);
            }
            (g, IDENTITY)
        };
        let keep = groups.get(group).is_some_and(|g| g.first == index);
        items.push(Item { body, group, keep, transform });
    }
    let progress = Stage::new("dxf_meshes", items.len());
    let bytes = AtomicUsize::new(0);
    let results = map_with_local_pools(&items, WORKERS, |item| {
        progress.check().map_err(|e| KernelError::Failed(e.into()))?;
        let mut mesh = item.body.body.reference_display_mesh_angular(tolerance, solidcraft_kernel::DISPLAY_ANGLE)?;
        if mesh.triangles.is_empty() {
            return Err(KernelError::Invalid(format!("{} has no exportable surface", item.body.name)));
        }
        let mut visibility: Vec<bool> = (0..mesh.edges.len()).map(|i| mesh.shaded_edge_visible(i)).collect();
        let tangent =
            if item.keep && tangent_mode != TangentEdges::Draw { tangent_edges(&item.body.body, &mesh) } else { vec![false; mesh.edges.len()] };
        if tangent_mode == TangentEdges::Omit {
            for (visible, tangent) in visibility.iter_mut().zip(&tangent) {
                if *tangent {
                    *visible = false;
                }
            }
        }
        let circles = if item.keep && curves { edge_circles(&item.body.body, &mesh) } else { vec![None; mesh.edges.len()] };
        let triangles = mesh.triangles.len();
        let mesh = if item.keep {
            // These are owned export buffers, not the document's cached mesh. Picking and
            // shading data are unused by vector occlusion; edge choices were captured above.
            mesh.normals = Vec::new();
            mesh.tri_face = Vec::new();
            mesh.seams = Vec::new();
            mesh.smooth_edges = Vec::new();
            mesh.sharp_edges = Vec::new();
            mesh.edge_faces = Vec::new();
            if !claim(&bytes, buffer_bytes(&mesh), MESH_BYTES) {
                return Err(KernelError::Invalid("view exceeds export memory budget; hide bodies or increase tolerance".into()));
            }
            Some(Arc::new(mesh))
        } else {
            None
        };
        progress.advance(1).map_err(|e| KernelError::Failed(e.into()))?;
        Ok(Prepared { mesh, visibility, triangles, circles, tangent })
    });
    let mut prepared = results.into_iter().collect::<solidcraft_kernel::Result<Vec<_>>>()?;
    let mut meshes: Vec<Option<Arc<Mesh>>> = vec![None; groups.len()];
    let mut curves: Vec<Vec<Option<EdgeCircle>>> = vec![Vec::new(); groups.len()];
    let mut tangent: Vec<Vec<bool>> = vec![Vec::new(); groups.len()];
    for (item, p) in items.iter().zip(&mut prepared) {
        if let Some(mesh) = p.mesh.take()
            && let Some(slot) = meshes.get_mut(item.group)
        {
            *slot = Some(mesh);
            if let Some(slot) = curves.get_mut(item.group) {
                *slot = std::mem::take(&mut p.circles);
            }
            if let Some(slot) = tangent.get_mut(item.group) {
                *slot = std::mem::take(&mut p.tangent);
            }
        }
    }
    let meshes = meshes.into_iter().map(|m| m.ok_or_else(|| bad(CMD, "missing export geometry"))).collect::<Result<Vec<_>>>()?;
    let mut placements = Vec::with_capacity(items.len());
    let mut triangles = 0usize;
    for (item, p) in items.iter().zip(prepared) {
        if meshes.get(item.group).is_none_or(|m| m.edges.len() != p.visibility.len()) {
            return Err(bad(CMD, "placed edge identity changed"));
        }
        triangles = triangles.saturating_add(p.triangles);
        placements.push(Placement { mesh: item.group, transform: item.transform, visibility: p.visibility });
    }
    Ok(Scene { meshes, placements, triangles, bytes: bytes.load(Ordering::Relaxed), curves, tangent, cut: None })
}

/// The plane–triangle intersections of every visible body, in world millimetres, and the same
/// scene clipped to the half behind the plane (`normal` points towards the viewer, so the kept
/// half is where `(p − point)·normal <= 0`).
///
/// The clipped triangles are baked into world space (their placement transform applied), so the
/// cutting plane needs no per-placement transform and the caller's meshes are never changed. The
/// plane boundary of the clipped triangles is *not* a mesh crease: [`Scene::cut`] tells the
/// hidden-line projection to skip silhouette edges that lie in the plane; the caller draws the
/// cut outline from the returned segments instead.
pub(crate) fn clip_scene(scene: &Scene, point: Vec3, normal: Vec3) -> std::result::Result<(Scene, Vec<[Vec3; 2]>), &'static str> {
    let len = normal.len();
    if !(len.is_finite() && len > 0.0) || !point.is_finite() {
        return Err("the cutting plane is not a finite point and direction");
    }
    let normal = normal * (1.0 / len);
    let scale = scene
        .placements
        .iter()
        .filter_map(|p| scene.meshes.get(p.mesh).map(|m| transformed_bounds(m, &p.transform)))
        .fold(Aabb3::EMPTY, |b, p| b.union(&p))
        .diagonal()
        .max(1.0);
    let tol = scale * 1e-9;
    let key_eps = scale * 1e-7;
    let key = |p: Vec3| [(p.x / key_eps).round() as i64, (p.y / key_eps).round() as i64, (p.z / key_eps).round() as i64];
    let mut cut = BTreeMap::<[[i64; 3]; 2], [Vec3; 2]>::new();
    let mut meshes = Vec::with_capacity(scene.placements.len());
    let mut curves: Vec<Vec<Option<EdgeCircle>>> = Vec::with_capacity(scene.placements.len());
    let mut tangent: Vec<Vec<bool>> = Vec::with_capacity(scene.placements.len());
    let mut placements = Vec::with_capacity(scene.placements.len());
    let mut triangles_total = 0usize;
    for placed in &scene.placements {
        let mesh = scene.meshes.get(placed.mesh).ok_or("missing export geometry")?;
        let world: Vec<Vec3> = mesh.positions.iter().map(|p| solidcraft_doc::apply_point(&placed.transform, *p)).collect();
        let mut positions = Vec::new();
        let mut triangles = Vec::new();
        let push = |p: Vec3, positions: &mut Vec<Vec3>| -> u32 {
            positions.push(p);
            u32::try_from(positions.len().saturating_sub(1)).unwrap_or(u32::MAX)
        };
        let crossing = |a: Vec3, b: Vec3, sa: f64, sb: f64| -> Vec3 {
            let t = if (sa - sb).abs() > 1e-300 { (sa - tol) / (sa - sb) } else { 0.0 };
            let t = t.clamp(0.0, 1.0);
            a + (b - a) * t
        };
        for indices in &mesh.triangles {
            let [a, b, c] = mesh.tri(indices).ok_or("invalid export triangle")?;
            let p = [
                world.get(indices[0] as usize).copied().unwrap_or(a),
                world.get(indices[1] as usize).copied().unwrap_or(b),
                world.get(indices[2] as usize).copied().unwrap_or(c),
            ];
            let side = p.map(|v| (v - point).dot(normal));
            if side.iter().all(|s| !s.is_finite()) {
                return Err("model contains invalid coordinates");
            }
            let inside = |s: f64| s <= tol;
            // Sutherland–Hodgman over the half space; the kept polygon keeps the triangle's
            // winding, so the clipped mesh still faces the same way.
            let mut polygon: Vec<Vec3> = Vec::with_capacity(4);
            for i in 0..3 {
                let (va, vb) = (p[i], p[(i + 1) % 3]);
                let (sa, sb) = (side[i], side[(i + 1) % 3]);
                if inside(sa) {
                    polygon.push(va);
                }
                if inside(sa) != inside(sb) {
                    polygon.push(crossing(va, vb, sa, sb));
                }
            }
            // The two plane crossings are the cut outline's edges through this triangle.
            let crossings: Vec<Vec3> = (0..3)
                .filter(|i| inside(side[*i]) != inside(side[(*i + 1) % 3]))
                .map(|i| crossing(p[i], p[(i + 1) % 3], side[i], side[(i + 1) % 3]))
                .collect();
            if let [a, b] = crossings.as_slice()
                && a.dist(*b) > tol
            {
                let (ka, kb) = (key(*a), key(*b));
                let k = if ka < kb { [ka, kb] } else { [kb, ka] };
                cut.entry(k).or_insert([*a, *b]);
            }
            if polygon.len() < 3 {
                continue;
            }
            let first = push(polygon[0], &mut positions);
            for pair in polygon[1..].windows(2) {
                if let [q, r] = pair {
                    let (iq, ir) = (push(*q, &mut positions), push(*r, &mut positions));
                    triangles.push([first, iq, ir]);
                }
            }
        }
        // B-rep edges: keep only the runs behind the plane. The new endpoints on the plane are
        // the cut outline's contact points, not extra creases.
        let mut edges: Vec<Vec<Vec3>> = Vec::new();
        let mut edge_curves: Vec<Option<EdgeCircle>> = Vec::new();
        let mut edge_tangent: Vec<bool> = Vec::new();
        let placed_curve = |index: usize| {
            scene
                .curves
                .get(placed.mesh)
                .and_then(|curves| curves.get(index))
                .copied()
                .flatten()
                .map(|circle| transform_circle(&circle, &placed.transform))
        };
        let mut push = |run: Vec<Vec3>, index: usize, edges: &mut Vec<Vec<Vec3>>| {
            edges.push(run);
            edge_curves.push(placed_curve(index));
            edge_tangent.push(scene.tangent.get(placed.mesh).and_then(|flags| flags.get(index)).copied().unwrap_or(false));
        };
        for (index, edge) in mesh.edges.iter().enumerate() {
            if !placed.visibility.get(index).copied().unwrap_or(false) {
                continue;
            }
            let points: Vec<Vec3> = edge.iter().map(|p| solidcraft_doc::apply_point(&placed.transform, *p)).collect();
            let mut run: Vec<Vec3> = Vec::new();
            for pair in points.windows(2) {
                let [a, b] = [pair[0], pair[1]];
                let (sa, sb) = ((a - point).dot(normal), (b - point).dot(normal));
                if sa <= tol && sb <= tol {
                    if run.is_empty() {
                        run.push(a);
                    }
                    run.push(b);
                } else if sa <= tol {
                    run.push(crossing(a, b, sa, sb));
                    if run.len() > 1 {
                        push(std::mem::take(&mut run), index, &mut edges);
                    }
                } else if sb <= tol {
                    run.push(crossing(a, b, sa, sb));
                    run.push(b);
                } else if run.len() > 1 {
                    push(std::mem::take(&mut run), index, &mut edges);
                }
            }
            if run.len() > 1 {
                push(run, index, &mut edges);
            }
        }
        let count = triangles.len();
        if count == 0 {
            continue;
        }
        triangles_total = triangles_total.saturating_add(count);
        meshes.push(Arc::new(Mesh { positions, triangles, edges, ..Mesh::default() }));
        curves.push(edge_curves);
        tangent.push(edge_tangent);
        placements.push(Placement { mesh: meshes.len().saturating_sub(1), transform: IDENTITY, visibility: Vec::new() });
    }
    let segments = cut.into_values().collect();
    // `visibility` is per edge and the hidden-line projection asks for it by edge index: the
    // clipped edge list is already filtered, so every remaining edge is drawn.
    for (mesh, placed) in meshes.iter().zip(&mut placements) {
        placed.visibility = vec![true; mesh.edges.len()];
    }
    Ok((Scene { meshes, placements, triangles: triangles_total, bytes: 0, curves, tangent, cut: Some((point, normal)) }, segments))
}

/// A circle of a mesh carried through a placement (a rigid motion: radius and angle unchanged).
fn transform_circle(circle: &EdgeCircle, m: &Mat) -> EdgeCircle {
    let axis = Vec3::new(circle.axis.x, circle.axis.y, circle.axis.z);
    let axis = solidcraft_doc::apply_vector(m, axis).unit().unwrap_or(solidcraft_geom::UnitVec3::Z);
    EdgeCircle {
        center: solidcraft_doc::apply_point(m, circle.center),
        axis,
        radius: circle.radius,
        start: solidcraft_doc::apply_point(m, circle.start),
        end: solidcraft_doc::apply_point(m, circle.end),
        angle: circle.angle,
    }
}

pub(super) fn transformed_bounds(mesh: &Mesh, m: &Mat) -> Aabb3 {
    let b = mesh.bounds();
    let mut out = Aabb3::EMPTY;
    for x in [b.min.x, b.max.x] {
        for y in [b.min.y, b.max.y] {
            for z in [b.min.z, b.max.z] {
                out.add(solidcraft_doc::apply_point(m, Vec3::new(x, y, z)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_proper_rigid_placements_share_geometry() {
        assert!(rigid(&solidcraft_doc::rigid(Vec3::new(1., 2., 3.), Vec3::ZERO, Vec3::Z, 0.7)));
        let mut m = IDENTITY;
        m[0][0] = -1.;
        assert!(!rigid(&m));
        m[0][0] = 2.;
        assert!(!rigid(&m));
        m[0][0] = f64::NAN;
        assert!(!rigid(&m));
    }
}
