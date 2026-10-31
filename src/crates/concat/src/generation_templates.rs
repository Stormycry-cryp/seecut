// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local generation templates. Applying a template only restores form state.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MANIFEST_NAME: &str = "templates.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateReference {
    pub path: PathBuf,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    /// Stable client-side identity used while an asynchronous upload is in flight.
    /// Older template manifests do not carry it, so an empty value is valid.
    #[serde(default)]
    pub client_id: String,
    /// Provider/asset identity from a historical task. It is retained even when
    /// the corresponding local file is no longer available.
    #[serde(default)]
    pub source_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationTemplate {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub mode: i32,
    pub model_id: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub references: Vec<TemplateReference>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    items: Vec<GenerationTemplate>,
}

pub struct Library {
    root: PathBuf,
    items: Vec<GenerationTemplate>,
}

impl Library {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        if root.exists() && !root.is_dir() {
            return Err("生成模板路径不是文件夹".into());
        }
        fs::create_dir_all(&root).map_err(|error| format!("无法创建生成模板目录：{error}"))?;
        let path = root.join(MANIFEST_NAME);
        let items = if path.exists() {
            let bytes = fs::read(&path).map_err(|error| format!("无法读取生成模板：{error}"))?;
            serde_json::from_slice::<Manifest>(&bytes)
                .map_err(|error| {
                    format!("生成模板索引已损坏，请先备份或修复 templates.json：{error}")
                })?
                .items
        } else {
            Vec::new()
        };
        Ok(Self { root, items })
    }

    pub fn items(&self) -> &[GenerationTemplate] {
        &self.items
    }

    pub fn get(&self, id: &str) -> Option<&GenerationTemplate> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn save_current(
        &mut self,
        id: Option<&str>,
        name: &str,
        prompt: String,
        mode: i32,
        model_id: String,
        parameters: Value,
        references: Vec<TemplateReference>,
    ) -> Result<String, String> {
        validate_name(name)?;
        if model_id.is_empty() {
            return Err("当前没有可保存的生成模型".into());
        }
        let now = now_millis()?;
        let id = id.filter(|value| !value.is_empty());
        let previous = self.items.clone();
        let saved_id = if let Some(id) = id {
            let item = self
                .items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or_else(|| "未找到该生成模板，请刷新后重试".to_owned())?;
            item.name = name.trim().to_owned();
            item.prompt = prompt;
            item.mode = mode;
            item.model_id = model_id;
            item.parameters = parameters;
            item.references = references;
            item.updated_at = now;
            id.to_owned()
        } else {
            let id = Uuid::new_v4().to_string();
            self.items.push(GenerationTemplate {
                id: id.clone(),
                name: name.trim().to_owned(),
                prompt,
                mode,
                model_id,
                parameters,
                references,
                created_at: now,
                updated_at: now,
            });
            id
        };
        if let Err(error) = self.save() {
            self.items = previous;
            return Err(error);
        }
        Ok(saved_id)
    }

    pub fn update_text(&mut self, id: &str, name: &str, prompt: String) -> Result<(), String> {
        validate_name(name)?;
        let previous = self.items.clone();
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "未找到该生成模板，请刷新后重试".to_owned())?;
        item.name = name.trim().to_owned();
        item.prompt = prompt;
        item.updated_at = now_millis()?;
        if let Err(error) = self.save() {
            self.items = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return Err("未找到该生成模板，请刷新后重试".into());
        };
        let removed = self.items.remove(index);
        if let Err(error) = self.save() {
            self.items.insert(index, removed);
            return Err(error);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&Manifest {
            items: self.items.clone(),
        })
        .map_err(|error| format!("无法生成模板索引：{error}"))?;
        let temporary = self
            .root
            .join(format!(".{MANIFEST_NAME}.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file =
                File::create(&temporary).map_err(|error| format!("无法写入生成模板：{error}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("无法写入生成模板：{error}"))?;
            fs::rename(&temporary, self.root.join(MANIFEST_NAME))
                .map_err(|error| format!("无法更新生成模板：{error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("模板名称不能为空".into());
    }
    if trimmed.len() > 80 || name.chars().any(char::is_control) {
        return Err("模板名称最多 80 个字符，且不能包含控制字符".into());
    }
    Ok(())
}

fn now_millis() -> Result<u64, String> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间无效，无法保存模板".to_owned())?
        .as_millis();
    u64::try_from(value).map_err(|_| "系统时间超出支持范围".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn crud_round_trip_preserves_parameters_and_references() {
        let root = std::env::temp_dir().join(format!("seecut-templates-{}", Uuid::new_v4()));
        let reference = root.join("reference.png");
        fs::create_dir_all(&root).unwrap();
        fs::write(&reference, b"image").unwrap();
        let mut library = Library::load(&root).unwrap();
        let id = library
            .save_current(
                None,
                "海报",
                "一张海报".into(),
                0,
                "image-model".into(),
                json!({"size":"1024x1024","quality":"high"}),
                vec![TemplateReference {
                    path: reference,
                    name: "参考图".into(),
                    kind: "image".into(),
                    client_id: String::new(),
                    source_id: String::new(),
                }],
            )
            .unwrap();
        library
            .update_text(&id, "新海报", "更新提示词".into())
            .unwrap();
        let reloaded = Library::load(&root).unwrap();
        let item = reloaded.get(&id).unwrap();
        assert_eq!(item.name, "新海报");
        assert_eq!(item.parameters["size"], "1024x1024");
        assert_eq!(item.references.len(), 1);
        library.delete(&id).unwrap();
        assert!(library.items().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
