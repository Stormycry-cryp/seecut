// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use concat_canvas::lookup::{CubeLut, MAX_FILE_BYTES};
use std::io::Read;

#[derive(Clone, Debug)]
pub(crate) struct LookupTarget {
    generation: u64,
    binding_epoch: u64,
    revision: u64,
    revision_clock: u64,
    sequence: u64,
    layer: concat_canvas::LayerId,
}

/// Only reads the selected local file; returns immutable parsed data without
/// saving its path, touching the document, or registering an asset.
pub(crate) fn read_cube(path: &Path) -> Result<CubeLut, String> {
    if !path
        .extension()
        .is_some_and(|v| v.to_string_lossy().eq_ignore_ascii_case("cube"))
    {
        return Err("请选择三维 .cube LUT 文件".into());
    }
    let file = std::fs::File::open(path).map_err(|_| "无法读取 LUT 文件")?;
    let meta = file.metadata().map_err(|_| "无法读取 LUT 文件信息")?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES as u64 {
        return Err("请选择不超过 32 MiB 的 LUT 文件".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取 LUT 文件")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "LUT 文件须为 UTF-8 文本")?;
    CubeLut::parse(
        text,
        path.file_stem().and_then(|v| v.to_str()).unwrap_or("LUT"),
    )
}
impl CanvasPane {
    pub(crate) fn lookup_name(&self) -> String {
        match self.active.and_then(|id| self.document.as_ref()?.find(id)) {
            Some(LayerNode::Adjustment(a)) => match &a.adjustment {
                Adjustment::ColorLookup { lut: Some(lut), .. } => {
                    if lut.title().trim().is_empty() {
                        "LUT".into()
                    } else {
                        lut.title().into()
                    }
                }
                _ => "未选择 LUT".into(),
            },
            _ => "未选择 LUT".into(),
        }
    }
    fn lookup_edit_busy(&self) -> bool {
        self.pending_history.is_some()
            || self.parameter_gesture.is_some()
            || self.text_session.is_some()
            || self.shape_session.is_some()
            || self.transform_session.is_some()
            || self.stroke.is_some()
    }
    pub(crate) fn begin_lookup_import(&mut self) -> Result<LookupTarget, String> {
        if self.lookup_edit_busy() {
            return Err("请先完成当前编辑，再导入 LUT".into());
        }
        let layer = self.active.ok_or("请先选择颜色查找调整层")?;
        if !matches!(self.document.as_ref().and_then(|d|d.find(layer)),Some(LayerNode::Adjustment(a)) if matches!(a.adjustment,Adjustment::ColorLookup { .. }))
        {
            return Err("请先选择颜色查找调整层".into());
        }
        self.lookup_import_sequence = self
            .lookup_import_sequence
            .checked_add(1)
            .ok_or("请重新打开工程")?;
        Ok(LookupTarget {
            generation: self.document_generation,
            binding_epoch: self.mcp_binding_epoch,
            revision: self.revision,
            revision_clock: self.revision_clock,
            sequence: self.lookup_import_sequence,
            layer,
        })
    }
    pub(crate) fn finish_lookup_import(
        &mut self,
        target: LookupTarget,
        result: Result<CubeLut, String>,
    ) -> Result<(), String> {
        if self.lookup_edit_busy()
            || target.generation != self.document_generation
            || target.binding_epoch != self.mcp_binding_epoch
            || target.revision != self.revision
            || target.revision_clock != self.revision_clock
            || target.sequence != self.lookup_import_sequence
            || self.active != Some(target.layer)
        {
            return Err("当前图层已变化，请重新导入 LUT".into());
        }
        let lut = result?;
        if !matches!(self.document.as_ref().and_then(|d|d.find(target.layer)),Some(LayerNode::Adjustment(a)) if matches!(a.adjustment,Adjustment::ColorLookup { .. }))
        {
            return Err("请选择颜色查找调整层".into());
        }
        self.history_edit(|p| {
            if let Some(LayerNode::Adjustment(a)) =
                p.document.as_mut().and_then(|d| d.find_mut(target.layer))
                && let Adjustment::ColorLookup { lut: current, .. } = &mut a.adjustment
            {
                *current = Some(lut);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
