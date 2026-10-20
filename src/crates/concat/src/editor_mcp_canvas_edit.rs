// SPDX-License-Identifier: AGPL-3.0-or-later
//! Closed native editing adapter. Resolve stable IDs immediately before the
//! existing helpers, and read back actual content rather than helper returns.
use std::collections::BTreeSet;

use concat_canvas::{ImageDocument, LayerGroup, LayerId, LayerNode, PixelId};
use concat_editor_mcp::{CanvasEdit, CanvasPaintTarget, LayerDirection};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::panes::canvas::{CanvasMoveCheckedError, CanvasPane};
use concat_editor_mcp::Response;

const MAX_NODES: usize = 10_000;
const MAX_HASH_PIXELS: u64 = 67_108_864;
const MAX_NEW_PIXELS: u64 = 16_777_216;
const MAX_BRUSH_WORK: u64 = 16_777_216;
const MAX_EDIT_BYTES: usize = 16_384;
const MAX_DELETE_NODES: usize = 512;
const MAX_READBACK_BYTES: usize = 12_288;
const MAX_RECEIPT_BYTES: usize = 49_152;

fn layer_id(id: &str) -> Result<LayerId, &'static str> {
    let id = concat_editor_mcp::parse_layer_id(id).map_err(|_| "invalidInput")?;
    serde_json::from_value(json!(id)).map_err(|_| "invalidInput")
}
fn id(id: LayerId) -> String {
    format!("layer:{}", id.as_u64())
}

fn row(pane: &CanvasPane, object: &str) -> Result<i32, &'static str> {
    let target = layer_id(object)?;
    let doc = pane.document.as_ref().ok_or("wrongProject")?;
    if doc.find(target).is_none() {
        return Err("objectGone");
    }
    // Folded descendants remain in the document but have no current panel row.
    pane.agent_layers()
        .iter()
        .position(|r| r.0 == target.as_u64())
        .map(|n| n as i32)
        .ok_or("unsupportedTarget")
}
fn selected(pane: &CanvasPane, object: &str) -> Result<(), &'static str> {
    row(pane, object)?;
    if pane.active != Some(layer_id(object)?) {
        return Err("selectionChanged");
    }
    Ok(())
}
fn paint_target(
    pane: &CanvasPane,
    object: &str,
    target: CanvasPaintTarget,
) -> Result<PixelId, &'static str> {
    selected(pane, object)?;
    if pane.agent_paint_mask() != (target == CanvasPaintTarget::LayerMask) {
        return Err("selectionChanged");
    }
    let doc = pane.document.as_ref().ok_or("wrongProject")?;
    let node = doc.find(layer_id(object)?).ok_or("objectGone")?;
    let pixels = if target == CanvasPaintTarget::LayerMask {
        let mask = node.mask().ok_or("unsupportedTarget")?;
        if mask.vector.is_some() {
            return Err("unsupportedTarget");
        }
        mask.pixels
    } else {
        let LayerNode::Layer(layer) = node else {
            return Err("unsupportedTarget");
        };
        if layer.vector.is_some() || layer.text.is_some() || !layer.transform.is_invertible() {
            return Err("unsupportedTarget");
        }
        layer.pixels
    };
    pane.store.get(pixels).ok_or("unsupportedTarget")?;
    Ok(pixels)
}
fn parent_slot(
    group: &LayerGroup,
    target: LayerId,
    parent: Option<LayerId>,
) -> Option<(Option<LayerId>, usize)> {
    for (index, node) in group.children.iter().enumerate() {
        if node.id() == target {
            return Some((parent, index));
        }
        if let LayerNode::Group(child) = node
            && let Some(slot) = parent_slot(child, target, Some(child.id))
        {
            return Some(slot);
        }
    }
    None
}
fn pixels(doc: &ImageDocument) -> BTreeSet<PixelId> {
    let mut pixels = BTreeSet::new();
    for node in doc.walk() {
        if let LayerNode::Layer(layer) = node {
            pixels.insert(layer.pixels);
        }
        if let Some(mask) = node.mask() {
            pixels.insert(mask.pixels);
        }
    }
    if let Some(mask) = &doc.root.mask {
        pixels.insert(mask.pixels);
    }
    pixels
}
pub(super) fn document_state_id(pane: &CanvasPane) -> Result<String, &'static str> {
    let doc = pane.document.as_ref().ok_or("wrongProject")?;
    if doc.walk().len() > MAX_NODES {
        return Err("resourceLimit");
    }
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(doc).map_err(|_| "ioFailure")?);
    let mut total = 0_u64;
    for key in pixels(doc) {
        let frame = pane.store.get(key).ok_or("unsupportedTarget")?;
        total = total
            .checked_add(u64::from(frame.width()) * u64::from(frame.height()))
            .ok_or("resourceLimit")?;
        if total > MAX_HASH_PIXELS {
            return Err("resourceLimit");
        }
        digest.update(key.as_u64().to_le_bytes());
        digest.update(frame.width().to_le_bytes());
        digest.update(frame.height().to_le_bytes());
        digest.update(frame.pixels());
    }
    Ok(format!("canvas-v1-{:x}", digest.finalize()))
}
fn selection_state(pane: &CanvasPane) -> Option<String> {
    pane.selection.as_ref().map(|mask| {
        let mut digest = Sha256::new();
        digest.update(mask.width.to_le_bytes());
        digest.update(mask.height.to_le_bytes());
        digest.update(&mask.bytes);
        format!("mask-v1-{:x}", digest.finalize())
    })
}
pub(super) fn layer_value(pane: &CanvasPane, target: LayerId) -> Option<Value> {
    let doc = pane.document.as_ref()?;
    let node = doc.find(target)?;
    let (parent, order) = parent_slot(&doc.root, target, None)?;
    let rows = pane.agent_layers();
    let expanded = matches!(node, LayerNode::Group(_))
        .then(|| rows.iter().find(|r| r.0 == target.as_u64()).map(|r| r.6))
        .flatten();
    Some(
        json!({"id":id(target),"kind":match node{LayerNode::Layer(_)=>"image",LayerNode::Group(_)=>"group",LayerNode::Adjustment(_)=>"adjustment"},
            "name":node.name().chars().take(256).collect::<String>(),"parentId":parent.map(id),"order":order,
            "hidden":node.hidden(),"opacity":node.opacity(),"hasMask":node.mask().is_some(),
            "maskEnabled":node.mask().map(|m|m.enabled),"expanded":expanded,
            "parameters":if let LayerNode::Adjustment(a)=node{Some(json!(a.adjustment))}else{None},
        }),
    )
}
pub(super) fn readback(pane: &CanvasPane, related: &[String]) -> Value {
    let mut ids = related
        .iter()
        .filter_map(|v| layer_id(v).ok())
        .collect::<BTreeSet<_>>();
    if let Some(active) = pane.active {
        ids.insert(active);
    }
    let layers = ids
        .into_iter()
        .filter_map(|target| layer_value(pane, target))
        .collect::<Vec<_>>();
    let brush = pane.agent_brush();
    let (kind, parameters) = pane.agent_adjustment_state();
    json!({
        "rootLayerId":pane.document.as_ref().map(|doc|id(doc.root.id)),
        "selectedLayerId":pane.active.map(id),
        "selectionBounds":pane.agent_selection_bounds().map(|(x,y,width,height)|json!({"x":x,"y":y,"width":width,"height":height})),
        "selectionStateId":selection_state(pane),"paintingLayerMask":pane.agent_paint_mask(),
        "brush":{"diameter":brush.diameter,"opacity":brush.opacity,"hardness":brush.hardness,"colorRgb":brush.color,"eraser":brush.erasing},
        "adjustment":{"kind":kind,"parameters":parameters.into_iter().enumerate().map(|(index,(name,value,min,max))|json!({"index":index,"name":name,"value":value,"min":min,"max":max})).collect::<Vec<_>>()},
        "curves":pane.agent_curve_channels(),"canUndo":pane.can_undo(),"canRedo":pane.can_redo(),"layers":layers,
    })
}
pub(super) fn state(pane: &CanvasPane, related: &[String]) -> Result<Value, &'static str> {
    let document = document_state_id(pane)?;
    Ok(
        json!({"documentStateId":document,"editStateId":edit_state_id(pane,&document),"readback":readback(pane,related)}),
    )
}

/// Stable edit context, independent of readback target subsets and publish
/// epochs. Selection hashes cover every mask byte, including holes with equal
/// bounds; brush/tool and native destination identity participate explicitly.
fn edit_state_id(pane: &CanvasPane, document: &str) -> String {
    let brush = pane.agent_brush();
    let data = json!({
        "documentStateId":document,"active":pane.active.map(id),"cachedPixelId":pane.layer.map(|p|p.as_u64()),
        "pixelSelectionStateId":selection_state(pane),"paintingLayerMask":pane.agent_paint_mask(),
        "brush":{"diameter":brush.diameter,"opacity":brush.opacity,"hardness":brush.hardness,"colorRgb":brush.color,"eraser":brush.erasing},
        "tool":pane.tool,"rows":pane.agent_layers().iter().map(|row|json!([row.0,row.6])).collect::<Vec<_>>(),
        "canUndo":pane.can_undo(),"canRedo":pane.can_redo(),
    });
    format!(
        "edit-v1-{:x}",
        Sha256::digest(serde_json::to_vec(&data).expect("native edit context serializes"))
    )
}

pub(super) fn preflight(
    pane: &CanvasPane,
    edit: &CanvasEdit,
    expected_edit_state_id: &str,
) -> Result<(), &'static str> {
    edit.validate().map_err(|_| "invalidInput")?;
    let doc = pane.document.as_ref().ok_or("wrongProject")?;
    if pane.mcp_state().2 {
        return Err("busy");
    }
    if edit_state_id(pane, &document_state_id(pane)?) != expected_edit_state_id {
        return Err("staleContext");
    }
    if let Some(object) = edit.object_id() {
        row(pane, object)?;
    }
    // Reserve space for the original edit, complete deletion IDs and native
    // readback. Never truncate identity arrays after a committed mutation.
    let edit_bytes = serde_json::to_vec(edit).map_err(|_| "invalidInput")?.len();
    if edit_bytes > MAX_EDIT_BYTES {
        return Err("resourceLimit");
    }
    let related = edit
        .object_id()
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    let readback_bytes = serde_json::to_vec(&readback(pane, &related))
        .map_err(|_| "ioFailure")?
        .len();
    if readback_bytes > MAX_READBACK_BYTES {
        return Err("resourceLimit");
    }
    let deleted_count = if let CanvasEdit::LayerDelete { object_id } = edit {
        let target = layer_id(object_id)?;
        match doc.find(target).ok_or("objectGone")? {
            LayerNode::Group(group) => {
                let mut count = 1;
                let mut pending = group.children.iter().collect::<Vec<_>>();
                while let Some(node) = pending.pop() {
                    count += 1;
                    if count > MAX_DELETE_NODES {
                        break;
                    }
                    if let LayerNode::Group(child) = node {
                        pending.extend(child.children.iter());
                    }
                }
                count
            }
            _ => 1,
        }
    } else {
        0
    };
    if deleted_count > MAX_DELETE_NODES
        || edit_bytes + readback_bytes + deleted_count * 32 + 12_288 > MAX_RECEIPT_BYTES
    {
        return Err("resourceLimit");
    }
    if matches!(edit, CanvasEdit::Undo | CanvasEdit::Redo) {
        let target = pane
            .mcp_history_target(matches!(edit, CanvasEdit::Redo))
            .ok_or("unsupportedTarget")?;
        let before = doc.walk().iter().map(|n| n.id()).collect::<BTreeSet<_>>();
        let after = target
            .document
            .walk()
            .iter()
            .map(|n| n.id())
            .collect::<BTreeSet<_>>();
        let created = after.difference(&before).copied().collect::<BTreeSet<_>>();
        let delta = created.len() + before.difference(&after).count();
        if after.len() > MAX_NODES || delta > MAX_DELETE_NODES {
            return Err("resourceLimit");
        }
        let mut row_ids = created;
        if let Some(active) = target.active {
            row_ids.insert(active);
        }
        // Names are bounded at 256 characters in the actual row projection;
        // leave 768 bytes per row for other scalar fields and escaped names.
        let mut projected = 4096_usize;
        for key in row_ids {
            if let Some(node) = target.document.find(key) {
                projected = projected.saturating_add(
                    768 + node
                        .name()
                        .chars()
                        .take(256)
                        .map(|c| if c.is_control() { 6 } else { c.len_utf8() })
                        .sum::<usize>(),
                );
                if let LayerNode::Adjustment(a) = node {
                    let parameters = serde_json::to_vec(&a.adjustment)
                        .map_err(|_| "ioFailure")?
                        .len();
                    projected = projected.saturating_add(
                        parameters.saturating_mul(if target.active == Some(key) { 3 } else { 1 }),
                    );
                }
            }
        }
        if edit_bytes + projected.max(readback_bytes) + delta * 32 + 12_288 > MAX_RECEIPT_BYTES {
            return Err("resourceLimit");
        }
        let mut total = 0_u64;
        for key in pixels(target.document) {
            let frame = target.store.get(key).ok_or("unsupportedTarget")?;
            total = total.saturating_add(u64::from(frame.width()) * u64::from(frame.height()));
            if total > MAX_HASH_PIXELS {
                return Err("resourceLimit");
            }
        }
    }
    let allocation = match edit {
        CanvasEdit::LayerAdd => u64::from(doc.width) * u64::from(doc.height),
        CanvasEdit::MaskAdd { object_id }
            if doc
                .find(layer_id(object_id)?)
                .and_then(LayerNode::mask)
                .is_none() =>
        {
            u64::from(doc.width) * u64::from(doc.height)
        }
        _ => 0,
    };
    if allocation > 0 {
        let mut total = allocation;
        for key in pixels(doc) {
            let frame = pane.store.get(key).ok_or("unsupportedTarget")?;
            total = total
                .checked_add(u64::from(frame.width()) * u64::from(frame.height()))
                .ok_or("resourceLimit")?;
            if total > MAX_HASH_PIXELS {
                return Err("resourceLimit");
            }
        }
    }
    match edit {
        CanvasEdit::LayerAdd
        | CanvasEdit::MaskAdd { .. }
        | CanvasEdit::SelectionAll
        | CanvasEdit::SelectionRect { .. } => {
            if u64::from(doc.width) * u64::from(doc.height) > MAX_NEW_PIXELS {
                return Err("resourceLimit");
            }
        }
        _ => {}
    }
    match edit {
        CanvasEdit::LayerAdd | CanvasEdit::GroupAdd | CanvasEdit::AdjustmentAdd { .. }
            if doc.walk().len() >= MAX_NODES =>
        {
            return Err("resourceLimit");
        }
        CanvasEdit::LayerMoveInto {
            object_id,
            parent_id: Some(parent),
        } => {
            row(pane, parent)?;
            let moving = layer_id(object_id)?;
            let target = layer_id(parent)?;
            if !matches!(doc.find(target), Some(LayerNode::Group(_))) {
                return Err("unsupportedTarget");
            }
            if moving == target
                || matches!(doc.find(moving),Some(LayerNode::Group(g)) if g.find(target).is_some())
            {
                return Err("invalidInput");
            }
        }
        CanvasEdit::GroupSetExpanded { object_id, .. } => {
            if !matches!(doc.find(layer_id(object_id)?), Some(LayerNode::Group(_))) {
                return Err("unsupportedTarget");
            }
        }
        CanvasEdit::SelectionRect {
            x,
            y,
            width,
            height,
            ..
        } => {
            if *x < 0.0
                || *y < 0.0
                || x + width > f64::from(doc.width)
                || y + height > f64::from(doc.height)
            {
                return Err("invalidInput");
            }
        }
        CanvasEdit::SelectionFill { object_id, target }
        | CanvasEdit::SelectionDelete { object_id, target } => {
            let pixels = paint_target(pane, object_id, *target)?;
            let frame = pane.store.get(pixels).ok_or("unsupportedTarget")?;
            if u64::from(frame.width()) * u64::from(frame.height()) > MAX_NEW_PIXELS {
                return Err("resourceLimit");
            }
            pane.mcp_paint_path(&[]).ok_or("unsupportedTarget")?;
            if pane.agent_selection_bounds().is_none() {
                return Err("unsupportedTarget");
            }
        }
        CanvasEdit::BrushSet { .. }
            if pane.agent_paint_mask()
                && pane
                    .active
                    .and_then(|id| doc.find(id))
                    .and_then(LayerNode::mask)
                    .is_some_and(|mask| mask.vector.is_some()) =>
        {
            return Err("unsupportedTarget");
        }
        CanvasEdit::BrushStroke {
            object_id,
            target,
            points,
        } => {
            paint_target(pane, object_id, *target)?;
            if points.iter().any(|p| {
                p.x < 0.0
                    || p.y < 0.0
                    || p.x >= f64::from(doc.width)
                    || p.y >= f64::from(doc.height)
            }) {
                return Err("invalidInput");
            }
            let path = points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
            let crate::panes::canvas::MappedPaintPath {
                points: mapped,
                width,
                height,
            } = pane.mcp_paint_path(&path).ok_or("unsupportedTarget")?;
            let first = mapped[0];
            if first.0 < 0.0
                || first.1 < 0.0
                || first.0 >= f64::from(width)
                || first.1 >= f64::from(height)
            {
                return Err("invalidInput");
            }
            concat_canvas::BrushStroke::estimate_work(
                width,
                height,
                pane.agent_brush(),
                &mapped,
                MAX_BRUSH_WORK,
            )
            .map_err(|_| "resourceLimit")?;
        }
        CanvasEdit::MaskRemove { object_id }
        | CanvasEdit::MaskSetEnabled { object_id, .. }
        | CanvasEdit::MaskSelect { object_id } => {
            let mask = doc
                .find(layer_id(object_id)?)
                .and_then(LayerNode::mask)
                .ok_or("unsupportedTarget")?;
            if matches!(edit, CanvasEdit::MaskSelect { .. }) && mask.vector.is_some() {
                return Err("unsupportedTarget");
            }
        }
        CanvasEdit::Undo if !pane.can_undo() => return Err("unsupportedTarget"),
        CanvasEdit::Redo if !pane.can_redo() => return Err("unsupportedTarget"),
        CanvasEdit::AdjustmentSetParameter {
            object_id,
            index,
            value,
        } => {
            selected(pane, object_id)?;
            let (_, params) = pane.agent_adjustment_state();
            let (_, _, min, max) = params.get(*index as usize).ok_or("unsupportedTarget")?;
            if (*value as f32) < *min || (*value as f32) > *max {
                return Err("invalidInput");
            }
        }
        CanvasEdit::CurveSet {
            object_id, channel, ..
        }
        | CanvasEdit::CurveAdd {
            object_id, channel, ..
        }
        | CanvasEdit::CurveRemove {
            object_id, channel, ..
        } => {
            selected(pane, object_id)?;
            let channels = pane.agent_curve_channels();
            let points = channels.get(channel.index()).ok_or("unsupportedTarget")?;
            if points.len() < 2
                || points.len() > 16
                || points.windows(2).any(|pair| pair[1].0 <= pair[0].0)
            {
                return Err("unsupportedTarget");
            }
            match edit {
                CanvasEdit::CurveSet { index, x, .. } => {
                    let n = *index as usize;
                    if n >= points.len() {
                        return Err("invalidInput");
                    }
                    if (n == 0 && *x != 0.0)
                        || (n + 1 == points.len() && *x != 1.0)
                        || (n > 0
                            && n + 1 < points.len()
                            && (points[n - 1].0 + 0.001 > points[n + 1].0 - 0.001
                                || (*x as f32) < points[n - 1].0 + 0.001
                                || (*x as f32) > points[n + 1].0 - 0.001))
                    {
                        return Err("invalidInput");
                    }
                }
                CanvasEdit::CurveAdd { x, .. } => {
                    if points.len() >= 16 {
                        return Err("resourceLimit");
                    }
                    if *x <= 0.0
                        || *x >= 1.0
                        || points.iter().any(|p| (p.0 - (*x as f32)).abs() < 0.002)
                    {
                        return Err("invalidInput");
                    }
                }
                CanvasEdit::CurveRemove { index, .. } => {
                    if *index == 0 || (*index as usize) + 1 >= points.len() {
                        return Err("invalidInput");
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

enum EditCheckedError {
    Edit(&'static str),
    Commit(CanvasMoveCheckedError<Response>),
}
impl From<&'static str> for EditCheckedError {
    fn from(status: &'static str) -> Self {
        Self::Edit(status)
    }
}

/// Finish all read-only preparation, then require a fresh commit check before
/// the first helper can mutate native history, document or editor state.
pub(super) fn apply_checked(
    pane: &mut CanvasPane,
    edit: &CanvasEdit,
    expected_edit_state_id: &str,
    commit: impl FnOnce(&CanvasPane) -> Result<(), CanvasMoveCheckedError<Response>>,
) -> Result<Value, CanvasMoveCheckedError<Response>> {
    apply_inner(pane, edit, expected_edit_state_id, commit).map_err(|error| match error {
        EditCheckedError::Edit(status) => CanvasMoveCheckedError::Commit(Response::error(status)),
        EditCheckedError::Commit(error) => error,
    })
}

#[cfg(test)]
fn apply(pane: &mut CanvasPane, edit: &CanvasEdit, expected: &str) -> Result<Value, &'static str> {
    apply_inner(pane, edit, expected, |_| Ok(())).map_err(|error| match error {
        EditCheckedError::Edit(status) => status,
        EditCheckedError::Commit(_) => "unexpectedTestCommitFailure",
    })
}

fn apply_inner(
    pane: &mut CanvasPane,
    edit: &CanvasEdit,
    expected_edit_state_id: &str,
    commit: impl FnOnce(&CanvasPane) -> Result<(), CanvasMoveCheckedError<Response>>,
) -> Result<Value, EditCheckedError> {
    preflight(pane, edit, expected_edit_state_id)?;
    let source = document_state_id(pane)?;
    let source_edit_state = edit_state_id(pane, &source);
    let source_revision = pane.mcp_state().1;
    let before = readback(pane, &[]);
    let before_tool = pane.tool;
    let before_rows = pane.agent_layers();
    let before_ids = pane
        .document
        .as_ref()
        .ok_or("wrongProject")?
        .walk()
        .iter()
        .map(|n| n.id())
        .collect::<BTreeSet<_>>();
    let target = edit
        .object_id()
        .map(|object| row(pane, object))
        .transpose()?;
    // Hashes, budgets, source snapshots and stable-ID row resolution are now
    // ready. No event-loop reentry occurs between this guard and mutation.
    commit(pane).map_err(EditCheckedError::Commit)?;
    match edit {
        CanvasEdit::LayerSelect { .. } => pane.agent_layer_pick(target.ok_or("objectGone")?),
        CanvasEdit::LayerAdd => pane.agent_layer_add(),
        CanvasEdit::LayerDelete { .. } => pane.agent_layer_delete(target.ok_or("objectGone")?),
        CanvasEdit::LayerMove { direction, .. } => pane.agent_layer_move(
            target.ok_or("objectGone")?,
            if *direction == LayerDirection::Up {
                -1
            } else {
                1
            },
        ),
        CanvasEdit::GroupAdd => pane.agent_layer_group(),
        CanvasEdit::LayerMoveInto { parent_id, .. } => pane.agent_layer_move_into(
            target.ok_or("objectGone")?,
            parent_id.as_ref().map(|p| row(pane, p)).transpose()?,
        ),
        CanvasEdit::GroupSetExpanded { expanded, .. } => {
            let r = target.ok_or("objectGone")? as usize;
            if before_rows[r].6 != *expanded {
                pane.agent_layer_fold(r as i32);
            }
        }
        CanvasEdit::LayerSetVisibility { visible, .. } => {
            let r = target.ok_or("objectGone")? as usize;
            if before_rows[r].2 == *visible {
                pane.agent_layer_toggle_visibility(r as i32);
            }
        }
        CanvasEdit::LayerSetOpacity { opacity, .. } => {
            let r = target.ok_or("objectGone")? as usize;
            if before_rows[r].3 != *opacity as f32 {
                pane.agent_layer_opacity(r as i32, *opacity as f32);
            }
        }
        CanvasEdit::SelectionRect {
            x,
            y,
            width,
            height,
            additive,
        } => {
            if !additive {
                pane.agent_deselect();
            }
            pane.agent_select_rect(*x, *y, *width, *height);
        }
        CanvasEdit::SelectionAll => pane.agent_select_all(),
        CanvasEdit::SelectionClear => pane.agent_deselect(),
        CanvasEdit::SelectionFill { .. } => pane.agent_fill_selection(),
        CanvasEdit::SelectionDelete { .. } => pane.agent_delete_selection(),
        CanvasEdit::MaskAdd { .. } => {
            let r = target.ok_or("objectGone")? as usize;
            if !before_rows[r].8 {
                pane.agent_layer_mask_add(r as i32);
            }
        }
        CanvasEdit::MaskRemove { .. } => pane.agent_layer_mask_remove(target.ok_or("objectGone")?),
        CanvasEdit::MaskSetEnabled { enabled, .. } => {
            let r = target.ok_or("objectGone")? as usize;
            if before_rows[r].10 != *enabled {
                pane.agent_layer_mask_toggle(r as i32);
            }
        }
        CanvasEdit::MaskSelect { .. } => pane.agent_layer_mask_select(target.ok_or("objectGone")?),
        CanvasEdit::BrushSet {
            diameter,
            opacity,
            hardness,
            color_rgb,
            eraser,
        } => {
            pane.agent_set_brush(*diameter, *opacity, *hardness, 0);
            pane.agent_set_brush_rgb(color_rgb[0], color_rgb[1], color_rgb[2]);
            pane.agent_select_tool(if *eraser { 4 } else { 3 });
        }
        CanvasEdit::BrushStroke { points, .. } => {
            pane.agent_paint_stroke(&points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>())
        }
        CanvasEdit::Undo => pane.agent_undo(),
        CanvasEdit::Redo => pane.agent_redo(),
        CanvasEdit::AdjustmentAdd { adjustment } => pane.agent_adjustment_add(adjustment.index()),
        CanvasEdit::AdjustmentSetParameter { index, value, .. } => {
            if pane.agent_adjustment_state().1[*index as usize].1 != *value as f32 {
                pane.agent_adjustment_param(*index as i32, *value as f32);
            }
        }
        CanvasEdit::CurveSet {
            channel,
            index,
            x,
            y,
            ..
        } => pane.agent_curve_set(channel.index() as i32, *index as i32, *x, *y),
        CanvasEdit::CurveAdd { channel, x, y, .. } => {
            pane.agent_curve_add(channel.index() as i32, *x, *y)
        }
        CanvasEdit::CurveRemove { channel, index, .. } => {
            pane.agent_curve_remove(channel.index() as i32, *index as i32)
        }
    }
    let after_ids = pane
        .document
        .as_ref()
        .ok_or("wrongProject")?
        .walk()
        .iter()
        .map(|n| n.id())
        .collect::<BTreeSet<_>>();
    let created = after_ids
        .difference(&before_ids)
        .copied()
        .map(id)
        .collect::<Vec<_>>();
    let deleted = before_ids
        .difference(&after_ids)
        .copied()
        .map(id)
        .collect::<Vec<_>>();
    let mut related = edit
        .object_id()
        .map(str::to_owned)
        .into_iter()
        .chain(created.iter().cloned())
        .collect::<Vec<_>>();
    if let CanvasEdit::LayerMoveInto {
        parent_id: Some(parent),
        ..
    } = edit
    {
        related.push(parent.clone());
    }
    let after = readback(pane, &related);
    let document_state = document_state_id(pane).map_err(|_| "outcomeUnknown")?;
    let document_changed = source != document_state;
    // Compare selection/tool state without target-dependent layer subsets or
    // history availability; a no-op pixel helper can still allocate history.
    let mut before_view = before.clone();
    before_view.as_object_mut().unwrap().remove("layers");
    before_view.as_object_mut().unwrap().remove("canUndo");
    before_view.as_object_mut().unwrap().remove("canRedo");
    let mut after_view = after.clone();
    after_view.as_object_mut().unwrap().remove("layers");
    after_view.as_object_mut().unwrap().remove("canUndo");
    after_view.as_object_mut().unwrap().remove("canRedo");
    let changed = document_changed
        || before_view != after_view
        || before_tool != pane.tool
        || before_rows != pane.agent_layers();
    Ok(
        json!({"edit":edit,"changed":changed,"documentChanged":document_changed,
            "sourceDocumentStateId":source,"documentStateId":document_state,"sourceRevision":source_revision,
            "sourceEditStateId":source_edit_state,"editStateId":edit_state_id(pane,&document_state),
            "revision":pane.mcp_state().1,"createdLayerIds":created,"deletedLayerIds":deleted,
            "selectedObjectIds":pane.active.map(id).into_iter().collect::<Vec<_>>(),"readback":after,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_core::frame::Frame;
    use concat_editor_mcp::{CanvasAdjustment, CanvasPoint, CurveChannel};
    fn apply(pane: &mut CanvasPane, edit: &CanvasEdit) -> Result<Value, &'static str> {
        let expected = edit_state_id(pane, &document_state_id(pane)?);
        super::apply(pane, edit, &expected)
    }
    fn preflight(pane: &CanvasPane, edit: &CanvasEdit) -> Result<(), &'static str> {
        let expected = edit_state_id(pane, &document_state_id(pane)?);
        super::preflight(pane, edit, &expected)
    }
    fn fixture() -> CanvasPane {
        let mut p = CanvasPane::default();
        let mut d = ImageDocument::new(12, 8);
        let pixels = p.store.put(Frame::transparent(12, 8));
        p.active = Some(d.new_layer("test", pixels));
        p.layer = Some(pixels);
        p.document = Some(d);
        p
    }
    fn active(p: &CanvasPane) -> String {
        id(p.active.unwrap())
    }
    #[test]
    fn canvas_edit_commit_rechecks_owner_and_authorization_after_preparation() {
        use crate::panes::canvas::CanvasMoveError;
        for owner_failure in [true, false] {
            let mut p = fixture();
            let original = document_state_id(&p).unwrap();
            let revision = p.mcp_state().1;
            let original_state = state(&p, &[]).unwrap();
            let expected = original_state["editStateId"].as_str().unwrap().to_owned();
            let checked = std::cell::Cell::new(false);
            let result =
                super::apply_checked(&mut p, &CanvasEdit::LayerAdd, &expected, |prepared| {
                    // Invoked only once hashes/budgets/source snapshots are ready;
                    // model a writer or permission invalidation at this boundary.
                    checked.set(true);
                    assert_eq!(state(prepared, &[]).unwrap(), original_state);
                    if owner_failure {
                        Err(CanvasMoveCheckedError::Canvas(
                            CanvasMoveError::OwnerMismatch,
                        ))
                    } else {
                        Err(CanvasMoveCheckedError::Commit(Response::error(
                            "notAuthorized",
                        )))
                    }
                });
            assert!(checked.get());
            match result {
                Err(CanvasMoveCheckedError::Canvas(CanvasMoveError::OwnerMismatch))
                    if owner_failure => {}
                Err(CanvasMoveCheckedError::Commit(response)) if !owner_failure => {
                    assert_eq!(response.status, "notAuthorized")
                }
                _ => panic!("commit refusal must retain the existing checked-error mapping"),
            }
            assert_eq!(document_state_id(&p).unwrap(), original);
            assert_eq!(p.mcp_state().1, revision);
            assert_eq!(state(&p, &[]).unwrap(), original_state);
            assert!(!p.can_undo());
            assert!(!p.can_redo());
        }
    }
    #[test]
    fn canvas_edit_state_rejects_mask_and_brush_drift_before_any_native_edit() {
        let mut p = fixture();
        let object = active(&p);
        apply(&mut p, &CanvasEdit::SelectionAll).unwrap();
        let original = document_state_id(&p).unwrap();
        let revision = p.mcp_state().1;
        let action = CanvasEdit::SelectionFill {
            object_id: object.clone(),
            target: CanvasPaintTarget::LayerPixels,
        };
        let expected = state(&p, &[]).unwrap()["editStateId"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            state(&p, &[object.clone()]).unwrap()["editStateId"],
            expected,
            "stamp is independent of related layer subsets"
        );
        let bounds = p.agent_selection_bounds();
        p.selection.as_mut().unwrap().bytes[4] = 0;
        assert_eq!(p.agent_selection_bounds(), bounds);
        assert_eq!(
            super::apply(&mut p, &action, &expected),
            Err("staleContext")
        );
        assert_eq!(document_state_id(&p).unwrap(), original);
        assert_eq!(p.mcp_state().1, revision);
        assert!(!p.can_undo());
        let expected = state(&p, &[]).unwrap()["editStateId"]
            .as_str()
            .unwrap()
            .to_owned();
        p.brush.color = [255, 0, 0];
        assert_eq!(
            super::apply(&mut p, &action, &expected),
            Err("staleContext")
        );
        assert_eq!(document_state_id(&p).unwrap(), original);
        assert_eq!(p.mcp_state().1, revision);
        assert!(!p.can_undo());
        let expected = state(&p, &[]).unwrap()["editStateId"]
            .as_str()
            .unwrap()
            .to_owned();
        let result = super::apply(&mut p, &action, &expected).unwrap();
        assert_eq!(result["sourceEditStateId"], expected);
        assert_eq!(result["changed"], true);
        assert_eq!(
            result["editStateId"],
            state(&p, &[]).unwrap()["editStateId"]
        );
    }
    #[test]
    fn canvas_edit_mask_select_is_explicit_and_idempotent() {
        let mut p = fixture();
        let object = active(&p);
        apply(
            &mut p,
            &CanvasEdit::MaskAdd {
                object_id: object.clone(),
            },
        )
        .unwrap();
        apply(
            &mut p,
            &CanvasEdit::LayerSelect {
                object_id: object.clone(),
            },
        )
        .unwrap();
        assert!(!p.agent_paint_mask());
        let action = CanvasEdit::MaskSelect { object_id: object };
        let first = apply(&mut p, &action).unwrap();
        assert!(p.agent_paint_mask());
        assert_eq!(first["changed"], true);
        let expected = state(&p, &[]).unwrap()["editStateId"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = super::apply(&mut p, &action, &expected).unwrap();
        assert!(p.agent_paint_mask());
        assert_eq!(second["changed"], false);
        assert_eq!(second["editStateId"], expected);
    }
    #[test]
    fn canvas_edit_work_and_receipt_budgets_reject_before_document_or_history_changes() {
        let mut p = CanvasPane::default();
        let mut doc = ImageDocument::new(1024, 1024);
        let pixels = p.store.put(Frame::transparent(1024, 1024));
        p.active = Some(doc.new_layer("budget", pixels));
        p.layer = Some(pixels);
        p.document = Some(doc);
        let object = active(&p);
        let original = document_state_id(&p).unwrap();
        let revision = p.mcp_state().1;
        apply(
            &mut p,
            &CanvasEdit::BrushSet {
                diameter: 2000.0,
                opacity: 1.0,
                hardness: 0.0,
                color_rgb: [255, 0, 0],
                eraser: false,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut p,
                &CanvasEdit::BrushStroke {
                    object_id: object.clone(),
                    target: CanvasPaintTarget::LayerPixels,
                    points: vec![CanvasPoint { x: 512.0, y: 512.0 }]
                }
            ),
            Err("resourceLimit")
        );
        apply(
            &mut p,
            &CanvasEdit::BrushSet {
                diameter: 20.0,
                opacity: 1.0,
                hardness: 1.0,
                color_rgb: [255, 0, 0],
                eraser: false,
            },
        )
        .unwrap();
        let points = (0..64)
            .map(|i| {
                if i % 2 == 0 {
                    CanvasPoint { x: 16.0, y: 16.0 }
                } else {
                    CanvasPoint {
                        x: 1000.0,
                        y: 1000.0,
                    }
                }
            })
            .collect();
        assert_eq!(
            apply(
                &mut p,
                &CanvasEdit::BrushStroke {
                    object_id: object.clone(),
                    target: CanvasPaintTarget::LayerPixels,
                    points
                }
            ),
            Err("resourceLimit")
        );
        assert_eq!(document_state_id(&p).unwrap(), original);
        assert_eq!(p.mcp_state().1, revision);
        assert!(!p.can_undo());
        assert!(
            preflight(
                &p,
                &CanvasEdit::BrushStroke {
                    object_id: object,
                    target: CanvasPaintTarget::LayerPixels,
                    points: vec![
                        CanvasPoint { x: 16.0, y: 16.0 },
                        CanvasPoint { x: 24.0, y: 24.0 }
                    ]
                }
            )
            .is_ok()
        );
        // A large group deletion must remain queryable through the bounded
        // original-operation receipt; reject it without deleting any IDs.
        let group = p.document.as_mut().unwrap().new_group("large");
        for _ in 0..MAX_DELETE_NODES {
            let child = p
                .document
                .as_mut()
                .unwrap()
                .new_adjustment("invert", concat_canvas::Adjustment::Invert);
            p.document
                .as_mut()
                .unwrap()
                .move_node(child, Some(group), 0);
        }
        let original = document_state_id(&p).unwrap();
        assert_eq!(
            apply(
                &mut p,
                &CanvasEdit::LayerDelete {
                    object_id: id(group)
                }
            ),
            Err("resourceLimit")
        );
        assert_eq!(document_state_id(&p).unwrap(), original);
        assert!(!p.can_undo());
        let group_row = p
            .agent_layers()
            .iter()
            .position(|row| row.0 == group.as_u64())
            .unwrap();
        p.agent_layer_delete(group_row as i32);
        let before_history = document_state_id(&p).unwrap();
        let before_revision = p.mcp_state().1;
        assert_eq!(
            apply(&mut p, &CanvasEdit::Undo),
            Err("resourceLimit"),
            "a large UI history restoration must be checked before undo"
        );
        assert_eq!(document_state_id(&p).unwrap(), before_history);
        assert_eq!(p.mcp_state().1, before_revision);
    }

    #[test]
    fn canvas_edit_unchanged_pixel_content_is_never_reported_as_changed() {
        let mut p = fixture();
        let object = active(&p);
        apply(
            &mut p,
            &CanvasEdit::BrushSet {
                diameter: 2.0,
                opacity: 1.0,
                hardness: 1.0,
                color_rgb: [0, 0, 0],
                eraser: true,
            },
        )
        .unwrap();
        let result = apply(
            &mut p,
            &CanvasEdit::BrushStroke {
                object_id: object,
                target: CanvasPaintTarget::LayerPixels,
                points: vec![CanvasPoint { x: 4.0, y: 3.0 }],
            },
        )
        .unwrap();
        assert_eq!(result["documentChanged"], false);
        assert_eq!(result["changed"], false);
        assert_eq!(result["sourceDocumentStateId"], result["documentStateId"]);
    }
    #[test]
    fn canvas_edit_transformed_multisample_stroke_keeps_document_coordinates() {
        let mut p = fixture();
        let layer = p.active.unwrap();
        let transform = &mut p
            .document
            .as_mut()
            .unwrap()
            .layer_mut(layer)
            .unwrap()
            .transform;
        transform.x = 2.0;
        transform.scale_x = 2.0;
        transform.scale_y = 2.0;
        apply(
            &mut p,
            &CanvasEdit::BrushSet {
                diameter: 2.0,
                opacity: 1.0,
                hardness: 1.0,
                color_rgb: [255, 0, 0],
                eraser: false,
            },
        )
        .unwrap();
        let object = active(&p);
        apply(
            &mut p,
            &CanvasEdit::BrushStroke {
                object_id: object,
                target: CanvasPaintTarget::LayerPixels,
                points: vec![
                    CanvasPoint { x: 2.0, y: 2.0 },
                    CanvasPoint { x: 8.0, y: 2.0 },
                ],
            },
        )
        .unwrap();
        let frame = p.store.get(p.layer.unwrap()).unwrap();
        assert!(
            frame.pixel(6, 3).unwrap()[3] > 0,
            "mapped stroke end was painted"
        );
        assert_eq!(
            frame.pixel(8, 2).unwrap()[3],
            0,
            "document coordinate was not used as a source point"
        );
    }

    #[test]
    fn canvas_edit_all_local_helper_families_have_real_readback() {
        let mut p = fixture();
        let first = active(&p);
        apply(&mut p, &CanvasEdit::LayerAdd).unwrap();
        let second = active(&p);
        apply(
            &mut p,
            &CanvasEdit::LayerMove {
                object_id: first.clone(),
                direction: LayerDirection::Up,
            },
        )
        .unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&first).unwrap()).unwrap()["order"],
            1
        );
        apply(
            &mut p,
            &CanvasEdit::LayerMove {
                object_id: first.clone(),
                direction: LayerDirection::Down,
            },
        )
        .unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&first).unwrap()).unwrap()["order"],
            0
        );
        apply(
            &mut p,
            &CanvasEdit::LayerSetVisibility {
                object_id: first.clone(),
                visible: false,
            },
        )
        .unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&first).unwrap()).unwrap()["hidden"],
            true
        );
        apply(&mut p, &CanvasEdit::GroupAdd).unwrap();
        let group = active(&p);
        apply(
            &mut p,
            &CanvasEdit::LayerMoveInto {
                object_id: second.clone(),
                parent_id: Some(group.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&second).unwrap()).unwrap()["parentId"],
            group
        );
        apply(
            &mut p,
            &CanvasEdit::LayerMoveInto {
                object_id: second.clone(),
                parent_id: None,
            },
        )
        .unwrap();
        assert!(layer_value(&p, layer_id(&second).unwrap()).unwrap()["parentId"].is_null());
        apply(
            &mut p,
            &CanvasEdit::LayerSelect {
                object_id: second.clone(),
            },
        )
        .unwrap();
        apply(
            &mut p,
            &CanvasEdit::MaskAdd {
                object_id: second.clone(),
            },
        )
        .unwrap();
        apply(
            &mut p,
            &CanvasEdit::LayerSelect {
                object_id: second.clone(),
            },
        )
        .unwrap();
        assert!(!p.agent_paint_mask());
        apply(
            &mut p,
            &CanvasEdit::MaskSelect {
                object_id: second.clone(),
            },
        )
        .unwrap();
        assert!(p.agent_paint_mask());
        apply(&mut p, &CanvasEdit::SelectionAll).unwrap();
        let deleted = apply(
            &mut p,
            &CanvasEdit::SelectionDelete {
                object_id: second.clone(),
                target: CanvasPaintTarget::LayerMask,
            },
        )
        .unwrap();
        assert_eq!(deleted["documentChanged"], true);
        apply(&mut p, &CanvasEdit::SelectionClear).unwrap();
        assert!(p.selection.is_none());
        apply(
            &mut p,
            &CanvasEdit::MaskRemove {
                object_id: second.clone(),
            },
        )
        .unwrap();
        assert!(!p.agent_paint_mask());
        apply(
            &mut p,
            &CanvasEdit::AdjustmentAdd {
                adjustment: CanvasAdjustment::Exposure,
            },
        )
        .unwrap();
        let adjustment = active(&p);
        apply(
            &mut p,
            &CanvasEdit::AdjustmentSetParameter {
                object_id: adjustment,
                index: 0,
                value: 1.0,
            },
        )
        .unwrap();
        assert_eq!(p.agent_adjustment_state().1[0].1, 1.0);
        let deleted = apply(
            &mut p,
            &CanvasEdit::LayerDelete {
                object_id: first.clone(),
            },
        )
        .unwrap();
        assert_eq!(deleted["deletedLayerIds"], json!([first]));
        let read = state(&p, &[second]).unwrap();
        assert_eq!(read["documentStateId"], document_state_id(&p).unwrap());
        assert!(read["readback"]["canUndo"].as_bool().unwrap());
    }

    #[test]
    fn canvas_edit_original_registry_replay_does_not_repeat_native_history() {
        use super::super::write_registry::{
            Begin, OperationKey, RegistryError, Scope, WriteRegistry,
        };
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let scope = Scope {
            instance: "i".into(),
            project: "p".into(),
            session: "s".into(),
        };
        let key = OperationKey {
            client: "c".into(),
            document_session: "s".into(),
            client_sequence: 1,
        };
        let edit = CanvasEdit::LayerAdd;
        let mut registry = WriteRegistry::<CanvasEdit, Value>::new();
        registry.bind_scope(Some(scope.clone()));
        registry
            .trusted_grant(&scope, "c", "write".into(), now)
            .unwrap();
        let mut p = fixture();
        let before = document_state_id(&p).unwrap();
        assert_eq!(
            registry.begin(&scope, &scope, &key, "read", &edit, now),
            Err(RegistryError::Unauthorized)
        );
        assert_eq!(document_state_id(&p).unwrap(), before);
        assert_eq!(
            registry.begin(&scope, &scope, &key, "write", &edit, now),
            Ok(Begin::New)
        );
        let receipt = apply(&mut p, &edit).unwrap();
        registry.finish(&scope, &key, receipt.clone(), now).unwrap();
        let revision = p.mcp_state().1;
        assert_eq!(
            registry
                .cached(&scope, &scope, &key, "write", &edit, now)
                .unwrap(),
            Some(receipt)
        );
        assert_eq!(p.mcp_state().1, revision);
        assert_eq!(
            registry.cached(&scope, &scope, &key, "write", &CanvasEdit::GroupAdd, now),
            Err(RegistryError::ParameterMismatch)
        );
        assert_eq!(
            registry.authorize(&scope, &scope, "c", "write", now + Duration::from_secs(301)),
            Err(RegistryError::LeaseExpired)
        );
        let mut other = scope.clone();
        other.session = "other".into();
        assert!(
            registry
                .authorize(&other, &scope, "c", "write", now)
                .is_err()
        );
    }
    #[test]
    fn canvas_edit_layer_history_and_stable_identity_are_real() {
        let mut p = fixture();
        let first = active(&p);
        let original = document_state_id(&p).unwrap();
        let add = apply(&mut p, &CanvasEdit::LayerAdd).unwrap();
        let created = add["createdLayerIds"][0].as_str().unwrap().to_owned();
        assert_ne!(first, created);
        assert_eq!(add["documentChanged"], true);
        let opacity = CanvasEdit::LayerSetOpacity {
            object_id: first.clone(),
            opacity: 0.3,
        };
        apply(&mut p, &opacity).unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&first).unwrap()).unwrap()["opacity"],
            json!(0.3_f32)
        );
        let unchanged = apply(&mut p, &opacity).unwrap();
        assert_eq!(unchanged["changed"], false);
        let revision = p.mcp_state().1;
        assert_eq!(unchanged["revision"], revision);
        apply(&mut p, &CanvasEdit::Undo).unwrap();
        apply(&mut p, &CanvasEdit::Undo).unwrap();
        assert_eq!(document_state_id(&p).unwrap(), original);
        assert!(p.can_redo());
        apply(&mut p, &CanvasEdit::Redo).unwrap();
        assert!(
            p.document
                .as_ref()
                .unwrap()
                .find(layer_id(&created).unwrap())
                .is_some()
        );
    }
    #[test]
    fn canvas_edit_paints_pixels_and_restores_them_through_native_undo() {
        let mut p = fixture();
        let object = active(&p);
        let before = document_state_id(&p).unwrap();
        apply(
            &mut p,
            &CanvasEdit::BrushSet {
                diameter: 3.0,
                opacity: 1.0,
                hardness: 1.0,
                color_rgb: [255, 0, 0],
                eraser: false,
            },
        )
        .unwrap();
        let result = apply(
            &mut p,
            &CanvasEdit::BrushStroke {
                object_id: object.clone(),
                target: CanvasPaintTarget::LayerPixels,
                points: vec![
                    CanvasPoint { x: 4.0, y: 3.0 },
                    CanvasPoint { x: 7.0, y: 3.0 },
                ],
            },
        )
        .unwrap();
        assert_eq!(result["documentChanged"], true);
        let frame = p.store.get(p.layer.unwrap()).unwrap();
        assert!(frame.pixel(4, 3).unwrap()[0] > 0);
        apply(&mut p, &CanvasEdit::Undo).unwrap();
        assert_eq!(document_state_id(&p).unwrap(), before);
        let result = apply(
            &mut p,
            &CanvasEdit::SelectionRect {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 2.0,
                additive: false,
            },
        )
        .unwrap();
        assert_eq!(result["documentChanged"], false);
        assert_eq!(result["changed"], true);
        assert_eq!(p.agent_selection_bounds(), Some((1, 2, 3, 2)));
        let fill = apply(
            &mut p,
            &CanvasEdit::SelectionFill {
                object_id: object,
                target: CanvasPaintTarget::LayerPixels,
            },
        )
        .unwrap();
        assert_eq!(fill["documentChanged"], true);
    }
    #[test]
    fn canvas_edit_masks_adjustments_and_curve_validation_use_actual_targets() {
        let mut p = fixture();
        let object = active(&p);
        apply(
            &mut p,
            &CanvasEdit::MaskAdd {
                object_id: object.clone(),
            },
        )
        .unwrap();
        assert!(p.agent_paint_mask());
        assert_eq!(
            preflight(
                &p,
                &CanvasEdit::SelectionFill {
                    object_id: object.clone(),
                    target: CanvasPaintTarget::LayerPixels
                }
            ),
            Err("selectionChanged")
        );
        apply(
            &mut p,
            &CanvasEdit::MaskSetEnabled {
                object_id: object.clone(),
                enabled: false,
            },
        )
        .unwrap();
        assert_eq!(
            layer_value(&p, layer_id(&object).unwrap()).unwrap()["maskEnabled"],
            false
        );
        apply(
            &mut p,
            &CanvasEdit::AdjustmentAdd {
                adjustment: CanvasAdjustment::Curves,
            },
        )
        .unwrap();
        let curves = active(&p);
        apply(
            &mut p,
            &CanvasEdit::CurveAdd {
                object_id: curves.clone(),
                channel: CurveChannel::Red,
                x: 0.5,
                y: 0.8,
            },
        )
        .unwrap();
        assert_eq!(p.agent_curve_channels()[0].len(), 3);
        assert_eq!(
            preflight(
                &p,
                &CanvasEdit::CurveRemove {
                    object_id: curves.clone(),
                    channel: CurveChannel::Red,
                    index: 0
                }
            ),
            Err("invalidInput")
        );
        apply(
            &mut p,
            &CanvasEdit::CurveRemove {
                object_id: curves,
                channel: CurveChannel::Red,
                index: 1,
            },
        )
        .unwrap();
        assert_eq!(p.agent_curve_channels()[0].len(), 2);
    }
    #[test]
    fn canvas_edit_invalid_target_and_unavailable_history_do_not_mutate() {
        let mut p = fixture();
        let before = document_state_id(&p).unwrap();
        assert_eq!(apply(&mut p, &CanvasEdit::Undo), Err("unsupportedTarget"));
        assert_eq!(
            apply(
                &mut p,
                &CanvasEdit::LayerDelete {
                    object_id: "layer:999".into()
                }
            ),
            Err("objectGone")
        );
        assert_eq!(document_state_id(&p).unwrap(), before);
        assert!(!p.can_undo());
        let first = active(&p);
        apply(&mut p, &CanvasEdit::GroupAdd).unwrap();
        let group = active(&p);
        apply(
            &mut p,
            &CanvasEdit::LayerMoveInto {
                object_id: first.clone(),
                parent_id: Some(group.clone()),
            },
        )
        .unwrap();
        apply(
            &mut p,
            &CanvasEdit::GroupSetExpanded {
                object_id: group.clone(),
                expanded: false,
            },
        )
        .unwrap();
        let before = document_state_id(&p).unwrap();
        assert_eq!(
            apply(&mut p, &CanvasEdit::LayerDelete { object_id: first }),
            Err("unsupportedTarget")
        );
        assert_eq!(document_state_id(&p).unwrap(), before);
        assert_eq!(
            apply(
                &mut p,
                &CanvasEdit::LayerMoveInto {
                    object_id: group.clone(),
                    parent_id: Some(group)
                }
            ),
            Err("invalidInput")
        );
    }
}
