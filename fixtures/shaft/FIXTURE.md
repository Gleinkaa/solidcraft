# Fixture `shaft`: Stepped shaft with cross hole and blind bore

## Files

- Recipe: `shaft.json` (CLI command script, sha256 `23d98552ce267f4a…`).
- Design: `shaft.solidcraft` (sha256 `90ad6ef53d9c6f67…`).
- Origin: Modelled from scratch with `solidcraft-cli 0.20.0` (`solidcraft-cli run shaft.json --save shaft.solidcraft`), 2026-10-23. Same file as `crates/engine/tests/fixtures/drawing/shaft.solidcraft` on the reference branch.
- Sheet outputs in `snapshot/`: `shaft.pdf`, `shaft.dxf`, `shaft.sheet.json` (`drawing.sheet`).

Reproduce (with the reference snapshot's `solidcraft-cli`):

```sh
solidcraft-cli run shaft.json --save shaft.solidcraft          # rebuild the design from the recipe
solidcraft-cli exec drawing.export '{"path":"'$PWD'/shaft.pdf"}' --in shaft.solidcraft
solidcraft-cli exec drawing.export '{"path":"'$PWD'/shaft.dxf"}' --in shaft.solidcraft
solidcraft-cli exec drawing.sheet '{}' --in shaft.solidcraft > shaft.sheet.json
```

## The part as modelled

- One closed (x, r) profile revolved 360° about X: Ø20 from x = 0 to 30, Ø28 from 30 to 80, Ø16 from 80 to 120.
- Blind bore Ø8 from the x = 120 face, 20 mm of cylindrical depth, then a 118° point (half angle 59°, point height 2.403442, apex at x = 97.5965575).
- Cross hole Ø6 along Z through the Ø28 step at x = 55 (a `solid.cylinder` r 3 cut).
- Volume, hand: 46 422.173 mm³; `solidcraft-cli eval`: 46 420.376 mm³ (0.0039 %, facet tolerance).

## View basis, units, paper

- **Projection: first angle (ISO method 1)**, stated explicitly; the sheet's own default, unchanged.
- Views: `front`, `right` (`view_choice` = `turned`, arrangement `first-angle`).
- Units: millimetres (model, sheet and DXF `$INSUNITS` = 4).
- Paper: A4 landscape (297 × 210 mm). Scale **1:1**, chosen by the automatic scale picker (no scale given).
- Section: a local break-out in the front view (rule: a solid turned part gets one break-out per internal feature).

## Expected dimensions (hand-derived) vs. the automatic sheet

| Feature | Expected | On the sheet | Status |
|---|---|---|---|
| diameter, step 1 | Ø20 | Ø20 | ok |
| diameter, step 2 | Ø28 | Ø28 | ok |
| diameter, step 3 | Ø16 | Ø16 | ok |
| length of step 1 | 30 | 30 | ok |
| length of step 2 | 50 | 50 | ok |
| length of step 3 | 40 | — | derived (closing link of the chain 30 + 50 + 40 = 120) |
| overall length | 120 | 120 | ok |
| cross hole position from the left end | 55 | 55 | ok |
| cross hole | Ø6 through | `Bohrung Ø6 durchgehend` | ok |
| blind bore | Ø8 × 20 deep | `Bohrung Ø8 x 20 tief` | ok |
| drill point | 118° (by convention not dimensioned) | — | ok (convention) |

Sheet lists: `unplaced` = []; `undimensioned` = 0 face(s).

## Checks, kept separate

### 1. Drawing geometry

- True curves in the DXF vs. the hand-derived B-rep circles and arcs: **4/4 matched**. Max deviation: radius 0.0 mm, centre 0.0 mm, arc end angle 0.0°. Read from the CIRCLE/ARC parameters back through the view origin and scale; see `expected.json`.
- The prototype's engine tests also compare sampled points of the drawn arcs with the B-rep, within 0.01 mm. That is **measured evidence at the sampled points, not an analytic bound** between the samples.

### 2. Annotation placement

- `unplaced` is empty. The prototype's own tests assert placement rules on this part: the arrow tip sits on its edge, no dimension line lies on a drawn edge, and a break-out stays inside the outline.
- No independent overlap checker is part of this pack. The observations above are from the rendered sheet.

### 3. Update / undo

- **Not offered.** The prototype has no editable annotations (no model-edit/defer/update, no manually placed labels, no annotation transfer on delete, no save/reopen/undo of annotations). These cases belong to the shared commands as they are delivered.

### 4. Exported physical scale

- PDF page: 297.0 × 210.002 mm. The visible outline (0.5 mm strokes, Béziers sampled) of the `front` view measures **120.0 × 28.0 mm**; expected 120 × 28 mm (part size × 1:1).
- DXF: model space holds the sheet in paper millimetres, with the views already scaled by 1:1. Layout `Blatt 1` has one VIEWPORT at 1:1. So model space is **not** in part units (see the DXF matrix).
