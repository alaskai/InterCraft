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

LINEAR_TOL = 0.05  # mm
ANGULAR_TOL = 0.3  # rad

_base_ns = {"math": math}
exec("from build123d import *", _base_ns)

# chained hash -> (shape, report, mesh path)
_cache = {}


def send(msg):
    _proto.write(json.dumps(msg) + "\n")
    _proto.flush()


def feature_hash(prev, feature):
    h = hashlib.sha1()
    h.update(prev.encode())
    h.update(feature["code"].encode())
    h.update(json.dumps(feature.get("params", {}), sort_keys=True).encode())
    h.update(f"{LINEAR_TOL}/{ANGULAR_TOL}".encode())
    return h.hexdigest()


def write_mesh(shape, path):
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

    tmp = path + ".tmp"
    with open(tmp, "wb") as f:
        f.write(b"ICM1")
        f.write(struct.pack("<II", len(v), len(t)))
        f.write(v.astype("<f4").tobytes())
        f.write(n.astype("<f4").tobytes())
        f.write(t.astype("<u4").tobytes())
    os.replace(tmp, path)


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


def run_feature(prev_shape, feature):
    ns = dict(_base_ns)
    ns["part"] = prev_shape
    ns["p"] = feature.get("params", {})
    exec(compile(feature["code"], feature.get("id", "feature"), "exec"), ns)
    shape = ns.get("part")
    if shape is None:
        raise ValueError("snippet finished without assigning `part`")
    return shape


def rebuild(req):
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
            event.update(ok=False, error=traceback.format_exc(limit=4))
            event["ms"] = (time.perf_counter() - start) * 1000
            send(event)
            break
        event["ms"] = (time.perf_counter() - start) * 1000
        send(event)
        prev_hash = key

    send({"id": rid, "event": "done", "part": part})


def main():
    send({"event": "ready", "build123d": build123d.__version__})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            if req.get("cmd") == "rebuild":
                rebuild(req)
            else:
                send({"id": req.get("id"), "event": "error", "error": f"unknown cmd {req.get('cmd')}"})
        except Exception:
            send({"event": "error", "error": traceback.format_exc(limit=4)})


if __name__ == "__main__":
    main()
