# Checks a 3D printer cares about, on the real shape: walls too thin to print,
# holes too small to come out round, and overhangs steep enough to need support.
# Each problem comes with a position, so the viewer can point at it.

import math

import numpy as np

# Below this angle from horizontal a downward-facing surface needs support.
OVERHANG_DEGREES = 45.0
SMALL_HOLE = 2.0  # mm
PLA_DENSITY = 1.24  # g/cm³


def _triangles(shape, tolerance):
    """Every face's triangles, with the index of the face each belongs to."""
    points, faces = [], []
    for index, face in enumerate(shape.faces()):
        verts, tris = face.tessellate(tolerance, 0.3)
        if not tris:
            continue
        v = np.array([(q.X, q.Y, q.Z) for q in verts], dtype=float)
        points.append(v[np.array(tris)])
        faces.extend([index] * len(tris))
    if not points:
        return np.zeros((0, 3, 3)), np.zeros(0, dtype=int)
    return np.concatenate(points), np.array(faces)


def _hits(origins, direction_vectors, tris):
    """Distance from each origin along its direction to the nearest triangle (inf if
    none), and which triangle that is."""
    a, b, c = tris[:, 0], tris[:, 1], tris[:, 2]
    e1, e2 = b - a, c - a
    best = np.full(len(origins), np.inf)
    which = np.zeros(len(origins), dtype=int)
    # In chunks, so a large part doesn't need a huge temporary array.
    for start in range(0, len(origins), 64):
        o = origins[start:start + 64, None, :]
        d = direction_vectors[start:start + 64, None, :]
        p = np.cross(d, e2[None])
        det = np.einsum("ijk,jk->ij", p, e1)
        ok = np.abs(det) > 1e-12
        inv = np.where(ok, 1.0 / np.where(ok, det, 1.0), 0.0)
        s = o - a[None]
        u = np.einsum("ijk,ijk->ij", s, p) * inv
        q = np.cross(s, e1[None])
        v = np.einsum("ijk,ijk->ij", d, q) * inv
        t = np.einsum("jk,ijk->ij", e2, q) * inv
        hit = ok & (u >= 0) & (v >= 0) & (u + v <= 1) & (t > 1e-4)
        t = np.where(hit, t, np.inf)
        best[start:start + 64] = t.min(axis=1)
        which[start:start + 64] = t.argmin(axis=1)
    return best, which


def _round(v):
    return [round(float(x), 2) for x in v]


def check(shape, min_wall=1.2, up=(0.0, 0.0, 1.0), samples=600):
    up = np.array(up, dtype=float)
    up = up / (np.linalg.norm(up) or 1.0)
    tris, face_of = _triangles(shape, 0.1)
    problems = []
    if len(tris) == 0:
        return {"ok": False, "problems": [], "note": "The part has no surface to check."}

    edges1, edges2 = tris[:, 1] - tris[:, 0], tris[:, 2] - tris[:, 0]
    cross = np.cross(edges1, edges2)
    area = np.linalg.norm(cross, axis=1) / 2
    normal = cross / np.where(area[:, None] > 0, 2 * area[:, None], 1)
    centre = tris.mean(axis=1)

    # Thin walls: from points spread over the surface (more on bigger triangles), look
    # straight into the material and measure how far it is to the other side.
    rng = np.random.default_rng(1)
    weights = area / area.sum()
    pick = rng.choice(len(tris), size=min(samples, len(tris) * 4), p=weights)
    bary = rng.dirichlet([1, 1, 1], size=len(pick))
    points = np.einsum("ij,ijk->ik", bary, tris[pick])
    inward = -normal[pick]
    thickness, hit = _hits(points + inward * 1e-3, inward, tris)
    thin = np.isfinite(thickness) & (thickness < min_wall)
    # A wall is the pair of faces either side of it, seen from both. Keep the
    # thinnest spot of each pair, so each thin wall is reported once.
    seen = {}
    for i in np.flatnonzero(thin):
        wall = frozenset((int(face_of[pick[i]]), int(face_of[hit[i]])))
        if wall not in seen or thickness[i] < thickness[seen[wall]]:
            seen[wall] = i
    for i in sorted(seen.values(), key=lambda i: thickness[i])[:8]:
        problems.append({
            "kind": "thin wall",
            "at": _round(points[i]),
            "thickness": round(float(thickness[i]), 2),
            "message": f"Wall only {thickness[i]:.2f} mm thick (aim for at least {min_wall:g} mm).",
        })

    # Small holes: round faces whose diameter is too small to print cleanly.
    for index, face in enumerate(shape.faces()):
        if face.geom_type.name != "CYLINDER":
            continue
        try:
            diameter = face.radius * 2
        except Exception:
            continue
        if diameter < SMALL_HOLE:
            problems.append({
                "kind": "small hole",
                "at": _round(face.center()),
                "diameter": round(diameter, 2),
                "message": f"Round feature {diameter:.2f} mm across. Under {SMALL_HOLE:g} mm it prints rough or closes up; drill it out after printing, or make it bigger.",
            })

    # Overhangs: surfaces facing down more steeply than the limit, not resting on the bed.
    height = centre @ up
    bed = height.min()
    facing_down = normal @ up < -math.sin(math.radians(OVERHANG_DEGREES))
    overhang = facing_down & (height > bed + 0.05)
    overhang_area = float(area[overhang].sum())
    for face in np.unique(face_of[overhang]):
        mask = overhang & (face_of == face)
        face_area = float(area[mask].sum())
        if face_area < 1.0:
            continue
        spot = (centre[mask] * area[mask, None]).sum(axis=0) / face_area
        flat = bool(np.all(normal[mask] @ up < -0.99))
        message = f"{face_area:.0f} mm² faces down steeper than {OVERHANG_DEGREES:g}°, so it needs support or a different print orientation."
        if flat:
            message += " If it is a short flat span between two walls (a bridge), it usually prints without support."
        problems.append({"kind": "overhang", "at": _round(spot), "area": round(face_area, 1), "flat": flat, "message": message})

    volume = float(shape.volume)
    surface = float(area.sum())
    # A rough print weight: the shell at full density plus 20 % infill inside it.
    shell = min(volume, surface * 0.8)
    grams = (shell + (volume - shell) * 0.2) * PLA_DENSITY / 1000
    return {
        "ok": not problems,
        "problems": problems,
        "overhang_area": round(overhang_area, 1),
        "material": {"solid_grams": round(volume * PLA_DENSITY / 1000, 1), "estimated_grams": round(grams, 1)},
        "settings": {"min_wall": min_wall, "up": _round(up), "overhang_degrees": OVERHANG_DEGREES},
        "note": "Estimated weight assumes PLA with two walls and 20 % infill. Thin walls are checked at sample points across the surface.",
    }
