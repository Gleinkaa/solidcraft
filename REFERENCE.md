# Drawing prototype: read-only reference snapshot

**What this is.** This is a snapshot of a working Drawing-workspace prototype, published as reference and test
evidence for [#860](https://github.com/tangency-labs/solidcraft/issues/860). **It is not a pull request and not meant to
be merged.** It keeps its own section, hidden-line and sheet paths, and it predates the shared decisions. The
integration targets are **G3** (embedded design), the shared **W17a** view frames and the **paper scene**, not this
code. Pieces that the slice owners find useful can be lifted from it individually, through their own issues.

- **Branch:** `drawing-prototype-reference`, one commit on top of tag `v0.24.0`.
- **Cut from:** `67776441c7d3f5a4a9127aacd58781bdc4ee91fa` ("Merge tag 'v0.24.0' into merge-0.24.0-v2", private fork
  `Gleinkaa/solidcraft-drawing`, branch `drawing/m1` = `02d98648` merged with `v0.24.0`).
- **Changes from that commit** (the provenance pass below; behaviour is unchanged apart from layer names):
  - `drawing-dev/` (phase briefs and reports) is removed;
  - the DXF layers have neutral names (table below), replacing a school CAD template's layer set;
  - school, person and exercise names in test values and comments are replaced by neutral ones;
  - dead test branches for a part that is not original are removed;
  - an ignored test writes its sample PDF to the temp directory;
  - this file is added.
- **Author:** Niklas (GitHub `Gleinkaa`). Written by AI coding agents, directed and reviewed by him; the commits on the
  private branch carry co-author trailers.
- **Licence:** the same as SolidCraft (MIT OR Apache-2.0). No GPL/LGPL source was read, `crates/kernel/` is not
  touched, and nothing was imported from or produced with commercial CAD.

## Build

```sh
cargo build --release -p solidcraft-cli        # the app: cargo build --release -p solidcraft
```

## Reproduce the sheets

Every sheet is one command on a design file, with defaults (automatic views, scale, dimensions, sections, title):

```sh
B=crates/engine/tests/fixtures/drawing
for part in shaft crank flywheel; do
  target/release/solidcraft-cli exec drawing.export "{\"path\":\"$PWD/$part.pdf\"}" --in $B/$part.solidcraft
  target/release/solidcraft-cli exec drawing.export "{\"path\":\"$PWD/$part.dxf\"}" --in $B/$part.solidcraft
  target/release/solidcraft-cli exec drawing.sheet '{}' --in $B/$part.solidcraft > $part.sheet.json
done
```

`drawing.sheet` returns the same sheet as data: views, dimensions with their B-rep source (`auto_dimensions`),
`unplaced` and `undimensioned` lists, layers and primitives. The sheet is A4 landscape, **first-angle projection
(ISO method 1)**, in millimetres.

The env-gated acceptance tests run on these fixtures:

```sh
SOLIDCRAFT_D71_FIXTURES=1 SOLIDCRAFT_D8_FIXTURES=1 SOLIDCRAFT_D9_FIXTURES=1 \
  cargo test --release -p solidcraft-engine drawing
```

The fixture pack (recipes, hand-derived expected dimensions, a supported/unsupported matrix, and three further
blind-test parts) and the DXF findings matrix are published with this snapshot in #860.

## Fixtures

| File | What | Origin |
|---|---|---|
| `shaft.solidcraft` | stepped shaft along X: Ø20 × 30, Ø28 × 50, Ø16 × 40 (120 overall), Ø6 cross hole at x = 55, Ø8 × 20 blind bore with a 118° point | modelled with `solidcraft-cli 0.20.0` command scripts (recipe in the fixture pack), 2026-10-23 |
| `crank.solidcraft` | 10 mm plate, outline of R20 and R12 arcs 60 mm apart joined by tangents, Ø16 and Ø10 bores | same |
| `flywheel.solidcraft` | disc Ø120 × 20, hub Ø40 × 35, Ø16 through bore, Ø90 × 10 annular recess | same |
| `test_plate.step` | small plate with tangent arcs and a hole 3° off axis (skew-axis case) | modelled in SolidCraft by the author and exported to STEP by SolidCraft |

## DXF layers

| Layer | Content | ISO 128-2 line | Width (mm) | Linetype |
|---|---|---|---|---|
| `FRAME` | frame, centring marks | continuous wide | 0.70 | CONTINUOUS |
| `VISIBLE` | visible outlines | 01.2 continuous wide | 0.50 | CONTINUOUS |
| `HIDDEN` | hidden edges | 02.1 dashed thin | 0.25 | HIDDEN |
| `THIN` | thin lines (projection symbol) | 01.1 continuous thin | 0.25 | CONTINUOUS |
| `CENTER` | centre lines and marks | 04.1 long-dash dotted thin | 0.25 | CENTER |
| `DIMENSION` | DIMENSION, LEADER, their text | 01.1 | 0.25 | CONTINUOUS |
| `HATCH` | HATCH (section hatching) | 01.1 | 0.25 | CONTINUOUS |
| `SECTION` | thick ends and arrows of a cutting plane | wide | 0.50 | CONTINUOUS |
| `SECTION_LINE` | thin trace of a cutting plane | 04.1 | 0.25 | ISO_SECTION |
| `TANGENT` | tangent edges (only with `tangent_edges: "thin"`) | 01.1 | 0.25 | CONTINUOUS |
| `FREEHAND` | break-out boundary | 01.1 freehand | 0.25 | CONTINUOUS |
| `TITLE` | title-block and notes rules | 01.1 | 0.25 | CONTINUOUS |
| `TEXT` | lettering | — | — | CONTINUOUS |
| `TITLE_BLOCK` | the paper-space INSERT of the frame/title block | — | — | — |
| `VIEWPORT` | the paper-space VIEWPORT | — | — | — |

## Provenance (short form)

| Item | Verdict |
|---|---|
| shaft / crank / flywheel | original: SolidCraft command scripts only, no imported geometry; sizes chosen for the test |
| `test_plate.step` | original, SolidCraft export |
| section icons `section_{auto,breakout,full,none}.svg` | original, in `ATTRIBUTION.md` (MIT OR Apache-2.0) |
| fonts | none bundled; the PDF uses base-14 Helvetica |
| DXF layer names | renamed from a school template's layer set to the neutral names above |
| test values (names, school, exercise number) | replaced by neutral values |
| `drawing-dev/` (development briefs and reports, a generated sample PDF) | removed |
| standards | rule names only (ISO 2768-mK, ISO 13715, ISO 7200, ISO 5455/5456/5457, ISO 128); no standards text or tables |
| an external drawing checker used during development | not included; its results are not cited as evidence |

## Known limits (from a blind test on three new parts)

The automatic sheet is not complete enough to make every part from. These are missing today:

- axial lengths on a bored flange;
- PCD hole patterns;
- slot length and position;
- pocket size and depth;
- the counterbore callout including its hole;
- through vs. blind when a hole starts on a recessed face;
- sections for prismatic parts;
- the section type for hollow turned parts;
- a scale picker that does not over-reduce;
- fits (H7).

There are no editable annotations, so the edit/update, manual-label and undo cases cannot be tested here.
