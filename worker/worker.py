# InterCraft geometry worker.
# Python 3.11, build123d (OpenCascade), numpy. Versions are pinned in requirements.txt.
#
# Talks JSON lines on stdin/stdout. Each feature is a build123d snippet that
# reads and reassigns `part`, with its parameters in `p`. All units are mm.

import hashlib
import json
import math
import os
import struct
import sys
import time
import traceback

# Keep the protocol on its own handle. Anything a snippet or OpenCascade prints
# goes to stderr, so it can't corrupt the JSON stream.
_proto = os.fdopen(os.dup(1), "w", encoding="utf-8", newline="\n")
os.dup2(2, 1)
sys.stdout = sys.stderr

import numpy as np
import build123d

import parts_library
import print_check
import sandbox

LINEAR_TOL = 0.05  # mm
ANGULAR_TOL = 0.3  # rad

_base_ns = sandbox.build_namespace()

# chained hash -> the shape in the part's own frame, which the next feature builds on
_local = {}
# shown key (chained hash plus placement) -> (shape as placed, report, mesh path)
_cache = {}


def send(msg):
    _proto.write(json.dumps(msg) + "\n")
    _proto.flush()


# Bump when the mesh file format changes, so old cache files are rebuilt.
MESH_FORMAT = "ICM2"
STEP_EXTENSIONS = (".step", ".stp")


def feature_hash(prev, feature):
    h = hashlib.sha1()
    h.update(prev.encode())
    h.update(feature.get("code", "").encode())
    h.update(json.dumps(feature.get("params", {}), sort_keys=True).encode())
    h.update(f"{LINEAR_TOL}/{ANGULAR_TOL}/{MESH_FORMAT}".encode())
    if feature.get("import"):
        # An imported file's content is part of what shaped the result.
        with open(feature["import"], "rb") as f:
            h.update(hashlib.sha1(f.read()).digest())
    return h.hexdigest()


def write_mesh(shape, path, tick=lambda: None):
    # Mesh files are named by a hash of everything that shaped them, so an existing
    # file is already right. The viewer and an MCP server each run a worker and can
    # build the same step at the same moment.
    if os.path.exists(path):
        return
    verts, tris = shape.tessellate(LINEAR_TOL, ANGULAR_TOL)
    tick()
    v = np.array([(q.X, q.Y, q.Z) for q in verts], dtype=np.float32).reshape(-1, 3)
    t = np.array(tris, dtype=np.uint32).reshape(-1, 3)

    # Area-weighted vertex normals. Vertices aren't shared between faces,
    # so real edges stay sharp while curved faces shade smoothly.
    n = np.zeros_like(v)
    if len(t):
        fn = np.cross(v[t[:, 1]] - v[t[:, 0]], v[t[:, 2]] - v[t[:, 0]])
        for i in range(3):
            np.add.at(n, t[:, i], fn)
        length = np.linalg.norm(n, axis=1, keepdims=True)
        n = n / np.where(length > 0, length, 1)

    edges = []
    for e in shape.edges():
        edges.append(edge_points(e))
        if len(edges) % 2000 == 0:
            tick()
    corners = [tuple(vx) for vx in shape.vertices()]

    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "wb") as f:
        f.write(MESH_FORMAT.encode())
        f.write(struct.pack("<II", len(v), len(t)))
        f.write(v.astype("<f4").tobytes())
        f.write(n.astype("<f4").tobytes())
        f.write(t.astype("<u4").tobytes())
        # Edges as polylines, in shape.edges() order so an index means the same edge
        # to the viewer and to the measure command. Then the corner points.
        f.write(struct.pack("<I", len(edges)))
        for points in edges:
            f.write(struct.pack("<I", len(points)))
            f.write(np.asarray(points, dtype="<f4").tobytes())
        f.write(struct.pack("<I", len(corners)))
        f.write(np.asarray(corners, dtype="<f4").reshape(-1, 3).tobytes())
    try:
        os.replace(tmp, path)
    except OSError:
        # Windows refuses to replace a file another process has open. If another
        # worker finished the same mesh first, that file is identical, so use it.
        os.remove(tmp)
        if not os.path.exists(path):
            raise


def edge_points(edge):
    """A polyline that follows the edge closely enough to draw and pick."""
    if edge.geom_type == build123d.GeomType.LINE:
        count = 2
    else:
        count = max(8, min(96, int(edge.length / 0.5) + 2))
    return [tuple(edge.position_at(i / (count - 1))) for i in range(count)]


def make_report(shape, volume_before):
    bb = shape.bounding_box()
    return {
        "volume": float(shape.volume),
        "volume_before": volume_before,
        "bbox_min": [bb.min.X, bb.min.Y, bb.min.Z],
        "bbox_max": [bb.max.X, bb.max.Y, bb.max.Z],
        "faces": len(shape.faces()),
        "edges": len(shape.edges()),
        "valid": bool(shape.is_valid),
    }


def import_step(path):
    """The base shape of a part made from an imported STEP file. The app has already
    checked the path is inside the project's imports folder."""
    if not path.lower().endswith(STEP_EXTENSIONS):
        raise ValueError("only .step and .stp files can be imported")
    shape = build123d.import_step(path)
    if shape is None or not shape.solids():
        raise ValueError("the STEP file has no solid bodies")
    return shape


# The last STEP file read for splitting: (path, size, mtime) and its bodies.
_step_bodies = (None, [])


def step_bodies(path, tick=lambda: None):
    """Every body in a STEP file as (name, shape), each placed where it sits in the
    whole assembly. Bodies inside sub-assemblies are found too."""
    global _step_bodies
    if not path.lower().endswith(STEP_EXTENSIONS):
        raise ValueError("only .step and .stp files can be imported")
    stat = os.stat(path)
    key = (path, stat.st_size, stat.st_mtime)
    if _step_bodies[0] == key:
        return _step_bodies[1]
    shape = build123d.import_step(path)
    tick()
    bodies = []

    def walk(node, above):
        here = above * node.location
        if node.children:
            for child in node.children:
                walk(child, here)
            return
        if not node.solids():
            return  # a sketch or a surface, nothing to build on
        # A fresh copy with no parent; exporting a node still in the tree fails.
        alone = type(node)(node.wrapped.Moved(above.wrapped))
        bodies.append((node.label or "", alone))

    if shape is not None:
        if not shape.children and len(shape.solids()) > 1:
            # Several bodies with no assembly structure: one per solid.
            for i, solid in enumerate(shape.solids()):
                bodies.append((f"{shape.label or 'Body'}_{i + 1}", solid))
        else:
            walk(shape, build123d.Location())
    _step_bodies = (key, bodies)
    return bodies


def step_contents(req):
    def tick():
        send({"id": req["id"], "event": "progress"})

    bodies = step_bodies(req["step"], tick)
    names = [{"name": name, "solids": len(shape.solids())} for name, shape in bodies]
    send({"id": req["id"], "event": "info", "data": {"bodies": names}})


def split_step(req):
    """Write each body of a STEP file to its own file, in the order step_contents gave."""
    bodies = step_bodies(req["step"])
    files = req["files"]
    if len(files) != len(bodies):
        raise ValueError(f"{len(files)} file names for {len(bodies)} bodies")
    for (name, shape), path in zip(bodies, files):
        if not str(path).lower().endswith(STEP_EXTENSIONS):
            raise ValueError("bodies are written as .step files")
        shape.label = name
        tmp = f"{path}.{os.getpid()}.tmp.step"
        build123d.export_step(shape, tmp)
        os.replace(tmp, path)
        send({"id": req["id"], "event": "progress"})
    send({"id": req["id"], "event": "info", "data": {"written": len(files)}})


def shown_key(key, placement):
    """Cache key of a step as it is shown: its chain hash, plus where the part sits."""
    if not placement or not any(placement):
        return key
    return hashlib.sha1((key + json.dumps([float(v) for v in placement])).encode()).hexdigest()


def place(shape, placement):
    """Move a shape from the part's own frame to its place in the assembly."""
    if not placement or not any(placement):
        return shape
    x, y, z, rx, ry, rz = (float(v) for v in placement)
    return shape.moved(build123d.Location((x, y, z), (rx, ry, rz)))


def run_feature(prev_shape, feature):
    if feature.get("import"):
        return import_step(feature["import"])
    shape = sandbox.run(feature["code"], prev_shape, feature.get("params", {}), _base_ns, feature.get("id", "feature"))
    if shape is None:
        raise ValueError("snippet finished without assigning `part`")
    if not isinstance(shape, build123d.Shape):
        raise ValueError(f"`part` must be a build123d shape, not {type(shape).__name__}")
    return shape


def rebuild(req, emit=True):
    """Build the feature chain. Returns (final shape as placed, error text)."""
    rid = req["id"]
    part = req["part"]
    placement = req.get("placement")
    shown = None
    cache_dir = req["cache_dir"]
    os.makedirs(cache_dir, exist_ok=True)

    prev_hash = part
    shape = None
    volume = None

    def tick():
        # A big import takes several slow stages; each one shows the app it's alive.
        if emit:
            send({"id": rid, "event": "progress"})

    for index, feature in enumerate(req["features"]):
        start = time.perf_counter()
        key = feature_hash(prev_hash, feature)
        event = {
            "id": rid,
            "event": "feature",
            "part": part,
            "feature_id": feature["id"],
            "index": index,
        }
        try:
            # Moving a part re-places its cached steps without running the snippets again.
            placed = shown_key(key, placement)
            cached = placed in _cache and key in _local
            if cached:
                shape = _local[key]
                shown, report, mesh_path = _cache[placed]
            else:
                if key in _local:
                    shape = _local[key]
                else:
                    shape = run_feature(shape, feature)
                    _local[key] = shape
                    tick()
                shown = place(shape, placement)
                report = make_report(shown, volume)
                tick()
                mesh_path = os.path.join(cache_dir, placed + ".icm")
                write_mesh(shown, mesh_path, tick)
                _cache[placed] = (shown, report, mesh_path)
            volume = report["volume"]
            event.update(ok=True, cached=cached, mesh=mesh_path, report=report)
        except Exception:
            error = traceback.format_exc(limit=4)
            event.update(ok=False, error=error)
            event["ms"] = (time.perf_counter() - start) * 1000
            if emit:
                send(event)
                send({"id": rid, "event": "done", "part": part})
            return None, f"{feature['id']} failed: {error}"
        event["ms"] = (time.perf_counter() - start) * 1000
        if emit:
            send(event)
        prev_hash = key

    if emit:
        send({"id": rid, "event": "done", "part": part})
    return shown, None


def export(req):
    shape, error = rebuild(req, emit=False)
    if shape is None:
        raise ValueError(error or "the part has no features to export")
    path = req["path"]
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    if req["format"] == "step":
        build123d.export_step(shape, path)
    elif req["format"] == "stl":
        build123d.export_stl(shape, path, tolerance=0.01, angular_tolerance=0.1)
    else:
        raise ValueError(f"unknown export format {req['format']}")
    send({"id": req["id"], "event": "exported", "path": path})


# ---------- inspecting and measuring built shapes ----------

def cached_shape(mesh_key):
    entry = _cache.get(mesh_key)
    if entry is None:
        raise ValueError("that shape is no longer cached; rebuild the part and try again")
    return entry[0]


def vec(v, digits=4):
    return [round(float(v.X), digits), round(float(v.Y), digits), round(float(v.Z), digits)]


def entity(selection):
    """The edge, vertex, face or free point a measurement refers to."""
    kind = selection.get("kind")
    if kind == "point":
        return build123d.Vertex(*selection["point"])
    shape = cached_shape(selection["mesh"])
    items = {"edge": shape.edges, "vertex": shape.vertices, "face": shape.faces}.get(kind)
    if items is None:
        raise ValueError(f"unknown kind {kind!r}; use edge, vertex, face or point")
    found = items()
    index = int(selection["index"])
    if not 0 <= index < len(found):
        raise ValueError(f"{kind} {index} doesn't exist; the shape has {len(found)}")
    return found[index]


def describe(item):
    """Facts about one edge, face or vertex, for a single-click measurement."""
    if isinstance(item, build123d.Vertex):
        return {"kind": "vertex", "position": [round(c, 4) for c in tuple(item)]}
    if isinstance(item, build123d.Edge):
        info = {"kind": "edge", "type": item.geom_type.name.lower(), "length": round(item.length, 4)}
        if item.geom_type == build123d.GeomType.CIRCLE:
            info["radius"] = round(item.radius, 4)
            info["diameter"] = round(item.radius * 2, 4)
            info["centre"] = vec(item.arc_center)
        elif item.geom_type == build123d.GeomType.LINE:
            info["direction"] = vec((item.position_at(1) - item.position_at(0)).normalized())
        return info
    if isinstance(item, build123d.Face):
        return face_info(item)
    return {"kind": type(item).__name__}


def face_info(face, index=None):
    info = {"kind": "face", "type": face.geom_type.name.lower(), "area": round(face.area, 3), "centre": vec(face.center())}
    if index is not None:
        info["index"] = index
    if face.geom_type == build123d.GeomType.PLANE:
        info["normal"] = vec(face.normal_at())
    elif face.geom_type in (build123d.GeomType.CYLINDER, build123d.GeomType.CONE, build123d.GeomType.SPHERE):
        try:
            info["radius"] = round(face.radius, 4)
            info["diameter"] = round(face.radius * 2, 4)
        except Exception:
            pass
        if face.geom_type == build123d.GeomType.CYLINDER:
            try:
                axis = face.axis_of_rotation
                info["axis_point"] = vec(axis.position)
                info["axis_direction"] = vec(axis.direction)
            except Exception:
                pass
    return info


def measure(req):
    a = entity(req["a"])
    result = {"a": describe(a)}
    if req.get("b"):
        b = entity(req["b"])
        distance, pa, pb = a.distance_to_with_closest_points(b)
        result.update(
            b=describe(b),
            distance=round(distance, 4),
            a_point=vec(pa),
            b_point=vec(pb),
            delta=[round(float(pb.X - pa.X), 4), round(float(pb.Y - pa.Y), 4), round(float(pb.Z - pa.Z), 4)],
        )
        # Angle between two straight edges, or two flat faces.
        da, db = result["a"].get("direction") or result["a"].get("normal"), result["b"].get("direction") or result["b"].get("normal")
        if da and db:
            dot = abs(sum(x * y for x, y in zip(da, db)))
            result["angle_deg"] = round(math.degrees(math.acos(min(1.0, dot))), 3)
    send({"id": req["id"], "event": "measured", "data": result})


def inspect(req):
    """A compact description of a built shape, so a model can work with it precisely."""
    shape = cached_shape(req["mesh"])
    bb = shape.bounding_box()
    faces = shape.faces()
    infos = [face_info(f, i) for i, f in enumerate(faces)]
    holes = [f for f in infos if f["type"] == "cylinder"]
    circles = {}
    for i, e in enumerate(shape.edges()):
        if e.geom_type == build123d.GeomType.CIRCLE:
            circles.setdefault(round(e.radius * 2, 3), []).append(i)
    limit = int(req.get("max_faces", 60))
    largest = sorted(infos, key=lambda f: -f["area"])[:limit]
    send({"id": req["id"], "event": "inspected", "data": {
        "volume": round(shape.volume, 3),
        "bbox_min": vec(bb.min), "bbox_max": vec(bb.max),
        "solids": len(shape.solids()), "faces": len(faces), "edges": len(shape.edges()),
        "vertices": len(shape.vertices()),
        "cylindrical_faces": holes[:limit],
        "circle_edge_diameters": {str(d): ids for d, ids in sorted(circles.items())},
        "largest_faces": largest,
        "note": f"Faces listed: {len(largest)} of {len(faces)}, largest first. Indices match measure.",
    }})


def fit(req):
    """Check every pair of parts: where they overlap and by how much, or the gap between them."""
    items = [(p["name"], p["mesh"], cached_shape(p["mesh"])) for p in req["parts"]]
    pairs = []
    for i in range(len(items)):
        for j in range(i + 1, len(items)):
            (name_a, key_a, a), (name_b, key_b, b) = items[i], items[j]
            pair = {"a": name_a, "b": name_b}
            common = a.intersect(b)
            solids = [s for item in (common or []) for s in item.solids()]
            volume = sum(s.volume for s in solids)
            if volume > 1e-6:
                overlap = build123d.Compound(solids)
                bb = overlap.bounding_box()
                name = hashlib.sha1(f"overlap/{key_a}/{key_b}".encode()).hexdigest()
                path = os.path.join(req["cache_dir"], name + ".icm")
                write_mesh(overlap, path)
                pair.update(status="overlap", overlap_volume=round(volume, 3), bbox_min=vec(bb.min), bbox_max=vec(bb.max), mesh=path)
            else:
                distance, pa, pb = a.distance_to_with_closest_points(b)
                pair.update(
                    status="touching" if distance < 1e-6 else "clear",
                    clearance=round(distance, 4),
                    a_point=vec(pa),
                    b_point=vec(pb),
                )
            pairs.append(pair)
    send({"id": req["id"], "event": "fitted", "data": {"pairs": pairs}})


def boxes_meet(a, b):
    """Whether two bounding boxes share any space, one inside the other included."""
    return all(
        getattr(a.min, axis) <= getattr(b.max, axis) and getattr(b.min, axis) <= getattr(a.max, axis)
        for axis in "XYZ"
    )


def sweep(req):
    """Move parts through a joint's range and check the moving ones against the rest at
    each step. Each moving part brings one extra move per step, from where its shape
    was built to where the joint puts it."""
    moving, still = {}, {}
    for p in req["parts"]:
        shape = cached_shape(p["mesh"])
        if p.get("moves"):
            moving[p["name"]] = (shape, p["moves"])
        else:
            still[p["name"]] = shape
    boxes = {name: shape.bounding_box() for name, shape in still.items()}
    steps = []
    for i, value in enumerate(req["values"]):
        clashes, gaps = [], []
        for a, (shape, moves) in moving.items():
            placed = place(shape, moves[i])
            box = placed.bounding_box()
            for b, other in still.items():
                # Boxes apart means no overlap, so the boolean can be skipped.
                if boxes_meet(box, boxes[b]):
                    common = placed.intersect(other)
                    volume = sum(s.volume for item in (common or []) for s in item.solids())
                    if volume > 1e-6:
                        clashes.append({"a": a, "b": b, "overlap_volume": round(volume, 3)})
                        continue
                # Every pair, so a part resting on another doesn't hide a tight gap elsewhere.
                gaps.append({"a": a, "b": b, "clearance": round(placed.distance_to(other), 4)})
        steps.append({"at": value, "clashes": clashes, "gaps": gaps})
        send({"id": req["id"], "event": "progress"})
    send({"id": req["id"], "event": "info", "data": {"steps": steps}})


def main():
    send({"event": "ready", "build123d": build123d.__version__})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = None
        try:
            req = json.loads(line)
            if req.get("cmd") == "rebuild":
                rebuild(req)
            elif req.get("cmd") == "export":
                export(req)
            elif req.get("cmd") == "measure":
                measure(req)
            elif req.get("cmd") == "inspect":
                inspect(req)
            elif req.get("cmd") == "fit":
                fit(req)
            elif req.get("cmd") == "sweep":
                sweep(req)
            elif req.get("cmd") == "print_check":
                shape = cached_shape(req["mesh"])
                min_wall = float(req.get("min_wall") or 1.2)
                up = req.get("up") or [0, 0, 1]
                send({"id": req["id"], "event": "info", "data": print_check.check(shape, min_wall, up)})
            elif req.get("cmd") == "step_contents":
                step_contents(req)
            elif req.get("cmd") == "split_step":
                split_step(req)
            elif req.get("cmd") == "fastener_info":
                send({"id": req["id"], "event": "info", "data": parts_library.fastener_info(str(req["size"]))})
            else:
                send({"id": req.get("id"), "event": "error", "error": f"unknown cmd {req.get('cmd')}"})
        except Exception as e:
            request_id = req.get("id") if isinstance(req, dict) else None
            detail = str(e) if isinstance(e, ValueError) else traceback.format_exc(limit=4)
            send({"id": request_id, "event": "error", "error": detail})


if __name__ == "__main__":
    main()
