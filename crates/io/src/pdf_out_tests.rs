use super::*;

fn page(width_mm: f64, height_mm: f64, items: Vec<PdfItem>) -> PdfPage {
    PdfPage { width_mm, height_mm, items, title: "Test".into() }
}

fn text(at: [f64; 2], height_mm: f64, angle_deg: f64, value: &str, anchor: TextAnchor, bold: bool) -> PdfItem {
    PdfItem::Text { at, height_mm, angle_deg, text: value.into(), anchor, bold }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|window| window == needle)
}

/// The bytes of object `number`'s stream, using the object's own `/Length`.
fn stream_of(pdf: &[u8], number: usize) -> Vec<u8> {
    let object = format!("{number} 0 obj\n");
    let start = find(pdf, object.as_bytes()).unwrap();
    let length_at = find(&pdf[start..], b"/Length ").unwrap() + start + 8;
    let length_end = find(&pdf[length_at..], b" ").unwrap() + length_at;
    let length: usize = std::str::from_utf8(&pdf[length_at..length_end]).unwrap().parse().unwrap();
    let data = find(&pdf[start..], b"stream\n").unwrap() + start + 7;
    let end = data + length;
    assert_eq!(&pdf[end..end + 9], b"endstream");
    pdf[data..end].to_vec()
}

fn content_of(pdf: &[u8]) -> String {
    // Text is WinAnsi (one byte per character), not UTF-8: lossy is enough for the ASCII syntax.
    String::from_utf8_lossy(&stream_of(pdf, 4)).into_owned()
}

fn file_text(pdf: &[u8]) -> String {
    // The header's binary comment is deliberately not UTF-8.
    String::from_utf8_lossy(pdf).into_owned()
}

/// The six numbers before `operator` in a content stream.
fn operands(content: &str, operator: &str, count: usize) -> Vec<f64> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let at = tokens.iter().position(|token| *token == operator).unwrap();
    tokens[at - count..at].iter().map(|token| token.parse().unwrap()).collect()
}

#[test]
fn text_width_uses_the_written_font_metrics_and_winansi() {
    // AFM widths in 1/1000 em at font size height/0.718: H 722, i 222, - 333.
    let em = |units: f64| 10.0 / CAP_HEIGHT * units / 1000.0;
    assert!((text_width_mm("H", 10.0) - em(722.0)).abs() < 1e-12);
    assert!((text_width_mm("Hi-", 10.0) - em(722.0 + 222.0 + 333.0)).abs() < 1e-12);
    // ä (WinAnsi 0xE4) and € (0x80) are mapped; an unmapped character is '?' (556).
    assert!((text_width_mm("ä", 10.0) - em(556.0)).abs() < 1e-12);
    assert!((text_width_mm("€", 10.0) - em(556.0)).abs() < 1e-12);
    assert!((text_width_mm("→", 10.0) - em(556.0)).abs() < 1e-12);
    assert_eq!(text_width_mm("", 10.0), 0.0);
    assert!((text_width_mm("Hello", 5.0) * 2.0 - text_width_mm("Hello", 10.0)).abs() < 1e-12);
}

#[test]
fn every_xref_offset_points_at_its_object() {
    let line = PdfItem::Path { points: vec![[1.0, 2.0], [3.0, 4.0]], closed: false, width_mm: 0.5, dash_mm: vec![], fill: false };
    let pdf = write_pdf(&page(297.0, 210.0, vec![line, text([5.0, 5.0], 3.0, 0.0, "Hallo", TextAnchor::Center, false)])).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4\n%"));
    let start = rfind(&pdf, b"startxref\n").unwrap() + 10;
    let end = find(&pdf[start..], b"\n").unwrap() + start;
    let xref_at: usize = std::str::from_utf8(&pdf[start..end]).unwrap().parse().unwrap();
    assert_eq!(&pdf[xref_at..xref_at + 5], b"xref\n");
    // The subsection header: first object 0, eight entries (the free one and seven objects).
    let mut pos = xref_at + 5;
    let line_end = find(&pdf[pos..], b"\n").unwrap() + pos;
    let header = std::str::from_utf8(&pdf[pos..line_end]).unwrap();
    let mut fields = header.split_whitespace();
    assert_eq!(fields.next(), Some("0"));
    let size: usize = fields.next().unwrap().parse().unwrap();
    assert_eq!(size, 8);
    pos = line_end + 1;
    // Entry 0 is the free entry; every other offset begins `N 0 obj`.
    assert_eq!(&pdf[pos..pos + 20], b"0000000000 65535 f \n");
    pos += 20;
    for number in 1..size {
        let entry = &pdf[pos..pos + 20];
        pos += 20;
        assert_eq!(&entry[17..18], b"n");
        assert_eq!(&entry[10..11], b" ");
        let offset: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
        let want = format!("{number} 0 obj\n");
        assert_eq!(&pdf[offset..offset + want.len()], want.as_bytes(), "object {number} starts at its xref offset");
    }
    let trailer = std::str::from_utf8(&pdf[pos..pos + 60]).unwrap();
    assert!(trailer.starts_with("trailer\n<< /Size 8 /Root 1 0 R /Info 7 0 R >>"), "{trailer}");
    assert!(pdf.ends_with(b"%%EOF\n"));
}

#[test]
fn a4_landscape_has_the_expected_media_box_in_points() {
    let file = file_text(&write_pdf(&page(297.0, 210.0, vec![])).unwrap());
    assert!(file.contains("/MediaBox [0 0 841.89 595.28]"), "{file}");
    // A4 portrait the other way round, and a page whose sides are given in whole millimetres.
    let file = file_text(&write_pdf(&page(210.0, 297.0, vec![])).unwrap());
    assert!(file.contains("/MediaBox [0 0 595.28 841.89]"));
    let file = file_text(&write_pdf(&page(100.0, 50.0, vec![])).unwrap());
    assert!(file.contains("/MediaBox [0 0 283.46 141.73]"), "{file}");
}

#[test]
fn a_known_line_is_true_scale_under_the_centimetre_transform() {
    let line = PdfItem::Path { points: vec![[10.0, 20.0], [30.0, 40.0]], closed: false, width_mm: 0.5, dash_mm: vec![4.0, 1.5], fill: false };
    let pdf = write_pdf(&page(100.0, 100.0, vec![line])).unwrap();
    let content = content_of(&pdf);
    // The `cm` maps millimetres to points: 72 / 25.4 in both scale slots, no skew.
    let cm = operands(&content, "cm", 6);
    assert!(cm.iter().zip([PT_PER_MM, 0.0, 0.0, PT_PER_MM, 0.0, 0.0]).all(|(a, b)| (a - b).abs() < 1e-6), "{cm:?}");
    // The moveto and lineto operands, transformed by that matrix, are the endpoints in points.
    let m = operands(&content, "m", 2);
    let l = operands(&content, "l", 2);
    let to_page = |p: &[f64]| [cm[0] * p[0] + cm[2] * p[1] + cm[4], cm[1] * p[0] + cm[3] * p[1] + cm[5]];
    let (start, end) = (to_page(&m), to_page(&l));
    let mm = |x: f64| x * 72.0 / 25.4;
    assert!((start[0] - mm(10.0)).abs() < 1e-3 && (start[1] - mm(20.0)).abs() < 1e-3, "{start:?}");
    assert!((end[0] - mm(30.0)).abs() < 1e-3 && (end[1] - mm(40.0)).abs() < 1e-3, "{end:?}");
    // A 0.5 mm stroke, round caps and joins, black, and the dash array in millimetres.
    let tokens: Vec<&str> = content.split_whitespace().collect();
    assert!(tokens.windows(2).any(|w| w == ["0.5", "w"]));
    assert!(content.contains("1 J 1 j 0 G"));
    let d = tokens.iter().position(|token| *token == "d").unwrap();
    assert_eq!(&tokens[d - 5..d], ["[", "4.0", "1.5", "]", "0"]);
    // Content is uncompressed and its /Length is exact.
    assert!(content.starts_with("q\n") && content.ends_with("Q\n"));
}

#[test]
fn winansi_maps_the_drawing_characters_and_never_panics() {
    assert_eq!(
        winansi_bytes("Maßstab Ø20 ±0,1 °"),
        [0x4d, 0x61, 0xdf, 0x73, 0x74, 0x61, 0x62, 0x20, 0xd8, 0x32, 0x30, 0x20, 0xb1, 0x30, 0x2c, 0x31, 0x20, 0xb0]
    );
    assert_eq!(winansi_bytes("äöüÄÖÜß µ²×"), [0xe4, 0xf6, 0xfc, 0xc4, 0xd6, 0xdc, 0xdf, 0x20, 0xb5, 0xb2, 0xd7]);
    // Windows-1252 punctuation keeps its single byte: é, €, en dash, curly quotes, ….
    assert_eq!(winansi_bytes("é€–“”…"), [0xe9, 0x80, 0x96, 0x93, 0x94, 0x85]);
    // Everything else — CJK, a typographic minus, control characters — is a question mark.
    assert_eq!(winansi_bytes("中\n\u{2212}\u{1f600}"), b"????");
    assert_eq!(winansi_bytes(""), Vec::<u8>::new());
}

#[test]
fn a_pdf_string_escapes_parentheses_and_backslashes() {
    let item = text([10.0, 10.0], 3.0, 0.0, "A(B)\\C", TextAnchor::BottomLeft, false);
    let pdf = write_pdf(&page(100.0, 100.0, vec![item])).unwrap();
    let content = content_of(&pdf);
    assert!(content.contains("(A\\(B\\)\\\\C) Tj"), "{content}");
    // The umlaut is one WinAnsi byte in the stream, not UTF-8.
    let item = text([10.0, 10.0], 3.0, 0.0, "Größe", TextAnchor::BottomLeft, false);
    let pdf = write_pdf(&page(100.0, 100.0, vec![item])).unwrap();
    let content = stream_of(&pdf, 4);
    assert!(find(&content, b"(Gr\xf6\xdfe) Tj").is_some());
}

#[test]
fn all_nine_anchors_put_the_box_corner_at_the_anchor() {
    let height = 3.5;
    let font_size = height / 0.718;
    let width = font_size * 0.667; // Helvetica "A" is 667/1000 em.
    let cases = [
        (TextAnchor::BottomLeft, 0.0, 0.0),
        (TextAnchor::BottomCenter, -width / 2.0, 0.0),
        (TextAnchor::BottomRight, -width, 0.0),
        (TextAnchor::MiddleLeft, 0.0, -height / 2.0),
        (TextAnchor::Center, -width / 2.0, -height / 2.0),
        (TextAnchor::MiddleRight, -width, -height / 2.0),
        (TextAnchor::TopLeft, 0.0, -height),
        (TextAnchor::TopCenter, -width / 2.0, -height),
        (TextAnchor::TopRight, -width, -height),
    ];
    for (anchor, dx, dy) in cases {
        let pdf = write_pdf(&page(100.0, 100.0, vec![text([50.0, 50.0], height, 0.0, "A", anchor, false)])).unwrap();
        let content = content_of(&pdf);
        let tm = operands(&content, "Tm", 6);
        assert!((tm[0] - 1.0).abs() < 1e-9 && tm[1].abs() < 1e-9 && tm[2].abs() < 1e-9 && (tm[3] - 1.0).abs() < 1e-9);
        assert!((tm[4] - (50.0 + dx)).abs() < 1e-4, "{anchor:?}: {}", tm[4]);
        assert!((tm[5] - (50.0 + dy)).abs() < 1e-4, "{anchor:?}: {}", tm[5]);
        // The font size follows the cap-height convention (a DXF TEXT height), in mm.
        let tf = operands(&content, "Tf", 1)[0];
        assert!((tf - font_size).abs() < 1e-3, "{anchor:?}: {tf} vs {font_size}");
        assert!(content.contains("/F1 "));
    }
}

#[test]
fn rotation_turns_the_box_about_the_anchor() {
    // Top-left of the box at `at`, turned a quarter turn: the anchor stays the box's top-left
    // corner in its own (turned) frame, so the box stands up from `at` and the baseline runs
    // upwards.
    let pdf = write_pdf(&page(100.0, 100.0, vec![text([50.0, 50.0], 3.5, 90.0, "A", TextAnchor::TopLeft, false)])).unwrap();
    let content = content_of(&pdf);
    let tm = operands(&content, "Tm", 6);
    assert!(tm[0].abs() < 1e-6 && (tm[1] - 1.0).abs() < 1e-6 && (tm[2] + 1.0).abs() < 1e-6 && tm[3].abs() < 1e-6, "{tm:?}");
    assert!((tm[4] - (50.0 + 3.5)).abs() < 1e-4 && (tm[5] - 50.0).abs() < 1e-4, "{tm:?}");
    // And a negative angle turns the other way: (cos, sin) = (√3/2, −1/2).
    let pdf = write_pdf(&page(100.0, 100.0, vec![text([20.0, 30.0], 3.0, -30.0, "A", TextAnchor::BottomLeft, false)])).unwrap();
    let tm = operands(&content_of(&pdf), "Tm", 6);
    assert!((tm[0] - 0.866_025_403_784_438_6).abs() < 1e-6 && (tm[1] + 0.5).abs() < 1e-6, "{tm:?}");
    assert!((tm[4] - 20.0).abs() < 1e-4 && (tm[5] - 30.0).abs() < 1e-4, "{tm:?}");
}

#[test]
fn bold_text_uses_helvetica_bold_and_its_own_advance_width() {
    let height = 3.5;
    let font_size = height / 0.718;
    let width = font_size * 0.722; // Helvetica-Bold "A" is 722/1000 em.
    let pdf = write_pdf(&page(100.0, 100.0, vec![text([50.0, 50.0], height, 0.0, "A", TextAnchor::Center, true)])).unwrap();
    let content = content_of(&pdf);
    assert!(content.contains("/F2 "), "{content}");
    let tm = operands(&content, "Tm", 6);
    assert!((tm[4] - (50.0 - width / 2.0)).abs() < 1e-4, "{}", tm[4]);
    // Both fonts are standard Helvetica faces with WinAnsi encoding, not embedded.
    let file = file_text(&pdf);
    assert!(file.contains("/BaseFont /Helvetica /Encoding /WinAnsiEncoding"));
    assert!(file.contains("/BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding"));
    assert!(!file.contains("/FontFile"));
}

#[test]
fn paths_close_fill_and_dash_as_asked() {
    let path = |closed: bool, fill: bool, dash: Vec<f64>| PdfItem::Path {
        points: vec![[10.0, 10.0], [20.0, 10.0], [20.0, 20.0]],
        closed,
        width_mm: 0.3,
        dash_mm: dash,
        fill,
    };
    let content = |item| content_of(&write_pdf(&page(100.0, 100.0, vec![item])).unwrap());
    assert!(content(path(true, true, vec![])).contains("h\nf\n"));
    assert!(content(path(true, false, vec![])).contains("h\ns\n"));
    // Filling an open path is not a closed area: it is only stroked.
    assert!(content(path(false, true, vec![])).contains("S\n"));
    assert!(content(path(false, false, vec![])).contains("S\n"));
    // A solid line still resets the dash array; an empty dash array is `[]`.
    assert!(content(path(false, false, vec![])).contains("[ ] 0 d"));
    // A one-point path is a dot: a zero-length segment with round caps.
    let dot = PdfItem::Path { points: vec![[5.0, 5.0]], closed: false, width_mm: 0.5, dash_mm: vec![], fill: false };
    let content = content_of(&write_pdf(&page(10.0, 10.0, vec![dot])).unwrap());
    assert!(content.contains("5.0 5.0 m\n5.0 5.0 l\nS\n"), "{content}");
}

#[test]
fn hostile_pages_are_errors_not_panics() {
    let good = || page(100.0, 100.0, vec![]);
    for (width, height) in [(f64::NAN, 100.0), (100.0, f64::INFINITY), (0.0, 100.0), (-5.0, 100.0), (2000.1, 100.0), (100.0, 0.0)] {
        assert!(matches!(write_pdf(&page(width, height, vec![])), Err(PdfError::PageSize { .. })), "{width} × {height}");
    }
    let bad_path = |points: Vec<[f64; 2]>, width_mm: f64, dash_mm: Vec<f64>| PdfItem::Path { points, closed: true, width_mm, dash_mm, fill: false };
    for points in [vec![[f64::NAN, 0.0]], vec![[0.0, f64::INFINITY]], vec![[0.0, 0.0], [1.0, f64::NEG_INFINITY]]] {
        assert!(matches!(write_pdf(&page(100.0, 100.0, vec![bad_path(points, 0.5, vec![])])), Err(PdfError::Item { index: 0, .. })));
    }
    for width_mm in [0.0, -0.5, 10.1, f64::NAN, f64::INFINITY] {
        assert!(matches!(write_pdf(&page(100.0, 100.0, vec![bad_path(vec![[0.0, 0.0]], width_mm, vec![])])), Err(PdfError::Item { .. })));
    }
    for dash in [vec![-1.0], vec![0.0], vec![0.0, 0.0], vec![f64::NAN], vec![1.0; 65]] {
        assert!(matches!(write_pdf(&page(100.0, 100.0, vec![bad_path(vec![[0.0, 0.0]], 0.5, dash)])), Err(PdfError::Item { .. })));
    }
    for (at, height, angle) in [
        ([f64::NAN, 0.0], 3.0, 0.0),
        ([0.0, f64::INFINITY], 3.0, 0.0),
        ([0.0, 0.0], 0.0, 0.0),
        ([0.0, 0.0], -1.0, 0.0),
        ([0.0, 0.0], 2001.0, 0.0),
        ([0.0, 0.0], 3.0, f64::NAN),
    ] {
        assert!(matches!(write_pdf(&page(100.0, 100.0, vec![text(at, height, angle, "x", TextAnchor::Center, false)])), Err(PdfError::Item { .. })));
    }
    // The limits themselves are still accepted.
    assert!(write_pdf(&page(2000.0, 2000.0, vec![bad_path(vec![[0.0, 0.0]], 10.0, vec![])])).is_ok());
    assert!(write_pdf(&page(1.0, 1.0, vec![bad_path(vec![[0.0, 0.0]], 0.001, vec![0.0, 1.0])])).is_ok());
    // More than a million items is refused before anything is written.
    let mut items = Vec::with_capacity(MAX_ITEMS + 1);
    items.resize_with(MAX_ITEMS + 1, || PdfItem::Path { points: Vec::new(), closed: false, width_mm: 0.5, dash_mm: Vec::new(), fill: false });
    assert!(matches!(write_pdf(&PdfPage { items, ..good() }), Err(PdfError::TooManyItems { count: 1_000_001 })));
    // A title with characters outside WinAnsi is still a valid document.
    let mut titled = good();
    titled.title = "Zeichnung (Test) \\ ä 中".into();
    let pdf = write_pdf(&titled).unwrap();
    assert!(find(&pdf, b"/Title (Zeichnung \\(Test\\) \\\\ \xe4 ?)").is_some());
    assert!(find(&pdf, b"/Producer (SolidCraft)").is_some());
}

#[test]
fn empty_pages_are_valid_documents() {
    let pdf = write_pdf(&page(10.0, 10.0, vec![])).unwrap();
    let file = file_text(&pdf);
    assert!(file.contains("/Count 1") && file.contains("/Contents 4 0 R"));
    assert!(file.contains("stream\nq\n2.83464567 0 0 2.83464567 0 0 cm\n1 J 1 j 0 G\nQ\nendstream"));
}

/// The A4 landscape sample the PyMuPDF check reads: frame, dashed line, a circle as a 72-gon,
/// all nine anchors, rotated text, umlauts and a bold title.
fn sample_page() -> PdfPage {
    // ISO 5457 A4 landscape: 20 mm margin on the left, 10 mm on the other sides, 0.7 mm frame;
    // a title with a rule under it, and a dashed 0.35 mm line.
    let mut items = vec![
        PdfItem::Path {
            points: vec![[20.0, 10.0], [287.0, 10.0], [287.0, 200.0], [20.0, 200.0]],
            closed: true,
            width_mm: 0.7,
            dash_mm: Vec::new(),
            fill: false,
        },
        text([22.0, 194.0], 5.0, 0.0, "SolidCraft — D2b Prüfblatt", TextAnchor::BottomLeft, true),
        PdfItem::Path { points: vec![[20.0, 192.0], [287.0, 192.0]], closed: false, width_mm: 0.35, dash_mm: Vec::new(), fill: false },
        PdfItem::Path { points: vec![[30.0, 175.0], [150.0, 175.0]], closed: false, width_mm: 0.35, dash_mm: vec![5.0, 2.0], fill: false },
        text([30.0, 178.0], 3.0, 0.0, "gestrichelt 0,35 mm", TextAnchor::BottomLeft, false),
    ];

    // A 0.5 mm circle, approximated by 72 points.
    let circle: Vec<[f64; 2]> = (0..72)
        .map(|i| {
            let angle = std::f64::consts::TAU * i as f64 / 72.0;
            [240.0 + 18.0 * angle.cos(), 170.0 + 18.0 * angle.sin()]
        })
        .collect();
    items.push(PdfItem::Path { points: circle, closed: true, width_mm: 0.5, dash_mm: Vec::new(), fill: false });
    items.push(text([222.0, 148.0], 3.0, 0.0, "Kreis, 72-Eck, 0,5 mm", TextAnchor::BottomLeft, false));

    // A cross at every anchor and the text placed by it.
    let anchors = [
        ([30.0, 130.0], TextAnchor::TopLeft, "TopLeft"),
        ([110.0, 130.0], TextAnchor::TopCenter, "TopCenter"),
        ([190.0, 130.0], TextAnchor::TopRight, "TopRight"),
        ([30.0, 110.0], TextAnchor::MiddleLeft, "MiddleLeft"),
        ([110.0, 110.0], TextAnchor::Center, "Center"),
        ([190.0, 110.0], TextAnchor::MiddleRight, "MiddleRight"),
        ([30.0, 90.0], TextAnchor::BottomLeft, "BottomLeft"),
        ([110.0, 90.0], TextAnchor::BottomCenter, "BottomCenter"),
        ([190.0, 90.0], TextAnchor::BottomRight, "BottomRight"),
    ];
    for (at, anchor, label) in anchors {
        for points in [vec![[at[0] - 2.0, at[1]], [at[0] + 2.0, at[1]]], vec![[at[0], at[1] - 2.0], [at[0], at[1] + 2.0]]] {
            items.push(PdfItem::Path { points, closed: false, width_mm: 0.15, dash_mm: Vec::new(), fill: false });
        }
        items.push(text(at, 3.0, 0.0, label, anchor, false));
    }

    // The WinAnsi characters the drawing needs, and rotated text.
    items.push(text([30.0, 70.0], 4.0, 0.0, "Maßstab Ø20 ±0,1 °  äöüÄÖÜß", TextAnchor::BottomLeft, false));
    items.push(text([30.0, 30.0], 3.5, 90.0, "gedreht 90°", TextAnchor::BottomLeft, false));
    items.push(text([150.0, 30.0], 3.5, 30.0, "Drehung 30°", TextAnchor::BottomLeft, false));

    PdfPage { width_mm: 297.0, height_mm: 210.0, items, title: "SolidCraft D2b Prüfblatt".into() }
}

#[test]
fn the_sample_page_is_valid() {
    let pdf = write_pdf(&sample_page()).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4\n"));
    // The frame is a closed 0.7 mm path, the circle one of 72 points.
    let content = content_of(&pdf);
    assert!(content.contains("0.7 w") && content.contains("0.5 w") && content.contains("0.35 w"));
}

#[test]
#[ignore = "writes d2b_sample.pdf to the temp directory for the PyMuPDF acceptance check"]
fn write_the_sample_pdf() {
    let pdf = write_pdf(&sample_page()).unwrap();
    let dir = std::env::temp_dir().join("solidcraft-d2b");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("d2b_sample.pdf");
    std::fs::write(&path, &pdf).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), pdf);
}

/// The bezier control points of a content stream's path: the `m` start followed by each `c`.
fn bezier_path(content: &str) -> (Vec<[f64; 2]>, Vec<[[f64; 2]; 3]>) {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let number = |i: usize| tokens.get(i).and_then(|t| t.parse::<f64>().ok());
    let mut start = [f64::NAN; 2];
    let mut curves = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "m" => {
                start = [number(i - 2).unwrap(), number(i - 1).unwrap()];
            }
            "c" => {
                curves.push([
                    [number(i - 6).unwrap(), number(i - 5).unwrap()],
                    [number(i - 4).unwrap(), number(i - 3).unwrap()],
                    [number(i - 2).unwrap(), number(i - 1).unwrap()],
                ]);
            }
            _ => {}
        }
        i += 1;
    }
    (vec![start], curves)
}

fn bezier_at(p: [[f64; 2]; 3], start: [f64; 2], u: f64) -> [f64; 2] {
    let v = 1.0 - u;
    let w = [v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u];
    let points = [start, p[0], p[1], p[2]];
    [0, 1].map(|k| (0..4).map(|i| w[i] * points[i][k]).sum())
}

#[test]
fn an_arc_is_written_as_bezier_curves_that_stay_on_the_arc() {
    let (c, major, ratio, t0, t1) = ([100.0, 100.0], [30.0, 0.0], 1.0, 0.3, 2.5);
    let arc = PdfItem::EllipseArc { c, major, ratio, t0, t1, width_mm: 0.25, dash_mm: Vec::new() };
    let pdf = write_pdf(&page(297.0, 210.0, vec![arc])).unwrap();
    let content = content_of(&pdf);
    // No chords: a stroked arc is `m` and `c` operators only.
    assert_eq!(content.matches(" l\n").count(), 0);
    assert_eq!(content.matches(" c\n").count(), 3, "{content}");
    let (start, curves) = bezier_path(&content);
    assert!((start[0][0] - (c[0] + 30.0 * t0.cos())).abs() < 1e-4 && (start[0][1] - (c[1] + 30.0 * t0.sin())).abs() < 1e-4);
    // Every sampled point of every Bezier is within a thousandth of a millimetre of the circle.
    let mut from = start[0];
    for curve in curves {
        for u in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let p = bezier_at(curve, from, u);
            let d = ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt();
            assert!((d - 30.0).abs() < 1e-3, "a Bezier point is {d} mm from the centre: {p:?}");
        }
        from = curve[2];
    }
    assert!((from[0] - (c[0] + 30.0 * t1.cos())).abs() < 1e-4 && (from[1] - (c[1] + 30.0 * t1.sin())).abs() < 1e-4);
}

#[test]
fn a_whole_circle_is_eight_beziers_and_a_bad_arc_is_refused() {
    let circle =
        PdfItem::EllipseArc { c: [0.0, 0.0], major: [10.0, 0.0], ratio: 1.0, t0: 0.0, t1: std::f64::consts::TAU, width_mm: 0.5, dash_mm: vec![] };
    let content = content_of(&write_pdf(&page(297.0, 210.0, vec![circle])).unwrap());
    assert_eq!(content.matches(" c\n").count(), 8, "{content}");
    // An oblique circle: the same circle seen at 60°, ratio cos 60° = 0.5, is still Beziers.
    let oval = PdfItem::EllipseArc { c: [50.0, 50.0], major: [20.0, 0.0], ratio: 0.5, t0: 0.0, t1: 2.0, width_mm: 0.25, dash_mm: vec![3.0, 0.75] };
    let content = content_of(&write_pdf(&page(297.0, 210.0, vec![oval])).unwrap());
    assert!(content.contains("[ 3.0 0.75 ] 0 d") && content.matches(" c\n").count() >= 2);
    for bad in [
        PdfItem::EllipseArc { c: [0.0, 0.0], major: [1.0, 0.0], ratio: 0.0, t0: 0.0, t1: 1.0, width_mm: 0.5, dash_mm: vec![] },
        PdfItem::EllipseArc { c: [0.0, 0.0], major: [1.0, 0.0], ratio: 1.0, t0: 2.0, t1: 1.0, width_mm: 0.5, dash_mm: vec![] },
        PdfItem::EllipseArc { c: [f64::NAN, 0.0], major: [1.0, 0.0], ratio: 1.0, t0: 0.0, t1: 1.0, width_mm: 0.5, dash_mm: vec![] },
        PdfItem::EllipseArc { c: [0.0, 0.0], major: [0.0, 0.0], ratio: 1.0, t0: 0.0, t1: 1.0, width_mm: 0.5, dash_mm: vec![] },
    ] {
        assert!(write_pdf(&page(297.0, 210.0, vec![bad])).is_err());
    }
}
