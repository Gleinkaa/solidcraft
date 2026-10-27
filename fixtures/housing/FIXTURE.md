# Fixture `housing`: Bearing block with bore, counterbores and pocket (blind test)

## Files

- Recipe: `housing.json` (CLI command script, sha256 `9c0330d383181b79…`).
- Design: `housing.solidcraft` (sha256 `85b203923bb5a2fc…`).
- Origin: Self-modelled for the blind test with `solidcraft-cli 0.24.0`, 2026-10-27 (`housing.json`).
- Sheet outputs in `snapshot/`: `housing.pdf`, `housing.dxf`, `housing.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run housing.json --save housing.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/housing.pdf"}' --in housing.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/housing.dxf"}' --in housing.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in housing.solidcraft > housing.sheet.json
```

## The part as modelled

- Block 100 × 60 × 40.
- Bore Ø35 through, vertical, at (50, 30). It was modelled from the top face before the pocket was cut, so in the finished part it starts on the pocket floor.
- 2× counterbore for M6: Ø6.6 through, Ø11 × 6.5 deep, at x 12 and 88, y 30.
- Pocket on top 50 × 40 × 5 deep, corner R5, centred (x 25…75, y 10…50).
- Volume, hand: 192 905 mm³; `eval`: 192 907 mm³.

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `front`, `top`, `right` (`view_choice` = `general`, arrangement `first-angle`).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:5**, chosen by the automatic scale picker (no scale given).
- Section: none (rule: general part, no section; a full section through the bore and counterbores is what a drafter would draw).

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| length | 100 | 100 | ok |
| width | 60 | 60 | ok |
| height | 40 | 40 | ok |
| bore | Ø35 through | `Ø35 x 35 tief` | wrong (through reads as blind, depth from the pocket floor) |
| bore position | 50; 30 | 50, 30 | ok |
| counterbores | 2× Ø6.6 through, ⌴ Ø11 ↧ 6.5 | `2x Ø11 x 6,5 tief`; `2x Bohrung Ø6,6 x 33,5 tief` in `unplaced` | wrong (the hole is dropped; the unplaced note reads it as blind) |
| counterbore positions | 12; 88; 30 | 12, 88, 30 | ok |
| pocket size | 50 × 40 | — | missing |
| pocket depth | 5 | — | missing |
| pocket position | 25; 10 (or symmetric) | — | missing |
| pocket corners | 4× R5 | `4x R5` (lands on the notes block) | ok value, placement wrong |

Sheet lists: `unplaced` = ["top: 2x Bohrung Ø6,6 x 33,5 tief"]; `undimensioned` = 0 face(s).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **9/9 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- 2 extra curve(s) that are not in the hand list: ARC on `HIDDEN` (top) r 3.3 at [88.0, 30.0], [0.0, 1.484]°; ARC on `HIDDEN` (top) r 3.3 at [12.0, 30.0], [0.0, 1.484]°. These are short hidden slivers lying on top of a visible arc: a hidden-line artefact.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- The leaders of `Ø35 x 35 tief` and `2x Ø11 x 6,5 tief` are collinear at 45° across the view. `4x R5` is set on the notes block (it overlaps "ISO 2768-m").
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `front` view measures **20.0 × 8.0 mm**; expected 20 × 8 mm (part size × 1:5).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:5. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
