// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{Adjustment, apply_one, lookup::CubeLut};

pub(crate) fn cube(size: u32, mode: u32) -> String {
    let mut text = format!("TITLE \"Synthetic {mode}\"\nLUT_3D_SIZE {size}\n");
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let c = [
                    r as f32 / (size - 1) as f32,
                    g as f32 / (size - 1) as f32,
                    b as f32 / (size - 1) as f32,
                ];
                let v = match mode {
                    1 => [c[2], 1.0 - c[0], c[1]],
                    2 => {
                        [if r == size - 1 && g == size - 1 && b == size - 1 {
                            1.0
                        } else {
                            0.0
                        }; 3]
                    }
                    3 => c.map(|v| v * 3.0 - 1.0),
                    _ => c,
                };
                text.push_str(&format!("{} {} {}\n", v[0], v[1], v[2]));
            }
        }
    }
    text
}
fn near(a: [f32; 3], b: [f32; 3]) {
    for (a, b) in a.into_iter().zip(b) {
        assert!((a - b).abs() < 0.00001, "{a} vs {b}");
    }
}

#[test]
fn lookup_common_sizes_order_and_identity() {
    for size in [2, 17, 33, 64, 65] {
        let lut = CubeLut::parse(&cube(size, 0), "test").unwrap();
        assert_eq!(lut.size(), size);
        for c in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.23, 0.67, 0.89]] {
            near(lut.sample(c), c);
        }
    }
    let lut = CubeLut::parse(&cube(2, 1), "test").unwrap();
    near(lut.sample([0.2, 0.4, 0.6]), [0.6, 0.8, 0.4]);
}
#[test]
fn lookup_tetrahedra_have_known_nontrilinear_result() {
    // Only corner 111 is 1: tetrahedral gives min(R,G,B), trilinear R*G*B.
    let lut = CubeLut::parse(&cube(2, 2), "test").unwrap();
    for c in [
        [0.2, 0.4, 0.6],
        [0.2, 0.6, 0.4],
        [0.4, 0.2, 0.6],
        [0.4, 0.6, 0.2],
        [0.6, 0.2, 0.4],
        [0.6, 0.4, 0.2],
        [0.4, 0.4, 0.4],
    ] {
        near(lut.sample(c), [c[0].min(c[1]).min(c[2]); 3]);
    }
}
#[test]
fn lookup_domain_bounds_strength_and_alpha() {
    let text = format!("DOMAIN_MIN -1 -1 -1\nDOMAIN_MAX 1 1 1\n{}", cube(2, 0));
    let lut = CubeLut::parse(&text, "test").unwrap();
    near(lut.sample([0.0, 0.5, 1.0]), [0.5, 0.75, 1.0]);
    near(lut.sample([-2.0, 2.0, 0.0]), [0.0, 1.0, 0.5]);
    let range =
        CubeLut::parse(&format!("LUT_3D_INPUT_RANGE -1 1\n{}", cube(2, 0)), "test").unwrap();
    assert_eq!(range.digest(), lut.digest());
    let hdr = CubeLut::parse(&cube(2, 3), "test").unwrap();
    near(hdr.sample([0.0, 0.5, 1.0]), [-1.0, 0.5, 2.0]);
    for alpha in [0, 1, 128, 255] {
        let p = [30, 120, 240, alpha];
        assert_eq!(
            apply_one(
                &Adjustment::ColorLookup {
                    lut: None,
                    amount: 1.0
                },
                p
            ),
            p
        );
        assert_eq!(
            apply_one(
                &Adjustment::ColorLookup {
                    lut: Some(hdr.clone()),
                    amount: 0.0
                },
                p
            ),
            p
        );
        let out = apply_one(
            &Adjustment::ColorLookup {
                lut: Some(lut.clone()),
                amount: 1.0,
            },
            [0, 128, 255, alpha],
        );
        assert_eq!(out, [128, 192, 255, alpha]);
    }
}
#[test]
fn lookup_digest_immutable_clone_and_tamper_rejection() {
    let text = cube(2, 1);
    let a = CubeLut::parse(&text, "a").unwrap();
    let b = CubeLut::parse(
        &format!(
            "\u{feff}# comment\r\n{}",
            text.replace("Synthetic 1", "Name # 2")
        ),
        "b",
    )
    .unwrap();
    assert_eq!(a.digest(), b.digest());
    let clone = a.clone();
    assert_eq!(a.values().as_ptr(), clone.values().as_ptr());
    let json = serde_json::to_string(&a).unwrap();
    assert_eq!(serde_json::from_str::<CubeLut>(&json).unwrap(), a);
    let mut broken: serde_json::Value = serde_json::from_str(&json).unwrap();
    broken["rgb"][0][0] = serde_json::json!(0.5);
    assert!(
        serde_json::from_value::<CubeLut>(broken)
            .unwrap_err()
            .to_string()
            .contains("digest")
    );
}
#[test]
fn lookup_bad_inputs_are_rejected_without_unbounded_allocation() {
    let good = cube(2, 0);
    for bad in [
        "LUT_1D_SIZE 2\n".to_owned(),
        format!("LUT_1D_SIZE 2\n{good}"),
        good.replace("SIZE 2", "SIZE 66"),
        good.replace("SIZE 2", "SIZE 2 3"),
        format!("LUT_3D_SIZE 2\n{good}"),
        format!("{good}0 0 0\n"),
        good.lines().take(9).collect::<Vec<_>>().join("\n"),
        format!("DOMAIN_MIN 1 0 0\n{good}"),
        format!("DOMAIN_MAX NaN 1 1\n{good}"),
        format!("DOMAIN_MAX 1e40 1 1\n{good}"),
        format!("DOMAIN_MAX 1e-40 1 1\n{good}"),
        format!("{good}DOMAIN_MIN 0 0 0\n"),
        good.replace("0 0 0", "NaN 0 0"),
        good.replace("0 0 0", "inf 0 0"),
        good.replace("0 0 0", "1000001 0 0"),
        good.replace("0 0 0", "0 0 0 unexpected"),
        format!("UNKNOWN_FORMAT 1\n{good}"),
        format!("DOMAIN_MIN 0 0 0\nLUT_3D_INPUT_RANGE 0 1\n{good}"),
        "x".repeat(crate::lookup::MAX_FILE_BYTES + 1),
        format!("TITLE \"{}\"\n{}", "x".repeat(5000), good),
    ] {
        assert!(
            CubeLut::parse(&bad, "test").is_err(),
            "accepted {}",
            &bad[..bad.len().min(100)]
        );
    }
}

#[test]
fn lookup_fractional_resource_survives_manifest_value_round_trip() {
    let text = format!(
        "DOMAIN_MIN -0.12345679 -0.2 0.12345679\nDOMAIN_MAX 0.9876543 1.2345679 0.9876543\n{}",
        cube(18, 3)
    );
    let original = CubeLut::parse(&text, "fractional").unwrap();
    // The project manifest converts the document to Value before writing,
    // then parses Value before decoding the typed document. Preserve f32 bits
    // through that f64 JSON intermediate so the stable resource digest holds.
    let manifest = serde_json::json!({"lut":original});
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let reopened: CubeLut = serde_json::from_value(value["lut"].clone()).unwrap();
    assert_eq!(original.digest(), reopened.digest());
    assert_eq!(original.values(), reopened.values());
}
