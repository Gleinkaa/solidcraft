# Fixture `bracket`: L-bracket with holes and slot (blind test)

## Files

- Recipe: `bracket.json` (CLI command script, sha256 `10a6ebd6a74a0fa8…`).
- Design: `bracket.solidcraft` (sha256 `60dd84f8a8e75642…`).
- Origin: Self-modelled for the blind test with `solidcraft-cli 0.24.0`, 2026-10-27 (`bracket.json`). A solid, not a sheet-metal feature.
- Sheet outputs in `snapshot/`: `bracket.pdf`, `bracket.dxf`, `bracket.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run bracket.json --save bracket.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/bracket.pdf"}' --in bracket.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/bracket.dxf"}' --in bracket.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in bracket.solidcraft > bracket.sheet.json
```

## The part as modelled

- Long leg 80 (outside) × 60 wide × 8 thick; short leg 50 high (outside). Bend: inner R8, outer R16 (fillets).
- 2× Ø6.6 through in the long leg, 55 from the outer back face, 15 and 45 from the side edge.
- Slot 6.6 wide × 20 overall (centres 13.4 apart) across the short leg, centred 30 from the side and 33 from the bottom.
- Volume, hand: 54 559 mm³; `eval`: 54 559 mm³.

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `front`, `top`, `right` (`view_choice` = `general`, arrangement `first-angle`).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:5**, chosen by the automatic scale picker (no scale given).
- Section: none (rule: general part, no section).

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| length | 80 | 80 | ok |
| width | 60 | 60 | ok |
| height | 50 | 50 | ok |
| thickness | 8 | — | missing |
| inner bend radius | R8 | R8 | ok |
| outer bend radius | (R16, redundant once 8 and R8 exist) | R16 (printed upside down) | ok value, placement wrong |
| hole positions | 55; 15; 45 | 55, 15, 45 | ok |
| holes | 2× Ø6.6 through | `2x Bohrung Ø6,6 x 8 tief` | wrong (through reads as blind) |
| slot width | 6.6 | `2x R3,3` | wrong form |
| slot length | 20 (or 13.4 between centres) | — | missing |
| slot position | 30 from the side; 33 from the bottom | — | missing |

Sheet lists: `unplaced` = []; `undimensioned` = 0 face(s).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **6/6 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- 5 extra curve(s) that are not in the hand list: ARC on `HIDDEN` (right) r 3.3 at [36.7, 33.0], [14.25, 21.233]°; ARC on `HIDDEN` (right) r 3.3 at [36.7, 33.0], [58.716, 61.332]°; ARC on `HIDDEN` (right) r 3.3 at [36.7, 33.0], [64.011, 66.491]°; ARC on `HIDDEN` (right) r 3.3 at [36.7, 33.0], [69.017, 75.953]°; ARC on `HIDDEN` (right) r 3.3 at [36.7, 33.0], [78.188, 80.273]°. These are short hidden slivers lying on top of a visible arc: a hidden-line artefact.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- `R16` is printed rotated 180°. The R8/R16 leaders are crossed by the `50` dimension line. The `2x R3,3` text is slanted along its leader, and its arrow ends past the slot.
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `front` view measures **16.0 × 10.0 mm**; expected 16 × 10 mm (part size × 1:5).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:5. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
