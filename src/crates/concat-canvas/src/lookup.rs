// SPDX-License-Identifier: AGPL-3.0-or-later
//! Immutable user 3D .cube resources. See the Color Lookup evidence's SOURCES.md.
//! Input is the canvas's straight encoded RGB (no implicit gamma conversion).
//! DOMAIN maps input coordinates, never output values. Tetrahedral interpolation
//! precedes output clipping, preserving over/under-range table values.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// 65³ supports common 17/33/64/65 grids at <=3.15 MiB of RGB float data.
pub const MAX_SIZE: u32 = 65;
/// Bounded text input, including comments. Does not depend on file metadata alone.
pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    size: u32,
    title: String,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    rgb: Vec<[f32; 3]>,
    sha256: String,
}

/// Validated, immutable data. Clones share table storage; replacement creates a
/// new resource so undo and sibling layers can never change beneath the editor.
#[derive(Clone, Debug)]
pub struct CubeLut(Arc<Data>);
impl PartialEq for CubeLut {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}
impl Serialize for CubeLut {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(s)
    }
}
impl<'de> Deserialize<'de> for CubeLut {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let data = Data::deserialize(d)?;
        data.validate().map_err(serde::de::Error::custom)?;
        if data.sha256 != data.digest() {
            return Err(serde::de::Error::custom("LUT digest mismatch"));
        }
        Ok(Self(Arc::new(data)))
    }
}
impl Data {
    fn validate(&self) -> Result<(), String> {
        if !(2..=MAX_SIZE).contains(&self.size) || self.rgb.len() != (self.size as usize).pow(3) {
            return Err("LUT 网格须为 2–65，且数据数量须完整".into());
        }
        if self.title.chars().count() > 128 || self.title.chars().any(char::is_control) {
            return Err("LUT 标题无效".into());
        }
        for i in 0..3 {
            let span = self.domain_max[i] - self.domain_min[i];
            // A positive normal f32 span keeps GPU division defined (no
            // flush-to-zero subnormal domains or overflowing subtraction).
            if !self.domain_min[i].is_finite()
                || !self.domain_max[i].is_finite()
                || !span.is_normal()
                || span <= 0.0
            {
                return Err("LUT 输入域须有限，且最小值小于最大值".into());
            }
        }
        if self
            .rgb
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1.0e6)
        {
            return Err("LUT 数值须有限且在 ±1000000 内".into());
        }
        Ok(())
    }
    fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"SeeCut cube RGB f32 v1\0");
        h.update(self.size.to_le_bytes());
        for v in self
            .domain_min
            .iter()
            .chain(&self.domain_max)
            .chain(self.rgb.iter().flatten())
        {
            h.update((if *v == 0.0 { 0.0f32 } else { *v }).to_le_bytes());
        }
        format!("{h:x}", h = h.finalize())
    }
}
impl CubeLut {
    /// Strict Adobe/IRIDAS 3D subset; unknown transform/range directives are
    /// rejected rather than silently changing the meaning of a user table.
    pub fn parse(text: &str, fallback_name: &str) -> Result<Self, String> {
        if text.len() > MAX_FILE_BYTES {
            return Err("LUT 文件不能超过 32 MiB".into());
        }
        let mut data = Data {
            size: 0,
            title: fallback_name
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect(),
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            rgb: Vec::new(),
            sha256: String::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for (line_no, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            // '#' within a quoted TITLE is content; outside it starts a comment.
            let mut quoted = false;
            let end = raw
                .char_indices()
                .find_map(|(i, c)| {
                    if c == '"' {
                        quoted = !quoted;
                    }
                    if c == '#' && !quoted { Some(i) } else { None }
                })
                .unwrap_or(raw.len());
            let line = raw[..end].trim();
            if line.is_empty() {
                continue;
            }
            if line.len() > 4096 {
                return Err(format!("LUT 第 {} 行过长", line_no + 1));
            }
            let tokens: Vec<_> = line.split_whitespace().collect();
            let key = tokens[0];
            let fail = |message: &str| format!("LUT 第 {} 行：{message}", line_no + 1);
            if key.starts_with("LUT_1D") {
                return Err(fail("暂不支持一维或组合 LUT"));
            }
            if [
                "TITLE",
                "LUT_3D_SIZE",
                "DOMAIN_MIN",
                "DOMAIN_MAX",
                "LUT_3D_INPUT_RANGE",
            ]
            .contains(&key)
            {
                if !data.rgb.is_empty() || !seen.insert(key.to_owned()) {
                    return Err(fail("重复或位于数据后的头字段"));
                }
                match key {
                    "TITLE" => {
                        let value = line[5..].trim();
                        if value.len() < 2
                            || !value.starts_with('"')
                            || !value.ends_with('"')
                            || value[1..value.len() - 1].contains('"')
                        {
                            return Err(fail("标题须用双引号包围"));
                        }
                        data.title = value[1..value.len() - 1].to_owned();
                    }
                    "LUT_3D_SIZE" => {
                        if tokens.len() != 2 {
                            return Err(fail("网格大小无效"));
                        }
                        data.size = tokens[1].parse().map_err(|_| fail("网格大小无效"))?;
                        if !(2..=MAX_SIZE).contains(&data.size) {
                            return Err(fail("支持的网格大小为 2–65"));
                        }
                        data.rgb.reserve((data.size as usize).pow(3));
                    }
                    "LUT_3D_INPUT_RANGE" => {
                        if seen.contains("DOMAIN_MIN")
                            || seen.contains("DOMAIN_MAX")
                            || tokens.len() != 3
                        {
                            return Err(fail("输入域声明冲突"));
                        }
                        data.domain_min = [tokens[1].parse().map_err(|_| fail("输入域无效"))?; 3];
                        data.domain_max = [tokens[2].parse().map_err(|_| fail("输入域无效"))?; 3];
                    }
                    _ => {
                        if seen.contains("LUT_3D_INPUT_RANGE") {
                            return Err(fail("输入域声明冲突"));
                        }
                        let values =
                            triple(&tokens[1..]).map_err(|_| fail("输入域需要三个数值"))?;
                        if key == "DOMAIN_MIN" {
                            data.domain_min = values;
                        } else {
                            data.domain_max = values;
                        }
                    }
                }
            } else {
                if data.size == 0 {
                    return Err(fail("需要 LUT_3D_SIZE 三维网格声明"));
                }
                if data.rgb.len() >= (data.size as usize).pow(3) {
                    return Err(fail("数据行超过网格数量"));
                }
                data.rgb
                    .push(triple(&tokens).map_err(|_| fail("未知字段或无效 RGB 数据"))?);
            }
        }
        data.validate()?;
        data.sha256 = data.digest();
        Ok(Self(Arc::new(data)))
    }
    /// Short display name, never a source path.
    pub fn title(&self) -> &str {
        &self.0.title
    }
    /// Stable digest of normalized domain and float table, independent of title.
    pub fn digest(&self) -> &str {
        &self.0.sha256
    }
    /// Grid edge length.
    pub fn size(&self) -> u32 {
        self.0.size
    }
    /// Input coordinate bounds.
    pub fn domain(&self) -> ([f32; 3], [f32; 3]) {
        (self.0.domain_min, self.0.domain_max)
    }
    /// Immutable red-fastest table for GPU upload.
    pub fn values(&self) -> &[[f32; 3]] {
        &self.0.rgb
    }
    /// Tetrahedral interpolation in input-domain coordinates. Clamp only input
    /// coordinates here; output clipping happens after interpolation/strength.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.0.size as usize;
        let mut lo = [0usize; 3];
        let mut f = [0.0; 3];
        for c in 0..3 {
            let p = ((rgb[c] - self.0.domain_min[c])
                / (self.0.domain_max[c] - self.0.domain_min[c]))
                .clamp(0.0, 1.0)
                * (n - 1) as f32;
            lo[c] = (p.floor() as usize).min(n - 2);
            f[c] = p - lo[c] as f32;
        }
        let mut axes = [0usize, 1, 2];
        for i in 0..2 {
            for j in i + 1..3 {
                if f[axes[j]] > f[axes[i]] {
                    axes.swap(i, j);
                }
            }
        }
        let mut corners = [lo; 4];
        corners[1][axes[0]] += 1;
        corners[2] = corners[1];
        corners[2][axes[1]] += 1;
        corners[3] = lo.map(|v| v + 1);
        let weights = [
            1.0 - f[axes[0]],
            f[axes[0]] - f[axes[1]],
            f[axes[1]] - f[axes[2]],
            f[axes[2]],
        ];
        let mut out = [0.0; 3];
        for (corner, w) in corners.iter().zip(weights) {
            let v = self.0.rgb[corner[2] * n * n + corner[1] * n + corner[0]];
            for c in 0..3 {
                out[c] += v[c] * w;
            }
        }
        out
    }
}
fn triple(tokens: &[&str]) -> Result<[f32; 3], ()> {
    if tokens.len() != 3 {
        return Err(());
    }
    let mut out = [0.0_f32; 3];
    for (i, s) in tokens.iter().enumerate() {
        out[i] = s.parse().map_err(|_| ())?;
        if !out[i].is_finite() {
            return Err(());
        }
    }
    Ok(out)
}
