// SPDX-License-Identifier: AGPL-3.0-or-later
//! Focused colour adjustment contracts. Native persistence is tested in the host.
use crate::{Adjustment, ImageDocument, compositor::apply_one};

#[test]
fn c02_vibrance_selective_gain_neutrals_and_alpha() {
    let boost = Adjustment::Vibrance { amount: 1.0 };
    for alpha in [0, 1, 128, 255] {
        for rgb in [
            [0, 0, 0],
            [128, 128, 128],
            [255, 255, 255],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
        ] {
            let p = [rgb[0], rgb[1], rgb[2], alpha];
            assert_eq!(apply_one(&boost, p), p);
        }
        let p = [140, 110, 110, alpha];
        assert_eq!(apply_one(&Adjustment::Vibrance { amount: 0.0 }, p), p);
        assert_eq!(apply_one(&boost, p), [153, 97, 97, alpha]);
        assert_eq!(
            apply_one(&Adjustment::Vibrance { amount: -1.0 }, p),
            [125, 125, 125, alpha]
        );
        // A near-saturated red gains only 9 bytes of chroma; the low
        // saturation sample above gains 26, fully saturated red stays put.
        assert_eq!(apply_one(&boost, [240, 10, 10, alpha]), [249, 1, 1, alpha]);
    }
}

#[test]
fn c02_black_white_independent_weights_and_alpha() {
    let bw = Adjustment::BlackWhite {
        red: 0.299,
        green: 0.587,
        blue: 0.114,
    };
    for alpha in [0, 128, 255] {
        for (rgb, v) in [
            ([255, 0, 0], 76),
            ([0, 255, 0], 150),
            ([0, 0, 255], 29),
            ([128, 128, 128], 128),
        ] {
            assert_eq!(
                apply_one(&bw, [rgb[0], rgb[1], rgb[2], alpha]),
                [v, v, v, alpha]
            );
        }
        assert_eq!(
            apply_one(
                &Adjustment::BlackWhite {
                    red: 2.0,
                    green: 0.0,
                    blue: 0.0
                },
                [200, 80, 20, alpha]
            ),
            [255, 255, 255, alpha]
        );
        assert_eq!(
            apply_one(
                &Adjustment::BlackWhite {
                    red: 0.0,
                    green: 0.0,
                    blue: 0.0
                },
                [200, 80, 20, alpha]
            ),
            [0, 0, 0, alpha]
        );
    }
}

#[test]
fn c02_document_validation_and_round_trip() {
    for adjustment in [
        Adjustment::Vibrance { amount: -1.0 },
        Adjustment::Vibrance { amount: 1.0 },
        Adjustment::BlackWhite {
            red: 0.299,
            green: 0.587,
            blue: 0.114,
        },
        Adjustment::BlackWhite {
            red: 0.0,
            green: 2.0,
            blue: 0.0,
        },
    ] {
        let mut d = ImageDocument::new(16, 16);
        d.new_adjustment("adjustment", adjustment);
        d.validate().unwrap();
        let json = d.to_json().unwrap();
        let reopened: ImageDocument = serde_json::from_str(&json).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened.to_json().unwrap(), json);
    }
    for adjustment in [
        Adjustment::Vibrance { amount: f32::NAN },
        Adjustment::Vibrance { amount: 1.001 },
        Adjustment::BlackWhite {
            red: f32::INFINITY,
            green: 0.5,
            blue: 0.0,
        },
        Adjustment::BlackWhite {
            red: -0.1,
            green: 0.5,
            blue: 0.0,
        },
        Adjustment::BlackWhite {
            red: 0.0,
            green: 2.001,
            blue: 0.0,
        },
    ] {
        let mut d = ImageDocument::new(16, 16);
        d.new_adjustment("invalid", adjustment);
        assert!(matches!(
            d.validate(),
            Err(crate::document::DocumentError::InvalidAdjustment(_))
        ));
    }
}
