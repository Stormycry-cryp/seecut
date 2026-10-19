//! Draft-only recommendations. This module never submits media generation.
use super::*;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

pub(crate) fn candidates() -> Value {
    json!(
        crate::generation_templates::inspiration::curated()
            .into_iter()
            .map(|item| json!({"id":item.id,"name":item.name,"prompt":item.prompt}))
            .collect::<Vec<_>>()
    )
}

#[derive(Default)]
struct Assistance {
    rewrite_backup: Option<Box<inspiration::Draft>>,
    epoch: u64,
    observed: String,
    changed: Option<Instant>,
    next: Option<Instant>,
    completed: String,
    retries: usize,
    pulse_until: Option<Instant>,
    capabilities: Option<Value>,
    context_failed: String,
    pending: Option<(String, &'static str, Receiver<Result<Value, ClientError>>)>,
}

fn signature(app: &App, cloud: &Cloud) -> String {
    let ui = app.global::<SeeCut>();
    serde_json::to_string(&json!([
        cloud.epoch,
        ui.get_page(),
        ui.get_prompt_context_revision(),
        ui.get_prompt_draft_revision(),
        ui.get_canvas_prompt_open(),
        ui.get_mode(),
        selected_model(&ui, cloud),
        current_template_parameters(&ui, cloud),
        cloud.effort_selection,
        ui.get_prompt().as_str(),
        cloud.references
    ]))
    .unwrap_or_default()
}

fn images(paths: Vec<PathBuf>) -> Result<Vec<String>, ClientError> {
    use base64::Engine;
    let mut output = Vec::new();
    if paths.len() > 4 {
        return Err("推荐最多支持四张参考图".into());
    }
    for path in paths {
        let meta = std::fs::metadata(&path).map_err(|_| ClientError::from("参考图不可用"))?;
        if meta.len() > 32 * 1024 * 1024 {
            return Err("参考图过大".into());
        }
        let source = image::ImageReader::open(path)
            .map_err(|_| ClientError::from("参考图不可用"))?
            .with_guessed_format()
            .map_err(|_| ClientError::from("参考图不可用"))?
            .decode()
            .map_err(|_| ClientError::from("参考图不可用"))?
            .thumbnail(512, 512)
            .to_rgb8();
        let mut canvas = image::RgbImage::from_pixel(
            source.width().max(280),
            source.height().max(280),
            image::Rgb([255, 255, 255]),
        );
        let x = (canvas.width() - source.width()) / 2;
        let y = (canvas.height() - source.height()) / 2;
        image::imageops::overlay(&mut canvas, &source, x as i64, y as i64);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(canvas)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .map_err(|_| ClientError::from("无法准备推荐参考图"))?;
        output.push(format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        ));
    }
    Ok(output)
}

fn launch(app: &App, cloud: &Cloud, a: &mut Assistance, operation: &'static str, selected: String) {
    let ui = app.global::<SeeCut>();
    if operation == "rewrite" {
        match inspiration::capture_draft(app, cloud) {
            Ok(backup) => a.rewrite_backup = Some(Box::new(backup)),
            Err(error) => {
                ui.set_prompt_rewriting(false);
                ui.set_prompt_recommend_error(error.into());
                return;
            }
        }
    }
    let snapshot = current_task_snapshot(&ui, cloud);
    let paths = snapshot
        .references
        .into_iter()
        .filter(|r| r.kind == "image" || r.kind.is_empty())
        .map(|r| r.path)
        .collect();
    let prompt = snapshot.prompt;
    let cloud = cloud.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    a.pending = Some((a.observed.clone(), operation, rx));
    if operation == "recommend" {
        a.next = Some(Instant::now() + Duration::from_secs(10));
        ui.set_prompt_recommend_status("正在匹配…".into());
    }
    if operation == "rewrite" {
        ui.set_prompt_rewriting(true);
        ui.set_prompt_recommend_error("".into());
    }
    std::thread::spawn(move || {
        let result = (|| {
            if operation == "context" {
                return call(
                    &cloud,
                    &request("GET", "/api/prompts/capabilities", Value::Null),
                );
            }
            let mut body =
                json!({"prompt":prompt,"images":images(paths)?,"templates":candidates()});
            if operation == "rewrite" {
                body["templateId"] = json!(selected);
            }
            let mut req = request("POST", format!("/api/prompts/{operation}"), body);
            req.idem = Some(uuid::Uuid::new_v4().to_string());
            call(&cloud, &req)
        })();
        let _ = tx.send(result);
    });
}

pub(super) fn start(weak: slint::Weak<App>, cloud: Rc<RefCell<Cloud>>) {
    tick(weak, cloud, Rc::new(RefCell::new(Assistance::default())));
}
fn tick(weak: slint::Weak<App>, cloud: Rc<RefCell<Cloud>>, state: Rc<RefCell<Assistance>>) {
    slint::Timer::single_shot(Duration::from_millis(200), move || {
        let Some(app) = weak.upgrade() else { return };
        let ui = app.global::<SeeCut>();
        let mut c = cloud.borrow_mut();
        let mut a = state.borrow_mut();
        if a.epoch != c.epoch {
            *a = Assistance {
                epoch: c.epoch,
                ..Default::default()
            };
            ui.set_prompt_recommendations(rows(Vec::<crate::ui::PromptRecommendation>::new()));
            ui.set_prompt_rewriting(false);
            ui.set_prompt_apply_id("".into());
            ui.set_prompt_apply_prompt("".into());
            ui.set_prompt_recommend_error("".into());
        }
        let fingerprint = signature(&app, &c);
        if fingerprint != a.observed {
            a.observed = fingerprint;
            a.changed = Some(Instant::now());
            a.retries = 0;
            ui.set_prompt_recommend_error("".into());
            if !ui.get_prompt_rewriting() {
                ui.set_prompt_recommend_status("等待更新".into());
            }
        }
        let mut refresh = false;
        let received = a
            .pending
            .as_ref()
            .and_then(|(_, _, rx)| match rx.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Disconnected) => Some(Err("提示词请求已结束".into())),
                Err(TryRecvError::Empty) => None,
            });
        if let Some(result) = received {
            let (frozen, operation, _) = a.pending.take().unwrap();
            if operation == "context" {
                match result {
                    Ok(value) => a.capabilities = Some(value),
                    Err(_) => a.context_failed = a.observed.clone(),
                }
            } else if frozen == a.observed {
                match result {
                    Ok(value) if operation == "recommend" => {
                        let cards = value["recommendations"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|item| crate::ui::PromptRecommendation {
                                id: text(item, "id").into(),
                                label: text(item, "label").into(),
                            })
                            .collect();
                        ui.set_prompt_recommendations(rows(cards));
                        a.pulse_until = Some(Instant::now() + Duration::from_millis(700));
                        a.completed = a.observed.clone();
                        ui.set_prompt_recommend_status("为你推荐".into());
                    }
                    Ok(value) => {
                        if let Some(prompt) = value["prompt"].as_str() {
                            // Reuse the existing full-draft restore transaction, including references.
                            c.inspiration_backup = a.rewrite_backup.take();
                            ui.set_inspiration_can_restore(c.inspiration_backup.is_some());
                            ui.set_prompt(prompt.into());
                            ui.set_prompt_cursor(prompt.len() as i32);
                            ui.set_prompt_focus_revision(ui.get_prompt_focus_revision() + 1);
                            a.observed = signature(&app, &c);
                            a.completed = a.observed.clone();
                            ui.set_prompt_recommend_status("为你推荐".into());
                            refresh = true;
                        }
                    }
                    Err(error) => {
                        a.completed = a.observed.clone();
                        if operation == "rewrite" {
                            ui.set_prompt_recommend_error("应用失败，原文已保留，请重试".into());
                        } else {
                            ui.set_prompt_recommend_status("".into());
                        }
                        if error.status == 429 && a.retries < 1 {
                            a.retries += 1;
                            a.next = Some(
                                Instant::now()
                                    + Duration::from_secs(error.retry_after.clamp(10, 3600) as u64),
                            );
                            a.completed.clear();
                            ui.set_prompt_recommend_status("等待更新".into());
                        }
                    }
                }
            }
            if operation == "rewrite" {
                a.rewrite_backup = None;
                ui.set_prompt_rewriting(false);
            }
        }
        let selected = ui.get_prompt_apply_id().to_string();
        ui.set_prompt_apply_id("".into());
        let eligible = (ui.get_page() == 1
            || (ui.get_page() == 6
                && ui.get_canvas_prompt_open()
                && !ui.get_canvas_gallery_open()))
            && ui.get_signed_in()
            && (!ui.get_prompt().trim().is_empty() || !c.references.is_empty());
        if !eligible {
            ui.set_prompt_recommendations(rows(Vec::<crate::ui::PromptRecommendation>::new()));
            ui.set_prompt_recommend_status("".into());
        }
        let rewriting = a
            .pending
            .as_ref()
            .is_some_and(|(_, operation, _)| *operation == "rewrite");
        let current_click = ui.get_prompt_apply_prompt() == ui.get_prompt()
            && ui.get_prompt_apply_context() == ui.get_prompt_context_revision()
            && ui.get_prompt_apply_draft() == ui.get_prompt_draft_revision();
        ui.set_prompt_apply_prompt("".into());
        match queued_apply(
            !selected.is_empty(),
            eligible && current_click && !ui.get_working(),
            rewriting,
        ) {
            ApplyDecision::Launch => {
                // Cancel only a pending read. An in-flight rewrite retains its identity and lock.
                a.pending = None;
                launch(&app, &c, &mut a, "rewrite", selected);
            }
            ApplyDecision::Cancel => ui.set_prompt_rewriting(false),
            ApplyDecision::Keep => {}
        }
        if eligible && a.pending.is_none() {
            if a.capabilities.is_none() && a.context_failed != a.observed {
                launch(&app, &c, &mut a, "context", String::new());
            } else if a
                .capabilities
                .as_ref()
                .is_some_and(|v| v["recommend"] == true)
                && a.completed != a.observed
                && a.changed
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
                && a.next.is_none_or(|t| Instant::now() >= t)
            {
                launch(&app, &c, &mut a, "recommend", String::new());
            } else if a
                .capabilities
                .as_ref()
                .is_some_and(|v| v["recommend"] != true)
            {
                ui.set_prompt_recommend_status("".into());
            }
        }
        ui.set_prompt_recommend_pulse(a.pulse_until.is_some_and(|t| Instant::now() < t));
        drop(a);
        drop(c);
        if refresh {
            refresh_quote(&app, &cloud);
        }
        tick(weak, cloud, state);
    });
}

#[derive(Debug, PartialEq)]
enum ApplyDecision {
    Launch,
    Cancel,
    Keep,
}
fn queued_apply(selected: bool, current: bool, rewriting: bool) -> ApplyDecision {
    if !selected || rewriting {
        ApplyDecision::Keep
    } else if current {
        ApplyDecision::Launch
    } else {
        ApplyDecision::Cancel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_choice_cancels_without_stranding_the_form() {
        assert_eq!(queued_apply(true, true, false), ApplyDecision::Launch);
        assert_eq!(queued_apply(true, false, false), ApplyDecision::Cancel);
    }
    #[test]
    fn cancel_or_second_choice_never_unlocks_an_inflight_rewrite() {
        assert_eq!(queued_apply(true, false, true), ApplyDecision::Keep);
        assert_eq!(queued_apply(true, true, true), ApplyDecision::Keep);
        assert_eq!(queued_apply(false, false, true), ApplyDecision::Keep);
    }
}
