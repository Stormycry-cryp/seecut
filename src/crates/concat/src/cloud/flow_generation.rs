//! Thin Flow adapters over the existing catalogue, transport, cache and global library.
use super::{
    AccountAccess, App, Cloud, PickerSelection, SeeCut, begin_asset_picker,
    begin_handoff_with_imports, call, library_root, request, text,
};
use crate::personal_library::{
    Asset, AssetKind, AssetReference, Library, OutputProvenance, checked_managed_version_path,
    verify_asset_reference, with_library_write_lock,
};
use seecut_flow_core::{execution::Attempt, host::GenerationDraft, runner::Scope};
use serde_json::{Value, json};
use slint::ComponentHandle;
use std::{cell::RefCell, path::PathBuf, rc::Rc};

type PickerCallback = Rc<dyn Fn(&App, String)>;
thread_local! { static FLOW_PICKER: RefCell<Option<PickerCallback>> = const { RefCell::new(None) }; }

#[derive(Clone)]
pub(crate) struct ImageClient {
    cloud: Cloud,
    pub(crate) scope: Scope,
}
pub(crate) struct RegisteredImage {
    pub(crate) asset: Asset,
    pub(crate) thumbnail: Option<PathBuf>,
}
pub(super) fn output_identity(
    cloud: &Cloud,
    key: &str,
) -> Option<crate::registration_retry::GenerationIdentity> {
    let account = cloud
        .history_name
        .strip_suffix("-history.json")
        .filter(|v| !v.is_empty())?;
    let (task, output) = key.split_once(':')?;
    Some(crate::registration_retry::GenerationIdentity {
        scope: seecut_flow_core::digest(&Scope {
            account: account.into(),
            service: seecut_flow_core::digest(&cloud.base),
        }),
        task: task.into(),
        output: output.into(),
    })
}
pub(super) fn output_folder(cloud: &Cloud) -> PathBuf {
    output_identity(cloud, "scope:cache").map_or_else(
        || cloud.folder.clone(),
        |identity| cloud.folder.join("scopes").join(identity.scope),
    )
}
pub(super) fn output_plans(cloud: &Cloud, task: &Value) -> Vec<crate::generation_outputs::Output> {
    match output_identity(cloud, "scope:cache") {
        Some(identity) => crate::generation_outputs::scoped_plan(
            task,
            &cloud.folder,
            &identity.scope,
            &cloud.local,
        ),
        None => crate::generation_outputs::plan(task, &cloud.folder, &cloud.local),
    }
}
impl AccountAccess {
    pub(crate) fn image_client(&self) -> Option<ImageClient> {
        let state = self.state.upgrade()?;
        let cloud = state.borrow();
        let account = cloud.history_name.strip_suffix("-history.json")?;
        if cloud.token.is_empty() || account.is_empty() {
            return None;
        }
        Some(ImageClient {
            scope: Scope {
                account: account.into(),
                service: seecut_flow_core::digest(&cloud.base),
            },
            cloud: Cloud {
                base: cloud.base.clone(),
                token: cloud.token.clone(),
                folder: cloud.folder.clone(),
                history_name: cloud.history_name.clone(),
                epoch: cloud.epoch,
                ..Cloud::default()
            },
        })
    }
    pub(crate) fn image_output_client(&self) -> Option<ImageClient> {
        let mut client = self.image_client()?;
        client.cloud.local = self.state.upgrade()?.borrow().local.clone();
        Some(client)
    }
    pub(crate) fn flow_scope(&self) -> Scope {
        if let Some(state) = self.state.upgrade() {
            let cloud = state.borrow();
            if !cloud.token.is_empty() {
                if let Some(account) = cloud
                    .history_name
                    .strip_suffix("-history.json")
                    .filter(|id| !id.is_empty())
                {
                    return Scope {
                        account: account.into(),
                        service: seecut_flow_core::digest(&cloud.base),
                    };
                }
            }
        }
        Scope {
            account: "local".into(),
            service: "personal-library".into(),
        }
    }
    pub(crate) fn pick_flow_image(&self, app: &App, callback: PickerCallback) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        FLOW_PICKER.with(|slot| *slot.borrow_mut() = Some(callback));
        begin_asset_picker(app, &state, "flow-reference", 0);
    }
    pub(crate) fn flow_handoff(&self, app: &App, kind: &str, asset: &Asset) -> Result<(), String> {
        let state = self.state.upgrade().ok_or("资产交付不可用")?;
        let import = crate::panes::canvas::CanvasImport::from_library(
            &serde_json::to_value(asset).map_err(|e| e.to_string())?,
        );
        begin_handoff_with_imports(app, &state, kind, vec![import]);
        Ok(())
    }
}
pub(super) fn accept_picker(app: &App, selected: &[PickerSelection]) {
    let ui = app.global::<SeeCut>();
    if ui.get_asset_picker_source() != 0
        || selected.len() != 1
        || !selected[0].kind.eq_ignore_ascii_case("image")
    {
        ui.set_error("请从个人资产库选择一张图片".into());
        return;
    }
    let callback = FLOW_PICKER.with(|slot| slot.borrow_mut().take());
    ui.set_asset_picker_open(false);
    if let Some(callback) = callback {
        callback(app, selected[0].id.clone());
    }
}
pub(crate) fn library_image(id: &str, version: Option<&str>) -> Result<RegisteredImage, String> {
    let root = library_root()?;
    let asset = with_library_write_lock(|| {
        let library = Library::load(&root)?;
        let asset = library
            .get(id)
            .filter(|a| !a.trashed && a.kind == AssetKind::Image)
            .ok_or("库内图片不可用")?
            .clone();
        let current = serde_json::to_string(
            asset
                .version
                .as_ref()
                .ok_or("素材缺少不可变版本，请重新导入")?,
        )
        .map_err(|e| e.to_string())?;
        if version.is_some_and(|v| v != current) {
            return Err("库内素材版本已变化，请重新选择".into());
        }
        checked_managed_version_path(&root, &asset)?;
        Ok::<_, String>(asset)
    })?;
    let thumbnail = crate::personal_library::thumbnail(&asset, &root);
    Ok(RegisteredImage { asset, thumbnail })
}
pub(crate) fn image_body(
    model: &Value,
    prompt: &str,
    parameters: &Value,
    ids: &[String],
    effort: Option<&Value>,
) -> Result<Value, String> {
    if model["kind"] != "image" || model["available"] != true {
        return Err("该图片模型当前不可运行".into());
    }
    let mut body = json!({"model":model["id"],"prompt":super::rewrite_mentions(prompt.trim(),|n|format!("第{n}张参考图片")),"operation":if ids.is_empty(){"generate"}else{"edit"}});
    for field in ["size", "quality", "quantity"] {
        let parameter = &model["parameters"][field];
        if parameter.is_null() {
            continue;
        }
        let selected = parameters
            .get(field)
            .cloned()
            .or_else(|| {
                (field == "quantity")
                    .then(|| parameters["count"].clone())
                    .filter(|v| !v.is_null())
            })
            .unwrap_or_else(|| parameter["default"].clone());
        if !parameter["values"]
            .as_array()
            .is_some_and(|v| v.contains(&selected))
        {
            return Err(format!("{field} 不在当前模型能力中"));
        }
        body[field] = selected;
    }
    if !ids.is_empty() {
        body["reference_asset_ids"] = json!(ids);
    }
    if let Some(effort) = effort {
        body["effort_id"] = effort["id"].clone();
        body["policy_revision"] = effort["policy_revision"].clone();
    }
    Ok(body)
}
impl ImageClient {
    fn api(
        &self,
        method: &str,
        path: impl Into<String>,
        body: Value,
        key: Option<&str>,
    ) -> Result<Value, String> {
        let mut req = request(method, path, body);
        req.idem = key.map(str::to_owned);
        call(&self.cloud, &req).map_err(|e| format!("{}: {}", e.code, e.message))
    }
    pub(crate) fn catalog(&self) -> Result<Value, String> {
        self.api("GET", "/api/generation/models", Value::Null, None)
    }
    pub(crate) fn prepare(&self, draft: &GenerationDraft) -> Result<Value, String> {
        if draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        let catalog = self.catalog()?;
        let model = catalog["models"]
            .as_array()
            .and_then(|v| {
                v.iter()
                    .find(|m| m["id"] == draft.model_id && m["kind"] == "image")
            })
            .ok_or("模型已不可用")?;
        if seecut_flow_core::digest(model) != draft.capability_version {
            return Err("模型能力已变化，请重新选择".into());
        }
        let effort = if let Some(id) = draft.parameters["effort_id"].as_str() {
            Some(
                catalog["efforts"]
                    .as_array()
                    .and_then(|v| {
                        v.iter().find(|e| {
                            e["id"] == id
                                && e["available"] == true
                                && e["policy_revision"] == draft.parameters["policy_revision"]
                        })
                    })
                    .ok_or("生成策略已变化，请重新选择")?,
            )
        } else {
            None
        };
        let pins = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        for (index, reference) in draft.references.iter().enumerate() {
            let selected = library_image(&reference.id, Some(&reference.version))?;
            let extension = selected
                .asset
                .path
                .extension()
                .and_then(|e| e.to_str())
                .ok_or("参考图片格式无效")?;
            let pinned = pins.path().join(format!("reference-{index}.{extension}"));
            std::fs::copy(&selected.asset.path, &pinned).map_err(|e| e.to_string())?;
            let reference = AssetReference {
                asset_id: selected.asset.id.clone(),
                version: selected.asset.version.clone(),
            };
            verify_asset_reference(&pinned, Some(&reference))?;
            let uploaded=self.api("POST","local:upload",json!({"path":pinned,"purpose":"generation_input","team":"","client_id":reference.asset_id}),None)?;
            ids.push(
                uploaded["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("参考上传未返回业务身份")?
                    .to_owned(),
            );
        }
        let mut body = image_body(model, &draft.prompt, &draft.parameters, &ids, effort)?;
        let mut quote_body = body.clone();
        quote_body["kind"] = json!("image");
        let quote = self.api("POST", "/api/generation/quote", quote_body, None)?;
        body["quote_id"] = quote["quote_id"].clone();
        if body["quote_id"].as_str().is_none_or(str::is_empty) {
            return Err("报价未完成".into());
        }
        Ok(body)
    }
    /// Called only after the owner Store successfully committed the original key/body.
    pub(crate) fn submit(&self, attempt: &Attempt) -> Result<Value, String> {
        if attempt.draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        for input in &attempt.draft.references {
            library_image(&input.id, Some(&input.version))?;
        }
        self.api(
            "POST",
            "/api/generation/images",
            attempt.body.clone().ok_or("请求尚未持久保存")?,
            Some(&attempt.key),
        )
    }
    pub(crate) fn lookup(&self, attempt: &Attempt) -> Result<Value, String> {
        if attempt.draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        self.api(
            "GET",
            "/api/generation/tasks/by-request",
            Value::Null,
            Some(&attempt.key),
        )
    }
    pub(crate) fn task(&self, task: &str) -> Result<Value, String> {
        self.api(
            "GET",
            format!("/api/generation/tasks/{task}"),
            Value::Null,
            None,
        )
    }
    pub(crate) fn register(
        &self,
        attempt: &Attempt,
        task: &Value,
        output: &str,
    ) -> Result<RegisteredImage, String> {
        if attempt.draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        let expected = attempt
            .tasks
            .get(&text(task, "id"))
            .and_then(|t| t.outputs.get(output))
            .ok_or("冻结输出不存在")?
            .sha256
            .clone();
        let identity = crate::registration_retry::GenerationIdentity {
            scope: seecut_flow_core::digest(&self.scope),
            task: text(task, "id"),
            output: output.into(),
        };
        let root = library_root()?;
        let inputs = attempt
            .draft
            .references
            .iter()
            .map(|a| {
                Ok(AssetReference {
                    asset_id: a.id.clone(),
                    version: Some(
                        serde_json::from_str(&a.version)
                            .map_err(|e| format!("输入版本无效: {e}"))?,
                    ),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let provenance = OutputProvenance {
            kind: "flow_image".into(),
            project_path: None,
            document_sha256: attempt.source_digest.clone(),
            inputs,
            parameters: json!({"document_id":attempt.draft.context.document_id,"graph_id":attempt.draft.context.graph_id,"node_id":attempt.draft.context.node_id,"attempt_id":attempt.id,"request":attempt.body,"frozen_input":attempt.draft}),
        };
        let existing = with_library_write_lock(|| {
            let _ = crate::registration_retry::retry(&root);
            crate::registration_retry::generation_asset(&root, &identity, &expected)
        })?;
        let path = if let Some(asset) = &existing {
            asset.path.clone()
        } else {
            let folder = output_folder(&self.cloud);
            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
            let mut cached_task = task.clone();
            cached_task["status"] = json!("succeeded");
            let plan = output_plans(&self.cloud, &cached_task)
                .into_iter()
                .find(|p| p.key == format!("{}:{output}", text(task, "id")))
                .ok_or("输出尚不可下载")?;
            if plan.sha256.as_deref() != Some(expected.as_str()) {
                return Err("输出缺少可信摘要或冻结版本不一致".into());
            }
            crate::generation_outputs::prepare(&plan, |target| {
                if task["status"] != "succeeded" {
                    return Err("远端输出已不可下载；可恢复已有本地副本".into());
                }
                crate::cloud_files::download(&self.cloud.base, &self.cloud.token, &plan.url, target)
            })?
        };
        let asset = with_library_write_lock(|| {
            crate::registration_retry::register_generation_output(
                &root,
                &path,
                Some("Flow 图片"),
                &identity,
                &expected,
                Some(provenance),
            )
        })?;
        let thumbnail = crate::personal_library::thumbnail(&asset, &root);
        Ok(RegisteredImage { asset, thumbnail })
    }
}
