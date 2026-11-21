# Clearance between two parts, quick enough for a whole assembly. The exact distance
# between freeform faces (a leadscrew thread, a spring, gear teeth) can take many
# minutes, so the meshes find the faces that come close first. Planes, cylinders and
# other simple faces are then measured exactly; freeform ones are measured on the
# mesh, which is close but not exact, and the answer says by how much.

from build123d import Vector
from OCP.BRep import BRep_Builder, BRep_Tool
from OCP.BRepAdaptor import BRepAdaptor_Surface
from OCP.BRepClass3d import BRepClass3d_SolidClassifier
from OCP.BRepExtrema import BRepExtrema_DistShapeShape, BRepExtrema_ShapeProximity
from OCP.GeomAbs import GeomAbs_Cone, GeomAbs_Cylinder, GeomAbs_Plane, GeomAbs_Sphere, GeomAbs_Torus
from OCP.TopAbs import TopAbs_FACE, TopAbs_IN
from OCP.TopExp import TopExp_Explorer
from OCP.TopLoc import TopLoc_Location
from OCP.TopoDS import TopoDS, TopoDS_Compound

SIMPLE = {GeomAbs_Plane, GeomAbs_Cylinder, GeomAbs_Cone, GeomAbs_Sphere, GeomAbs_Torus}


def _faces(shape):
    """Every face, in the order the proximity check numbers them."""
    explorer, faces = TopExp_Explorer(shape, TopAbs_FACE), []
    while explorer.More():
        faces.append(TopoDS.Face(explorer.Current()))
        explorer.Next()
    return faces


def _mesh(face):
    return BRep_Tool.Triangulation_s(face, TopLoc_Location())


def _compound(faces):
    compound, builder = TopoDS_Compound(), BRep_Builder()
    builder.MakeCompound(compound)
    for face in faces:
        builder.Add(compound, face)
    return compound


def _near(a, b, tolerance):
    """The faces of a and of b whose meshes come within `tolerance` of each other."""
    prox = BRepExtrema_ShapeProximity(a, b, tolerance)
    prox.Perform()
    if not prox.IsDone():
        return [], []
    near_a = [f for i, f in enumerate(_faces(a)) if prox.OverlapSubShapes1().IsBound(i)]
    near_b = [f for i, f in enumerate(_faces(b)) if prox.OverlapSubShapes2().IsBound(i)]
    return near_a, near_b


def _deflection(faces):
    """How far the mesh may sit from the real surface, at most."""
    return max((_mesh(f).Deflection() for f in faces if _mesh(f) is not None), default=0.0)


def _mesh_distance(a, b, high):
    """The closest the two meshes come, to within 1% of `high`, by narrowing the
    proximity check's tolerance. The mesh itself is only roughly on the surface."""
    low = 0.0
    for _ in range(8):
        middle = (low + high) / 2
        if _near(a, b, middle)[0]:
            high = middle
        else:
            low = middle
    return high


def _inside(outer, inner):
    """A corner of `inner` that lies inside the solid `outer`, or None."""
    corners = inner.vertices()
    if not corners or not outer.solids():
        return None
    point = corners[0].center()
    classifier = BRepClass3d_SolidClassifier(outer.wrapped, point.to_pnt(), 1e-6)
    return point if classifier.State() == TopAbs_IN else None


def _point(p):
    return Vector(p.X(), p.Y(), p.Z())


def clearance(a, b, within):
    """(distance, point on a, point on b, accuracy) when the parts come within
    `within` mm, else None. Both must be meshed already. An exact answer has accuracy
    None; one measured on the mesh has no points and is good to `accuracy` mm."""
    faces_a, faces_b = _faces(a.wrapped), _faces(b.wrapped)
    if any(_mesh(f) is None for f in faces_a + faces_b):
        # The meshes can't see a face OpenCascade failed to mesh (see meshing.py).
        distance, pa, pb = a.distance_to_with_closest_points(b)
        return (distance, pa, pb, None) if distance <= within else None
    margin = _deflection(faces_a) + _deflection(faces_b)
    near_a, near_b = _near(a.wrapped, b.wrapped, within + margin)
    found = None
    if near_a and near_b:
        simple = lambda f: BRepAdaptor_Surface(f).GetType() in SIMPLE
        simple_a, free_a = [f for f in near_a if simple(f)], [f for f in near_a if not simple(f)]
        simple_b, free_b = [f for f in near_b if simple(f)], [f for f in near_b if not simple(f)]
        if simple_a and simple_b:
            exact = BRepExtrema_DistShapeShape(_compound(simple_a), _compound(simple_b))
            if exact.IsDone() and exact.Value() <= within:
                found = (exact.Value(), _point(exact.PointOnShape1(1)), _point(exact.PointOnShape2(1)), None)
        # Wherever a freeform face is involved, measure on the mesh.
        rough = None
        for x, y in ((free_a, near_b), (simple_a, free_b)):
            if x and y and _near(_compound(x), _compound(y), within + margin)[0]:
                distance = _mesh_distance(_compound(x), _compound(y), within + margin)
                # OpenCascade's own deflection figure for a freeform face can be under
                # the real error (1.3 mm against 0.98 on a lofted test part), so allow twice.
                accuracy = 2 * (_deflection(x) + _deflection(y)) + (within + margin) / 100
                rough = min(rough or (distance, accuracy), (distance, accuracy))
        # The exact answer stands unless a freeform face may come closer still.
        if rough and rough[0] <= within + rough[1] and (found is None or rough[0] < found[0]):
            found = (rough[0], None, None, rough[1])
    if found and found[0] == 0:
        return found
    # Surfaces apart, but one part may sit wholly inside the other's material.
    for outer, inner in ((a, b), (b, a)):
        point = _inside(outer, inner)
        if point is not None:
            return 0.0, point, point, None
    return found
