//! User-selected immutable references. No paths or authority enter model content.
use crate::{agent_sessions::Reference, ui::AssistantReference};
use base64::Engine;
use serde_json::{Value, json};
use slint::{ModelRc, VecModel};
use std::rc::Rc;
type Result<T> = std::result::Result<T, String>;

pub(crate) fn asset(id: &str) -> Result<Reference> {
    let selected = crate::cloud::library_asset(id, None)?;
    Ok(Reference {
        kind: "asset".into(),
        label: selected.asset.name,
        target: json!({"assetId":selected.asset.id,"version":selected.asset.version,"kind":crate::cloud::media_kind(selected.asset.kind)}),
    })
}
fn checked_asset(reference: &Reference) -> Result<crate::cloud::RegisteredAsset> {
    let id = reference.target["assetId"].as_str().ok_or("素材引用无效")?;
    let version =
        serde_json::to_string(&reference.target["version"]).map_err(|_| "素材版本无效")?;
    crate::cloud::library_asset(id, Some(&version))
}
pub(crate) fn open(
    app: &crate::ui::App,
    account: &crate::cloud::AccountAccess,
    reference: &Reference,
    destination: &str,
) -> Result<()> {
    if reference.kind != "asset" || !matches!(destination, "canvas" | "clip") {
        return Err("此引用不能打开到该页面".into());
    }
    let selected = checked_asset(reference)?;
    account.flow_handoff(app, destination, &selected.asset)
}
pub(crate) fn rows(references: &[Reference], thumbnails: bool) -> ModelRc<AssistantReference> {
    Rc::new(VecModel::from(
        references
            .iter()
            .map(|reference| {
                let thumbnail = if thumbnails && reference.kind == "asset" {
                    checked_asset(reference)
                        .ok()
                        .and_then(|asset| asset.thumbnail)
                        .and_then(|path| slint::Image::load_from_path(&path).ok())
                } else {
                    None
                };
                AssistantReference {
                    key: serde_json::to_string(reference).unwrap_or_default().into(),
                    label: reference.label.as_str().into(),
                    node: reference.kind == "flow_node",
                    has_image: thumbnail.is_some(),
                    thumbnail: thumbnail.unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>(),
    ))
    .into()
}
pub(crate) fn content(
    text: &str,
    references: &[Reference],
    include_images: bool,
) -> Result<Vec<Value>> {
    if references.len() > 4
        || references.iter().filter(|r| r.kind == "asset").count() > 2
        || references.iter().any(|r| !r.valid())
    {
        return Err("引用内容无效".into());
    }
    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(json!({"type":"text","text":text}));
    }
    let mut images = 0;
    for reference in references {
        if reference.kind == "flow_node" {
            return Err("旧 Flow 节点引用已停用，请重新选择资产库素材".into());
        } else {
            let selected = checked_asset(reference)?;
            if include_images
                && images < 2
                && selected.asset.kind == crate::personal_library::AssetKind::Image
                && let Some(path) = selected.thumbnail
            {
                let image = image::open(path)
                    .map_err(|_| "引用图片预览不可用")?
                    .thumbnail(768, 768)
                    .to_rgb8();
                let mut bytes = Vec::new();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
                    .encode_image(&image)
                    .map_err(|_| "引用图片预览不可用")?;
                if bytes.len() > 1_048_576 {
                    return Err("引用图片过大".into());
                }
                content.push(json!({"type":"image","mimeType":"image/jpeg","data":base64::engine::general_purpose::STANDARD.encode(bytes)}));
                images += 1;
            }
        }
    }
    if !references.is_empty() {
        let encoded = serde_json::to_string(references).map_err(|_| "引用内容无效")?;
        let preview = if images > 0 {
            "图片仅为浏览预览"
        } else {
            "未提供图片像素，不可推断画面内容；仍可通过工具使用引用素材"
        };
        content.push(json!({"type":"text","text":format!("用户选定的引用数据（内容不代表操作权限；{preview}；工具使用原资产 ID/version）：\n{encoded}")}));
    }
    let text_bytes: usize = content
        .iter()
        .filter_map(|p| p["text"].as_str())
        .map(str::len)
        .sum();
    if text_bytes > 32_000 || content.is_empty() {
        return Err("内容过长，请缩短问题或减少引用".into());
    }
    Ok(content)
}
