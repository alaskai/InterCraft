# Makes surface.step for the import tests: one open surface (a single face), no solid.
# build123d 0.13.0. Run with the worker's Python: python make_surface.py surface.step
import sys
from build123d import *

face = Face.make_rect(40, 20)
face.label = "Sheet"
export_step(face, sys.argv[1])
