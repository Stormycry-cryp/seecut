// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded PSD v1 / 8-bit RGB reader. Independently implemented from Adobe's
//! public file-format specification; no third-party parser is embedded.
//! Unsupported layer semantics fail the entire import, never silently flatten.
use crate::document::BitmapMaskBounds;
use crate::{BlendMode, ImageDocument, ImageLayer, LayerGroup, LayerMask, LayerNode, PixelStore};
use concat_core::frame::Frame;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum encoded input bytes.
pub const FILE_LIMIT: usize = 64 * 1024 * 1024;
/// Maximum estimated decoded/render peak.
pub const PIXEL_LIMIT: usize = 512 * 1024 * 1024;
const LAYER_LIMIT: usize = 512;
const DEPTH_LIMIT: usize = 16;

/// A controlled parsing or unsupported-feature failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError(pub String);
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ImportError {}
type Result<T> = std::result::Result<T, ImportError>;
fn error(s: impl Into<String>) -> ImportError {
    ImportError(s.into())
}
fn require(ok: bool, s: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(error(s)) }
}
fn cancelled(flag: &AtomicBool) -> Result<()> {
    require(!flag.load(Ordering::Relaxed), "导入已取消")
}

/// Fully validated temporary import ready for a guarded atomic commit.
pub struct Imported {
    /// Editable tree.
    pub document: ImageDocument,
    /// Immutable raster planes.
    pub store: PixelStore,
    /// Conservative peak: input, decoded channels, stored RGBA, render and group buffers.
    pub estimated_peak: usize,
    /// User-visible limitations.
    pub warnings: Vec<String>,
}
#[derive(Clone, Copy)]
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn left(&self) -> usize {
        self.data.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| error("段长度溢出"))?;
        let out = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| error("PSD 段已截断"))?;
        self.pos = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn block(&mut self) -> Result<Self> {
        let n = self.u32()? as usize;
        Ok(Self::new(self.take(n)?))
    }
    fn zeros(&mut self) -> Result<()> {
        require(
            self.take(self.left())?.iter().all(|x| *x == 0),
            "段尾包含未支持数据",
        )
    }
}
#[derive(Clone, Copy, Debug)]
struct Rect {
    top: i32,
    left: i32,
    width: u32,
    height: u32,
}
impl Rect {
    fn read(c: &mut Cursor<'_>, limit: u32) -> Result<Self> {
        let top = c.i32()?;
        let left = c.i32()?;
        let bottom = c.i32()?;
        let right = c.i32()?;
        let w = i64::from(right) - i64::from(left);
        let h = i64::from(bottom) - i64::from(top);
        require(
            w >= 0
                && h >= 0
                && w <= i64::from(limit)
                && h <= i64::from(limit)
                && [left, top, right, bottom]
                    .iter()
                    .all(|x| x.unsigned_abs() <= 1_000_000),
            "图层/蒙版矩形超出尺寸限制",
        )?;
        Ok(Self {
            top,
            left,
            width: w as u32,
            height: h as u32,
        })
    }
    fn area(self) -> Result<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .ok_or_else(|| error("像素尺寸溢出"))
    }
}
struct Mask {
    rect: Rect,
    outside: u8,
    flags: u8,
}
struct Record<'a> {
    name: String,
    rect: Rect,
    channels: Vec<(i16, usize, &'a [u8])>,
    blend: BlendMode,
    opacity: u8,
    hidden: bool,
    section: u32,
    mask: Option<Mask>,
}
fn blend(key: &[u8]) -> Result<BlendMode> {
    use BlendMode::*;
    Ok(match key {
        b"norm" => Normal,
        b"mul " => Multiply,
        b"scrn" => Screen,
        b"over" => Overlay,
        b"dark" => Darken,
        b"lite" => Lighten,
        b"diff" => Difference,
        b"div " => ColorDodge,
        b"idiv" => ColorBurn,
        b"hue " => Hue,
        b"sat " => Saturation,
        b"colr" => Color,
        b"lum " => Luminosity,
        _ => {
            return Err(error(format!(
                "不支持混合模式 {}（含 Pass Through）",
                String::from_utf8_lossy(key)
            )));
        }
    })
}
fn record<'a>(c: &mut Cursor<'a>, limit: u32) -> Result<Record<'a>> {
    let rect = Rect::read(c, limit)?;
    let count = c.u16()? as usize;
    require(count <= 5, "不支持额外通道")?;
    let mut channels = Vec::new();
    for _ in 0..count {
        let id = c.u16()? as i16;
        let len = c.u32()? as usize;
        require(
            (-2..=2).contains(&id) && !channels.iter().any(|(i, _, _)| *i == id),
            "重复或不支持的通道",
        )?;
        require(len >= 2, "缺少通道压缩标识")?;
        channels.push((id, len, &[][..]));
    }
    require(c.take(4)? == b"8BIM", "图层混合签名无效")?;
    let key = c.take(4)?;
    let opacity = c.u8()?;
    let clipping = c.u8()?;
    let flags = c.u8()?;
    require(c.u8()? == 0, "图层保留字段无效")?;
    let mut extra = c.block()?;
    let mut mc = extra.block()?;
    let mask_result = (|| -> Result<Option<Mask>> {
        Ok(if mc.left() == 0 {
            None
        } else {
            require(
                mc.left() == 20,
                "仅支持基本位图蒙版（不支持密度、羽化和矢量蒙版）",
            )?;
            let mr = Rect::read(&mut mc, limit)?;
            let outside = mc.u8()?;
            let mf = mc.u8()?;
            require(
                matches!(outside, 0 | 255) && mf & !3 == 0,
                "不支持蒙版默认值或蒙版参数",
            )?;
            mc.zeros()?;
            Some(Mask {
                rect: mr,
                outside,
                flags: mf,
            })
        })
    })();
    let ranges = extra.block()?;
    let ranges_default =
        ranges.data.len() % 8 == 0 && ranges.data.chunks_exact(4).all(|x| x == [0, 0, 255, 255]);
    let n = extra.u8()? as usize;
    let legacy_name = extra.take(n)?;
    let legacy_valid = std::str::from_utf8(legacy_name).is_ok();
    let mut unicode_name = false;
    let mut name = String::from_utf8_lossy(legacy_name).into_owned();
    extra.take((4 - (n + 1) % 4) % 4)?;
    let mut section = 0;
    let mut group_blend = None;
    let mut unsupported = Vec::new();
    if clipping != 0 {
        unsupported.push("剪贴图层".into());
    }
    if !ranges_default {
        unsupported.push("Blend If".into());
    }
    let mut seen = std::collections::HashSet::new();
    while extra.left() >= 12 {
        require(seen.len() < 256, "单层附加块超过 256 项，拒绝过量元数据")?;
        require(extra.take(4)? == b"8BIM", "不支持图层附加块签名")?;
        let tag = extra.take(4)?;
        require(seen.insert(tag), "重复图层附加块")?;
        let mut data = extra.block()?;
        let len = data.left();
        match tag {
            b"luni" => {
                unicode_name = true;
                let n = data.u32()? as usize;
                require(n <= 4096, "图层名称过长")?;
                let units: Vec<u16> = data
                    .take(n.checked_mul(2).ok_or_else(|| error("名称长度溢出"))?)?
                    .chunks_exact(2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .collect();
                name = String::from_utf16(&units)
                    .map_err(|_| error("图层 Unicode 名称无效"))?
                    .trim_end_matches('\0')
                    .to_owned();
                data.zeros()?;
            }
            b"lsct" | b"lsdk" => {
                require(matches!(len, 4 | 12 | 16) && section == 0, "组分隔块无效")?;
                section = data.u32()?;
                require(section <= 3, "不支持组类型")?;
                if len >= 12 {
                    require(data.take(4)? == b"8BIM", "组混合签名无效")?;
                    group_blend = Some(blend(data.take(4)?));
                }
                if len == 16 {
                    require(data.u32()? == 0, "不支持场景组")?;
                }
            }
            // Identity, editor protection, labels and non-rendering metadata only.
            b"lyid" | b"lclr" | b"lnsr" | b"lyvr" | b"shmd" | b"fxrp" | b"lspf" => {}
            // Only identity defaults may be ignored. Non-default fill/knockout/
            // blend-internal settings change Photoshop compositing semantics.
            b"iOpa" => {
                if data.u8()? != 255 {
                    unsupported.push("填充不透明度".into());
                }
            }
            b"knko" => {
                if data.u8()? != 0 {
                    unsupported.push("挖空".into());
                }
            }
            b"infx" => {
                if data.u8()? != 0 {
                    unsupported.push("内部效果混合".into());
                }
            }
            b"clbl" => {
                if data.u8()? != 1 {
                    unsupported.push("剪贴混合设置".into());
                }
            }
            b"tsly" => {
                if data.u8()? != 1 {
                    unsupported.push("透明形状设置".into());
                }
            }
            _ => unsupported.push(format!("未支持特性 {}", String::from_utf8_lossy(tag))),
        }
        extra.take(len % 2)?;
    }
    extra.zeros()?;
    require(
        unicode_name || legacy_valid,
        "非 UTF-8 旧版图层名缺少 Unicode 名称块，无法无损导入",
    )?;
    let mask = mask_result.map_err(|e| error(format!("图层「{name}」：{e}")))?;
    if flags & !0x1b != 0 || (section == 0 && flags & 0x18 == 0x18) {
        unsupported.push("非栅格图层标志".into());
    }
    // Group boundary records carry a non-rendered placeholder blend key.
    let mode = if section == 3 {
        Ok(BlendMode::Normal)
    } else {
        group_blend.unwrap_or_else(|| blend(key))
    };
    if let Err(e) = &mode {
        unsupported.push(e.0.clone());
    }
    if !unsupported.is_empty() {
        return Err(error(format!("图层「{name}」：{}", unsupported.join("；"))));
    }
    Ok(Record {
        name,
        rect,
        channels,
        blend: mode?,
        opacity,
        hidden: flags & 2 != 0,
        section,
        mask,
    })
}

/// Decode one channel with exact row lengths; no allocation until all sizes pass preflight.
fn channel(bytes: &[u8], width: u32, height: u32, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut c = Cursor::new(bytes);
    let compression = c.u16()?;
    let area = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| error("通道尺寸溢出"))?;
    require(area <= PIXEL_LIMIT, "通道超过像素预算")?;
    if compression == 0 {
        let data = c.take(area)?.to_vec();
        require(c.left() == 0, "Raw 通道长度无效")?;
        return Ok(data);
    }
    require(compression == 1, "仅支持 Raw / RLE；ZIP 压缩未支持")?;
    let mut lengths = Vec::with_capacity(height as usize);
    for _ in 0..height {
        lengths.push(c.u16()? as usize);
    }
    let mut out = Vec::with_capacity(area);
    for length in lengths {
        cancelled(cancel)?;
        let mut row = Cursor::new(c.take(length)?);
        let end = out.len() + width as usize;
        while row.left() > 0 {
            let command = row.u8()? as i8;
            match command {
                0..=127 => {
                    let n = command as usize + 1;
                    require(out.len() + n <= end, "RLE 行溢出")?;
                    out.extend_from_slice(row.take(n)?);
                }
                -127..=-1 => {
                    let n = (1 - i16::from(command)) as usize;
                    require(out.len() + n <= end, "RLE 行溢出")?;
                    let value = row.u8()?;
                    out.resize(out.len() + n, value);
                }
                -128 => {}
            }
        }
        require(out.len() == end, "RLE 行长度不足")?;
    }
    require(c.left() == 0, "RLE 通道有多余数据")?;
    Ok(out)
}
fn add_budget(sum: &mut usize, amount: usize) -> Result<()> {
    *sum = sum
        .checked_add(amount)
        .ok_or_else(|| error("像素预算溢出"))?;
    require(*sum <= PIXEL_LIMIT, "PSD 解压与合成峰值超过 512 MiB 预算")
}

/// Pure temporary import; callers commit only after their document/session fence passes.
pub fn parse(bytes: &[u8], texture_limit: u32, cancel: &AtomicBool) -> Result<Imported> {
    cancelled(cancel)?;
    require(bytes.len() <= FILE_LIMIT, "PSD 文件超过 64 MiB")?;
    let limit = texture_limit.min(30_000);
    let mut c = Cursor::new(bytes);
    require(
        c.take(4)? == b"8BPS" && c.u16()? == 1,
        "仅支持 PSD v1，不支持 PSB",
    )?;
    require(c.take(6)? == [0; 6], "PSD 保留字段无效")?;
    let channels = c.u16()?;
    let height = c.u32()?;
    let width = c.u32()?;
    require(
        (3..=4).contains(&channels) && c.u16()? == 8 && c.u16()? == 3,
        "仅支持 8 位 RGB PSD（3/4 通道）",
    )?;
    require(
        width > 0 && height > 0 && width <= limit && height <= limit,
        "画布尺寸超过 GPU / PSD 限制",
    )?;
    require(c.block()?.left() == 0, "RGB 颜色表不应包含数据")?;
    let mut resources = c.block()?;
    let mut profile = false;
    while resources.left() > 0 {
        require(resources.take(4)? == b"8BIM", "图像资源签名无效")?;
        let id = resources.u16()?;
        let n = resources.u8()? as usize;
        resources.take(n)?;
        resources.take((n + 1) % 2)?;
        let data = resources.block()?;
        resources.take(data.left() % 2)?;
        if id == 1039 {
            profile = true;
        }
    }
    let mut lm = c.block()?;
    let mut layers = lm.block()?;
    let count = (layers.u16()? as i16).unsigned_abs() as usize;
    require(count > 0 && count <= LAYER_LIMIT, "需要 1–512 条图层记录")?;
    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        cancelled(cancel)?;
        records.push(
            record(&mut layers, limit).map_err(|e| error(format!("记录 {}：{}", index + 1, e)))?,
        );
    }
    // Full preflight BEFORE any channel or RGBA allocation, including nested
    // full-canvas compositing (CPU float buffers + GPU textures + upload copies).
    let canvas = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| error("画布尺寸溢出"))?;
    let mut peak = bytes.len();
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut bitmap_count = 0usize;
    for r in &mut records {
        match r.section {
            3 => {
                depth += 1;
                max_depth = max_depth.max(depth);
                require(depth <= DEPTH_LIMIT, "组嵌套超过 16 层")?;
            }
            1 | 2 => {
                require(depth > 0, "组边界未配对")?;
                depth -= 1;
            }
            _ => {}
        }
        if r.section == 0 {
            bitmap_count += 1;
            require(
                r.channels
                    .iter()
                    .filter(|(id, _, _)| (0..=2).contains(id))
                    .count()
                    == 3,
                "栅格层缺少 RGB 通道",
            )?;
            add_budget(
                &mut peak,
                r.rect
                    .area()?
                    .max(1)
                    .checked_mul(8)
                    .ok_or_else(|| error("RGBA 预算溢出"))?,
            )?;
        }
        if let Some(mask) = &r.mask {
            bitmap_count += 1;
            require(
                r.channels.iter().any(|(id, _, _)| *id == -2),
                "蒙版缺少覆盖通道",
            )?;
            add_budget(
                &mut peak,
                mask.rect
                    .area()?
                    .max(1)
                    .checked_mul(8)
                    .ok_or_else(|| error("蒙版预算溢出"))?,
            )?;
        }
        for (id, len, data) in &mut r.channels {
            let rect = if *id == -2 {
                r.mask
                    .as_ref()
                    .ok_or_else(|| error(format!("图层「{}」缺少蒙版矩形", r.name)))?
                    .rect
            } else {
                r.rect
            };
            add_budget(&mut peak, rect.area()?)?;
            *data = layers.take(*len)?;
            let comp = Cursor::new(data).u16()?;
            require(
                comp <= 1,
                &format!("图层「{}」：仅支持 Raw / RLE，不支持 ZIP", r.name),
            )?;
        }
    }
    require(depth == 0, "组边界未闭合")?;
    require(bitmap_count <= 1000, "位图数量超过项目存储限制")?;
    add_budget(
        &mut peak,
        canvas
            .checked_mul(64 * (max_depth + 2))
            .ok_or_else(|| error("组合成预算溢出"))?,
    )?;
    layers.zeros()?;
    if lm.left() > 0 {
        require(lm.block()?.left() == 0, "不支持全局图层蒙版")?;
        lm.zeros()?;
    }
    // Merged preview is deliberately not used as a substitute for layer data.
    require(c.left() >= 2, "缺少合成图像段")?;
    // Validate (but never import) the merged image too: truncated trailers and
    // unsupported compression must not turn into an apparently successful file.
    channel(
        c.take(c.left())?,
        width,
        height * u32::from(channels),
        cancel,
    )?;
    let mut document = ImageDocument::new(width, height);
    let mut store = PixelStore::new();
    let mut stack = vec![LayerGroup::new(document.root.id, "Root")];
    for r in records {
        cancelled(cancel)?;
        let mut planes = Vec::new();
        for (id, _, data) in &r.channels {
            let rect = if *id == -2 {
                r.mask.as_ref().unwrap().rect
            } else {
                r.rect
            };
            planes.push((
                *id,
                channel(data, rect.width, rect.height, cancel)
                    .map_err(|e| error(format!("图层「{}」：{e}", r.name)))?,
            ));
        }
        if r.section == 3 {
            require(
                r.mask.is_none() && r.rect.area()? == 0,
                "组边界包含非空像素/蒙版",
            )?;
            stack.push(LayerGroup::new(document.mint_id(), ""));
            continue;
        }
        let mut mask = None;
        if let Some(m) = r.mask {
            let data = &planes.iter().find(|(id, _)| *id == -2).unwrap().1;
            let mut rgba = Vec::with_capacity(data.len().max(1) * 4);
            for v in data {
                rgba.extend_from_slice(&[*v, *v, *v, 255]);
            }
            if data.is_empty() {
                rgba.extend_from_slice(&[m.outside, m.outside, m.outside, 255]);
            }
            let (mw, mh) = if data.is_empty() {
                (1, 1)
            } else {
                (m.rect.width, m.rect.height)
            };
            let pixels =
                store.put(Frame::from_rgba(mw, mh, rgba).ok_or_else(|| error("蒙版尺寸无效"))?);
            let (left, top) = if m.flags & 1 != 0 {
                (
                    m.rect.left.checked_add(r.rect.left),
                    m.rect.top.checked_add(r.rect.top),
                )
            } else {
                (Some(m.rect.left), Some(m.rect.top))
            };
            let mut layer_mask = LayerMask::new(pixels);
            layer_mask.enabled = m.flags & 2 == 0;
            layer_mask.bitmap_bounds = Some(BitmapMaskBounds {
                left: left.ok_or_else(|| error("蒙版位置溢出"))?,
                top: top.ok_or_else(|| error("蒙版位置溢出"))?,
                outside: m.outside,
            });
            mask = Some(layer_mask);
        }
        let node = if r.section == 1 || r.section == 2 {
            require(
                r.rect.area()? == 0,
                &format!("组「{}」包含非空栅格通道", r.name),
            )?;
            let mut group = stack.pop().ok_or_else(|| error("组边界无效"))?;
            group.name = r.name;
            group.opacity = r.opacity as f32 / 255.0;
            group.hidden = r.hidden;
            group.blend = r.blend;
            group.mask = mask;
            LayerNode::Group(group)
        } else {
            let area = r.rect.area()?;
            let mut rgba = vec![0; area.max(1) * 4];
            for i in 0..area {
                rgba[i * 4 + 3] = 255;
            }
            for (id, data) in planes {
                if id == -2 {
                    continue;
                }
                let component = if id == -1 { 3 } else { id as usize };
                for (i, v) in data.into_iter().enumerate() {
                    rgba[i * 4 + component] = v;
                }
            }
            let (w, h) = if area == 0 {
                (1, 1)
            } else {
                (r.rect.width, r.rect.height)
            };
            let mut layer = ImageLayer::new(
                document.mint_id(),
                r.name,
                store.put(Frame::from_rgba(w, h, rgba).ok_or_else(|| error("图层尺寸无效"))?),
            );
            layer.transform.x = r.rect.left as f32 + w as f32 / 2.0 - width as f32 / 2.0;
            layer.transform.y = r.rect.top as f32 + h as f32 / 2.0 - height as f32 / 2.0;
            layer.opacity = r.opacity as f32 / 255.0;
            layer.hidden = r.hidden;
            layer.blend = r.blend;
            if let Some(m) = &mut mask {
                m.anchor = Some(layer.transform);
            }
            layer.mask = mask;
            LayerNode::Layer(layer)
        };
        stack
            .last_mut()
            .ok_or_else(|| error("组栈无效"))?
            .children
            .push(node);
    }
    require(stack.len() == 1, "组树未闭合")?;
    document.root = stack.pop().unwrap();
    document
        .validate()
        .map_err(|e| error(format!("导入文档无效：{e}")))?;
    cancelled(cancel)?;
    let mut warnings =
        vec!["已按存储的 RGB 像素导入；未验证 Photoshop 色彩管理与合成保真。".into()];
    if profile {
        warnings.push("文件包含 ICC 配置，当前未应用 ICC 转换。".into());
    }
    Ok(Imported {
        document,
        store,
        estimated_peak: peak,
        warnings,
    })
}

#[cfg(test)]
mod tests;
