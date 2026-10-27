# Fixture `flywheel`: Flywheel disc with hub, bore and recess

## Files

- Recipe: `flywheel.json` (CLI command script, sha256 `bf2a7e544520efed…`).
- Design: `flywheel.solidcraft` (sha256 `0da199151ddf883f…`).
- Origin: Modelled from scratch with `solidcraft-cli 0.20.0` (`flywheel.json`), 2026-10-23.
- Sheet outputs in `snapshot/`: `flywheel.pdf`, `flywheel.dxf`, `flywheel.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run flywheel.json --save flywheel.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/flywheel.pdf"}' --in flywheel.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/flywheel.dxf"}' --in flywheel.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in flywheel.solidcraft > flywheel.sheet.json
```

## The part as modelled

- One closed (r, z) profile revolved about Z, no booleans: disc Ø120 × 20 (z 0…20); hub Ø40, 35 overall (z 0…35, 15 proud of the disc); Ø16 bore through the full 35.
- Annular recess Ø40…Ø90, cut into the hub-side face from z = 20 down to z = 10 (10 deep, leaving a 10 mm web).
- Volume, hand: 186 956.179 mm³; `eval`: 186 949.347 mm³ (0.0037 %).

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `front`, `top` (`view_choice` = `turned`, arrangement `side-by-side`).
  - Note: in method 1 a view from above belongs **below** the front view. The `side-by-side` pair is not in a method-1 position (blind test, finding 7).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:2**, chosen by the automatic scale picker (no scale given).
- Section: full section A–A (rule: a hollow turned part gets a full axial section).

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| disc diameter | Ø120 | Ø120 | ok |
| hub diameter | Ø40 | Ø40 | ok |
| bore | Ø16 (through) | Ø16 | ok |
| recess | Ø90 × 10 deep | `Ø90 x 10 tief` | ok |
| disc thickness | 20 | 20 | ok |
| overall length (hub) | 35 | 35 | ok |

Sheet lists: `unplaced` = []; `undimensioned` = 0 face(s).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **4/4 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- `unplaced` is empty. The same prototype tests run on this part.
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `front` view measures **60.0 × 17.5 mm**; expected 60 × 17.5 mm (part size × 1:2).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:2. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
