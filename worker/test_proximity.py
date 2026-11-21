# Run with: python -m unittest worker/test_proximity.py
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from build123d import Box, Circle, Cylinder, Pos, Rectangle, loft
from OCP.BRep import BRep_Builder

import proximity

TOL, ANG = 0.05, 0.3


def clearance(a, b, within):
    a.mesh(TOL, ANG)
    b.mesh(TOL, ANG)
    return proximity.clearance(a, b, within)


class ProximityTests(unittest.TestCase):
    def test_a_small_gap_is_measured_exactly(self):
        found = clearance(Box(10, 10, 10), Pos(10.02, 0, 0) * Box(10, 10, 10), 0.05)
        self.assertIsNotNone(found)
        self.assertAlmostEqual(found[0], 0.02, places=6)
        self.assertAlmostEqual(found[1].X, 5, places=6)
        self.assertIsNone(found[3])
        self.assertAlmostEqual(found[2].X, 5.02, places=6)

    def test_parts_further_apart_are_not_near(self):
        self.assertIsNone(clearance(Box(10, 10, 10), Pos(10.2, 0, 0) * Box(10, 10, 10), 0.05))
        self.assertIsNone(clearance(Box(10, 10, 10), Pos(40, 0, 0) * Box(10, 10, 10), 5))

    def test_curved_faces_match_the_exact_distance(self):
        # A pin over a bore: the mesh is coarser than the gap, the answer must not be.
        block = Box(20, 20, 30) - Pos(0, 0, 5) * Cylinder(2.1, 40)
        pin = Pos(0.7, 0, 10) * Cylinder(1.3, 10)
        expected = block.distance_to(pin)
        found = clearance(block, pin, 5)
        self.assertAlmostEqual(found[0], expected, places=6)

    def test_a_part_wholly_inside_another_touches_it(self):
        found = clearance(Box(50, 50, 50), Box(5, 5, 5), 0.05)
        self.assertEqual(found[0], 0.0)
        found = clearance(Box(5, 5, 5), Box(50, 50, 50), 0.05)
        self.assertEqual(found[0], 0.0)

    def test_parts_resting_on_each_other_touch(self):
        found = clearance(Box(100, 60, 5), Pos(30, 0, 17.5) * Box(20, 20, 30), 0.05)
        self.assertEqual(round(found[0], 6), 0.0)

    def test_a_face_without_a_mesh_is_still_seen(self):
        # The touching face of the second box has no mesh, as with some IGES faces.
        box, other = Box(10, 10, 10), Pos(10.02, 0, 0) * Box(10, 10, 10)
        box.mesh(TOL, ANG)
        other.mesh(TOL, ANG)
        face = min(other.faces(), key=lambda f: f.center().X)
        BRep_Builder().UpdateFace(face.wrapped, None)
        found = proximity.clearance(box, other, 0.05)
        self.assertIsNotNone(found)
        self.assertAlmostEqual(found[0], 0.02, places=6)

    def test_freeform_faces_are_measured_on_the_mesh_and_say_how_well(self):
        # A loft from a square to a circle has freeform sides, like a thread or spring.
        funnel = loft([Rectangle(20, 20), Pos(0, 0, 30) * Circle(6)])
        plate = Pos(0, 0, -2.5 - 0.3) * Box(40, 40, 5)
        found = clearance(funnel, plate, 1)
        self.assertEqual(found[3], None, "the funnel's flat base is measured exactly")
        self.assertAlmostEqual(found[0], 0.3, places=6)
        # Beside the sloping side, well above the flat base.
        wall = Pos(14, 0, 15) * Box(4, 40, 10)
        exact = funnel.distance_to(wall)
        distance, pa, pb, accuracy = clearance(funnel, wall, 5)
        self.assertIsNone(pa)
        self.assertGreater(accuracy, 0)
        self.assertLessEqual(abs(distance - exact), accuracy, (distance, exact, accuracy))


if __name__ == "__main__":
    unittest.main()
