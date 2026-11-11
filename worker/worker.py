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

import sandbox

LINEAR_TOL = 0.05  # mm
ANGULAR_TOL = 0.3  # rad

_base_ns = sandbox.build_namespace()

# chained hash -> (shape, report, mesh path)
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


def write_mesh(shape, path):
    # Mesh files are named by a hash of everything that shaped them, so an existing
    # file is already right. The viewer and an MCP server each run a worker and can
    # build the same step at the same moment.
    if os.path.exists(path):
        return
    verts, tris = shape.tessellate(LINEAR_TOL, ANGULAR_TOL)
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

    edges = [edge_points(e) for e in shape.edges()]
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
    """Build the feature chain. Returns (final shape, error text)."""
    rid = req["id"]
    part = req["part"]
    cache_dir = req["cache_dir"]
    os.makedirs(cache_dir, exist_ok=True)

    prev_hash = part
    shape = None
    volume = None

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
            cached = key in _cache
            if cached:
                shape, report, mesh_path = _cache[key]
            else:
                shape = run_feature(shape, feature)
                report = make_report(shape, volume)
                mesh_path = os.path.join(cache_dir, key + ".icm")
                write_mesh(shape, mesh_path)
                _cache[key] = (shape, report, mesh_path)
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
    return shape, None


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
            else:
                send({"id": req.get("id"), "event": "error", "error": f"unknown cmd {req.get('cmd')}"})
        except Exception as e:
            request_id = req.get("id") if isinstance(req, dict) else None
            detail = str(e) if isinstance(e, ValueError) else traceback.format_exc(limit=4)
            send({"id": request_id, "event": "error", "error": detail})


if __name__ == "__main__":
    main()
