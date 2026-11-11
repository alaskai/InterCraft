# Limits what a feature snippet can do. Snippets come from an LLM or from project
# files someone else wrote, so they are treated as untrusted code.
#
# Two layers:
# 1. The code is parsed and checked before it runs. Imports, names and attributes
#    that lead to files, processes or Python internals are refused.
# 2. It runs with a short list of builtins and only the build123d modelling names,
#    with every build123d import and export function removed.
#
# Python can't be sandboxed perfectly in-process, so the worker also runs in an OS
# job with a memory cap and is killed if a build takes too long (see crates/geo).

import ast
import builtins
import math

MAX_CODE_CHARS = 20_000

# Plain values and functions a modelling snippet needs. No open, eval, exec,
# getattr, type, globals, input or __import__.
_SAFE_BUILTIN_NAMES = [
    "abs", "all", "any", "bool", "dict", "divmod", "enumerate", "filter", "float",
    "frozenset", "int", "isinstance", "issubclass", "iter", "len", "list", "map", "max",
    "min", "next", "pow", "print", "range", "repr", "reversed", "round", "set", "slice",
    "sorted", "str", "sum", "tuple", "zip",
    "ArithmeticError", "AssertionError", "Exception", "IndexError", "KeyError",
    "TypeError", "ValueError", "ZeroDivisionError", "StopIteration",
]
SAFE_BUILTINS = {name: getattr(builtins, name) for name in _SAFE_BUILTIN_NAMES}

# Names a snippet may never use, even if they turn up some other way.
_BANNED_NAMES = {
    "eval", "exec", "compile", "open", "input", "breakpoint", "help", "getattr", "setattr",
    "delattr", "hasattr", "globals", "locals", "vars", "dir", "type", "object", "super",
    "memoryview", "classmethod", "staticmethod", "property", "exit", "quit", "license",
    "copyright", "credits",
}

# Attribute prefixes that reach frames, code objects or file I/O. Anything starting
# with "_" is refused too, which closes the usual __class__/__subclasses__ escapes.
_BANNED_ATTR_PREFIXES = (
    "_", "f_", "gi_", "cr_", "ag_", "tb_", "co_", "mro",
    "export", "import", "read", "write", "save", "load", "open", "dump", "system", "spawn",
)

# build123d functions and classes that read or write files.
_IO_NAMES = {
    "Export2D", "ExportDXF", "ExportSVG", "Mesher",
    "export_brep", "export_gltf", "export_obj", "export_step", "export_stl", "export_to_pcbway",
    "import_brep", "import_dxf", "import_step", "import_stl", "import_svg",
    "import_svg_as_buildline_code",
}


class SnippetRejected(Exception):
    """The snippet uses something it isn't allowed to."""


def build_namespace():
    """Modelling names every snippet starts with."""
    ns = {}
    exec("from build123d import *", ns)
    for name in list(ns):
        if name.startswith("_") or name in _IO_NAMES:
            del ns[name]
    ns["math"] = math
    return ns


def check(code):
    """Raise SnippetRejected if the code steps outside what modelling needs."""
    if len(code) > MAX_CODE_CHARS:
        raise SnippetRejected(f"snippet is {len(code)} characters; the limit is {MAX_CODE_CHARS}")
    try:
        tree = ast.parse(code)
    except SyntaxError as e:
        raise SnippetRejected(f"syntax error on line {e.lineno}: {e.msg}") from None

    for node in ast.walk(tree):
        line = getattr(node, "lineno", "?")
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            raise SnippetRejected(f"line {line}: imports aren't allowed (build123d and math are already available)")
        if isinstance(node, (ast.Global, ast.Nonlocal)):
            raise SnippetRejected(f"line {line}: global and nonlocal aren't allowed")
        if isinstance(node, ast.Name) and (node.id.startswith("__") or node.id in _BANNED_NAMES):
            raise SnippetRejected(f"line {line}: '{node.id}' isn't available in snippets")
        if isinstance(node, ast.Attribute) and node.attr.lower().startswith(_BANNED_ATTR_PREFIXES):
            raise SnippetRejected(f"line {line}: '.{node.attr}' isn't allowed in snippets")
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and "{" in node.value and "." in node.value:
            # "{0.__class__}".format(x) reaches attributes through a format string.
            if "_" in node.value:
                raise SnippetRejected(f"line {line}: format strings that reach attributes aren't allowed")
    return tree


def run(code, part, params, base_ns, filename="feature"):
    """Check and run a snippet. Returns the new `part`."""
    tree = check(code)
    ns = dict(base_ns)
    ns["__builtins__"] = SAFE_BUILTINS
    ns["part"] = part
    ns["p"] = dict(params)
    exec(compile(tree, filename, "exec"), ns)
    return ns.get("part")
