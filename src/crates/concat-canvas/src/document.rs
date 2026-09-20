// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
//
// Ported from Compositor (https://github.com/robbietilton/Compositor, MIT):
// the layered document, its non-destructive transforms, masks and adjustment
// layers.

//! A layered image document: pure data, no pixels, no IO.
//!
//! The tree is the familiar one: a root [`LayerGroup`] holding
//! [`LayerNode`]s, each of which is an [`ImageLayer`] or a nested group. The
//! document names pixels by [`PixelId`](crate::pixels::PixelId) - the
//! bitmaps live in a side store - so cloning a document, which is what every
//! undo snapshot does, costs a few dozen allocations whatever the canvas
//! size.
//!
//! Serialization is serde over the whole tree, ids included: a saved project
//! re-mints nothing, so a pixel id in the JSON names the image file written
//! beside it. The format is Concat's own; Compositor's `Codable` archive was
//! never a compatibility target, only a design reference.

use crate::blend::BlendMode;
use crate::pixels::PixelId;

/// A layer or group's identity, unique within its document and stable across
/// saves. Never zero.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct LayerId(pub(crate) u64);

impl LayerId {
    /// The identity's number, for UI rows and keys. Meaningless on its own;
    /// uniqueness within a document is the whole contract.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// A whole canvas: its size and its layers, back to front in the root
/// group's `children`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageDocument {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// The root group. Its own opacity and blend mode apply to the whole
    /// composite, the way Compositor's outermost folder does.
    pub root: LayerGroup,
    /// The next id `new_layer`/`new_group` will mint. Serialized so ids a
    /// save re-mints never collide with ids already in the tree.
    next_id: u64,
}

impl ImageDocument {
    /// An empty document: `width` x `height`, no layers.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            root: LayerGroup::new(LayerId(1), "Root"),
            next_id: 2,
        }
    }

    /// A layer id nothing else in this document has.
    pub fn mint_id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// A new image layer, named and filled with a pixel id, appended to the
    /// root group.
    pub fn new_layer(&mut self, name: impl Into<String>, pixels: PixelId) -> LayerId {
        let id = self.mint_id();
        self.root
            .children
            .push(LayerNode::Layer(ImageLayer::new(id, name, pixels)));
        id
    }

    /// A new, empty group appended to the root group.
    pub fn new_group(&mut self, name: impl Into<String>) -> LayerId {
        let id = self.mint_id();
        self.root
            .children
            .push(LayerNode::Group(LayerGroup::new(id, name)));
        id
    }

    /// A new adjustment layer, appended to the root group. The caller moves
    /// it into place with [`move_node`](Self::move_node).
    pub fn new_adjustment(&mut self, name: impl Into<String>, adjustment: Adjustment) -> LayerId {
        let id = self.mint_id();
        self.root
            .children
            .push(LayerNode::Adjustment(AdjustmentLayer::new(
                id, name, adjustment,
            )));
        id
    }

    /// Every node in the tree, root group included, back to front.
    pub fn walk(&self) -> Vec<&LayerNode> {
        let mut out = Vec::new();
        self.root.collect_nodes(&mut out);
        out
    }

    /// The node an id names, at any depth.
    pub fn find(&self, id: LayerId) -> Option<&LayerNode> {
        self.root.find(id)
    }

    /// The node an id names, mutably, at any depth.
    pub fn find_mut(&mut self, id: LayerId) -> Option<&mut LayerNode> {
        self.root.find_mut(id)
    }

    /// The group an id names, if it is one.
    pub fn group_mut(&mut self, id: LayerId) -> Option<&mut LayerGroup> {
        match self.find_mut(id) {
            Some(LayerNode::Group(group)) => Some(group),
            _ => None,
        }
    }

    /// The image layer an id names, if it is one.
    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut ImageLayer> {
        match self.find_mut(id) {
            Some(LayerNode::Layer(layer)) => Some(layer),
            _ => None,
        }
    }

    /// The adjustment layer an id names, if it is one.
    pub fn adjustment_mut(&mut self, id: LayerId) -> Option<&mut AdjustmentLayer> {
        match self.find_mut(id) {
            Some(LayerNode::Adjustment(adjustment)) => Some(adjustment),
            _ => None,
        }
    }

    /// Moves a node into a group, at `index` among its children. The node
    /// keeps its id; a move into its own subtree is refused (`false`), as is
    /// an id nothing names. `None` as the group means the root.
    pub fn move_node(&mut self, id: LayerId, into: Option<LayerId>, index: usize) -> bool {
        if into == Some(id) {
            return false;
        }
        if let Some(target) = into {
            // The target must exist and must not live inside the moved
            // subtree - checked before anything is taken, so a refusal
            // leaves the tree untouched.
            let inside_moved_subtree = match self.find(id) {
                Some(LayerNode::Group(group)) => group.find(target).is_some(),
                _ => false,
            };
            if inside_moved_subtree || self.find(target).is_none() {
                return false;
            }
        }
        let Some(node) = self.take_node(id) else {
            return false;
        };
        let target = match into {
            Some(group) => match self.find_mut(group) {
                Some(LayerNode::Group(group)) => group,
                _ => return false,
            },
            None => &mut self.root,
        };
        let index = index.min(target.children.len());
        target.children.insert(index, node);
        true
    }

    /// Removes a node from the tree and returns it. The root group cannot be
    /// removed.
    pub fn take_node(&mut self, id: LayerId) -> Option<LayerNode> {
        self.root.take(id)
    }

    /// Removes and returns a node, keeping the tree walk order it had.
    /// Groups that become empty stay; empty groups are a UI concern.
    pub fn remove(&mut self, id: LayerId) -> Option<LayerNode> {
        self.take_node(id)
    }

    /// Every pixel id the document still names - layers, masks and
    /// adjustment sources at any depth. What `PixelStore::retain_document`
    /// keeps.
    pub fn collect_pixels(&self, out: &mut Vec<PixelId>) {
        self.root.collect_pixels(out);
    }

    /// The number of nodes in the tree, groups included.
    pub fn node_count(&self) -> usize {
        self.root.node_count()
    }

    /// Checks every live alpha link names a layer that exists, is not a
    /// group, is not the layer itself, and does not lead back around in a
    /// cycle or a chain longer than [`CLIP_CHAIN_LIMIT`].
    ///
    /// The renderer walks these links while compositing; a document that
    /// fails this would either draw the wrong thing or never finish. Run it
    /// after loading a save and after any edit that removes or moves layers,
    /// which is where a dangling link can appear.
    pub fn validate(&self) -> Result<(), DocumentError> {
        for node in self.walk() {
            let LayerNode::Layer(layer) = node else {
                continue;
            };
            let Some(source) = layer.clips_to else {
                continue;
            };
            if source == layer.id {
                return Err(DocumentError::SelfClip(layer.id));
            }
            match self.find(source) {
                None => {
                    return Err(DocumentError::MissingClipSource {
                        layer: layer.id,
                        cited: source,
                    });
                }
                Some(LayerNode::Group(_)) => {
                    return Err(DocumentError::ClipSourceIsGroup {
                        layer: layer.id,
                        cited: source,
                    });
                }
                Some(LayerNode::Adjustment(_)) => {
                    return Err(DocumentError::ClipSourceNotALayer {
                        layer: layer.id,
                        cited: source,
                    });
                }
                Some(LayerNode::Layer(_)) => {}
            }
            // Follow the chain from the source: it must end, and must not
            // come back to a node it already passed.
            let mut visited = vec![layer.id];
            let mut current = Some(source);
            while let Some(id) = current {
                if visited.contains(&id) {
                    return Err(DocumentError::ClipCycle { start: layer.id });
                }
                visited.push(id);
                if visited.len() > CLIP_CHAIN_LIMIT {
                    return Err(DocumentError::ClipChainTooLong { start: layer.id });
                }
                current = match self.find(id) {
                    Some(LayerNode::Layer(next)) => next.clips_to,
                    _ => None,
                };
            }
        }
        Ok(())
    }

    /// Serializes the document to JSON. Round-trips through
    /// [`serde_json::from_str`]; ids are preserved.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Reads a document from JSON written by [`ImageDocument::to_json`].
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}

/// How many links a clipping chain may have before the document is refused.
/// The same ceiling the source format uses; past it a file is taken for
/// corrupt rather than walked.
pub const CLIP_CHAIN_LIMIT: usize = 256;

/// Why a document cannot be drawn: a live alpha link that does not resolve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// A layer clips to itself.
    #[error("layer {0:?} clips to itself")]
    SelfClip(LayerId),
    /// A layer clips to something that is not in the document.
    #[error("layer {layer:?} clips to {cited:?}, which is not in the document")]
    MissingClipSource {
        /// The layer carrying the link.
        layer: LayerId,
        /// The name the link holds.
        cited: LayerId,
    },
    /// A layer clips to a group, which has no pixels to take alpha from.
    #[error("layer {layer:?} clips to group {cited:?}")]
    ClipSourceIsGroup {
        /// The layer carrying the link.
        layer: LayerId,
        /// The group it names.
        cited: LayerId,
    },
    /// A layer clips to an adjustment, which has no pixels to take alpha
    /// from.
    #[error("layer {layer:?} clips to adjustment {cited:?}")]
    ClipSourceNotALayer {
        /// The layer carrying the link.
        layer: LayerId,
        /// The adjustment it names.
        cited: LayerId,
    },
    /// Following the links from a layer comes back to a node it passed.
    #[error("clipping chain from {start:?} loops")]
    ClipCycle {
        /// Where the walk started.
        start: LayerId,
    },
    /// Following the links from a layer goes on too long.
    #[error("clipping chain from {start:?} is longer than the {CLIP_CHAIN_LIMIT}-link ceiling")]
    ClipChainTooLong {
        /// Where the walk started.
        start: LayerId,
    },
}

/// One node of the layer tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LayerNode {
    /// A bitmap layer.
    Layer(ImageLayer),
    /// A folder of nodes.
    Group(LayerGroup),
    /// A colour change over everything beneath it in its group.
    Adjustment(AdjustmentLayer),
}

impl LayerNode {
    /// The node's id, whatever kind it is.
    pub fn id(&self) -> LayerId {
        match self {
            Self::Layer(layer) => layer.id,
            Self::Group(group) => group.id,
            Self::Adjustment(adjustment) => adjustment.id,
        }
    }

    /// The node's name, whatever kind it is.
    pub fn name(&self) -> &str {
        match self {
            Self::Layer(layer) => &layer.name,
            Self::Group(group) => &group.name,
            Self::Adjustment(adjustment) => &adjustment.name,
        }
    }

    /// Whether the node is drawn at all. A hidden group hides everything in
    /// it; a hidden layer or adjustment is simply skipped.
    pub fn hidden(&self) -> bool {
        match self {
            Self::Layer(layer) => layer.hidden,
            Self::Group(group) => group.hidden,
            Self::Adjustment(adjustment) => adjustment.hidden,
        }
    }

    /// The node's blend mode, whatever kind it is.
    pub fn blend(&self) -> BlendMode {
        match self {
            Self::Layer(layer) => layer.blend,
            Self::Group(group) => group.blend,
            Self::Adjustment(adjustment) => adjustment.blend,
        }
    }

    /// The node's mask, whatever kind it is.
    pub fn mask(&self) -> Option<&LayerMask> {
        match self {
            Self::Layer(layer) => layer.mask.as_ref(),
            Self::Group(group) => group.mask.as_ref(),
            Self::Adjustment(adjustment) => adjustment.mask.as_ref(),
        }
    }

    /// The node's opacity, whatever kind it is.
    pub fn opacity(&self) -> f32 {
        match self {
            Self::Layer(layer) => layer.opacity,
            Self::Group(group) => group.opacity,
            Self::Adjustment(adjustment) => adjustment.opacity,
        }
    }
}

/// A bitmap layer: pixels by name, placed on the canvas by a non-destructive
/// transform, optionally masked.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageLayer {
    /// The layer's identity.
    pub id: LayerId,
    /// The name shown in the layers panel.
    pub name: String,
    /// Blend strength, `0.0..=1.0`.
    pub opacity: f32,
    /// How the layer's colour meets what is beneath.
    pub blend: BlendMode,
    /// Where and how big the layer sits, short of any pixels being touched.
    pub transform: LayerTransform,
    /// The layer's mask, if it has one. White shows, black hides.
    pub mask: Option<LayerMask>,
    /// A live alpha link, upstream's clipping mask (`maskSourceID`): when
    /// set, the layer only draws where **that** layer's coverage is, computed
    /// in document coordinates from its pixels, transform, opacity, raster
    /// mask and its own upstream links - its colour and visibility do not
    /// contribute. Several layers may share one base.
    ///
    /// This is a reference, not a copy: deleting the source bakes the
    /// coverage into the dependent layer or drops the link, so the document
    /// is never left with a dangling name - see
    /// [`ImageDocument::validate`].
    pub clips_to: Option<LayerId>,
    /// How the layer's bitmap is sampled when it is not drawn at its own
    /// size.
    pub sampling: LayerSampling,
    /// Whether the layer is drawn at all.
    pub hidden: bool,
    /// The layer's pixels, by name. Never [`PixelId::NONE`] in a document
    /// that came from `new_layer`.
    pub pixels: PixelId,
}

/// How a layer's bitmap is sampled when the canvas draws it at a size other
/// than its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum LayerSampling {
    /// Interpolated: the default, and what keeps a photograph smooth when it
    /// is scaled.
    #[default]
    Smooth,
    /// Nearest neighbour: a pixel grid when zoomed in, so each document
    /// pixel is a square.
    Nearest,
}

impl ImageLayer {
    /// A visible, opaque, normal-blend, unclipped layer covering the canvas
    /// exactly.
    pub fn new(id: LayerId, name: impl Into<String>, pixels: PixelId) -> Self {
        Self {
            id,
            name: name.into(),
            opacity: 1.0,
            blend: BlendMode::default(),
            transform: LayerTransform::default(),
            mask: None,
            clips_to: None,
            sampling: LayerSampling::default(),
            hidden: false,
            pixels,
        }
    }
}

/// A folder of nodes. Its opacity and blend mode apply to the group's
/// composite as a whole, the way Photoshop's and Compositor's folders do.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerGroup {
    /// The group's identity.
    pub id: LayerId,
    /// The name shown in the layers panel.
    pub name: String,
    /// Group blend strength, `0.0..=1.0`, over what is beneath the group.
    pub opacity: f32,
    /// How the group's composite meets what is beneath it.
    pub blend: BlendMode,
    /// The group's mask, if it has one.
    pub mask: Option<LayerMask>,
    /// Whether the whole group is hidden.
    pub hidden: bool,
    /// Back-to-front: the last child draws on top.
    pub children: Vec<LayerNode>,
}
impl LayerGroup {
    /// A visible, opaque, normal-blend, empty group.
    pub fn new(id: LayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            opacity: 1.0,
            blend: BlendMode::default(),
            mask: None,
            hidden: false,
            children: Vec::new(),
        }
    }

    fn collect_nodes<'a>(&'a self, out: &mut Vec<&'a LayerNode>) {
        for child in &self.children {
            out.push(child);
            if let LayerNode::Group(group) = child {
                group.collect_nodes(out);
            }
        }
    }

    fn find(&self, id: LayerId) -> Option<&LayerNode> {
        for child in &self.children {
            if child.id() == id {
                return Some(child);
            }
            if let LayerNode::Group(group) = child
                && let Some(found) = group.find(id)
            {
                return Some(found);
            }
        }
        None
    }

    fn find_mut(&mut self, id: LayerId) -> Option<&mut LayerNode> {
        for child in &mut self.children {
            if child.id() == id {
                return Some(child);
            }
            if let LayerNode::Group(group) = child
                && let Some(found) = group.find_mut(id)
            {
                return Some(found);
            }
        }
        None
    }

    fn take(&mut self, id: LayerId) -> Option<LayerNode> {
        let index = self.children.iter().position(|node| node.id() == id)?;
        Some(self.children.remove(index))
    }

    fn collect_pixels(&self, out: &mut Vec<PixelId>) {
        if let Some(mask) = &self.mask {
            out.push(mask.pixels);
        }
        for child in &self.children {
            match child {
                LayerNode::Layer(layer) => {
                    out.push(layer.pixels);
                    if let Some(mask) = &layer.mask {
                        out.push(mask.pixels);
                    }
                }
                LayerNode::Adjustment(adjustment) => {
                    if let Some(mask) = &adjustment.mask {
                        out.push(mask.pixels);
                    }
                }
                LayerNode::Group(group) => group.collect_pixels(out),
            }
        }
    }

    fn node_count(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(LayerNode::node_count)
            .sum::<usize>()
    }
}

impl LayerNode {
    /// The subtree's node count, this node included.
    pub fn node_count(&self) -> usize {
        match self {
            Self::Layer(_) | Self::Adjustment(_) => 1,
            Self::Group(group) => group.node_count(),
        }
    }
}

/// A layer's placement, short of any pixels being touched: however small the
/// layer is made here, its bitmap keeps its full resolution and the shrink
/// is undone by changing this back.
///
/// The order is scale, then flip, then rotation, then translation, all about
/// the layer's centre; the centre sits `x/y` away from the canvas centre, so
/// the default places a bitmap of the canvas' own size exactly over it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerTransform {
    /// The layer centre's offset from the canvas centre, horizontal.
    pub x: f32,
    /// The layer centre's offset from the canvas centre, vertical.
    pub y: f32,
    /// Horizontal scale, `1.0` the bitmap's own size.
    pub scale_x: f32,
    /// Vertical scale, `1.0` the bitmap's own size.
    pub scale_y: f32,
    /// Rotation about the centre, clockwise, in radians.
    pub rotation: f32,
    /// Whether the layer is mirrored horizontally.
    pub flip_h: bool,
    /// Whether the layer is mirrored vertically.
    pub flip_v: bool,
}

impl Default for LayerTransform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }
}

/// A mask over a layer or group: grayscale pixels by name, white showing and
/// black hiding.
///
/// `linked` follows Compositor's vocabulary: an unlinked mask keeps its own
/// transform when the layer's changes, so the mask can be moved on its own.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerMask {
    /// Whether the mask is applied at all.
    pub enabled: bool,
    /// Whether the mask follows the layer's transform or holds its own.
    pub linked: bool,
    /// The mask's grayscale pixels, by name.
    pub pixels: PixelId,
}

impl LayerMask {
    /// An enabled, linked mask over the named pixels.
    pub fn new(pixels: PixelId) -> Self {
        Self {
            enabled: true,
            linked: true,
            pixels,
        }
    }
}

/// A non-destructive colour change over everything beneath it in its group:
/// the adjustment-layer parameters Compositor offers.
///
/// These are the numbers the inspector edits; the math that applies them
/// arrives with the render phase of the port, and lives beside the blend
/// modes when it does.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Adjustment {
    /// Hue shift in degrees, saturation and lightness in `-1.0..=1.0`.
    HueSaturation {
        /// Degrees of hue rotation, wrapping.
        hue: f32,
        /// Saturation change, `-1.0..=1.0`.
        saturation: f32,
        /// Lightness change, `-1.0..=1.0`.
        lightness: f32,
    },
    /// Remaps the input range: `in_black`..`in_white` stretched to
    /// `out_black`..`out_white`, `gamma` in between.
    Levels {
        /// The input black point, `0.0..=1.0`.
        in_black: f32,
        /// The input white point, `0.0..=1.0`.
        in_white: f32,
        /// The gamma applied between the points.
        gamma: f32,
        /// The output black point, `0.0..=1.0`.
        out_black: f32,
        /// The output white point, `0.0..=1.0`.
        out_white: f32,
    },
    /// The same tone curve per channel: control points as `(input, output)`
    /// pairs in `0.0..=1.0`, sorted by input. Two points are a straight line,
    /// which is what a channel with nothing done to it has.
    Curves {
        /// Red channel control points, thinned to the ones that matter.
        red: Vec<(f32, f32)>,
        /// Green channel control points.
        green: Vec<(f32, f32)>,
        /// Blue channel control points.
        blue: Vec<(f32, f32)>,
    },
    /// Exposure in stops, positive or negative.
    Exposure {
        /// Stops of exposure change.
        stops: f32,
    },
    /// Maps luminance onto a two-colour ramp.
    GradientMap {
        /// The ramp's low colour, RGB in `0.0..=1.0`.
        low: [f32; 3],
        /// The ramp's high colour, RGB in `0.0..=1.0`.
        high: [f32; 3],
    },
    /// Film grain strength, `0.0..=1.0`.
    Grain {
        /// How much grain, `0.0..=1.0`.
        amount: f32,
    },
    /// Inverts the channels beneath.
    Invert,
}

/// A non-destructive adjustment over the layers beneath it in its group: the
/// parameters plus the same appearance a layer has - opacity, blend mode,
/// mask - so a masked adjustment behaves exactly like a masked layer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdjustmentLayer {
    /// The adjustment's identity.
    pub id: LayerId,
    /// The name shown in the layers panel.
    pub name: String,
    /// Blend strength over what is beneath, `0.0..=1.0`.
    pub opacity: f32,
    /// How the adjusted colour meets what is beneath.
    pub blend: BlendMode,
    /// The adjustment's mask, if it has one - the usual way to keep an
    /// adjustment off part of the picture.
    pub mask: Option<LayerMask>,
    /// Whether the adjustment is applied at all.
    pub hidden: bool,
    /// What it does, and by how much.
    pub adjustment: Adjustment,
}

impl AdjustmentLayer {
    /// A fully-applied, unmasked, normal-blend adjustment.
    pub fn new(id: LayerId, name: impl Into<String>, adjustment: Adjustment) -> Self {
        Self {
            id,
            name: name.into(),
            opacity: 1.0,
            blend: BlendMode::default(),
            mask: None,
            hidden: false,
            adjustment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixels::PixelStore;
    use concat_core::frame::Frame;

    fn document_with_tree() -> (ImageDocument, PixelStore) {
        let mut doc = ImageDocument::new(640, 480);
        let mut store = PixelStore::new();
        let bg = doc.new_layer("Background", store.put(Frame::black(640, 480)));
        let group = doc.new_group("Folder");
        let inner = doc.new_layer("Inner", store.put(Frame::black(640, 480)));
        let _ = (bg, inner);
        // Move "Inner" into "Folder".
        assert!(doc.move_node(inner, Some(group), 0));
        (doc, store)
    }

    #[test]
    fn ids_are_unique_across_layers_and_groups() {
        let (doc, _) = document_with_tree();
        let mut ids: Vec<LayerId> = doc.walk().iter().map(|node| node.id()).collect();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn find_reaches_nested_nodes() {
        let (mut doc, _) = document_with_tree();
        let group = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Folder")
            .expect("folder")
            .id();
        assert!(matches!(doc.find(group), Some(LayerNode::Group(_))));
        doc.layer_mut(
            doc.walk()
                .iter()
                .find(|n| n.name() == "Inner")
                .expect("inner")
                .id(),
        )
        .expect("layer")
        .opacity = 0.5;
        let nodes = doc.walk();
        let inner = nodes.iter().find(|n| n.name() == "Inner").expect("inner");
        match inner {
            LayerNode::Layer(layer) => assert_eq!(layer.opacity, 0.5),
            _ => panic!("inner is a layer"),
        }
    }

    #[test]
    fn a_node_cannot_move_into_its_own_subtree() {
        let (mut doc, _) = document_with_tree();
        let group = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Folder")
            .expect("folder")
            .id();
        let before = doc.to_json().expect("serializes");
        assert!(!doc.move_node(group, Some(group), 0));
        assert_eq!(doc.to_json().expect("serializes"), before);
    }

    #[test]
    fn remove_takes_only_the_named_node() {
        let (mut doc, _) = document_with_tree();
        let group = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Folder")
            .expect("folder")
            .id();
        let taken = doc.remove(group).expect("group exists");
        assert!(matches!(taken, LayerNode::Group(_)));
        // "Inner" went with its folder.
        assert!(doc.walk().iter().all(|node| node.name() != "Inner"));
        assert!(doc.remove(group).is_none());
    }

    #[test]
    fn collect_pixels_names_layers_and_masks_at_depth() {
        let (mut doc, mut store) = document_with_tree();
        let inner_id = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Inner")
            .expect("inner")
            .id();
        let mask_pixels = store.put(Frame::black(640, 480));
        doc.layer_mut(inner_id).expect("layer").mask = Some(LayerMask::new(mask_pixels));

        let mut used = Vec::new();
        doc.collect_pixels(&mut used);
        assert!(used.contains(&mask_pixels));
        assert_eq!(used.len(), 3); // two layers, one mask

        let stale = store.put(Frame::black(1, 1));
        assert_eq!(store.len(), 4);
        let dropped = store.retain_document(&doc);
        assert_eq!(dropped, vec![stale]);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn json_round_trips_preserving_ids() {
        let (doc, _) = document_with_tree();
        let json = doc.to_json().expect("serializes");
        let back = ImageDocument::from_json(&json).expect("parses");
        assert_eq!(back.to_json().expect("serializes"), json);
        let ids: Vec<LayerId> = doc.walk().iter().map(|n| n.id()).collect();
        let back_ids: Vec<LayerId> = back.walk().iter().map(|n| n.id()).collect();
        assert_eq!(ids, back_ids);
        // Minted ids continue past the reloaded ones.
        let mut back = back;
        let fresh = back.mint_id();
        assert!(back.walk().iter().all(|node| node.id() != fresh));
    }

    #[test]
    fn a_clean_document_validates() {
        let (mut doc, mut store) = document_with_tree();
        let inner = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Inner")
            .expect("inner")
            .id();
        let base = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Background")
            .expect("background")
            .id();
        doc.layer_mut(inner).expect("layer").clips_to = Some(base);
        let _ = store.put(Frame::black(1, 1));
        assert_eq!(doc.validate(), Ok(()));
    }

    #[test]
    fn a_self_clip_is_refused() {
        let (mut doc, _store) = document_with_tree();
        let id = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Inner")
            .expect("inner")
            .id();
        doc.layer_mut(id).expect("layer").clips_to = Some(id);
        assert_eq!(doc.validate(), Err(DocumentError::SelfClip(id)));
    }

    #[test]
    fn a_clip_to_something_absent_is_refused() {
        let (mut doc, _store) = document_with_tree();
        let id = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Inner")
            .expect("inner")
            .id();
        let ghost = LayerId(9_999);
        doc.layer_mut(id).expect("layer").clips_to = Some(ghost);
        assert_eq!(
            doc.validate(),
            Err(DocumentError::MissingClipSource {
                layer: id,
                cited: ghost
            })
        );
    }

    #[test]
    fn a_clip_to_a_group_or_an_adjustment_is_refused() {
        let (mut doc, _store) = document_with_tree();
        let inner = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Inner")
            .expect("inner")
            .id();
        let folder = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Folder")
            .expect("folder")
            .id();
        doc.layer_mut(inner).expect("layer").clips_to = Some(folder);
        assert_eq!(
            doc.validate(),
            Err(DocumentError::ClipSourceIsGroup {
                layer: inner,
                cited: folder
            })
        );

        let adjustment = doc.new_adjustment("Levels", Adjustment::Invert);
        doc.layer_mut(inner).expect("layer").clips_to = Some(adjustment);
        assert_eq!(
            doc.validate(),
            Err(DocumentError::ClipSourceNotALayer {
                layer: inner,
                cited: adjustment
            })
        );
    }

    #[test]
    fn a_clip_loop_is_refused() {
        let (mut doc, mut store) = document_with_tree();
        let a = doc.new_layer("A", store.put(Frame::black(4, 4)));
        let b = doc.new_layer("B", store.put(Frame::black(4, 4)));
        doc.layer_mut(a).expect("layer").clips_to = Some(b);
        doc.layer_mut(b).expect("layer").clips_to = Some(a);
        assert_eq!(doc.validate(), Err(DocumentError::ClipCycle { start: a }));
    }

    #[test]
    fn a_chain_past_the_ceiling_is_refused() {
        let (mut doc, mut store) = document_with_tree();
        let mut previous = doc
            .walk()
            .iter()
            .find(|n| n.name() == "Background")
            .expect("background")
            .id();
        // The ceiling counts the links walked from the asking layer, so a
        // chain one link past it must be refused.
        for step in 0..CLIP_CHAIN_LIMIT + 1 {
            let id = doc.new_layer(format!("Chain {step}"), store.put(Frame::black(4, 4)));
            doc.layer_mut(id).expect("layer").clips_to = Some(previous);
            previous = id;
        }
        assert!(matches!(
            doc.validate(),
            Err(DocumentError::ClipChainTooLong { .. })
        ));
    }

    #[test]
    fn node_count_counts_the_root_group() {
        let (doc, _) = document_with_tree();
        // Root + Background + Folder + Inner.
        assert_eq!(doc.node_count(), 4);
    }
}
