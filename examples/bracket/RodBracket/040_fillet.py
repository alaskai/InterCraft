# Only the two vertical edges on the hinge side (the others are cut by the slit).
bb = part.bounding_box()
edges = part.edges().filter_by(Axis.Z).filter_by(lambda e: e.center().X < bb.min.X + 0.01)
part = fillet(edges, p["radius"])
