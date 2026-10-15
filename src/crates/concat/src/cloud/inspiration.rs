// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::generation_templates::{GenerationTemplate, inspiration as recipes};

#[derive(Clone)]
pub(super) struct Draft {
    epoch: u64,
    snapshot: TaskInputSnapshot,
    reference_versions: Vec<String>,
    template_id: String,
    template_name: String,
    warning: String,
}

fn templates() -> Result<Vec<GenerationTemplate>, String> {
    let mut items = recipes::curated();
    let library = crate::generation_templates::Library::load(template_root()?)?;
    let mut personal = library.items().to_vec();
    personal.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    items.extend(personal);
    Ok(items)
}

pub(super) fn render(app: &App) {
    let ui = app.global::<SeeCut>();
    let items = match templates() {
        Ok(items) => items,
        Err(error) => {
            ui.set_error(error.into());
            recipes::curated()
        }
    };
    let category_filter = ui.get_inspiration_category();
    let cards: Vec<crate::ui::InspirationItem> = items
        .into_iter()
        .filter(|item| {
            let category = item
                .metadata
                .as_ref()
                .map_or("personal", |m| m.category.as_str());
            category_filter == 0
                || category_filter
                    == match category {
                        "product" => 1,
                        "poster" => 2,
                        "cover" => 3,
                        _ => 4,
                    }
        })
        .map(|item| {
            let metadata = item.metadata.as_ref();
            let category = metadata.map_or("personal", |m| m.category.as_str());
            let preview = metadata
                .and_then(|m| {
                    m.preview_asset_id
                        .as_deref()
                        .zip(m.preview_version.as_deref())
                })
                .and_then(|(id, version)| library_asset(id, Some(version)).ok())
                .and_then(|asset| asset.thumbnail)
                .and_then(|path| slint::Image::load_from_path(&path).ok())
                .unwrap_or_default();
            crate::ui::InspirationItem {
                id: item.id.into(),
                name: item.name.into(),
                prompt: item.prompt.into(),
                preview,
                category: match category {
                    "product" => 1,
                    "poster" => 2,
                    "cover" => 3,
                    _ => 4,
                },
                purpose: metadata
                    .map_or_else(|| "个人模板".into(), |m| m.purpose.clone())
                    .into(),
                parameters: if item.parameters.as_object().is_some_and(|p| p.is_empty()) {
                    "沿用当前模型与参数".into()
                } else {
                    template_parameter_summary(&item.parameters).into()
                },
                provenance: metadata
                    .map_or_else(
                        || "个人保存 · 旧版模板".into(),
                        |m| format!("{} · v{}\n{}", m.source, m.version, m.license),
                    )
                    .into(),
            }
        })
        .collect();
    ui.set_inspiration_has_previews(cards.iter().any(|item| item.preview.size().width > 0));
    ui.set_inspirations(rows(cards));
}

fn ensure_draft_replaceable(cloud: &Cloud) -> Result<(), String> {
    if cloud.submission.is_some() {
        return Err("上次生成尚未核实，请先回到创建页重试原任务".into());
    }
    Ok(())
}

fn replace_draft(
    app: &App,
    state: &Rc<RefCell<Cloud>>,
    snapshot: TaskInputSnapshot,
) -> Result<(), String> {
    ensure_draft_replaceable(&state.borrow())?;
    let ui = app.global::<SeeCut>();
    ui.set_prompt(snapshot.prompt.clone().into());
    ui.set_prompt_cursor(snapshot.prompt.len() as i32);
    update_model_options_for_configuration(
        app,
        state,
        snapshot.mode,
        &snapshot.model_id,
        &snapshot.parameters,
    );
    {
        let mut cloud = state.borrow_mut();
        cloud.references.clear();
        cloud.quote_id.clear();
        cloud.quote_credits = None;
        cloud.quote_body = Value::Null;
        cloud.pending_template_id = None;
        cloud.pending_task_refill_id = None;
    }
    ui.set_quote("".into());
    ui.set_can_generate(false);
    ui.set_insufficient_credits(false);
    ui.set_reference_error("".into());
    for mut reference in snapshot.references {
        // Never let an older in-flight upload identify the newly restored input.
        reference.client_id = uuid::Uuid::new_v4().to_string();
        queue_configuration_reference(app, state, reference);
    }
    render_references(app, state);
    refresh_quote(app, state);
    ui.set_generation_step(0);
    Ok(())
}

fn apply(app: &App, state: &Rc<RefCell<Cloud>>, id: &str) -> Result<(), String> {
    let ui = app.global::<SeeCut>();
    ensure_draft_replaceable(&state.borrow())?;
    if ui.get_working() {
        return Err("请等待当前操作完成".into());
    }
    let template = templates()?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or("模板已失效，原草稿已保留")?;
    let cloud = state.borrow();
    let model_id = selected_model(&ui, &cloud);
    let model = cloud
        .models
        .iter()
        .find(|m| text(m, "id") == model_id)
        .ok_or("请先选择可用模型，原草稿已保留")?;
    let parameters = recipes::prepare(
        &template,
        model,
        ui.get_mode(),
        &current_template_parameters(&ui, &cloud),
    )?;
    validate_configuration_references(model, &template.references, "模板")?;
    let original = current_task_snapshot(&ui, &cloud);
    let reference_versions = recipes::freeze_references(&original.references)?;
    let backup = Draft {
        epoch: cloud.epoch,
        snapshot: original,
        reference_versions,
        template_id: ui.get_template_id().to_string(),
        template_name: ui.get_template_name().to_string(),
        warning: ui.get_recovery_warning().to_string(),
    };
    let snapshot = TaskInputSnapshot {
        mode: ui.get_mode(),
        prompt: template.prompt,
        model_id,
        parameters,
        references: template.references,
        effort: cloud.effort_selection.clone(),
    };
    drop(cloud);
    // Only after every model, parameter and byte-version check has passed.
    replace_draft(app, state, snapshot)?;
    state.borrow_mut().inspiration_backup = Some(Box::new(backup));
    ui.set_template_id("".into());
    ui.set_template_name("".into());
    clear_recovery_warning(&ui);
    ui.set_inspiration_can_restore(true);
    ui.set_inspiration_preview_open(false);
    ui.set_notice("已填入草稿".into());
    Ok(())
}

fn restore(app: &App, state: &Rc<RefCell<Cloud>>) -> Result<(), String> {
    let ui = app.global::<SeeCut>();
    ensure_draft_replaceable(&state.borrow())?;
    if ui.get_working() {
        return Err("请等待当前操作完成".into());
    }
    let backup = state
        .borrow()
        .inspiration_backup
        .clone()
        .ok_or("暂无可恢复的草稿")?;
    if backup.epoch != state.borrow().epoch {
        state.borrow_mut().inspiration_backup = None;
        ui.set_inspiration_can_restore(false);
        return Err("账户已切换，原账户草稿未回填".into());
    }
    let cloud = state.borrow();
    let model = cloud
        .models
        .iter()
        .find(|m| text(m, "id") == backup.snapshot.model_id)
        .ok_or("原模型暂不可用，草稿备份已保留")?;
    recipes::prepare_draft(
        backup.snapshot.mode,
        &backup.snapshot.parameters,
        &backup.snapshot.references,
        &backup.reference_versions,
        model,
        &json!({}),
    )
    .map_err(|_| "原草稿的模型、参数或素材已变化，当前草稿和备份均已保留")?;
    validate_configuration_references(model, &backup.snapshot.references, "原草稿")?;
    if cloud.effort_selection != backup.snapshot.effort {
        return Err("生成档位已变化，请先恢复原档位".into());
    }
    drop(cloud);
    replace_draft(app, state, backup.snapshot)?;
    ui.set_template_id(backup.template_id.into());
    ui.set_template_name(backup.template_name.into());
    ui.set_recovery_warning(backup.warning.into());
    state.borrow_mut().inspiration_backup = None;
    ui.set_inspiration_can_restore(false);
    ui.set_notice("已恢复原草稿".into());
    Ok(())
}

pub(super) fn action(app: &App, state: &Rc<RefCell<Cloud>>, name: &str, id: &str) {
    let ui = app.global::<SeeCut>();
    ui.set_error("".into());
    let result = match name {
        "inspiration-apply" => apply(app, state, id),
        "inspiration-restore" => restore(app, state),
        "inspiration-filter" => {
            render(app);
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        ui.set_error(error.into());
    }
}
