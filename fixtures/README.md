# #860 fixture pack: supplementary W23/W32 candidates

Six parts in the format asked for in #860 (0xlillith, 2026-10-27). Outputs from `snapshot` (reference snapshot `60661182`).

Each case is self-contained in its directory:

- the recipe (CLI JSON) and the design file;
- the view basis (**first angle stated explicitly**), units, paper size and scale;
- hand-derived expected dimensions with a supported/unsupported row per feature;
- four separate checks: geometry, annotation placement, update/undo (not offered) and exported physical scale.

| Part | What | Scale | Expected dims present with the right value | True curves matched |
|---|---|---|---|---|
| [`shaft`](shaft/FIXTURE.md) | Stepped shaft with cross hole and blind bore | 1:1 | 11/11 | 4/4 |
| [`crank`](crank/FIXTURE.md) | Crank plate with two bores | 1:1 | 8/8 | 4/4 |
| [`flywheel`](flywheel/FIXTURE.md) | Flywheel disc with hub, bore and recess | 1:2 | 6/6 | 4/4 |
| [`flange`](flange/FIXTURE.md) | Flange with hub, bore and bolt circle (blind test) | 1:2 | 2/8 | 11/11 |
| [`bracket`](bracket/FIXTURE.md) | L-bracket with holes and slot (blind test) | 1:5 | 6/11 | 6/6 |
| [`housing`](housing/FIXTURE.md) | Bearing block with bore, counterbores and pocket (blind test) | 1:5 | 6/11 | 9/9 |

**shaft, crank, flywheel** were modelled from scratch in SolidCraft command scripts (the recipe is included as `<part>.json`). They are the prototype's development parts, so their sheets are complete by construction. Treat them as regression cases, not as evidence of generality. **flange, bracket, housing** are the blind-test parts the prototype had never seen. Their gaps are the real coverage limits.

## Supported / unsupported today (all six parts)

| Capability | Status | Seen on |
|---|---|---|
| Ø per step, chained axial lengths, overall length (turned part) | supported | shaft, flywheel |
| Axial lengths on a turned part with a bore and an axial hole pattern | **unsupported** (no length at all) | flange |
| Hole notes: blind with depth, through (on a turned part) | supported | shaft (`Ø8 x 20 tief`, `Ø6 durchgehend`) |
| Through vs. blind on prismatic parts, depth from a recessed start face | **wrong** | bracket, housing |
| Counterbore callout including its hole | **unsupported** (the hole is dropped) | housing |
| PCD hole patterns (count, pitch circle) | **unsupported** | flange |
| Slot length/position | **unsupported** (`2x R3,3`) | bracket |
| Pocket size/depth/position | **unsupported** | housing |
| Overall sizes, hole positions from a baseline, grouped equal features (`2x`, `4x R5`) | supported | crank, bracket, housing |
| Radii on arcs and fillets | supported | crank, bracket, housing |
| Full section for a hollow turned part | supported / **misclassified** | flywheel ok; flange gets a break-out |
| Section for prismatic parts | **unsupported** | housing |
| Scale picker | over-reduces (1:5 on 80–100 mm parts) | bracket, housing |
| Fits (H7) | **cannot be modelled** | flange |
| View arrangement of turned/flat parts | `side-by-side` pair, not in a method-1 position | crank, flywheel, flange |
| True CIRCLE/ARC in DXF matching the B-rep | supported (exact on all six) | all |
| Exported physical scale (PDF) | supported (exact on all six) | all |
| Editable annotations, update/undo | **not offered** | — |

Wording: a 0.01 mm sampled B-rep comparison is **measured evidence**, not an analytic bound. The curve check in this pack reads the exact CIRCLE/ARC parameters the writer emits and compares them with hand-derived values.
