// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline checks against the production state transitions, using the preview App.
use super::*;
use std::path::Path;

pub(super) fn run(app: &App, directory: &Path) -> Result<(), String> {
    let ui = app.global::<SeeCut>();
    // Never attach the normal cloud callbacks, heartbeat, library, or HTTP client.
    ui.set_signed_in(false);
    ui.set_page(1);
    ui.set_mode(0);
    ui.set_model_index(0);
    ui.set_asset_picker_open(false);
    let state = Rc::new(RefCell::new(Cloud::default()));
    let image_parameters = json!({"size":"1536x1024", "quality":"high", "quantity":2});
    let video_parameters = json!({"resolution":"1080p", "duration":10, "aspect_ratio":"9:16", "generate_audio":false, "quantity":2});
    let image_model = |id: &str| {
        json!({
            "id":id, "name":id, "kind":"image",
            "parameters":{
                "size":{"values":["1024x1024","1536x1024"],"default":"1024x1024"},
                "quality":{"values":["standard","high"],"default":"standard"},
                "quantity":{"values":[1,2],"default":1}
            }
        })
    };
    let video_model = json!({
        "id":"offline-video", "name":"Offline video", "kind":"video",
        "parameters":{
            "resolution":{"values":["720p","1080p"],"default":"720p"},
            "duration":{"values":[5,10],"default":5},
            "aspect_ratio":{"values":["16:9","9:16"],"default":"16:9"},
            "generate_audio":{"default":true},
            "quantity":{"values":[1,2],"default":1}
        }
    });
    state.borrow_mut().models = vec![
        image_model("offline-image-a"),
        image_model("offline-image-b"),
        video_model,
    ];
    update_model_options(app, &state);
    let snapshot = |mode, model: &str, parameters: Value| TaskInputSnapshot {
        mode,
        prompt: format!("Offline recovery {model}"),
        model_id: model.into(),
        parameters,
        references: Vec::new(),
    };
    let verify = |condition: bool, message: &str| -> Result<(), String> {
        if condition {
            Ok(())
        } else {
            Err(format!("offline state validation: {message}"))
        }
    };
    let expect_parameters = |expected: &Value, message: &str| -> Result<(), String> {
        let actual = current_template_parameters(&ui, &state.borrow());
        if &actual == expected {
            Ok(())
        } else {
            Err(format!(
                "offline state validation: {message}; expected {expected}, got {actual}"
            ))
        }
    };
    let mut passed = Vec::new();

    ui.set_template_id("unrelated-template".into());
    apply_task_snapshot(
        app,
        &state,
        snapshot(0, "offline-image-a", image_parameters.clone()),
    )?;
    expect_parameters(&image_parameters, "same-model history restore")?;
    verify(
        ui.get_template_id().is_empty(),
        "history must detach template id",
    )?;
    apply_task_snapshot(
        app,
        &state,
        snapshot(0, "offline-image-b", image_parameters.clone()),
    )?;
    verify(
        selected_model(&ui, &state.borrow()) == "offline-image-b",
        "cross-model history id",
    )?;
    expect_parameters(&image_parameters, "cross-model history values")?;
    passed.push("same/cross-model history and template detachment");

    // Publish goes through the same capture-before-replace path as a catalog response.
    let mut reordered = state.borrow().models.clone();
    reordered.reverse();
    for model in &mut reordered {
        if let Some(parameters) = model["parameters"].as_object_mut() {
            for parameter in parameters.values_mut() {
                if let Some(values) = parameter["values"].as_array_mut() {
                    values.reverse();
                }
            }
        }
    }
    publish(app, &state, "models", json!({"models":reordered}));
    verify(
        selected_model(&ui, &state.borrow()) == "offline-image-b",
        "catalog reorder must preserve model id",
    )?;
    expect_parameters(
        &image_parameters,
        "catalog reorder must preserve raw parameter values",
    )?;
    passed.push("model and option catalog reorder");

    apply_task_snapshot(
        app,
        &state,
        snapshot(1, "offline-video", video_parameters.clone()),
    )?;
    expect_parameters(&video_parameters, "video history restore")?;
    ui.set_mode(0);
    update_model_options(app, &state);
    ui.set_mode(1);
    update_model_options(app, &state);
    expect_parameters(&video_parameters, "video/image/video draft retention")?;
    passed.push("video/image/video duration audio ratio retention");

    // Change the live draft without a model transition before refilling an image.
    let changed_video = json!({"resolution":"720p", "duration":5, "aspect_ratio":"16:9", "generate_audio":true, "quantity":1});
    let model = state
        .borrow()
        .models
        .iter()
        .find(|m| text(m, "id") == "offline-video")
        .cloned()
        .unwrap();
    ui.set_resolution_index(configuration_parameter_index(
        &model,
        "resolution",
        &changed_video["resolution"],
        "check",
    )?);
    ui.set_duration_index(configuration_parameter_index(
        &model,
        "duration",
        &changed_video["duration"],
        "check",
    )?);
    ui.set_ratio_index(configuration_parameter_index(
        &model,
        "aspect_ratio",
        &changed_video["aspect_ratio"],
        "check",
    )?);
    ui.set_quantity_index(configuration_parameter_index(
        &model,
        "quantity",
        &changed_video["quantity"],
        "check",
    )?);
    ui.set_generate_audio(true);
    apply_task_snapshot(
        app,
        &state,
        snapshot(0, "offline-image-a", image_parameters.clone()),
    )?;
    ui.set_mode(1);
    update_model_options(app, &state);
    expect_parameters(
        &changed_video,
        "image refill must preserve outgoing live video draft",
    )?;
    passed.push("cross-mode history retains outgoing edits");

    apply_task_snapshot(
        app,
        &state,
        snapshot(0, "removed-model", image_parameters.clone()),
    )?;
    verify(
        ui.get_prompt().as_str() == "Offline recovery removed-model",
        "missing model prompt recovery",
    )?;
    verify(
        ui.get_model_index() == -1 && selected_model(&ui, &state.borrow()).is_empty(),
        "missing model must stay unselected",
    )?;
    refresh_quote(app, &state);
    verify(
        !ui.get_can_generate() && ui.get_quote().is_empty() && state.borrow().quote_id.is_empty(),
        "missing model quote/generate gate",
    )?;
    passed.push("missing model recovery and generation gate");

    let mut missing = snapshot(0, "offline-image-a", image_parameters);
    missing.prompt = "@[图片1] @[图片2] @[图片3]".into();
    // All paths deliberately do not exist: the real restore function cannot upload.
    let absent = directory.join(format!("absent-{}", uuid::Uuid::new_v4()));
    missing.references = (1..=3)
        .map(|index| crate::generation_templates::TemplateReference {
            path: absent.join(format!("{index}.png")),
            name: format!("reference-{index}"),
            kind: "image".into(),
            client_id: format!("reference-{index}"),
            source_id: String::new(),
        })
        .collect();
    apply_task_snapshot(app, &state, missing)?;
    verify(
        state.borrow().references.len() == 3,
        "missing references must not be dropped",
    )?;
    // Simulate successful completion of the two surrounding entries, then render
    // the real row projection with the middle recovery placeholder still present.
    {
        let mut cloud = state.borrow_mut();
        for index in [0, 2] {
            cloud.references[index]["status"] = json!("ready");
            cloud.references[index]["missing"] = json!(false);
        }
    }
    render_references(app, &state);
    let references = ui.get_references();
    for index in 0..3 {
        let row = references
            .row_data(index)
            .ok_or("missing reference UI row")?;
        verify(
            row.id.as_str() == format!("reference-{}", index + 1),
            "reference identity order",
        )?;
        verify(
            row.detail.as_str() == format!("@[图片{}]", index + 1),
            "reference mention numbering",
        )?;
        verify(
            row.missing == (index == 1),
            "middle missing-reference UI flag",
        )?;
    }
    verify(
        ui.get_prompt().as_str() == "@[图片1] @[图片2] @[图片3]",
        "recovery must preserve prompt mentions",
    )?;
    passed.push("missing middle reference order and mention numbering");

    state.borrow_mut().quote_id = "offline-quote".into();
    ui.set_can_generate(true);
    remove_reference(app, &state, "reference-2");
    verify(
        ui.get_references().row_count() == 2,
        "reference removal count",
    )?;
    verify(
        ui.get_prompt().as_str().contains("[已移除参考图片]")
            && ui.get_prompt().as_str().contains("@[图片2]"),
        "removed mention marker and following number",
    )?;
    verify(
        state.borrow().quote_id.is_empty() && !ui.get_can_generate(),
        "reference removal invalidates quote",
    )?;
    passed.push("failed or missing reference removal keeps mention and quote state consistent");

    ui.set_prompt("前后".into());
    ui.set_prompt_cursor("前".len() as i32);
    state.borrow_mut().quote_id = "stale-quote".into();
    ui.set_can_generate(true);
    open_reference_mention(app, &state);
    verify(
        ui.get_prompt().as_str() == "前@后"
            && ui.get_prompt_cursor() == "前@".len() as i32
            && ui.get_mention_open(),
        "@ button inserts at the cursor and opens references",
    )?;
    verify(
        state.borrow().quote_id.is_empty() && !ui.get_can_generate(),
        "@ button invalidates the previous quote",
    )?;
    insert_mention(app, &state, "reference-1");
    verify(
        ui.get_prompt().as_str() == "前@[图片1] 后" && !ui.get_mention_open(),
        "keyboard reference selection replaces @ at the cursor",
    )?;
    passed.push("reference mention insertion and quote invalidation");

    state.borrow_mut().personal = vec![
        json!({"id":"offline-personal", "name":"Offline item", "kind":"image", "available":false, "trashed":false}),
    ];
    ui.set_personal_search("".into());
    ui.set_personal_filter(0);
    ui.set_personal_source(0);
    ui.set_personal_favorites(false);
    ui.set_personal_trash(false);
    ui.set_personal_selection_mode(true);
    render_personal(app, &state);
    verify(
        ui.get_personal_selection_mode() && ui.get_personal_selected_count() == 0,
        "zero selection must retain management mode",
    )?;
    personal_action(app, &state, "personal-select", "offline-personal");
    verify(
        ui.get_personal_selection_mode() && ui.get_personal_selected_count() == 1,
        "management selection",
    )?;
    personal_action(app, &state, "personal-select", "offline-personal");
    verify(
        ui.get_personal_selection_mode() && ui.get_personal_selected_count() == 0,
        "deselect last row must retain management mode",
    )?;
    passed.push("management mode at zero/one/zero selected");

    // Real picker confirmation uses a closed project to avoid starting Host.
    let fixture_id = uuid::Uuid::new_v4();
    let mut fixture_paths = Vec::new();
    for index in 0..2 {
        let path = directory.join(format!("state-picker-{fixture_id}-{index}.png"));
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(file, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&[32, 96, 160, 255])
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        fixture_paths.push(path);
    }
    state.borrow_mut().personal = fixture_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            json!({
                "id":format!("picker-{index}"), "name":format!("Picker {index}"),
                "kind":"image", "path":path, "available":true, "trashed":false,
            })
        })
        .collect();
    ui.set_project_open(false);
    ui.set_asset_picker_purpose("import".into());
    ui.set_asset_picker_source(0);
    ui.set_asset_picker_open(true);
    sync_picker_selection(&ui, &state);
    let reference_count = state.borrow().references.len();
    picker_toggle(app, &state, "picker-1");
    picker_toggle(app, &state, "picker-0");
    verify(
        ui.get_asset_picker_selected_count() == 2
            && state.borrow().references.len() == reference_count
            && state.borrow().pending_imports.is_empty(),
        "picker staging must not add/import",
    )?;
    // Use the same clear helper and state operations as the cancel handler;
    // callback registration also installs timers, so it stays outside this check.
    clear_picker_selection(&ui, &state);
    state.borrow_mut().picker_context.clear();
    ui.set_asset_picker_open(false);
    verify(
        state.borrow().picker_selected_ids.is_empty()
            && ui.get_asset_picker_selected_count() == 0
            && state.borrow().references.len() == reference_count
            && state.borrow().pending_imports.is_empty(),
        "cancel staged multi-selection without adding",
    )?;
    ui.set_asset_picker_open(true);
    picker_toggle(app, &state, "picker-1");
    picker_toggle(app, &state, "picker-0");
    picker_confirm(app, &state);
    verify(
        state.borrow().pending_imports == vec![fixture_paths[1].clone(), fixture_paths[0].clone()],
        "confirmed picker batch must preserve selection order",
    )?;
    verify(
        !ui.get_asset_picker_open()
            && state.borrow().picker_batch.is_none()
            && state.borrow().picker_selected_ids.is_empty(),
        "confirmed picker clears staging",
    )?;
    cancel_project_import(app, &state);
    verify(
        state.borrow().pending_imports.is_empty(),
        "cancel queued offline imports",
    )?;
    passed.push("local picker staging/cancel and confirmed import batch order (closed project)");

    let preview_item = || CloudItem {
        id: "same-id".into(),
        name: "Offline preview".into(),
        kind: "image".into(),
        ..Default::default()
    };
    let first_token = set_media_preview(app, &state, "asset", preview_item(), true, "");
    let second_token = set_media_preview(app, &state, "task", preview_item(), true, "");
    let fixture_path = fixture_paths[0].to_string_lossy().into_owned();
    verify(
        first_token != second_token,
        "new preview open must advance token",
    )?;
    finish_media_preview(app, &state, "same-id", &fixture_path, Some(first_token));
    fail_media_preview(app, &state, "same-id", first_token, "late asset failure");
    verify(
        ui.get_media_preview_source().as_str() == "task"
            && ui.get_media_preview_loading()
            && !ui.get_media_preview_item().ready
            && ui.get_media_preview_error().is_empty(),
        "same-id old-source completion/failure must not replace current preview",
    )?;
    finish_media_preview(
        app,
        &state,
        "different-id",
        &fixture_path,
        Some(second_token),
    );
    verify(
        !ui.get_media_preview_item().ready && ui.get_media_preview_loading(),
        "mismatched preview identity",
    )?;
    finish_media_preview(app, &state, "same-id", &fixture_path, Some(second_token));
    verify(
        ui.get_media_preview_item().ready
            && !ui.get_media_preview_loading()
            && ui.get_media_preview_item().preview.size().width == 1,
        "current preview completion loads fixture",
    )?;
    let closed_token = set_media_preview(app, &state, "reference", preview_item(), true, "");
    ui.set_media_preview_open(false);
    finish_media_preview(app, &state, "same-id", &fixture_path, Some(closed_token));
    fail_media_preview(app, &state, "same-id", closed_token, "late closed failure");
    verify(
        !ui.get_media_preview_open()
            && !ui.get_media_preview_item().ready
            && ui.get_media_preview_loading()
            && ui.get_media_preview_error().is_empty(),
        "closed preview must ignore late completion/failure",
    )?;
    passed.push("preview token source identity and closed late-result guards");

    verify(
        !ui.get_signed_in()
            && state.borrow().pending.is_empty()
            && state.borrow().active_name.is_empty()
            && state.borrow().submission.is_none(),
        "offline validation must not queue network/submission",
    )?;
    std::fs::write(
        directory.join("state-validation.json"),
        serde_json::to_vec_pretty(&json!({"status":"passed", "offline":true, "checks":passed, "limits":["Picker cancellation invokes the production clear helper and handler state operations without registering UI callbacks", "Reference uploads and authenticated HTTP are not exercised; confirmation uses the shared import batch path with a closed project"]}))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
