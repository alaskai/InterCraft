# Parametric CAD Agent Playbook

Techniques, tricks and pitfalls from the Ender 3 S1 Pro rod-conversion project (Y housing, dovetail, X carriage, split clamps). Written so a coding agent can be given this file as context and work effectively on mechanical designs with Fusion 360, STEP/STL files and a Python mesh toolchain.

---

## 0. The one-paragraph version

Work in **exact geometry (STEP / BRep) as long as possible**, measure with **code, not by eye**, verify every change with **numbers (volumes, gaps, clearances) before images**, keep **datums and decisions in a conventions file**, and only tessellate to STL for the final print file. Never trust a mesh you did not just measure. Never block the Fusion connection with a dialog.

---

## 1. Recommended architecture

```
 Fusion 360  <--MCP-->  agent  <-->  Python sandbox (build123d, trimesh, manifold3d)
     |                                   |
     +-- STEP export/import -------------+
     |
 Shared folder (STEP, STL, results .txt/.json, conventions.md)
```

- **Fusion** is the source of truth for the user's real model and where they inspect and print from.
- **Sandbox** is where heavy geometry/analysis happens (cheap, scriptable, repeatable).
- **Shared folder** is the bus. Everything crosses as files. Nothing crosses as pasted coordinates.
- **Project docs / conventions file** hold datums, decisions, open items so a fresh session can resume cold.

### Format rules
| Stage | Format | Why |
|---|---|---|
| Interchange of parametric parts | **STEP** | Exact cylinders, planes, radii. Hole diameters read straight off faces. |
| Analysis of existing meshes | STL / 3MF | Only choice if the source is a mesh. Treat as lossy. |
| Final print output | STL (tessellated last) | Printers need it. Tessellate once, at the end. |
| Intermediate collision/volume checks | Manifold solids | Exact booleans, fast. |

Every STL -> STL edit round-trip loses information and risks non-manifold output. Every STEP edit stays exact.

---

## 2. Fusion 360 via MCP: rules that cost real time to learn

1. **Units are centimetres internally.** Convert: mm = cm x 10. Check before writing any dimension.
2. **Scripts must define `def run(context):`**.
3. **Never call `ui.messageBox`.** It blocks the *entire* MCP connection until a human clicks OK. Write results to a file in a connected folder instead, then read that file.
4. **Use read-only mode for anything non-modifying** (`readOnly: true`). It is faster and cannot damage the model.
5. **Do not touch heavy assemblies** (the 'Untitled v21' printer assembly). Work in a clean document and import parts as STEP.
6. **Component-space vs world-space:** Fusion's `createSTLExportOptions` writes **component-space** coordinates. If the part sits in an assembly, the numbers will not match what the user sees. To get world coordinates, recursively compose `occurrence.transform2` up the hierarchy and transform the mesh vertices yourself.
7. **Mesh API details that bite:**
   - `TriangleMesh` exposes `nodeCoordinatesAsDouble` and `nodeIndices`.
   - `MeshBody` has **no** `meshManager`; use `displayMesh`.
   - For BReps, use `meshManager.createMeshCalculator().calculate()`.
   - `displayMeshes.bestMesh` on a BRep can fail with `InternalValidationError: count > 0`; use the mesh calculator instead.
8. **Name things.** Bodies called `Body1` are un-findable. The user cannot find a design if you do not tell them *which document and which component*. Always report: document name, component path, body name.
9. Importing: prefer STEP into a clean doc (e.g. under a `30 X Axis (WIP)` component). Tell the user where it landed.

### Export-to-file pattern (replace all dialogs)
```python
import adsk.core, adsk.fusion, json, traceback

def run(context):
    out = r"C:\Users\<user>\...\_claude_export\result.json"
    try:
        app = adsk.core.Application.get()
        design = adsk.fusion.Design.cast(app.activeProduct)
        data = {"bodies": [b.name for b in design.rootComponent.bRepBodies]}
        with open(out, "w") as f:
            json.dump(data, f)
    except:
        with open(out, "w") as f:
            f.write(traceback.format_exc())
```

---

## 3. Mesh and STEP toolchain (sandbox)

Installed/used: `trimesh`, `numpy`, `manifold3d`, `matplotlib`, `rtree`, `shapely`, `mapbox_earcut`, `cascadio`, `networkx`, `build123d` (OpenCascade).
`pip install --break-system-packages <pkg>`. Long installs (build123d) exceed a 120 s shell timeout: run in background and poll.

### 3.1 Load STEP exactly (build123d)
Loads a 7 MB assembly in ~3 s as individual solids, giving **exact cylindrical-face radii and centres**. Use this to read hole diameters and positions instead of fitting circles to triangles.

### 3.2 STEP -> mesh when you must (cascadio)
`cascadio.step_to_glb(tol_linear, tol_angular)`. glTF is **metres**; multiply by 1000 for mm. Finer tolerances can *worsen* topology problems; do not assume finer is safer.

### 3.3 Manifold booleans (exact, fast)
```python
from manifold3d import Manifold
a + b       # union
a - b       # difference
a ^ b       # intersection   (NOT &)
a.volume()
a.min_gap(b, search_length)
a.bounding_box()
```
- `Manifold.cylinder` and `extrude` run along **Z**. Rotate to orient.
- Use these for **collision checks**: `(a ^ b).volume() == 0` and `a.min_gap(b, 5)` give exact clearance. Requires watertight inputs.

### 3.4 Finding holes in a mesh
When only a mesh exists:
1. `mesh.section(plane_origin, plane_normal)` at 0.5-1 mm steps through the region.
2. Least-squares circle fit to each closed loop.
3. Keep fits where **max residual < 0.2-0.4 mm** (rejects non-circles).
4. Cluster fits that share a centre across slices -> a hole axis with diameter and depth.

This is how both brackets' bores (O8.20 x 26 mm, 37.445 mm spacing) and mounting-hole triangles were found.

### 3.5 Collision sweeps
- Watertight parts: exact manifold booleans (intersection volume + `min_gap`).
- Broken meshes: a **1 mm voxel grid of surface-sampled points**; coarse but needs no repair.
- Sweep a part along its travel and log the first collision. This produced the real X travel figure (230.0 mm).

### 3.6 Mesh repair (missing face / open edges)
A mesh with a hole in a face is not a solid and will break booleans.
1. Find edges used by **exactly one** triangle -> boundary edges.
2. Group into loops (`networkx.cycle_basis`).
3. Rebuild the missing face as a `shapely` Polygon (outer loop + hole loops).
4. Triangulate with `mapbox_earcut`.
5. **Weld by KD-tree index mapping.** Plain concatenate + merge does **not** weld; vertices stay duplicated.
6. Confirm `mesh.is_watertight` and `mesh.volume > 0`.

---

## 4. Verification discipline (the part that prevents wrong parts)

1. **Numbers first, pictures second.** Report volumes, min gaps, hole diameters, positions. Use plots only to *show* a result already proven.
2. **Every cut or add reports a before/after volume.** e.g. RIGHT 70 955 -> 70 142 mm^3, LEFT 57 069 -> 56 558 mm^3.
3. **Prove fit with a test body.** A O8.0 rod through every bore: zero interference, 0.094 mm gap. Do this for every mating feature.
4. **Check watertight and positive volume** on every file you deliver.
5. **State the frame** (which datum, which axis) with every coordinate you give.
6. **Separate measured from assumed.** If a dimension came from the user ("about 90 mm back to front") say so and flag it for confirmation.
7. **Never conclude from one view.** A wrong early conclusion ("bore depth causes travel loss") was corrected only after measuring real brackets; the true cause was carriage width.

### Visualisation that works vs doesn't
| Works | Doesn't |
|---|---|
| `mesh.section` plotted by plane axes (true cross sections) | Raw vertex scatter plots (CAD meshes look like noise) |
| Shaded triangles (`PolyCollection`) with fixed camera | Scatter of vertices |
| Annotated dimension callouts on a section | Unlabelled 3-D renders |
| Side-by-side before/after of the same view | Different cameras per image |

Watch for rotation bugs: an empty plot usually means the plane axes were swapped.

---

## 5. Coordinate frames and mapping

Always define a **named datum** per assembly and write it into the conventions file. Example from this project (bracket frame: gantry = y, vertical = z, depth = x):

```python
# German reference STEP frame -> user's bracket frame
A, D, E = -211.78, 493.1, 169.46
M = np.array([[ 0, 1, 0, A],
              [-1, 0, 0, D],
              [ 0, 0, 1, E],
              [ 0, 0, 0, 1]], dtype=float)
```
- Derive mappings from **matching features** (hole patterns, bore axes), not from guessing.
- Confirm by checking that mapped features coincide to < 0.1 mm.
- Keep every mapping matrix in the conventions file.

---

## 6. Design techniques that paid off

- **Split clamps instead of tight bores.** A 1.0 mm slit through the bore with a cross bolt lets a O8.00 rod slide in and clamp (slot past bore centre by 2 mm, hinge remains). Cuts at a near cheek: clearance hole; far cheek: undersized pilot; 1.2 mm spotface for the head. Result: rod passes with 0.094 mm gap, then clamps.
- **Reuse what exists.** The user's printed housings already had the correct bore and three-bolt pattern. Measure before redesigning.
- **Read the load path.** Remove a part (the extrusion) and ask what now carries the load. Three bolts + printed bosses -> check the bosses, not the bolts.
- **Do a stiffness estimate before printing.** Rod I = pi d^4/64 = 201 mm^4, E = 200 GPa, span 298 mm. Pinned vs clamped end conditions bound the answer (nozzle deflection 0.178 mm vs 0.044 mm; f1 37 vs 75 Hz). The overhang couple on the carriage (pitch) dominates over sag.
- **Travel = rod span - carriage width.** Find the real lever before optimising the wrong one.
- **Adapt, don't redesign** when the user chooses a reference design: map it onto their frame and modify the interfaces only.

---

## 7. Process rules for the agent

1. **Resume from the notes.** At session start read the project docs / conventions file *before* answering. Don't search the filesystem for them.
2. **Start with one sentence** saying what you're about to do, then work.
3. **Ask only when a wrong guess is expensive** (large builds, irreversible choices). Otherwise decide, state the decision, move on.
4. **Keep a task list**; last step is always verification.
5. **Don't narrate every step.** Report outcomes: what changed, the key numbers, the file, one next step.
6. **Write durable decisions** (datums, dimensions, chosen compromises, open items) to the project doc as you go. Rewriting a whole doc each turn is expensive; append sections and keep a short "Open items" list.
7. **Name every delivered file** with a part, version and format: `XCarriage_BearingMount_v2.step`. Superseded versions stay, with the new version number.
8. **Deliver both STEP (editable) and STL (printable).**
9. **Own mistakes briefly** and record the correction in the notes so it isn't repeated.

---

## 8. Known pitfalls checklist

- [ ] Used `ui.messageBox` -> connection frozen. Never.
- [ ] Assumed mm in Fusion -> it's cm.
- [ ] Exported STL in component space -> numbers don't match the assembly.
- [ ] Concatenated meshes without welding -> still non-manifold.
- [ ] Used `&` instead of `^` for manifold intersection.
- [ ] Assumed extrusion size (2040 vs 2020). **Confirm profile sizes with the user.**
- [ ] Assumed which bodies are the metal part vs the printed part. **Identify by hole pattern and thickness (e.g. 3.00 mm sheet = metal).**
- [ ] Ran a long `pip install` in the foreground.
- [ ] Trusted a mesh with a hole in a face.
- [ ] Reported a figure without saying which frame it's in.
- [ ] Treated a user's eyeballed dimension as exact.

---

## 9. Conventions file template (give this to the agent every session)

```markdown
# <Project> conventions

## Units & frames
- Fusion internal: cm. All deliverables: mm.
- Datum <name>: origin = ..., +X = ..., +Y = ..., +Z = ...
- Mapping <ref> -> <name>: matrix ...

## Fixed parts (do not modify)
- <part>: <thickness/size>, holes: ...

## Parts under design
| Part | Version | Files | Status | Open issues |

## Key dimensions (measured)
| Feature | Value | Source (STEP / mesh / user) | Date |

## Decisions
- <decision> - <reason> - <date>

## Open items
- ...

## Tooling notes
- How to run exports, paths, folder locations
```

---

## 10. Spec for the utility you want to build

A thin tool that makes this loop cheaper and visual. Suggested scope, in order of value:

1. **`cad_env.py`** - one-time bootstrap that installs the toolchain and verifies imports.
2. **`geo.py` library** - the recurring functions:
   - `load_step(path)` -> solids with faces/cylinders
   - `find_holes(mesh, axis, step)` (section + circle fit)
   - `repair_open_faces(mesh)`
   - `sweep_collision(a, b, path)`
   - `check_fit(part, test_body)` -> volume, min_gap
   - `report(part)` -> watertight, volume, bbox
3. **Fusion bridge helper** - small script templates for: list bodies, export world-space STL/STEP to the shared folder, import STEP into a named component, write results to JSON.
4. **Viewer** - a local web page (three.js or `trimesh` viewer) that auto-reloads when an output file changes, showing the part, section planes and before/after diff. Gives the user the visual feedback Fusion gives, without Fusion.
5. **`conventions.md`** - the template above, loaded automatically into every session.
6. **Optionally a Claude skill** that packages sections 2-9 so it loads on demand instead of every prompt (saves usage).

### Cutting usage
- STEP-based pipeline removes most repair and re-measurement cycles.
- A small tested library replaces re-writing 60-line scripts each time.
- Conventions file replaces re-deriving datums and re-reading big files.
- Results to short JSON files (not pasted tables of coordinates).
- Avoid image renders until a number needs showing.

---

## 11. Prompt block to paste into a coding agent

```
You are assisting with parametric mechanical design (Fusion 360 + Python).
Rules:
- Prefer STEP over STL. Tessellate only for final print files.
- Fusion units are cm. Never call ui.messageBox; write results to files.
- Measure with code: hole diameters from STEP faces, or section + circle fit on meshes.
- Check fits with exact booleans (manifold3d: + union, - diff, ^ intersect, .volume(), .min_gap()).
- Every deliverable: watertight, positive volume, before/after volume reported.
- State the coordinate frame with every number. Frame definitions live in conventions.md.
- Separate measured values from assumed ones; flag assumptions.
- Read conventions.md first; update it with decisions and open items as you go.
- Report outcomes concisely: what changed, key numbers, file names, one next step.
```