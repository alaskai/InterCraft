# Bore runs along Y, through the full depth.
bore = Rot(90, 0, 0) * Cylinder(p["bore_d"] / 2, 100)
part = part - bore
