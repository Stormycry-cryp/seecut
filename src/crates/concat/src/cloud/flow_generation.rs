//! Thin Flow adapters over the existing catalogue, transport, cache and global library.
use super::flow_parameters::generation_body;
use super::{
    AccountAccess, App, Cloud, PickerSelection, SeeCut, begin_asset_picker,
    begin_handoff_with_imports, call, library_root, request, text,
};
use crate::personal_library::{
    Asset, AssetKind, AssetReference, Library, OutputProvenance, checked_managed_version_path,
    verify_asset_reference, with_library_write_lock,
};
use seecut_flow_core::{
    execution::{Attempt, SubmissionFailure},
    graph::MediaKind,
    host::GenerationDraft,
    runner::Scope,
};
use serde_json::{Value, json};
use slint::ComponentHandle;
use std::{cell::RefCell, path::PathBuf, rc::Rc};

type PickerCallback = Rc<dyn Fn(&App, String)>;
thread_local! { static FLOW_PICKER: RefCell<Option<(Vec<MediaKind>, PickerCallback)>> = const { RefCell::new(None) }; }

#[derive(Clone)]
pub(crate) struct ImageClient {
    cloud: Cloud,
    pub(crate) scope: Scope,
}
pub(crate) struct RegisteredAsset {
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
            if !cloud.token.is_empty()
                && let Some(account) = cloud
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
        Scope {
            account: "local".into(),
            service: "personal-library".into(),
        }
    }
    pub(crate) fn pick_flow_asset(
        &self,
        app: &App,
        allowed: Vec<MediaKind>,
        callback: PickerCallback,
    ) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        FLOW_PICKER.with(|slot| *slot.borrow_mut() = Some((allowed, callback)));
        begin_asset_picker(app, &state, "flow-reference", 0);
    }
    pub(crate) fn flow_handoff(&self, app: &App, kind: &str, asset: &Asset) -> Result<(), String> {
        if kind == "canvas" && asset.kind != AssetKind::Image {
            return Err("画布仅支持图片，请将此素材放入剪辑".into());
        }
        let state = self.state.upgrade().ok_or("资产交付不可用")?;
        let import = crate::panes::canvas::CanvasImport::from_library(
            &serde_json::to_value(asset).map_err(|e| e.to_string())?,
        );
        begin_handoff_with_imports(app, &state, kind, vec![import]);
        Ok(())
    }
}
pub(super) fn picker_allows(kind: &str) -> bool {
    FLOW_PICKER.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|(allowed, _)| allowed.iter().any(|k| media_name(k) == kind))
    })
}
pub(crate) fn media_kind(kind: AssetKind) -> MediaKind {
    match kind {
        AssetKind::Image => MediaKind::Image,
        AssetKind::Video => MediaKind::Video,
        AssetKind::Audio => MediaKind::Audio,
    }
}
pub(crate) fn media_name(kind: &MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
    }
}
pub(super) fn accept_picker(app: &App, selected: &[PickerSelection]) {
    let ui = app.global::<SeeCut>();
    if ui.get_asset_picker_source() != 0 || selected.len() != 1 || !picker_allows(&selected[0].kind)
    {
        ui.set_error("请选择一项当前节点支持的个人素材".into());
        return;
    }
    let callback = FLOW_PICKER.with(|slot| slot.borrow_mut().take());
    ui.set_asset_picker_open(false);
    if let Some((_, callback)) = callback {
        callback(app, selected[0].id.clone());
    }
}
pub(crate) fn library_asset(id: &str, version: Option<&str>) -> Result<RegisteredAsset, String> {
    let root = library_root()?;
    let asset = with_library_write_lock(|| {
        let library = Library::load(&root)?;
        let asset = library
            .get(id)
            .filter(|a| !a.trashed)
            .ok_or("库内素材不可用")?
            .clone();
        let current = asset
            .version
            .as_ref()
            .ok_or("素材缺少不可变版本，请重新导入")?;
        if version.is_some_and(|v| !current.matches_json(v)) {
            return Err("库内素材版本已变化，请重新选择".into());
        }
        checked_managed_version_path(&root, &asset)?;
        Ok::<_, String>(asset)
    })?;
    let thumbnail = crate::personal_library::thumbnail(&asset, &root);
    Ok(RegisteredAsset { asset, thumbnail })
}
/// Read the already registered identity without migration, import or hashing large media.
/// Opening/using the result still resolves and verifies its immutable bytes.
pub(crate) fn library_receipt(id: &str, version: &str) -> Result<Value, String> {
    let root = library_root()?;
    with_library_write_lock(|| {
        let library = Library::load_for_relink(&root)?;
        let asset = library
            .get(id)
            .filter(|a| !a.trashed)
            .ok_or("库内素材不可用")?;
        if !asset
            .version
            .as_ref()
            .is_some_and(|current| current.matches_json(version))
        {
            return Err("库内素材版本已变化".into());
        }
        Ok(
            json!({"assetId":asset.id,"version":asset.version,"name":asset.name,"kind":media_kind(asset.kind)}),
        )
    })
}
fn validate_references(model: &Value, assets: &[RegisteredAsset]) -> Result<(), String> {
    let references: Vec<_> = assets
        .iter()
        .map(|selected| crate::generation_templates::TemplateReference {
            path: selected.asset.path.clone(),
            name: selected.asset.name.clone(),
            kind: media_name(&media_kind(selected.asset.kind)).into(),
            client_id: selected.asset.id.clone(),
            source_id: String::new(),
        })
        .collect();
    super::validate_configuration_references(model, &references, "Flow")?;
    let mut totals = std::collections::HashMap::<String, f64>::new();
    for reference in &references {
        let limits =
            &model["parameters"]["reference_asset_ids"]["duration_seconds"][&reference.kind];
        if limits.is_null() {
            continue;
        }
        let seconds = concat_media::probe(&reference.path)
            .map_err(|_| "无法读取参考素材时长")?
            .duration
            .map(|t| t.as_f64())
            .filter(|v| v.is_finite() && *v > 0.)
            .ok_or("无法读取参考素材时长")?;
        if limits["min_per_item"].as_f64().is_some_and(|v| seconds < v)
            || limits["max_per_item"].as_f64().is_some_and(|v| seconds > v)
        {
            return Err(format!("{}的时长超出模型范围", reference.name));
        }
        let total = totals.entry(reference.kind.clone()).or_default();
        *total += seconds;
        if limits["max_total"].as_f64().is_some_and(|v| *total > v) {
            return Err("参考素材总时长超出模型范围".into());
        }
    }
    Ok(())
}
impl ImageClient {
    /// 只供受信任的 Bun 业务桥使用；账户凭据始终留在 Rust。
    pub(crate) fn bridge_cloud_json(
        &self,
        path: &str,
    ) -> Result<Value, crate::toonflow_bridge::RpcError> {
        let response = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .get(&format!("{}{}", self.cloud.base, path))
            .set("Authorization", &format!("Bearer {}", self.cloud.token))
            .call()
            .map_err(crate::toonflow_bridge::RpcError::from_http)?;
        response.into_json().map_err(|_| {
            crate::toonflow_bridge::RpcError::new("INVALID_RESPONSE", "云端返回了无效 JSON")
        })
    }
    pub(crate) fn bridge_cloud_upload(
        &self,
        path: &str,
        file: &std::path::Path,
        sha256: &str,
        key: &str,
    ) -> Result<Value, crate::toonflow_bridge::RpcError> {
        let source = std::fs::File::open(file).map_err(crate::toonflow_bridge::RpcError::io)?;
        let size = source
            .metadata()
            .map_err(crate::toonflow_bridge::RpcError::io)?
            .len();
        let response = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .put(&format!("{}{}", self.cloud.base, path))
            .set("Authorization", &format!("Bearer {}", self.cloud.token))
            .set("Content-Type", "application/zip")
            .set("Content-Length", &size.to_string())
            .set("X-Content-SHA256", sha256)
            .set("Idempotency-Key", key)
            .send(source)
            .map_err(crate::toonflow_bridge::RpcError::from_http)?;
        response.into_json().map_err(|_| {
            crate::toonflow_bridge::RpcError::new("INVALID_RESPONSE", "云端返回了无效 JSON")
        })
    }
    pub(crate) fn bridge_download(
        &self,
        url: &str,
        target: &std::path::Path,
    ) -> Result<PathBuf, String> {
        crate::cloud_files::download(&self.cloud.base, &self.cloud.token, url, target)
    }
    fn api_raw(
        &self,
        method: &str,
        path: impl Into<String>,
        body: Value,
        key: Option<&str>,
    ) -> Result<Value, super::ClientError> {
        let mut req = request(method, path, body);
        req.idem = key.map(str::to_owned);
        call(&self.cloud, &req)
    }
    fn api(
        &self,
        method: &str,
        path: impl Into<String>,
        body: Value,
        key: Option<&str>,
    ) -> Result<Value, String> {
        self.api_raw(method, path, body, key)
            .map_err(|e| format!("{}: {}", e.code, e.message))
    }
    pub(crate) fn catalog(&self) -> Result<Value, String> {
        self.api("GET", "/api/generation/models", Value::Null, None)
    }
    pub(crate) fn prepare(&self, draft: &GenerationDraft) -> Result<Value, String> {
        self.prepare_with_quote(draft).map(|(body, _)| body)
    }
    pub(crate) fn prepare_with_quote(
        &self,
        draft: &GenerationDraft,
    ) -> Result<(Value, Value), String> {
        if draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        let catalog = self.catalog()?;
        let model = catalog["models"]
            .as_array()
            .and_then(|v| {
                v.iter().find(|m| {
                    m["id"] == draft.model_id && m["kind"] == media_name(&draft.media_kind)
                })
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
                                && super::generation_effort_available(e)
                                && e["policy_revision"] == draft.parameters["policy_revision"]
                        })
                    })
                    .ok_or("生成策略已变化，请重新选择")?,
            )
        } else {
            if catalog["efforts"].as_array().is_some_and(|v| !v.is_empty()) {
                return Err("请选择当前强度档位".into());
            }
            None
        };
        let assets = draft
            .references
            .iter()
            .map(|reference| {
                let asset = library_asset(&reference.id, Some(&reference.version))?;
                if media_kind(asset.asset.kind) != reference.kind {
                    return Err("素材类型已变化".into());
                }
                Ok(asset)
            })
            .collect::<Result<Vec<_>, String>>()?;
        validate_references(model, &assets)?;
        let prompt = if draft.media_kind == MediaKind::Image {
            super::rewrite_mentions(&draft.prompt, |n| format!("第{n}张参考图片"))
        } else {
            super::rewrite_media_mentions(&draft.prompt, |kind, n| format!("{kind}{n}"))
        };
        // Validate all choices before upload or quote; these dummy IDs are never dispatched.
        generation_body(
            model,
            &prompt,
            &draft.parameters,
            &vec![String::new(); assets.len()],
            effort,
        )?;
        let pins = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        for (index, selected) in assets.iter().enumerate() {
            let extension = selected
                .asset
                .path
                .extension()
                .and_then(|e| e.to_str())
                .ok_or("参考素材格式无效")?;
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
        let mut body = generation_body(model, &prompt, &draft.parameters, &ids, effort)?;
        let mut quote_body = body.clone();
        quote_body["kind"] = json!(media_name(&draft.media_kind));
        let quote = self.api("POST", "/api/generation/quote", quote_body, None)?;
        body["quote_id"] = quote["quote_id"].clone();
        if body["quote_id"].as_str().is_none_or(str::is_empty) {
            return Err("报价未完成".into());
        }
        Ok((body, quote))
    }
    /// Called only after the owner Store successfully committed the original key/body.
    pub(crate) fn submit(&self, attempt: &Attempt) -> Result<Value, SubmissionFailure> {
        if attempt.draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        for input in &attempt.draft.references {
            library_asset(&input.id, Some(&input.version))?;
        }
        let result = self.api_raw(
            "POST",
            if attempt.draft.media_kind == MediaKind::Video {
                "/api/generation/videos"
            } else {
                "/api/generation/images"
            },
            attempt.body.clone().ok_or("请求尚未持久保存")?,
            Some(&attempt.key),
        );
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let mut failure = SubmissionFailure {
                    status: error.status,
                    code: error.code,
                    message: error.message,
                    original_key_absent: false,
                };
                // QUOTE_MISMATCH can also arise while validating an existing task's
                // price snapshot. Only the original-key absence check disambiguates it.
                if failure.known_refusal() {
                    match self.api_raw(
                        "GET",
                        "/api/generation/tasks/by-request",
                        Value::Null,
                        Some(&attempt.key),
                    ) {
                        Ok(task) => return Ok(task),
                        Err(check)
                            if check.status == 404 && check.code == "GENERATION_TASK_NOT_FOUND" =>
                        {
                            failure.original_key_absent = true;
                        }
                        Err(check) => failure
                            .message
                            .push_str(&format!("；原键核实未完成：{}", check.message)),
                    }
                }
                Err(failure)
            }
        }
    }
    pub(crate) fn lookup(&self, attempt: &Attempt) -> Result<Value, SubmissionFailure> {
        if attempt.draft.context.scope != self.scope {
            return Err("账户或服务已切换".into());
        }
        self.api_raw(
            "GET",
            "/api/generation/tasks/by-request",
            Value::Null,
            Some(&attempt.key),
        )
        .map_err(|error| {
            if let Some(original) = attempt
                .submission_failure
                .as_ref()
                .filter(|failure| failure.known_refusal())
            {
                let mut failure = original.clone();
                failure.original_key_absent =
                    error.status == 404 && error.code == "GENERATION_TASK_NOT_FOUND";
                if !failure.original_key_absent {
                    failure
                        .message
                        .push_str(&format!("；原键核实未完成：{}", error.message));
                }
                failure
            } else {
                SubmissionFailure {
                    status: error.status,
                    code: error.code,
                    message: error.message,
                    original_key_absent: false,
                }
            }
        })
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
    ) -> Result<RegisteredAsset, String> {
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
            kind: format!("flow_{}", media_name(&attempt.draft.media_kind)),
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
                Some(if attempt.draft.media_kind == MediaKind::Video {
                    "Flow 视频"
                } else {
                    "Flow 图片"
                }),
                &identity,
                &expected,
                Some(provenance),
            )
        })?;
        let thumbnail = crate::personal_library::thumbnail(&asset, &root);
        Ok(RegisteredAsset { asset, thumbnail })
    }
}
