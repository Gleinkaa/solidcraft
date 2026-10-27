# Fixture `crank`: Crank plate with two bores

## Files

- Recipe: `crank.json` (CLI command script, sha256 `211e21c6d625f4dc…`).
- Design: `crank.solidcraft` (sha256 `6d333977ef2592f6…`).
- Origin: Modelled from scratch with `solidcraft-cli 0.20.0` (`crank.json`), 2026-10-23.
- Sheet outputs in `snapshot/`: `crank.pdf`, `crank.dxf`, `crank.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run crank.json --save crank.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/crank.pdf"}' --in crank.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/crank.dxf"}' --in crank.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in crank.solidcraft > crank.sheet.json
```

## The part as modelled

- 10 mm plate (`solid.extrude` of a sketch). Its outline is R20 about (0, 0) and R12 about (60, 0), joined by their two external common tangents. The tangent points are at x = 2.6667 and x = 61.6; θ = acos((20 − 12)/60) = 82.3377°.
- Bores Ø16 at (0, 0) and Ø10 at (60, 0), through (`solid.cylinder` cuts).
- Volume, hand: 25 120.035 mm³; `eval`: 25 119.916 mm³ (0.00048 %).

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `top`, `front` (`view_choice` = `flat`, arrangement `side-by-side`).
  - Note: in method 1 a view from above belongs **below** the front view. The `side-by-side` pair is not in a method-1 position (blind test, finding 7).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:1**, chosen by the automatic scale picker (no scale given).
- Section: none (rule: flat part, no section).

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| centre distance | 60 | 60 | ok |
| large end radius | R20 | R20 | ok |
| small end radius | R12 | R12 | ok |
| large bore | Ø16 | Ø16 | ok |
| small bore | Ø10 | Ø10 | ok |
| thickness | 10 | 10 | ok |
| overall length | 92 (auxiliary: 20 + 60 + 12) | 92 | ok, but not marked as auxiliary |
| overall width | 40 (auxiliary: 2 × R20) | 40 | ok, but not marked as auxiliary |

Sheet lists: `unplaced` = []; `undimensioned` = 0 face(s).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **4/4 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0003°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- `unplaced` is empty. The same prototype tests run on this part.
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `top` view measures **92.0 × 39.999 mm**; expected 92 × 40 mm (part size × 1:1).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:1. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
