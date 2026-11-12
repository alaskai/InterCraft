# Escape attempts against the snippet sandbox, plus normal modelling code that must
# keep working. Run with the worker's Python:
#   "%LOCALAPPDATA%\InterCraft\venv\Scripts\python.exe" -m unittest worker/test_sandbox.py

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sandbox  # noqa: E402

BASE = sandbox.build_namespace()

# Each of these must be refused before it runs, or fail without doing anything.
ESCAPES = {
    "import": "import os",
    "from import": "from os import system",
    "dunder import": "__import__('os').system('echo hi')",
    "open": "open('C:/Windows/win.ini').read()",
    "fullwidth open": "ｏｐｅｎ('C:/Windows/win.ini')",
    "eval": "eval('1+1')",
    "exec": "exec('x = 1')",
    "compile": "compile('1', 'x', 'eval')",
    "getattr": "getattr(part, 'wrapped')",
    "globals": "globals()",
    "vars": "vars()",
    "type": "type(part)",
    "object": "object.__subclasses__()",
    "subclasses chain": "().__class__.__bases__[0].__subclasses__()",
    "function globals": "(lambda: 0).__globals__",
    "builtin self": "print.__self__",
    "generator frame": "g = (x for x in [1]); g.gi_frame.f_back",
    "frame via traceback": "try:\n    1/0\nexcept Exception as e:\n    t = e.__traceback__",
    "format string attr": "'{0.__class__.__mro__}'.format(part)",
    "f-string attr": "x = f'{part.__class__}'",
    "math loader": "math.__loader__",
    "params class": "p.__class__",
    "class mro": "class A:\n    pass\nA.mro()",
    "export function": "export_stl(part, 'C:/temp/x.stl')",
    "export method": "part.export_stl('C:/temp/x.stl')",
    "import function": "import_step('C:/secret.step')",
    "svg exporter": "ExportSVG()",
    "mesher": "Mesher()",
    "global stmt": "def f():\n    global part\n",
    "breakpoint": "breakpoint()",
    "builtins name": "__builtins__",
    "too long": "x = 1\n" * 5000,
}

# Ordinary snippets in the style the model writes.
ALLOWED = {
    "box": 'part = Box(p["w"], p["d"], p["t"])',
    "bore": 'part = Box(40, 30, 20)\nbore = Rot(90, 0, 0) * Cylinder(p["bore_d"] / 2, 100)\npart = part - bore',
    "fillet lambda": (
        "part = Box(40, 30, 20)\nbb = part.bounding_box()\n"
        "edges = part.edges().filter_by(Axis.Z).filter_by(lambda e: e.center().X < bb.min.X + 0.01)\n"
        "part = fillet(edges, 3)"
    ),
    "loop and list": (
        "holes = [Pos(x, y, 0) * Cylinder(2.1, 20) for x in (-22, 22) for y in (-12, 12)]\n"
        "part = Box(60, 40, 5) - holes"
    ),
    "helper function": "def plate(w):\n    return Box(w, w, 2)\npart = plate(10)",
    "math": "r = 5 * math.cos(math.radians(30))\npart = Cylinder(r, 10)",
    "f-string plain": "name = f'plate {len([1, 2])}'\npart = Box(1, 1, 1)",
    "try except": "try:\n    x = 1 / 0\nexcept ZeroDivisionError:\n    x = 1\npart = Box(x, x, x)",
}


class SandboxTests(unittest.TestCase):
    def test_escapes_are_blocked(self):
        for name, code in ESCAPES.items():
            with self.subTest(name):
                with self.assertRaises((sandbox.SnippetRejected, NameError)):
                    sandbox.run(code, None, {"w": 1}, BASE)

    def test_normal_snippets_still_work(self):
        params = {"w": 60, "d": 40, "t": 5, "bore_d": 8.2}
        for name, code in ALLOWED.items():
            with self.subTest(name):
                part = sandbox.run(code, None, params, BASE)
                self.assertGreater(part.volume, 0)

    def test_namespace_has_no_io(self):
        for name in sandbox._IO_NAMES:
            self.assertNotIn(name, BASE)
        self.assertNotIn("__builtins__", BASE)

    def test_bracket_example_still_builds(self):
        root = os.path.join(os.path.dirname(__file__), "..", "examples", "bracket", "RodBracket")
        params = {"width": 40, "depth": 30, "height": 20, "bore_d": 8.2, "slit_w": 1.0, "past_centre": 2.0, "radius": 3}
        part = None
        for name in sorted(os.listdir(root)):
            with open(os.path.join(root, name), encoding="utf-8") as f:
                part = sandbox.run(f.read(), part, params, BASE, name)
        self.assertAlmostEqual(part.volume, 21861.13, places=1)



class StandardPartsStayInsideTheSandbox(unittest.TestCase):
    """The parts library is reachable only through its two functions."""

    def test_parts_build_and_tables_answer(self):
        ns = sandbox.build_namespace()
        shape = sandbox.run('part = standard_part("hex nut", "M4")', None, {}, ns, "t")
        self.assertGreater(shape.volume, 0)
        self.assertEqual(type(shape).__module__.split(".")[0], "build123d", "a plain build123d shape")
        self.assertAlmostEqual(sandbox.run('part = Box(1, 1, clearance_hole("M3"))', None, {}, ns, "t").volume, 3.4)

    def test_no_way_back_to_the_library(self):
        ns = sandbox.build_namespace()
        escapes = [
            "part = standard_part.__globals__",
            "part = clearance_hole.__code__",
            "import bd_warehouse",
            "part = standard_part('hex nut', 'M4').__class__",
            "part = standard_part('hex nut', 'M4').export_step('x.step')",
        ]
        for code in escapes:
            with self.assertRaises(sandbox.SnippetRejected, msg=code):
                sandbox.check(code)

if __name__ == "__main__":
    unittest.main()
