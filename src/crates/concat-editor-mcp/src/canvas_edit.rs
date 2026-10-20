//! Explicit, bounded local edits; no paths, scripts, or reflected fields.
use crate::parse_layer_id;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Direction within the current sibling container, as shown in the layer panel.
pub enum LayerDirection {
    /// Move toward the front of the compositing stack.
    Up,
    /// Move toward the back of the compositing stack.
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Exact raster destination; must match the current native paint selection.
pub enum CanvasPaintTarget {
    /// Paint the active raster image pixels.
    LayerPixels,
    /// Paint the active existing raster mask.
    LayerMask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// One native RGB curve channel.
pub enum CurveChannel {
    /// Red channel.
    Red,
    /// Green channel.
    Green,
    /// Blue channel.
    Blue,
}
impl CurveChannel {
    /// Native channel index: red zero, green one, blue two.
    pub fn index(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Built-in adjustment kinds requiring no external lookup file.
pub enum CanvasAdjustment {
    /// Invert RGB values.
    Invert,
    /// Adjust exposure in stops.
    Exposure,
    /// Adjust input/output levels and gamma.
    Levels,
    /// Adjust hue, saturation and lightness.
    HueSaturation,
    /// Map luminance through the native gradient defaults.
    GradientMap,
    /// Apply grain.
    Grain,
    /// Adjust RGB curves.
    Curves,
    /// Adjust vibrance.
    Vibrance,
    /// Mix RGB into monochrome.
    BlackWhite,
}
impl CanvasAdjustment {
    /// Existing native adjustment-kind number, one through nine.
    pub fn index(self) -> i32 {
        match self {
            Self::Invert => 1,
            Self::Exposure => 2,
            Self::Levels => 3,
            Self::HueSaturation => 4,
            Self::GradientMap => 5,
            Self::Grain => 6,
            Self::Curves => 7,
            Self::Vibrance => 8,
            Self::BlackWhite => 9,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// One stroke sample in document pixels, with top-left origin and downward y.
pub struct CanvasPoint {
    /// Horizontal document-pixel coordinate, bounded to +/-32768.
    pub x: f64,
    /// Vertical document-pixel coordinate, bounded to +/-32768.
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
/// One closed, explicitly approved local edit using the native history and tools.
pub enum CanvasEdit {
    /// Select the named visible panel row and its image pixels.
    LayerSelect {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
    },
    /// Add one transparent image layer and select it.
    LayerAdd,
    /// Delete the named node and its descendants through native history.
    LayerDelete {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
    },
    /// Move one sibling step in the layer panel.
    LayerMove {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// One sibling step in the specified direction.
        direction: LayerDirection,
    },
    /// Add an empty group above the current selection.
    GroupAdd,
    /// Append a node to the named group, or to the root when parentId is null.
    LayerMoveInto {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Stable target group identity; null means the document root.
        parent_id: Option<String>,
    },
    /// Set the group panel expansion without changing its children.
    GroupSetExpanded {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Whether the group children should appear in the panel.
        expanded: bool,
    },
    /// Set the named node visibility.
    LayerSetVisibility {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Whether the node should be visible.
        visible: bool,
    },
    /// Set the named node opacity.
    LayerSetOpacity {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Opacity in 0..1; brush opacity must be at least 0.01.
        opacity: f64,
    },
    /// Coordinates are document pixels. False replaces the current selection.
    SelectionRect {
        /// Horizontal coordinate: document pixels for selections, normalized input for curves.
        x: f64,
        /// Vertical coordinate: document pixels for selections, normalized output for curves.
        y: f64,
        /// Positive selection width in document pixels.
        width: f64,
        /// Positive selection height in document pixels.
        height: f64,
        /// True adds to the selection; false replaces it.
        additive: bool,
    },
    /// Select all document pixels.
    SelectionAll,
    /// Clear the document pixel selection.
    SelectionClear,
    /// Fill the current pixel selection using the native brush colour.
    SelectionFill {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact current raster image or mask destination.
        target: CanvasPaintTarget,
    },
    /// Clear the selected pixels on the exact current paint target.
    SelectionDelete {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact current raster image or mask destination.
        target: CanvasPaintTarget,
    },
    /// Add a white raster mask when absent, and select it for painting.
    MaskAdd {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
    },
    /// Remove the existing mask through native history.
    MaskRemove {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
    },
    /// Enable or disable the existing mask.
    MaskSetEnabled {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Whether the existing mask should be applied.
        enabled: bool,
    },
    /// Select the existing raster mask for painting.
    MaskSelect {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
    },
    /// Set the native brush parameters and choose brush or eraser.
    BrushSet {
        /// Native brush diameter in target pixels, 1..2000.
        diameter: f64,
        /// Opacity in 0..1; brush opacity must be at least 0.01.
        opacity: f64,
        /// Brush edge hardness in 0..1.
        hardness: f64,
        /// RGB bytes, each in 0..255.
        color_rgb: [u8; 3],
        /// True selects the native eraser; false selects the brush.
        eraser: bool,
    },
    /// Paint one bounded stroke through document-pixel samples in one history step.
    BrushStroke {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact current raster image or mask destination.
        target: CanvasPaintTarget,
        /// One to 2048 finite document-pixel samples, each within the document.
        points: Vec<CanvasPoint>,
    },
    /// Undo one available native history entry.
    Undo,
    /// Redo one available native history entry.
    Redo,
    /// Add and select one built-in adjustment layer.
    AdjustmentAdd {
        /// Built-in adjustment type, without filesystem input.
        adjustment: CanvasAdjustment,
    },
    /// Set a parameter on the exact active adjustment layer.
    AdjustmentSetParameter {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Existing native parameter or curve-point index, checked against current state.
        index: u32,
        /// Finite parameter value within the native adjustment parameter range.
        value: f64,
    },
    /// Set an existing curve point; endpoints retain x at zero and one.
    CurveSet {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact RGB curve channel.
        channel: CurveChannel,
        /// Existing native parameter or curve-point index, checked against current state.
        index: u32,
        /// Horizontal coordinate: document pixels for selections, normalized input for curves.
        x: f64,
        /// Vertical coordinate: document pixels for selections, normalized output for curves.
        y: f64,
    },
    /// Add one interior curve point; at most sixteen points per channel.
    CurveAdd {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact RGB curve channel.
        channel: CurveChannel,
        /// Horizontal coordinate: document pixels for selections, normalized input for curves.
        x: f64,
        /// Vertical coordinate: document pixels for selections, normalized output for curves.
        y: f64,
    },
    /// Remove one interior curve point; endpoints cannot be removed.
    CurveRemove {
        /// Stable canonical layer:n identity, resolved to a current native panel row.
        object_id: String,
        /// Exact RGB curve channel.
        channel: CurveChannel,
        /// Existing native parameter or curve-point index, checked against current state.
        index: u32,
    },
}
impl CanvasEdit {
    /// Exact primary target identity, when this action names an existing node.
    pub fn object_id(&self) -> Option<&str> {
        match self {
            Self::LayerSelect { object_id }
            | Self::LayerDelete { object_id }
            | Self::LayerMove { object_id, .. }
            | Self::LayerMoveInto { object_id, .. }
            | Self::GroupSetExpanded { object_id, .. }
            | Self::LayerSetVisibility { object_id, .. }
            | Self::LayerSetOpacity { object_id, .. }
            | Self::SelectionFill { object_id, .. }
            | Self::SelectionDelete { object_id, .. }
            | Self::MaskAdd { object_id }
            | Self::MaskRemove { object_id }
            | Self::MaskSetEnabled { object_id, .. }
            | Self::MaskSelect { object_id }
            | Self::BrushStroke { object_id, .. }
            | Self::AdjustmentSetParameter { object_id, .. }
            | Self::CurveSet { object_id, .. }
            | Self::CurveAdd { object_id, .. }
            | Self::CurveRemove { object_id, .. } => Some(object_id),
            _ => None,
        }
    }
    /// Check syntax, canonical identities, finite ranges and bounded work before IPC.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(id) = self.object_id() {
            parse_layer_id(id)?;
        }
        let bounded = |v: f64| v.is_finite() && v.abs() <= 32768.0;
        let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        let valid = match self {
            Self::LayerMoveInto {
                parent_id: Some(id),
                ..
            } => {
                parse_layer_id(id)?;
                true
            }
            Self::LayerSetOpacity { opacity, .. } => unit(*opacity),
            Self::SelectionRect {
                x,
                y,
                width,
                height,
                ..
            } => {
                bounded(*x)
                    && bounded(*y)
                    && bounded(*width)
                    && bounded(*height)
                    && *width > 0.0
                    && *height > 0.0
            }
            Self::BrushSet {
                diameter,
                opacity,
                hardness,
                ..
            } => {
                diameter.is_finite()
                    && (1.0..=2000.0).contains(diameter)
                    && opacity.is_finite()
                    && (0.01..=1.0).contains(opacity)
                    && unit(*hardness)
            }
            Self::BrushStroke { points, .. } => {
                !points.is_empty()
                    && points.len() <= 2048
                    && points.iter().all(|p| bounded(p.x) && bounded(p.y))
            }
            Self::AdjustmentSetParameter { index, value, .. } => {
                *index <= 4 && value.is_finite() && value.abs() <= 32768.0
            }
            Self::CurveSet { index, x, y, .. } => *index < 16 && unit(*x) && unit(*y),
            Self::CurveAdd { x, y, .. } => unit(*x) && unit(*y),
            Self::CurveRemove { index, .. } => *index < 16,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err("invalidInput: canvas edit bounds".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn canvas_edit_rejects_arbitrary_fields_targets_and_unbounded_work() {
        for v in [
            json!({"kind":"layerSelect","objectId":"layer:1","row":0}),
            json!({"kind":"execute","script":"x"}),
            json!({"kind":"brushSet","diameter":1,"opacity":1,"hardness":1,"colorRgb":[1,2,3],"eraser":false,"path":"/tmp/a"}),
        ] {
            assert!(serde_json::from_value::<CanvasEdit>(v).is_err());
        }
        let stroke = |points| CanvasEdit::BrushStroke {
            object_id: "layer:1".into(),
            target: CanvasPaintTarget::LayerPixels,
            points,
        };
        assert!(stroke(vec![]).validate().is_err());
        assert!(
            stroke(vec![CanvasPoint { x: 0.0, y: 0.0 }; 2049])
                .validate()
                .is_err()
        );
        assert!(
            stroke(vec![CanvasPoint {
                x: f64::NAN,
                y: 0.0
            }])
            .validate()
            .is_err()
        );
        assert!(
            CanvasEdit::LayerSelect {
                object_id: "layer:01".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            CanvasEdit::CurveAdd {
                object_id: "layer:1".into(),
                channel: CurveChannel::Red,
                x: 0.5,
                y: 1.01
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn canvas_edit_roundtrips_exact_semantics() {
        let action = CanvasEdit::SelectionRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            additive: false,
        };
        assert_eq!(
            serde_json::to_value(&action).unwrap(),
            json!({"kind":"selectionRect","x":1.0,"y":2.0,"width":3.0,"height":4.0,"additive":false})
        );
        assert!(action.validate().is_ok());
    }
}
