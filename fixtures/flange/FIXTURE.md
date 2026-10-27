# Fixture `flange`: Flange with hub, bore and bolt circle (blind test)

## Files

- Recipe: `flange.json` (CLI command script, sha256 `307c791c3710635c…`).
- Design: `flange.solidcraft` (sha256 `d106deb751a4b42f…`).
- Origin: Self-modelled for the blind test with `solidcraft-cli 0.24.0` (the drawing build), 2026-10-27: `solidcraft-cli run flange.json --save flange.solidcraft`. The prototype had never seen it.
- Sheet outputs in `snapshot/`: `flange.pdf`, `flange.dxf`, `flange.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run flange.json --save flange.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/flange.pdf"}' --in flange.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/flange.dxf"}' --in flange.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in flange.solidcraft > flange.sheet.json
```

## The part as modelled

- Disc Ø120 × 15 (z 0…15); hub Ø60 standing 25 proud (overall 40).
- Centre bore Ø30 through (meant as H7; a fit cannot be modelled). 6× Ø9 through on PCD Ø95, at 0°/60°/….
- Chamfers 1 × 45° on both outer disc edges and on the hub's end edge.
- Volume, hand: 205 864 mm³; `eval`: 205 862 mm³.

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `front`, `top` (`view_choice` = `turned`, arrangement `side-by-side`).
  - Note: in method 1 a view from above belongs **below** the front view. The `side-by-side` pair is not in a method-1 position (blind test, finding 7).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:2**, chosen by the automatic scale picker (no scale given).
- Section: a local break-out. The rule asks for a full section A–A for a hollow turned part, so this looks like a hollow/solid misclassification.

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| disc diameter | Ø120 | Ø120 | ok |
| hub diameter | Ø60 | Ø60 | ok |
| disc thickness | 15 | — | missing |
| overall length (or hub length 25) | 40 | — | missing |
| bore | Ø30 H7 | — | missing (and H7 cannot be modelled) |
| bolt holes | 6× Ø9 through | — | missing |
| pitch circle | PCD Ø95 (on a drawn pitch circle) | — | missing (no pitch circle drawn either) |
| chamfers | 3× 1 × 45° | one `Fase 1 x 45°`, its leader on the axis; 2 in `unplaced` | wrong placement / incomplete |

Sheet lists: `unplaced` = ["front: Fase 1 x 45°", "front: Fase 1 x 45°"]; `undimensioned` = 10 face(s) (diameter 30, diameter 9, plane -12, plane -27, plane 13).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **11/11 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- The chamfer leader ends on the axis (sheet ≈ 108.0, 119.8), not on a chamfered edge. Two chamfer notes are in `unplaced`. In the end view, the bolt holes are joined by a rectangular grid of centre lines; there is no pitch circle. In the front view, the hole axes run 25 mm (part) past the disc.
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `front` view measures **59.999 × 20.0 mm**; expected 60 × 20 mm (part size × 1:2).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:2. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
