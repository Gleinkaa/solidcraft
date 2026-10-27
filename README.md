# #860 evidence pack: fixtures and DXF findings

Evidence for tangency-labs/solidcraft#860, measured on the read-only reference snapshot
[`drawing-prototype-reference`](https://github.com/Gleinkaa/solidcraft/tree/drawing-prototype-reference) at `60661182`
(one commit on `v0.24.0`; see its `REFERENCE.md`). This branch holds documents and measurements only, no code.

| Path | What |
|---|---|
| [`fixtures/`](fixtures/README.md) | six original SolidCraft parts: recipe (`<part>.json`), design (`<part>.solidcraft`), `FIXTURE.md` with hand-derived expected dimensions and the supported/unsupported matrix, and the snapshot's PDF/DXF/sheet exports in `snapshot/` |
| [`dxf_matrix.md`](dxf_matrix.md) | DXF findings matrix (findings only, no writer code) |
| `analyse.py`, `compare.py`, `expected.json` | how the exports were measured (ezdxf 1.4.5 + PyMuPDF) and the hand-derived values they are checked against |
| `measure/snapshot/` | raw measurement results |

All fixtures are original SolidCraft documents built from the included command scripts. No file here comes from
commercial CAD, a school template or a third party.
