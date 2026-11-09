# Slit from the +X side to just past the bore centre, so a hinge remains.
bb = part.bounding_box()
length = bb.max.X + p["past_centre"]
slit = Pos(bb.max.X - length / 2, 0, 0) * Box(length, 100, p["slit_w"])
part = part - slit
