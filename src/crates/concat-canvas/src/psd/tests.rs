use super::*;
fn u16b(b: &mut Vec<u8>, v: u16) {
    b.extend(v.to_be_bytes());
}
fn u32b(b: &mut Vec<u8>, v: u32) {
    b.extend(v.to_be_bytes());
}
fn block(b: &mut Vec<u8>, v: &[u8]) {
    u32b(b, v.len() as u32);
    b.extend(v);
}
fn rect(b: &mut Vec<u8>, v: [i32; 4]) {
    for x in v {
        b.extend(x.to_be_bytes());
    }
}
fn tag(b: &mut Vec<u8>, key: &[u8; 4], data: &[u8]) {
    b.extend(b"8BIM");
    b.extend(key);
    block(b, data);
    if !data.len().is_multiple_of(2) {
        b.push(0)
    }
}
#[derive(Clone)]
struct Spec {
    name: &'static str,
    rect: [i32; 4],
    section: u32,
    key: [u8; 4],
    color: [u8; 4],
    opacity: u8,
    hidden: bool,
    mask: Option<([i32; 4], u8, u8, Vec<u8>)>,
    extra: Vec<u8>,
}
impl Spec {
    fn layer(name: &'static str, rect: [i32; 4], color: [u8; 4]) -> Self {
        Self {
            name,
            rect,
            section: 0,
            key: *b"norm",
            color,
            opacity: 255,
            hidden: false,
            mask: None,
            extra: vec![],
        }
    }
    fn group(name: &'static str, section: u32) -> Self {
        let mut s = Self::layer(name, [0; 4], [0; 4]);
        s.section = section;
        s
    }
}
fn encode(data: &[u8], w: usize, h: usize, rle: bool) -> Vec<u8> {
    let mut b = vec![];
    u16b(&mut b, u16::from(rle));
    if !rle {
        b.extend(data);
        return b;
    }
    let mut rows = vec![];
    for row in data.chunks(w.max(1)).take(h) {
        let mut encoded = vec![];
        for p in row.chunks(128) {
            encoded.push((p.len() - 1) as u8);
            encoded.extend(p);
        }
        u16b(&mut b, encoded.len() as u16);
        rows.extend(encoded);
    }
    b.extend(rows);
    b
}
fn fixture(specs: &[Spec], rle: bool) -> Vec<u8> {
    let mut records = vec![];
    let mut pixels = vec![];
    for s in specs {
        rect(&mut records, s.rect);
        let w = (s.rect[3] - s.rect[1]) as usize;
        let h = (s.rect[2] - s.rect[0]) as usize;
        let mut channels = vec![];
        if s.section == 0 {
            for (i, v) in s.color.iter().enumerate() {
                channels.push((
                    if i == 3 { -1i16 } else { i as i16 },
                    encode(&vec![*v; w * h], w, h, rle),
                ));
            }
        }
        if let Some((mr, _, _, data)) = &s.mask {
            channels.push((
                -2,
                encode(
                    data,
                    (mr[3] - mr[1]) as usize,
                    (mr[2] - mr[0]) as usize,
                    rle,
                ),
            ));
        }
        u16b(&mut records, channels.len() as u16);
        for (id, data) in channels {
            u16b(&mut records, id as u16);
            u32b(&mut records, data.len() as u32);
            pixels.extend(data);
        }
        records.extend(b"8BIM");
        records.extend(s.key);
        records.extend([s.opacity, 0, if s.hidden { 2 } else { 0 }, 0]);
        let mut extra = vec![];
        let mut mask = vec![];
        if let Some((r, outside, flags, _)) = &s.mask {
            rect(&mut mask, *r);
            mask.extend([*outside, *flags, 0, 0]);
        }
        block(&mut extra, &mask);
        u32b(&mut extra, 0);
        extra.extend([0; 4]);
        let mut unicode = vec![];
        u32b(&mut unicode, s.name.encode_utf16().count() as u32);
        for c in s.name.encode_utf16() {
            u16b(&mut unicode, c)
        }
        tag(&mut extra, b"luni", &unicode);
        if s.section > 0 {
            tag(&mut extra, b"lsct", &s.section.to_be_bytes());
        }
        extra.extend(&s.extra);
        block(&mut records, &extra);
    }
    let mut layers = vec![];
    u16b(&mut layers, specs.len() as u16);
    layers.extend(records);
    layers.extend(pixels);
    if layers.len() % 2 != 0 {
        layers.push(0)
    }
    let mut lm = vec![];
    block(&mut lm, &layers);
    u32b(&mut lm, 0);
    let mut b = b"8BPS".to_vec();
    u16b(&mut b, 1);
    b.extend([0; 6]);
    u16b(&mut b, 3);
    u32b(&mut b, 6);
    u32b(&mut b, 8);
    u16b(&mut b, 8);
    u16b(&mut b, 3);
    u32b(&mut b, 0);
    u32b(&mut b, 0);
    block(&mut b, &lm);
    u16b(&mut b, 0);
    b.extend([0; 6 * 8 * 3]);
    b
}
fn parse_ok(bytes: &[u8]) -> Imported {
    parse(bytes, 8192, &AtomicBool::new(false)).unwrap()
}
fn specs() -> Vec<Spec> {
    let mut raster = Spec::layer("中文·负坐标", [-1, -2, 3, 2], [200, 50, 20, 128]);
    raster.mask = Some(([-1, -1, 3, 3], 0, 1, vec![255; 16]));
    let mut hidden = Spec::layer("隐藏", [2, 3, 4, 5], [0, 255, 0, 255]);
    hidden.hidden = true;
    hidden.opacity = 127;
    hidden.key = *b"mul ";
    vec![
        Spec::layer("底图", [0, 0, 6, 8], [40, 80, 120, 255]),
        Spec::group("边界", 3),
        Spec::group("边界", 3),
        raster,
        Spec::group("内组", 1),
        hidden,
        Spec::group("外组", 2),
    ]
}
#[test]
fn raw_rle_nested_order_names_positions_masks_and_pixels() {
    let raw = parse_ok(&fixture(&specs(), false));
    let rle = parse_ok(&fixture(&specs(), true));
    assert_eq!(raw.document, rle.document);
    assert_eq!(raw.document.root.children.len(), 2);
    let LayerNode::Group(g) = &raw.document.root.children[1] else {
        panic!()
    };
    assert_eq!(g.name, "外组");
    let LayerNode::Group(inner) = &g.children[0] else {
        panic!()
    };
    assert_eq!(inner.name, "内组");
    let LayerNode::Layer(l) = &inner.children[0] else {
        panic!()
    };
    assert_eq!(l.name, "中文·负坐标");
    assert_eq!((l.transform.x, l.transform.y), (-4.0, -2.0));
    let m = l.mask.as_ref().unwrap();
    assert_eq!(
        m.bitmap_bounds,
        Some(BitmapMaskBounds {
            left: -3,
            top: -2,
            outside: 0
        })
    );
    assert_eq!(m.anchor, Some(l.transform));
    assert!(g.children[1].hidden());
    let a = crate::compose(&raw.document, &raw.store);
    let b = crate::compose(&rle.document, &rle.store);
    assert_eq!(a.pixels(), b.pixels());
    assert_eq!(a.pixel(7, 5), Some([40, 80, 120, 255]));
    assert!(a.pixel(0, 0).unwrap()[0] > 40);
}
#[test]
fn all_thirteen_blends_are_explicit_and_unknown_rejected() {
    for key in [
        *b"norm", *b"mul ", *b"scrn", *b"over", *b"dark", *b"lite", *b"diff", *b"div ", *b"idiv",
        *b"hue ", *b"sat ", *b"colr", *b"lum ",
    ] {
        let mut s = Spec::layer("混合", [0, 0, 2, 2], [50; 4]);
        s.key = key;
        let result = parse_ok(&fixture(&[s], false));
        assert_eq!(result.document.walk()[0].blend(), blend(&key).unwrap());
    }
    for key in [*b"pass", *b"xxxx"] {
        let mut s = Spec::layer("受影响层", [0, 0, 1, 1], [255; 4]);
        s.key = key;
        let e = parse(&fixture(&[s], false), 8192, &AtomicBool::new(false))
            .err()
            .unwrap();
        assert!(e.0.contains("受影响层"));
    }
}
#[test]
fn black_white_disabled_relative_and_linked_move_preserve_off_canvas_samples() {
    for outside in [0, 255] {
        for disabled in [false, true] {
            let mut s = Spec::layer("mask", [0, 0, 3, 3], [255; 4]);
            s.mask = Some((
                [-1, -1, 2, 2],
                outside,
                if disabled { 2 } else { 0 },
                vec![128; 9],
            ));
            let mut imported = parse_ok(&fixture(&[s], true));
            let id = imported.document.root.children[0].id();
            let layer = imported.document.layer_mut(id).unwrap();
            let mut current = layer.transform;
            current.x += 2.0;
            current.y += 2.0;
            layer.transform = current;
            let m = layer.mask.as_ref().unwrap();
            assert_eq!(m.enabled, !disabled);
            let frame = imported.store.get(m.pixels).unwrap();
            assert!(
                (m.coverage(&frame, Some(&current), (8, 6), 1, 1) - 128.0 / 255.0).abs() < 0.001
            );
            assert_eq!(
                m.coverage(&frame, Some(&current), (8, 6), 7, 5),
                outside as f32 / 255.0
            );
        }
    }
}
#[test]
fn truncated_malformed_and_cancelled_fail_without_panic() {
    let bytes = fixture(&specs(), true);
    for n in 0..bytes.len() {
        assert!(
            parse(&bytes[..n], 8192, &AtomicBool::new(false)).is_err(),
            "{n}"
        );
    }
    assert!(
        parse(&bytes, 8192, &AtomicBool::new(true))
            .unwrap_err_string()
            .contains("取消")
    );
    for broken in [
        vec![0, 1, 0, 2, 127, 5],
        vec![0, 1, 0, 1, 128],
        vec![0, 1, 0, 2, 255, 4],
    ] {
        assert!(channel(&broken, 3, 1, &AtomicBool::new(false)).is_err());
    }
    assert_eq!(
        channel(&[0, 1, 0, 3, 128, 254, 7], 3, 1, &AtomicBool::new(false)).unwrap(),
        vec![7; 3]
    );
    assert!(channel(&[0, 2], 1, 1, &AtomicBool::new(false)).is_err());
}
trait ErrorString {
    fn unwrap_err_string(self) -> String;
}
impl ErrorString for Result<Imported> {
    fn unwrap_err_string(self) -> String {
        self.err().expect("error").0
    }
}
#[test]
fn unsupported_semantics_and_limits_are_controlled_errors() {
    for key in [b"TySh", b"SoLd", b"vmsk", b"lrFX", b"levl", b"zzzz"] {
        let mut s = Spec::layer("对象", [0, 0, 1, 1], [255; 4]);
        tag(&mut s.extra, key, &[]);
        let e = parse(&fixture(&[s], false), 8192, &AtomicBool::new(false)).unwrap_err_string();
        assert!(e.contains("对象"));
    }
    for at in [4, 22, 24] {
        let mut b = fixture(&specs(), false);
        b[at + 1] = 2;
        assert!(parse(&b, 8192, &AtomicBool::new(false)).is_err());
    }
    assert!(parse(&fixture(&specs(), false), 4, &AtomicBool::new(false)).is_err());
    let mut b = fixture(&specs(), false);
    b[14..18].copy_from_slice(&30_000u32.to_be_bytes());
    b[18..22].copy_from_slice(&30_000u32.to_be_bytes());
    assert!(
        parse(&b, 30_000, &AtomicBool::new(false))
            .unwrap_err_string()
            .contains("预算")
    );
    let mut stack = vec![Spec::group("过深", 3); 17];
    stack.extend(vec![Spec::group("过深", 1); 17]);
    assert!(parse(&fixture(&stack, false), 8192, &AtomicBool::new(false)).is_err());
    assert!(
        parse(
            &fixture(&[Spec::group("坏组", 1)], false),
            8192,
            &AtomicBool::new(false)
        )
        .is_err()
    );
}
#[test]
fn external_gimp_psds_preserve_real_records() {
    for bytes in [
        include_bytes!("../../tests/fixtures/psd/2layers.psd").as_slice(),
        include_bytes!("../../tests/fixtures/psd/transparentbg-gimp.psd").as_slice(),
    ] {
        let imported = parse_ok(bytes);
        assert!(!imported.document.root.children.is_empty());
        assert_eq!(imported.document.root.children[0].name(), "Фон");
        let image = crate::compose(&imported.document, &imported.store);
        assert_eq!(image.width(), imported.document.width);
        assert!(image.pixels().iter().any(|v| *v > 0));
    }
}
#[test]
#[ignore = "writes generated PSD fixture only when SEECUT_PSD_SYNTHETIC is supplied"]
fn write_synthetic() {
    let path = std::env::var_os("SEECUT_PSD_SYNTHETIC").unwrap();
    std::fs::write(path, fixture(&specs(), true)).unwrap();
}

#[cfg(feature = "gpu")]
#[test]
fn psd_actual_gpu_matches_cpu_for_blends_groups_and_moved_mask_boundaries() {
    let mut gpu = crate::CanvasGpu::new().expect("actual GPU required");
    for outside in [0, 255] {
        for blend in BlendMode::ALL {
            let mut ss = specs();
            ss[3].mask.as_mut().unwrap().1 = outside;
            let mut imported = parse_ok(&fixture(&ss, true));
            let id = imported
                .document
                .walk()
                .into_iter()
                .find(|n| n.name() == "中文·负坐标")
                .unwrap()
                .id();
            let layer = imported.document.layer_mut(id).unwrap();
            layer.blend = blend;
            layer.transform.x += 3.0;
            layer.transform.y += 2.0;
            layer.transform.rotation = 0.1;
            let expected = crate::compose(&imported.document, &imported.store);
            let actual = gpu.compose_frame(&imported.document, &imported.store);
            for (a, b) in actual.pixels().iter().zip(expected.pixels()) {
                assert!((*a as i16 - *b as i16).abs() <= 2, "{blend:?}: {a} {b}");
            }
        }
    }
}
#[test]
fn randomized_length_and_tag_mutations_are_bounded_and_never_panic() {
    let bytes = fixture(&specs(), true);
    let mut seed = 42u32;
    for _ in 0..2048 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut damaged = bytes.clone();
        let index = seed as usize % damaged.len();
        damaged[index] ^= (seed >> 24) as u8 | 1;
        let _ = parse(&damaged, 8192, &AtomicBool::new(false));
    }
}

#[test]
fn isolated_group_mask_opacity_and_disabled_state_are_not_dropped() {
    for disabled in [false, true] {
        let mut header = Spec::group("带蒙版的组", 1);
        header.opacity = 128;
        header.mask = Some(([0, 0, 6, 8], 0, if disabled { 2 } else { 0 }, vec![128; 48]));
        let imported = parse_ok(&fixture(
            &[
                Spec::group("边界", 3),
                Spec::layer("白色", [0, 0, 6, 8], [255; 4]),
                header,
            ],
            true,
        ));
        let expected = crate::compose(&imported.document, &imported.store);
        assert_eq!(
            expected.pixel(1, 1).unwrap()[3],
            if disabled { 128 } else { 64 }
        );
    }
}

#[test]
fn excessive_metadata_is_rejected_before_diagnostic_growth() {
    let mut s = Spec::layer("metadata", [0, 0, 1, 1], [255; 4]);
    for i in 0..256u16 {
        tag(&mut s.extra, &[b'z', b'z', (i >> 8) as u8, i as u8], &[]);
    }
    let e = parse(&fixture(&[s], false), 8192, &AtomicBool::new(false)).unwrap_err_string();
    assert!(e.contains("256"), "{e}");
}
