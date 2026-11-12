# Standard parts and their real sizes, from bd_warehouse (Apache-2.0), which holds
# the ISO tables for screws, nuts, washers, heat-set inserts and bearings.
#
# Snippets never see bd_warehouse itself. They get two plain functions:
#   standard_part(kind, size, length=None)  -> a plain build123d solid
#   clearance_hole(size, fit="normal")      -> a hole diameter in mm
# and the model gets fastener_info() through a tool, so sizes come from the tables
# rather than from memory.

import math

import build123d

# What a user or model may ask for, and the bd_warehouse class and standard behind it.
_KINDS = {
    "socket head screw": ("SocketHeadCapScrew", "iso4762"),
    "button head screw": ("ButtonHeadScrew", "iso7380_1"),
    "countersunk screw": ("CounterSunkScrew", "iso10642"),
    "hex nut": ("HexNut", "iso4032"),
    "washer": ("PlainWasher", "iso7089"),
    "heat-set insert": ("HeatSetNut", "McMaster-Carr"),
    "ball bearing": ("SingleRowDeepGrooveBallBearing", "SKT"),
}

KINDS = sorted(_KINDS)
_FITS = {"close": "Close", "normal": "Normal", "loose": "Loose"}


def _warehouse():
    # Imported on first use: bd_warehouse reads its tables when loaded.
    import bd_warehouse.bearing
    import bd_warehouse.fastener
    return bd_warehouse.fastener, bd_warehouse.bearing


def _class(kind):
    if kind not in _KINDS:
        raise ValueError(f"unknown standard part {kind!r}; use one of: {', '.join(KINDS)}")
    fastener, bearing = _warehouse()
    name, standard = _KINDS[kind]
    module = bearing if "Bearing" in name else fastener
    return getattr(module, name), standard


def _full_size(cls, standard, size):
    """'M4' -> 'M4-0.7' (the coarse thread), 'M4-0.7' stays. Bearings: 'M8-22-7'."""
    size = size.strip()
    sizes = cls.sizes(standard)
    if size in sizes:
        return size
    # The first listed pitch is the coarse one; a heat-set insert prefers its standard length.
    matches = [s for s in sizes if s.split("-")[0] == size]
    standard_length = [s for s in matches if s.endswith("-Standard")]
    if standard_length or matches:
        return (standard_length or matches)[0]
    raise ValueError(f"no {size} in this table; sizes include {', '.join(sizes[:12])}")


def _build(kind, size, length=None):
    cls, standard = _class(kind)
    full = _full_size(cls, standard, size)
    if "Screw" in cls.__name__:
        if length is None:
            raise ValueError(f"a {kind} needs a length in mm")
        return cls(full, float(length), standard, simple=True), full
    if "Bearing" in cls.__name__ or "Washer" in cls.__name__:
        return cls(full, standard), full
    return cls(full, standard, simple=True), full


def standard_part(kind, size, length=None):
    """A standard part as a plain solid, its axis on Z. Screws hang down from the
    origin with the head on top, as they would sit in a hole drilled from above."""
    part, _ = _build(kind, size, length)
    # A plain copy: no bd_warehouse methods or tables come along with it.
    return build123d.Part(part.wrapped)


def _table(row, key):
    """A number from a bd_warehouse table row, whose keys sometimes end in a space."""
    for name, value in row.items():
        if name.strip() == key and str(value).strip():
            return float(value)
    raise ValueError(f"the table has no {key} for this size")


def _screw_size(size):
    """Size details from the tables alone. Building the solid takes seconds."""
    fastener, _ = _warehouse()
    screw = fastener.SocketHeadCapScrew
    full = _full_size(screw, "iso4762", size)
    return screw, full


def clearance_hole(size, fit="normal"):
    """Diameter of a hole a screw of this size passes through freely."""
    fit_name = _FITS.get(str(fit).lower())
    if fit_name is None:
        raise ValueError("fit must be close, normal or loose")
    screw, full = _screw_size(size)
    return float(screw.clearance_hole_data[full.split("-")[0]][fit_name])


def fastener_info(size):
    """Everything the model needs to design around one metric size, from the tables."""
    fastener, _ = _warehouse()
    screw, full = _screw_size(size)
    nominal = full.split("-")[0]
    head = screw.fastener_data[full]
    nut_size = _full_size(fastener.HexNut, "iso4032", nominal)
    nut = fastener.HexNut.fastener_data[nut_size]
    across_flats = _table(nut, "iso4032:s")
    info = {
        "size": full,
        "thread_diameter": float(nominal[1:]),
        "thread_pitch": float(full.split("-")[1]),
        "clearance_hole": {fit.lower(): round(d, 3) for fit, d in screw.clearance_hole_data[nominal].items()},
        "tap_drill": {kind.lower(): round(d, 3) for kind, d in screw.tap_hole_data.get(full, {}).items()},
        "socket_head": {"diameter": _table(head, "iso4762:dk"), "height": _table(head, "iso4762:k")},
        "hex_nut": {
            "across_flats": across_flats,
            "across_corners": round(across_flats / math.cos(math.radians(30)), 3),
            "thickness": _table(nut, "iso4032:m"),
        },
        "note": "Clearance: close for accurate location, normal for most parts, loose for 3D prints that come out slightly small. Tap drill: hole size for cutting a thread.",
    }
    try:
        insert = fastener.HeatSetNut
        insert_size = _full_size(insert, "McMaster-Carr", nominal)
        row = insert.fastener_data[insert_size]
        source = "McMaster-Carr"
        info["heat_set_insert"] = {
            "part": f"{insert_size} ({source} {row[source + ':part'].strip()})",
            "hole_diameter": _table(row, source + ":dc"),
            "outside_diameter": _table(row, source + ":s"),
            "length": _table(row, source + ":m"),
            # The maker's minimum thickness of material for the insert to sit in.
            "min_part_thickness": _table(row, source + ":material_thickness"),
            "note": "Hole diameter is the maker's recommended size for the insert. Make the hole at least 1 mm deeper than the insert so melted plastic has somewhere to go.",
        }
    except (ValueError, KeyError):
        pass  # Not every size comes as an insert.
    return info
