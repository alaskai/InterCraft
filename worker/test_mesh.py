# Run with: python -m unittest worker/test_mesh.py
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from build123d import Box
from OCP.BRep import BRep_Builder

import meshing


class MeshTests(unittest.TestCase):
    def test_a_box_meshes_whole(self):
        v, t, skipped = meshing.triangles(Box(10, 20, 30), 0.05, 0.3)
        self.assertEqual((len(t), skipped), (12, 0))
        self.assertAlmostEqual(float(v[:, 2].max() - v[:, 2].min()), 30, places=4)

    def test_a_face_without_a_mesh_is_left_out_not_fatal(self):
        # Some faces converted from IGES get no mesh from OpenCascade. Stand in for
        # one by clearing a face's mesh after the rest is done.
        box = Box(10, 20, 30)
        box.mesh(0.05, 0.3)
        BRep_Builder().UpdateFace(box.faces()[0].wrapped, None)
        v, t, skipped = meshing.collect(box)
        self.assertEqual((len(t), skipped), (10, 1))
        self.assertLess(int(t.max()), len(v))


if __name__ == "__main__":
    unittest.main()
