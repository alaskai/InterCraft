# Triangles for the viewer. build123d's own tessellate stops at the first face
# OpenCascade couldn't mesh, which happens with some faces converted from IGES, and
# then the whole part is lost. Here such faces are left out and counted instead.

import numpy as np
from OCP.BRep import BRep_Tool
from OCP.TopAbs import TopAbs_Orientation
from OCP.TopLoc import TopLoc_Location


def triangles(shape, tolerance, angular_tolerance):
    """(vertices, triangles, faces left out). Vertices aren't shared between faces."""
    shape.mesh(tolerance, angular_tolerance)
    return collect(shape)


def collect(shape):
    """The mesh already on the shape's faces, as numpy arrays."""
    verts, tris, skipped, offset = [], [], 0, 0
    for face in shape.faces():
        loc = TopLoc_Location()
        poly = BRep_Tool.Triangulation_s(face.wrapped, loc)
        if poly is None or poly.NbNodes() == 0:
            skipped += 1
            continue
        trsf = loc.Transformation()
        nodes = [poly.Node(i).Transformed(trsf) for i in range(1, poly.NbNodes() + 1)]
        verts.extend((p.X(), p.Y(), p.Z()) for p in nodes)
        corners = (poly.Triangle(i) for i in range(1, poly.NbTriangles() + 1))
        face_tris = np.array([(c.Value(1), c.Value(2), c.Value(3)) for c in corners], dtype=np.int64).reshape(-1, 3)
        if face.wrapped.Orientation() == TopAbs_Orientation.TopAbs_REVERSED:
            face_tris = face_tris[:, [0, 2, 1]]
        tris.append(face_tris - 1 + offset)
        offset += len(nodes)
    v = np.array(verts, dtype=np.float32).reshape(-1, 3)
    t = np.concatenate(tris).astype(np.uint32) if tris else np.zeros((0, 3), dtype=np.uint32)
    return v, t, skipped
