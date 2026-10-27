# #860 DXF findings matrix (no writer code)

**These are findings, not a second writer.** They are input for the shared chain X1 #5318 → X2 #5319 → X3 #5320 → X4
#5321 and Cam's paper-DXF adapter #4890, as measured on the prototype's export.

**Structural observations only.** An entity count and a clean audit say nothing about scale, curve identity, visible
geometry or dimension semantics. Those are the separate columns below. X1 keeps **source-certified circle identity**
and the **53 flat-pattern byte pins**. We are **not** proposing a looser near-circle tolerance. The prototype's
silhouette fit is a fallback for HLR chains that have no analytic edge behind them, not a route for X1.

## How it was measured

- **Fixtures:** the six parts of the fixture pack (`fixtures/<part>/`), all original SolidCraft documents.
  - The development parts `shaft`, `crank`, `flywheel` (recipes included as `fixtures/<part>/<part>.json`);
  - the blind-test parts `flange`, `bracket`, `housing`.
  - A development part that is not original is excluded.
- **Build / SHA:** the reference snapshot **`60661182`** (`drawing-prototype-reference`), `solidcraft-cli 0.24.0`,
  release. It was cut from `67776441`. Exports from both builds were compared: with the old layer names mapped to the
  new ones (REFERENCE.md, table "DXF layers"), the DXFs have 0 differences, the PDFs are byte-identical and the sheet
  JSON is equal. The "67776441" in the first column means the same output.
- **Export settings:** defaults only, `solidcraft-cli exec drawing.export '{"path":"…/<part>.dxf"}' --in <part>.solidcraft`.
  - automatic views, scale, dimensions and sections;
  - `tangent_edges` "omit";
  - A4 landscape, first angle.
- **Tools:** `analyse.py` (ezdxf 1.4.5 + PyMuPDF 2.0) reads each file. `compare.py` checks it against the
  hand-derived `expected.json`. The raw results are in `measure/snapshot/*.json` (identical to `measure/installed/`).

## The matrix

| Fixture + SHA | Export settings + version | Model vs. paper units | Curve parameters vs. B-rep | Layers + lineweights | Displayed dimension text vs. measured | Validator + version | Entities before → after true arcs |
|---|---|---|---|---|---|---|---|
| `shaft` (`90ad6ef5…`), 67776441 | defaults; AC1015 (R2000), `$INSUNITS` 4 mm, `$MEASUREMENT` 1, `$LWDISPLAY` 1; scale 1:1 | Model space = the whole sheet in paper mm, views pre-scaled (1:1 here). Paper space `Blatt 1`: 1 VIEWPORT 297 × 210 at 1:1 + 1 INSERT (frame/title/notes block). | right view: 4/4 CIRCLE exact (r 4, 8, 14 visible; r 10 hidden), centre on the axis, Δ = 0. The front view's silhouettes are LINEs. | VISIBLE 0.50 (167), HIDDEN 0.25 HIDDEN (131), DIMENSION 0.25 (15), CENTER 0.25 CENTER (4), HATCH 0.25 (4), FREEHAND 0.25 (2); every entity BYLAYER (370 = −1) | 7 DIMENSION: Ø20 Ø28 Ø16 30 50 120 55. Displayed = group 42 = defpoint distance ÷ scale = hand value | ezdxf 1.4.5 `audit`: 0 errors, 0 fixes | **4623 → 323** model space (4550 LINE → 300). With the 82-entity sheet block: 4623 → 405. Before = default export from the D9.1 build `79ab4323` (2026-10-24, the last before true arcs, all in model space); after = 67776441 |
| `crank` (`6d333977…`), 67776441 | as above; 1:1 | as above | top view: CIRCLE r 8 @ (0, 0), r 5 @ (60, 0); ARC r 20 82.338°→277.662°, ARC r 12 277.662°→82.338° (hand θ = acos(8/60) = 82.3377°); Δr = Δc = 0, Δangle 0.0003° | VISIBLE (10), HIDDEN (4), CENTER (5), DIMENSION (8) | 8: 92 40 10 Ø16 Ø10 60 R12 R20. All equal group 42 and the hand values | 0 / 0 | **486 → 27** (with sheet block 109) |
| `flywheel` (`0da19915…`), 67776441 | as above; **1:2** | as above. Model space at 1:2, so a model-space measurement reads half size. | top view: 4/4 CIRCLE exact (r 8, 20, 45, 60), Δ = 0 | VISIBLE (30), HIDDEN (1), CENTER (3), DIMENSION (6), HATCH (2), SECTION 0.50 (4), SECTION_LINE 0.25 ISO_SECTION (1), TEXT (3) | 6: Ø120 Ø40 20 35 Ø16 `Ø90 x 10 tief`. Displayed = group 42 = hand value. **DIMLFAC is not set, and the 2 linear dimensions have empty text (`<>`).** Their defpoints span 10 and 17.5 sheet mm, so a reader that regenerates the dimension would show **10 and 17.5, not 20 and 35** | 0 / 0 | **2542 → 50** (with sheet block 132) |
| `flange` (`d106deb7…`), 67776441 | as above; 1:2 | as above | top view: 11/11 CIRCLE exact: r 15, 29, 30, 59, 60, and 6 × r 4.5 at (±47.5, 0), (±23.75, ±41.1362). PCD 95 in the geometry, but no PCD dimension. | VISIBLE (29), HIDDEN (12), CENTER (13), DIMENSION (6), HATCH (2), FREEHAND (1) | 2: Ø120 Ø60, both with override text, so unaffected by DIMLFAC. 6 of the 8 expected sizes are missing (fixture pack). | 0 / 0 | n/a (made after the true-arc change): 63 (+82) |
| `bracket` (`60dd84f8…`), 67776441 | as above; **1:5** | as above. Model space at 1:5. | 6/6 exact: front ARC r 16 / r 8 @ (16, 16) 180°→270°; top CIRCLE r 3.3 @ (55, 15), (55, 45); right slot ARC r 3.3 @ (23.3, 33) and (36.7, 33), centres 13.4 apart. **Plus 5 short hidden ARC slivers (2°–7°) lying on the visible slot arc.** | VISIBLE (27), HIDDEN (13), CENTER (12), DIMENSION (13) | 9, displayed = group 42 = hand value. **6 linear dimensions with empty text and no DIMLFAC: a regeneration reads ÷5** (80 → 16) | 0 / 0 | n/a: 65 (+82) |
| `housing` (`85b20392…`), 67776441 | as above; 1:5 | as above | 9/9 exact: CIRCLE r 17.5 @ (50, 30), r 5.5 and r 3.3 @ (12, 30) and (88, 30); pocket ARC r 5 at (30\|70, 15\|45), each 90°. **Plus 2 hidden ARC slivers (1.5°) on the visible Ø6.6 circles.** | VISIBLE (25), HIDDEN (25), CENTER (8), DIMENSION (10) | 10, displayed = group 42 = hand value (the *values* are right; the meaning of `Ø35 x 35 tief` is wrong, see the fixture pack). **7 linear dimensions with empty text, no DIMLFAC: a regeneration reads ÷5.** | 0 / 0 | n/a: 68 (+82) |

Layer table, the same in every file: `0`, FRAME 0.70, VISIBLE 0.50, HIDDEN 0.25 (HIDDEN), THIN 0.25, CENTER 0.25 (CENTER),
DIMENSION 0.25, HATCH 0.25, SECTION 0.50, SECTION_LINE 0.25 (ISO_SECTION), TANGENT 0.25, FREEHAND 0.25, TITLE 0.25,
TEXT 0.25, TITLE_BLOCK 0.25, VIEWPORT 0.25, `Defpoints`. One ISO 128-2 line group (0.5 / 0.25) plus the 0.7 frame. The
counts in the matrix are model-space entities per layer; the frame, title and notes sit in the paper-space block.

The PDF of each sheet was measured too, for the physical scale. The page is 297.0 × 210.0 mm. The visible outline of
the main view (0.5 mm strokes, Béziers sampled) equals part size × scale to within 0.001 mm on all six:

| Part | View | Measured | Expected |
|---|---|---|---|
| shaft | front | 120.0 × 28.0 | 120 × 28 |
| crank | top | 92.0 × 39.999 | 92 × 40 |
| flywheel | front | 60.0 × 17.5 | 60 × 17.5 |
| flange | front | 59.999 × 20.0 | 60 × 20 |
| bracket | front | 16.0 × 10.0 | 16 × 10 |
| housing | front | 20.0 × 8.0 | 20 × 8 |

## Findings

1. **Curve identity holds where the B-rep has an analytic edge.** Every expected circle and arc (38 of 38) is a real
   CIRCLE/ARC whose centre, radius and end angles match the hand-derived B-rep values exactly (Δ ≤ 0.0003°). They come
   from the kernel's exact edge circle, not from re-fitting. ELLIPSE (an oblique circle) did not occur in these six;
   the test plate on the reference branch has two.
2. **Silhouettes of curved faces stay LINEs.** On the shaft, for example, the 300 LINEs are the visible (164), hidden (130)
   and centre (4) lines of the views, including every cylinder silhouette; 2 more are leader tails. The prototype fits an arc to such a chain only when every sample stays within 0.005 mm of every chord.
   That is a measured acceptance at the samples, **not** an analytic bound, and not a near-circle tolerance for X1.
3. **Model space is in paper units, not part units.** The views are pre-scaled into one sheet-sized model space, and
   paper space shows it through a 1:1 viewport. On a 1:2 or 1:5 sheet, a CAD user measuring in model space reads half
   or a fifth of the part. The paper-DXF adapter (#4890) has to decide this explicitly. The usual convention is part
   units in model space and the drawing scale in the viewport.
4. **Dimension semantics depend on cached blocks on scaled sheets.** Group 42 and the cached anonymous block carry the
   true value, but DIMLFAC is unset. Linear dimensions without override text would regenerate at sheet size (÷2, ÷5)
   in a reader that recomputes them. Affected: 2 of 6 dimensions on the flywheel, 6 of 9 on the bracket, 7 of 10 on the
   housing; none on the 1:1 sheets. Diameter and radius dimensions carry override text and are not affected.
5. **Hidden-line slivers.** The bracket has 5 and the housing 2 short hidden ARC pieces (1.5°–7°) lying on top of a
   visible arc of the same circle. A visibility-split rounding artefact; harmless when plotted, but noise for a reader
   that counts entities.
6. **Entity count.** True arcs, collinear merging and moving the frame/title into one paper-space block cut the shaft
   from 4623 to 323 model-space entities (×14; 405 with the block), the crank from 486 to 27 and the flywheel from 2542
   to 50. The before files are the D9.1 build's default exports (`79ab4323`), which reproduce the figures in the #860
   post. The post's "325" was the in-run count of the D10 build; 67776441 (after D10.1) writes 323. This is a structural/performance observation only (see the
   top of this page).
7. **Lineweights** are layer data (group 370) in one ISO 128-2 group, with every entity BYLAYER. Text uses CIF escapes
   (`\U+00D8` for Ø), as AC1015 requires.

## Not in this matrix

- A second writer, and any change to X1's circle identity or byte pins.
- Results from any external drawing checker or from opening the files in commercial CAD.
- A development part that is not original.
