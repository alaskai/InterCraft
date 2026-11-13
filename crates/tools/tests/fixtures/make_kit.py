# Makes kit.step for the split import test: a base plate, and a hinge sub-assembly
# (two leaves and a pin) moved 100 mm along Y. build123d 0.13.0.
# Run with the worker's Python: python make_kit.py kit.step
import sys
from build123d import *

base = Box(60, 40, 5)
base.label = "Base"
leaf_a = Pos(-10, 0, 0) * Box(15, 30, 2)
leaf_a.label = "Leaf"
leaf_b = Pos(10, 0, 0) * Box(15, 30, 2)
leaf_b.label = "Leaf"
pin = Rot(90, 0, 0) * Cylinder(1.5, 30)
pin.label = "Pin"
hinge = Compound(children=[leaf_a, leaf_b, pin], label="Hinge")
hinge = hinge.moved(Location((0, 100, 0)))
hinge.label = "Hinge"
kit = Compound(children=[base, hinge], label="Kit")
export_step(kit, sys.argv[1])
