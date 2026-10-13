// SPDX-License-Identifier: AGPL-3.0-or-later
//! Curated prompt content and strict, side-effect-free draft compatibility.
use super::{GenerationTemplate, TemplateReference};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub version: u32,
    pub category: String,
    pub purpose: String,
    pub source: String,
    pub license: String,
    pub capability: String,
    #[serde(default)]
    pub reference_versions: Vec<String>,
    /// Only a verified global-library output may supply an effect preview.
    #[serde(default)]
    pub preview_asset_id: Option<String>,
    #[serde(default)]
    pub preview_version: Option<String>,
}

pub fn digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "参考素材不可用，原草稿已保留")?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file
            .read(&mut buffer)
            .map_err(|_| "无法核验参考素材，原草稿已保留")?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn personal_metadata(references: &[TemplateReference]) -> Result<Metadata, String> {
    Ok(Metadata {
        version: 1,
        category: "personal".into(),
        purpose: "个人模板".into(),
        source: "个人保存".into(),
        license: "仅在个人模板中使用；未授予公开分发许可".into(),
        capability: if references.is_empty() {
            "generate"
        } else {
            "edit"
        }
        .into(),
        reference_versions: references
            .iter()
            .map(|r| digest(&r.path))
            .collect::<Result<_, _>>()?,
        preview_asset_id: None,
        preview_version: None,
    })
}

/// Original SeeCut prompt recipes. There are deliberately no unlicensed images
/// or invented model-output previews bundled with these prompt-only templates.
pub fn curated() -> Vec<GenerationTemplate> {
    [
        ("product-studio", "柔光棚拍", "product", "单品陈列 · 电商主图", "为一件可替换的商品创作电商主图：主体居中且完整，保留真实形状、材质与包装结构。暖白无缝背景，左前方大面积柔光，底部自然接触阴影，画面干净，商品占画面约三分之二。不要添加品牌、文字、水印或无关道具。如有商品参考图，以参考商品为准。"),
        ("product-material", "材质特写", "product", "细节质感 · 商品卖点", "创作突出商品材质的近景商业摄影。由用户指定商品与关键细节，围绕该细节构图，使用克制的侧逆光表现细纹、边缘和真实反射。背景取商品的低饱和同色系，保持主体轮廓清楚与合理比例。画面只强调一个卖点，不擅自增加文字或不存在的功能。"),
        ("poster-launch", "新品发布", "poster", "新品上新 · 活动视觉", "设计一张新品发布广告海报。使用用户提供的商品与准确标题；上部留出清晰标题区，中部以一个商品为视觉中心，下部保留少量信息区。背景采用单一主色与柔和光影，字号层级明确，字距舒适。只排版用户明确提供的文字，不编造价格、优惠、认证或品牌标识。"),
        ("poster-seasonal", "季节限定", "poster", "季节活动 · 品牌海报", "设计一张季节主题商业海报。根据用户指定的季节、商品和活动文案选用两种协调色彩，以一个季节元素作为辅助，商品仍是主角。保留充足留白和可读文字区，背景不堆叠装饰。只使用用户提供的优惠信息与品牌文字，未提供时留空，不虚构促销承诺。"),
        ("cover-editorial", "杂志封面", "cover", "社媒分享 · 内容封面", "设计一张适合社交媒体的编辑风格内容封面。围绕用户提供的主题组织一张核心画面、一个清晰标题与一处简短副标题。高对比但克制的配色，宽松边距，主体不被文字遮挡。标题逐字采用用户文案，未提供文字时只生成背景构图并留出标题空间。"),
        ("cover-video", "视频焦点", "cover", "视频封面 · 主题聚焦", "设计一张视频内容封面。以用户指定主题为基础，仅保留一个视觉焦点和一个简短标题区域。主体靠一侧，另一侧留出标题空间，用明暗区分前后景，在缩略图尺寸仍能辨认主体。不要生成播放按钮、平台标识、虚假数据或未经提供的人物身份；用户未给文案时不添加文字。"),
    ].into_iter().map(|(id, name, category, purpose, prompt)| GenerationTemplate {
        id: format!("seecut:{id}:v1"), name: name.into(), prompt: prompt.into(), mode: 0,
        model_id: String::new(), parameters: json!({}), references: vec![], created_at: 0, updated_at: 0,
        metadata: Some(Metadata { version: 1, category: category.into(), purpose: purpose.into(),
            source: "SeeCut 原创提示词".into(), license: "AGPL-3.0-or-later".into(), capability: "generate".into(),
            reference_versions: vec![], preview_asset_id: None, preview_version: None }),
    }).collect()
}

/// Validate the complete input before touching any live draft. An inspiration
/// never selects a model, nor changes parameters the recipe does not specify.
pub fn prepare(
    template: &GenerationTemplate,
    model: &Value,
    mode: i32,
    current_parameters: &Value,
) -> Result<Value, String> {
    let metadata = template
        .metadata
        .as_ref()
        .ok_or("旧模板尚未固定素材版本，请先重新保存")?;
    if metadata.version == 0 || template.prompt.trim().is_empty() {
        return Err("模板已失效，原草稿已保留".into());
    }
    let kind = if mode == 0 { "image" } else { "video" };
    if mode != template.mode
        || model["kind"] != kind
        || model["available"] != true
        || model["id"].as_str().unwrap_or_default().is_empty()
    {
        return Err("请先选择兼容的可用模型，原草稿已保留".into());
    }
    let operation = if kind == "image" && !template.references.is_empty() {
        "edit"
    } else {
        "generate"
    };
    if metadata.capability != operation {
        return Err("模板能力声明无效，原草稿已保留".into());
    }
    if !model["operations"]
        .as_array()
        .is_some_and(|ops| ops.iter().any(|op| op == operation))
    {
        return Err("当前模型不支持此模板，原草稿已保留".into());
    }
    if metadata.reference_versions.len() != template.references.len() {
        return Err("模板参考素材版本不完整，请重新保存".into());
    }
    for (reference, expected) in template.references.iter().zip(&metadata.reference_versions) {
        if expected.is_empty() || digest(&reference.path)? != *expected {
            return Err("参考素材已变化，原草稿已保留".into());
        }
    }
    let mut parameters = current_parameters.clone();
    for (key, value) in template.parameters.as_object().ok_or("模板参数无效")? {
        if value.is_null() {
            continue;
        }
        let definition = &model["parameters"][key];
        let compatible = definition["values"]
            .as_array()
            .is_some_and(|values| values.contains(value))
            || (definition["type"] == "boolean" && value.is_boolean());
        if !compatible {
            return Err(format!("当前模型不支持模板参数 {key}，原草稿已保留"));
        }
        parameters[key] = value.clone();
    }
    Ok(parameters)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> Value {
        json!({"id":"chosen-model","kind":"image","available":true,"operations":["generate","edit"],"parameters":{"quality":{"values":["high"]}}})
    }
    #[test]
    fn curated_content_is_original_versioned_and_never_fakes_effect_previews() {
        let items = curated();
        assert_eq!(items.len(), 6);
        for category in ["product", "poster", "cover"] {
            assert_eq!(
                items
                    .iter()
                    .filter(|t| t.metadata.as_ref().unwrap().category == category)
                    .count(),
                2
            );
        }
        for item in items {
            let m = item.metadata.unwrap();
            assert_eq!(m.version, 1);
            assert!(m.preview_asset_id.is_none());
            assert!(item.model_id.is_empty());
            assert!(item.references.is_empty());
        }
    }
    #[test]
    fn preserves_chosen_model_and_unspecified_parameters_and_rejects_incompatible_inputs() {
        let mut t = curated().remove(0);
        let current = json!({"quality":"high"});
        let m = model();
        assert_eq!(prepare(&t, &m, 0, &current).unwrap(), current);
        assert_eq!(m["id"], "chosen-model");
        assert!(prepare(&t, &m, 1, &current).is_err());
        assert!(prepare(&t, &Value::Null, 0, &current).is_err());
        t.parameters = json!({"quality":"ultra"});
        assert!(prepare(&t, &m, 0, &current).is_err());
        assert_eq!(current, json!({"quality":"high"}));
    }
    #[test]
    fn replaced_or_missing_reference_fails_before_draft_application() {
        let path = std::env::temp_dir().join(format!("seecut-template-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"original").unwrap();
        let mut t = curated().remove(0);
        t.references = vec![TemplateReference {
            path: path.clone(),
            name: "source".into(),
            kind: "image".into(),
            client_id: String::new(),
            source_id: String::new(),
        }];
        t.metadata = Some(personal_metadata(&t.references).unwrap());
        assert!(prepare(&t, &model(), 0, &json!({})).is_ok());
        std::fs::write(&path, b"replacement").unwrap();
        assert!(prepare(&t, &model(), 0, &json!({})).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(prepare(&t, &model(), 0, &json!({})).is_err());
    }
}
