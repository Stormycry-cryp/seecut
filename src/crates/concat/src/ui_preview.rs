// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic UI captures using synthetic data and Slint's own software renderer.
//! Opt in with SEECUT_UI_PREVIEW_DIR; normal startup never enters this module.
use crate::ui::{
    App, CanvasControls, CanvasLayerData, CanvasThumbs, CloudItem, Editor, GenerationBatch,
    GenerationTemplate, I18n, PersonalAssetGroup, RecentProjectData, SeeCut, StartData, Theme,
};
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::{cell::RefCell, path::Path, rc::Rc};

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}
fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(items)).into()
}
fn figma_artwork(seed: u8) -> Option<Image> {
    let directory = std::env::var_os("SEECUT_UI_PREVIEW_FIGMA_DIR")?;
    let sheet = image::open(Path::new(&directory).join("asset-normal-mac-controls.png"))
        .ok()?
        .to_rgba8();
    // These are the six media wells in the approved Figma screenshot. The
    // source screenshot stays untouched; preview data is never persisted.
    let wells = [
        (305, 230, 354, 214),
        (679, 230, 354, 214),
        (1054, 230, 354, 214),
        (305, 503, 354, 216),
        (679, 503, 354, 216),
        (1054, 503, 354, 216),
    ];
    let (x, y, width, height) = wells[usize::from(seed / 24) % wells.len()];
    if x + width > sheet.width() || y + height > sheet.height() {
        return None;
    }
    // The design screenshot already contains rounded white corner pixels.
    // Remove them so the capture exercises Slint's own card clipping.
    let inset = 12;
    let interior = image::imageops::crop_imm(
        &sheet,
        x + inset,
        y + inset,
        width - 2 * inset,
        height - 2 * inset,
    )
    .to_image();
    let pixels = image::imageops::resize(
        &interior,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pixels.as_raw(), width, height);
    Some(Image::from_rgba8(buffer))
}
fn artwork(seed: u8) -> Image {
    if let Some(image) = figma_artwork(seed) {
        return image;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(320, 240);
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = (i % 320) as i32;
        let y = (i / 320) as i32;
        let circle = (x - 160).pow(2) + (y - 108).pow(2) < 65_i32.pow(2);
        let stripe = y > 178 && (x / 24) % 2 == 0;
        *pixel = if circle {
            Rgba8Pixel::new(223, 178 + seed / 8, 105 + seed / 5, 255)
        } else if stripe {
            Rgba8Pixel::new(70, 94 + seed / 4, 112, 255)
        } else {
            Rgba8Pixel::new(36 + seed / 4, 55 + (y / 8) as u8, 70 + (x / 10) as u8, 255)
        };
    }
    Image::from_rgba8(buffer)
}

// Boundary fixture: preserve the requested aspect ratio and all four edges.
fn framed_artwork(seed: u8, width: u32, height: u32) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = i as u32 % width;
        let y = i as u32 / width;
        let border = x < 8 || y < 8 || x >= width - 8 || y >= height - 8;
        let centre = x > width / 4 && x < width * 3 / 4 && y > height / 4 && y < height * 3 / 4;
        *pixel = if border {
            Rgba8Pixel::new(248, 216, 111, 255)
        } else if centre {
            Rgba8Pixel::new(213, 138 + seed / 4, 99, 255)
        } else {
            Rgba8Pixel::new(32 + seed / 4, 60, 90, 255)
        };
    }
    Image::from_rgba8(buffer)
}

fn mask_artwork() -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(42, 28);
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = (i % 42) as i32;
        let y = (i / 42) as i32;
        let value = if (x - 21).pow(2) + (y - 14).pow(2) < 100 {
            255
        } else {
            0
        };
        *pixel = Rgba8Pixel::new(value, value, value, 255);
    }
    Image::from_rgba8(buffer)
}

fn settle(app: &App) -> Result<(), slint::PlatformError> {
    slint::platform::update_timers_and_animations();
    let _ = app.window().take_snapshot()?;
    std::thread::sleep(std::time::Duration::from_millis(220));
    slint::platform::update_timers_and_animations();
    Ok(())
}

// Real Slint hit testing and clicked callbacks, with no native mouse or CUA.
fn click_fixture(
    app: &App,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Result<(), slint::PlatformError> {
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    settle(app)?;
    let position = slint::LogicalPosition::new(x, y);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
    settle(app)?;
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    settle(app)
}

fn capture(
    app: &App,
    directory: &Path,
    name: &str,
    width: u32,
    height: u32,
) -> Result<(), slint::PlatformError> {
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    let editor = app.global::<Editor>();
    if app.global::<SeeCut>().get_page() == 6 {
        let sidebar = if editor.get_canvas_sidebar_open() {
            250.0
        } else {
            0.0
        };
        let stage_width = ((width as f32 - 120.0 - sidebar) * 0.72).min(600.0);
        editor.set_canvas_stage_w(stage_width);
        editor.set_canvas_stage_h(stage_width * 0.75);
        editor.set_canvas_zoom(stage_width / 320.0 * 100.0);
    }
    settle(app)?;
    let capture = app.window().take_snapshot()?;
    if capture.width() != width || capture.height() != height {
        return Err(slint::PlatformError::Other(format!(
            "Preview size mismatch: requested {width}x{height}, got {}x{}",
            capture.width(),
            capture.height()
        )));
    }
    let path = directory.join(format!("{name}.png"));
    let file =
        std::fs::File::create(&path).map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    let mut encoder = png::Encoder::new(file, capture.width(), capture.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(capture.as_bytes()))
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    println!("UI preview: {}", path.display());
    Ok(())
}
pub(crate) fn run(directory: &Path) -> Result<(), slint::PlatformError> {
    std::fs::create_dir_all(directory).map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window)))
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    let app = App::new()?;
    app.set_macos(crate::platform::MACOS);
    app.set_on_start(false);
    app.set_project_name("合成示例项目".into());
    let weak = app.as_weak();
    app.global::<Editor>()
        .on_workspace_resized(move |width, height| {
            if let Some(app) = weak.upgrade() {
                let dock = if width < 1000.0 {
                    crate::dock::compact_dock()
                } else {
                    crate::dock::default_dock()
                };
                let mut layout = crate::dock::DockLayout {
                    seats: vec![],
                    dividers: vec![],
                    extents: vec![],
                };
                crate::dock::lay_out(&dock, (0.0, 0.0, width, height), &mut layout);
                let editor = app.global::<Editor>();
                editor.set_seats(model(layout.seats));
                editor.set_dividers(model(layout.dividers));
            }
        });
    // Select the shipped Chinese catalogue without consulting the user's config.
    let fixture_dirs = concat_host::AppDirs {
        config: directory.join("fixture-config"),
        data: directory.join("fixture-data"),
    };
    crate::i18n::select("zh-Hans", &fixture_dirs);
    let words = app.global::<I18n>();
    words.on_lookup(|_, key| crate::i18n::t(&key).into());
    words.on_lookup1(|_, key, a| crate::i18n::tf(&key, &[&a]).into());
    words.on_lookup2(|_, key, a, b| crate::i18n::tf(&key, &[&a, &b]).into());
    words.set_lang(crate::i18n::current().into());
    app.global::<Theme>().set_dark(false);
    let state = app.global::<SeeCut>();
    let action_log = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let action_log_for_ui = action_log.clone();
    let action_app = app.as_weak();
    state.on_action(move |name, id| {
        action_log_for_ui
            .borrow_mut()
            .push((name.to_string(), id.to_string()));
        if name == "reference-mention-open"
            && let Some(app) = action_app.upgrade()
        {
            crate::cloud::open_reference_mention_ui(&app.global::<SeeCut>());
        }
    });
    state.set_auth_open(false);
    state.set_creator_mode(1);
    state.set_reduced_motion(true);
    state.set_signed_in(true);
    state.set_prompt("以参考素材为基础，制作一张暖色几何海报，保留主体轮廓与材质细节。".into());
    state.set_model_names(model(vec!["演示图像模型".into()]));
    state.set_ratios(model(vec!["4:3".into(), "1:1".into(), "16:9".into()]));
    state.set_resolutions(model(vec!["1024".into(), "2048".into()]));
    state.set_qualities(model(vec!["标准".into(), "高品质".into()]));
    state.set_quantities(model(vec!["1 张".into(), "2 张".into()]));
    state.set_reference_max(4);
    state.set_accepts_references(true);
    state.set_reference_limit("最多 4 张参考图片".into());
    let assets: Vec<_> = (0..8)
        .map(|i| CloudItem {
            id: format!("fixture-{i}").into(),
            name: format!("沙丘 · {:02}", i + 1).into(),
            detail: "图片 · 本地导入 · 1536 × 1024".into(),
            kind: "image".into(),
            preview: artwork(i * 24),
            ready: true,
            local: true,
            favorite: i == 1,
            date_label: if i < 5 { "2026-09-21" } else { "2026-09-20" }.into(),
            ..Default::default()
        })
        .collect();
    state.set_personal_assets(model(assets.clone()));
    state.set_personal_groups(model(vec![
        PersonalAssetGroup {
            date_label: "今天".into(),
            items: model(assets[..6].to_vec()),
        },
        PersonalAssetGroup {
            date_label: "2026-09-20".into(),
            items: model(assets[6..].to_vec()),
        },
    ]));
    state.set_templates(model(
        (0..4)
            .map(|i| GenerationTemplate {
                id: format!("template-{i}").into(),
                name: ["暖色几何海报", "静物光影", "产品展示", "留白构图"][i].into(),
                mode_label: "图片".into(),
                model_name: "演示模型".into(),
                parameter_summary: "4:3 · 1024p · 1 张".into(),
                prompt_preview: "简洁的几何构图，柔和的暖色光线，保留主体轮廓和材质细节。".into(),
                reference_summary: if i == 0 {
                    "6 项参考素材"
                } else {
                    "2 项参考素材"
                }
                .into(),
                updated_label: "2026-09-21".into(),
                references: model(if i == 0 {
                    assets[..6].to_vec()
                } else {
                    assets[i..i + 2].to_vec()
                }),
            })
            .collect(),
    ));
    let mut refs = assets[..2].to_vec();
    refs[0].detail = "@[图片1]".into();
    refs[1].detail = "@[图片2]".into();
    refs[0].status = "已上传".into();
    refs[1].status = "上传失败".into();
    refs[1].ready = false;
    state.set_references(model(refs.clone()));
    let mut result = assets[2].clone();
    result.name = "暖色几何海报".into();
    result.status = "已完成".into();
    result.refillable = true;
    let mut results = vec![result.clone()];
    for (i, asset) in assets.iter().take(5).enumerate() {
        let mut item = asset.clone();
        item.name = format!("生成结果 {}", i + 2).into();
        if i == 1 {
            item.kind = "video".into();
            item.detail = "8 秒 · 320 × 240".into();
        }
        item.status = if i == 3 {
            "生成中"
        } else if i == 4 {
            "生成失败"
        } else {
            "已完成"
        }
        .into();
        item.failed = i == 4;
        if item.failed {
            item.detail = "参考素材已过期，请替换后重新生成。".into();
        }
        item.refillable = i != 2;
        item.ready = i < 3;
        if !item.ready {
            item.preview = Image::default();
        }
        results.push(item);
    }
    state.set_team_names(model(vec!["演示创作团队".into()]));
    state.set_team_index(0);
    state.set_team_owner(true);
    state.set_assets(model(assets.clone()));
    state.set_email("creator@example.test".into());
    state.set_tasks(model(results.clone()));
    state.set_batches(model(vec![GenerationBatch {
        id: "fixture-batch".into(),
        label: "09-23 18:00".into(),
        summary: "生成 6 项 · 图片".into(),
        items: model(results.clone()),
    }]));
    state.set_personal_folder_names(model(vec![
        "全部素材".into(),
        "未分类".into(),
        "沙丘项目".into(),
        "灵感参考".into(),
        "成片".into(),
    ]));
    state.set_personal_move_folder_names(model(vec![
        "未分类".into(),
        "沙丘项目".into(),
        "灵感参考".into(),
        "成片".into(),
    ]));
    state.set_personal_folder_filter(2);
    state.set_canvas_projects(model(
        (0..5)
            .map(|i| CloudItem {
                id: format!("fixture-canvas-{i}").into(),
                name: ["沙丘主视觉", "日落之后", "蓝色时刻", "光的边界", "构图练习"][i].into(),
                detail: "今天 14:32 · 1920 × 1080".into(),
                preview: artwork((i * 24) as u8),
                ready: true,
                ..Default::default()
            })
            .collect(),
    ));
    state.set_canvas_gallery_open(false);
    state.set_selected_task(0);
    let editor = app.global::<Editor>();
    editor.set_output_width(320);
    editor.set_output_height(240);
    editor.set_has_picture(true);
    editor.set_preview_frame(artwork(72));
    editor.set_preview_clip_name("合成素材".into());
    editor.set_preview_duration(8.0);
    editor.set_canvas_has_document(true);
    editor.set_canvas_name("合成构图.comp".into());
    editor.set_canvas_frame(artwork(72));
    editor.set_canvas_stage_w(320.0);
    editor.set_canvas_stage_h(240.0);
    // Canvas publishes Navigator zoom multiplied by 100: this means 100%.
    editor.set_canvas_zoom(100.0);
    editor.set_canvas_tool(3);
    editor.set_canvas_can_undo(true);
    editor.set_canvas_active_layer(0);
    editor.set_canvas_layers(model(vec![CanvasLayerData {
        id: 0,
        name: "合成图层".into(),
        opacity: 1.0,
        active: true,
        ..Default::default()
    }]));
    app.global::<CanvasThumbs>()
        .set_layer_images(model(vec![artwork(48)]));
    app.global::<CanvasThumbs>()
        .set_mask_images(model(vec![Image::default()]));
    app.show()?;
    app.set_start_resolutions(model(
        crate::studio::RESOLUTIONS
            .iter()
            .map(|(label, _, _)| (*label).into())
            .collect(),
    ));
    app.set_start_rates(model(
        crate::studio::START_RATES
            .iter()
            .map(|(label, _, _)| (*label).into())
            .collect(),
    ));
    app.set_start(StartData {
        name: "新建剪辑项目".into(),
        location: "/fixture/projects".into(),
        resolution: 0,
        rate: 3,
        size_readout: "1920 x 1080".into(),
        rate_readout: "30/1 fps".into(),
        ..Default::default()
    });
    app.set_recents(model(
        (0..5)
            .map(|i| RecentProjectData {
                path: format!("/fixture/clip-{i}").into(),
                name: ["沙丘短片", "日落之后", "蓝色时刻", "光的边界", "镜头练习"][i].into(),
                detail: "00:32".into(),
                when: "今天".into(),
                poster: artwork((i * 24) as u8),
            })
            .collect(),
    ));
    state.set_page(0);
    app.set_on_start(true);
    capture(&app, directory, "clip-project-gallery", 1280, 800)?;
    capture(&app, directory, "clip-project-gallery-1440x960", 1440, 960)?;
    app.set_clip_create_open(true);
    capture(&app, directory, "clip-project-new", 1280, 800)?;
    app.set_clip_create_open(false);
    app.set_on_start(false);
    // Product-layout comparison at the exact approved viewport sizes. The
    // media is sampled from the local Figma screenshot when the opt-in source
    // directory is supplied; all tasks and prices below remain fixture data.
    let mut pro_refs = assets[..8].to_vec();
    for (i, reference) in pro_refs.iter_mut().enumerate() {
        reference.preview = artwork((i * 24) as u8);
        reference.ready = true;
        reference.status = "".into();
        reference.detail = format!("@[图片{}]", i + 1).into();
    }
    state.set_references(model(pro_refs));
    state.set_reference_max(8);
    state.set_model_names(model(vec!["Seedream".into()]));
    state.set_ratio_index(2);
    state.set_resolutions(model(vec!["1536 × 864".into(), "1024 × 1024".into()]));
    let mut pro_results = results[..4].to_vec();
    for (i, result) in pro_results.iter_mut().enumerate() {
        result.name = format!("沙丘 · {:02}", i + 1).into();
        result.preview = artwork((i * 24) as u8);
        result.ready = true;
        result.failed = false;
        result.status = "1536 × 864".into();
    }
    state.set_batches(model(vec![GenerationBatch {
        id: "fixture-pro-batch".into(),
        label: "今天 14:32".into(),
        summary: "4 张 · 16:9 · 1536 × 864".into(),
        items: model(pro_results),
    }]));
    state.set_can_generate(true);
    state.set_quote("本次 8 积分".into());
    state.set_prompt(
        "极简建筑立于开阔的沙地，低角度日光，细腻的混凝土与木材质感，安静的建筑摄影。".into(),
    );
    state.set_page(1);
    capture(&app, directory, "generation-pro-1440x960", 1440, 960)?;
    capture(&app, directory, "generation-pro-1024x960", 1024, 960)?;
    capture(&app, directory, "generation-pro-1440x800", 1440, 800)?;
    let pro_prompt = state.get_prompt();
    state.set_prompt("极简建筑立于开阔的沙地，低角度日光，细腻的混凝土与木材质感。\n保留建筑轮廓、入口和地面阴影，用自然光呈现材料肌理。\n镜头从正面缓慢靠近，画面边缘保留天空和远山，色调安静克制。\n请检查末行内容在输入框内清晰可读，光标和操作按钮互不遮挡。".into());
    click_fixture(&app, 1024, 960, 180.0, 405.0)?;
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Control.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::DownArrow.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::DownArrow.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Control.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "✓".into() });
    settle(&app)?;
    if !state.get_prompt().as_str().ends_with('✓') {
        return Err(slint::PlatformError::Other(format!(
            "Long prompt end was not editable (cursor {}, bytes {}, marker {})",
            state.get_prompt_cursor(),
            state.get_prompt().len(),
            state
                .get_prompt()
                .as_str()
                .find('✓')
                .map_or(-1, |pos| pos as i32),
        )));
    }
    capture(
        &app,
        directory,
        "generation-pro-long-prompt-1024x960",
        1024,
        960,
    )?;
    state.set_prompt(pro_prompt);
    state.set_prompt_cursor(state.get_prompt().len() as i32);
    let mention_count = action_log.borrow().len();
    click_fixture(&app, 1024, 960, 124.0, 475.0)?;
    if !state.get_mention_open()
        || !state.get_prompt().as_str().ends_with('@')
        || !action_log.borrow()[mention_count..]
            .iter()
            .any(|(name, _)| name == "reference-mention-open")
    {
        return Err(slint::PlatformError::Other(
            "Reference button did not open the mention list".into(),
        ));
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::DownArrow.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "\n".into() });
    if !action_log
        .borrow()
        .iter()
        .any(|(name, id)| name == "reference-mention" && !id.is_empty())
    {
        return Err(slint::PlatformError::Other(
            "Keyboard reference selection was not triggered".into(),
        ));
    }
    state.set_mention_open(false);
    state.set_handoff_kind("canvas".into());
    state.set_handoff_count(2);
    state.set_handoff_targets(model(vec![
        CloudItem {
            id: "new".into(),
            name: "新建画布项目".into(),
            ..Default::default()
        },
        CloudItem {
            id: "fixture-canvas-0".into(),
            name: "沙丘主视觉".into(),
            detail: "今天 14:32".into(),
            preview: artwork(0),
            ready: true,
            ..Default::default()
        },
        CloudItem {
            id: "fixture-canvas-1".into(),
            name: "建筑灵感板".into(),
            detail: "昨天 18:06".into(),
            preview: artwork(96),
            ready: true,
            ..Default::default()
        },
    ]));
    state.set_handoff_selected_id("".into());
    state.set_handoff_open(true);
    app.global::<Theme>().set_dark(true);
    click_fixture(&app, 1440, 960, 700.0, 390.0)?;
    if state.get_handoff_selected_id() != "fixture-canvas-0" {
        return Err(slint::PlatformError::Other(
            "Target selection did not update".into(),
        ));
    }
    capture(&app, directory, "handoff-canvas-1440x960", 1440, 960)?;
    app.global::<Theme>().set_dark(false);
    state.set_handoff_open(false);
    state.set_references(model(refs));
    state.set_reference_max(4);
    state.set_model_names(model(vec!["演示图像模型".into()]));
    state.set_ratio_index(0);
    state.set_resolutions(model(vec!["1024".into(), "2048".into()]));
    state.set_can_generate(false);
    state.set_quote("".into());
    state.set_reference_error("参考素材上传失败，请重试或移除".into());
    state.set_prompt("以参考素材为基础，制作一张暖色几何海报，保留主体轮廓与材质细节。".into());
    state.set_batches(model(vec![GenerationBatch {
        id: "fixture-batch".into(),
        label: "09-23 18:00".into(),
        summary: "生成 6 项 · 图片".into(),
        items: model(results.clone()),
    }]));
    for (page, name) in [
        (1, "generation"),
        (0, "editing"),
        (5, "personal"),
        (2, "team"),
        (4, "account"),
        (3, "credits"),
        (7, "templates"),
        (6, "canvas"),
    ] {
        state.set_page(page);
        for (width, height, suffix) in [(1400, 900, "wide"), (900, 640, "narrow")] {
            capture(&app, directory, &format!("{name}-{suffix}"), width, height)?;
        }
    }
    let mut boundary_assets = assets.clone();
    boundary_assets[0].preview = framed_artwork(0, 180, 360);
    boundary_assets[0].detail = "图片 · 本地导入 · 180 × 360".into();
    boundary_assets[1].preview = framed_artwork(24, 400, 160);
    boundary_assets[1].detail = "图片 · 本地导入 · 400 × 160".into();
    state.set_personal_assets(model(boundary_assets.clone()));
    state.set_personal_groups(model(vec![PersonalAssetGroup {
        date_label: "比例边界".into(),
        items: model(boundary_assets[..2].to_vec()),
    }]));
    state.set_page(5);
    capture(&app, directory, "personal-ratio-bounds-1440x960", 1440, 960)?;
    capture(&app, directory, "personal-ratio-bounds-900x640", 900, 640)?;
    state.set_personal_assets(model(assets.clone()));
    state.set_personal_groups(model(vec![
        PersonalAssetGroup {
            date_label: "今天".into(),
            items: model(assets[..6].to_vec()),
        },
        PersonalAssetGroup {
            date_label: "2026-09-20".into(),
            items: model(assets[6..].to_vec()),
        },
    ]));
    app.global::<Theme>().set_dark(true);
    for (page, name) in [
        (1, "generation"),
        (4, "account"),
        (7, "templates"),
        (6, "canvas"),
    ] {
        state.set_page(page);
        capture(&app, directory, &format!("{name}-dark-narrow"), 900, 640)?;
    }
    app.global::<Theme>().set_dark(false);
    state.set_page(6);
    state.set_canvas_gallery_open(true);
    capture(&app, directory, "canvas-project-gallery", 1280, 800)?;
    capture(
        &app,
        directory,
        "canvas-project-gallery-1440x960",
        1440,
        960,
    )?;
    app.global::<Theme>().set_dark(true);
    capture(
        &app,
        directory,
        "canvas-project-gallery-dark-1440x960",
        1440,
        960,
    )?;
    app.global::<Theme>().set_dark(false);
    state.set_canvas_gallery_open(false);
    state.set_page(1);
    for width in [1024, 1280, 1440] {
        capture(&app, directory, &format!("generation-{width}"), width, 800)?;
    }
    state.set_page(5);
    for width in [1024, 1280, 1440] {
        capture(&app, directory, &format!("personal-{width}"), width, 800)?;
    }
    state.set_page(2);
    click_fixture(&app, 1400, 900, 1185.0, 438.0)?;
    capture(&app, directory, "team-use-menu-wide", 1400, 900)?;
    state.set_team_menu(0);
    click_fixture(&app, 900, 640, 840.0, 443.0)?;
    capture(&app, directory, "team-manage-menu-narrow", 900, 640)?;
    state.set_team_menu(0);
    state.set_page(6);
    editor.set_canvas_sidebar_open(false);
    capture(&app, directory, "canvas-collapsed-narrow", 900, 640)?;
    editor.set_canvas_sidebar_open(true);
    editor.set_canvas_modified(true);
    editor.set_canvas_open_confirm(true);
    capture(&app, directory, "canvas-unsaved-narrow", 900, 640)?;
    editor.set_canvas_open_confirm(false);
    let controls = app.global::<CanvasControls>();
    controls.set_red(58.0);
    controls.set_green(118.0);
    controls.set_blue(205.0);
    controls.set_color(slint::Color::from_rgb_u8(58, 118, 205));
    controls.set_hex("#3A76CD".into());
    controls.set_target("蒙版".into());
    editor.set_canvas_layers(model(vec![
        CanvasLayerData {
            id: 1,
            name: "主体与阴影".into(),
            group: true,
            expanded: true,
            opacity: 1.0,
            ..Default::default()
        },
        CanvasLayerData {
            id: 2,
            name: "主体".into(),
            depth: 1,
            active: true,
            masked: true,
            mask_paint: true,
            mask_enabled: true,
            opacity: 1.0,
            ..Default::default()
        },
        CanvasLayerData {
            id: 3,
            name: "底色".into(),
            depth: 1,
            opacity: 0.75,
            ..Default::default()
        },
    ]));
    app.global::<CanvasThumbs>().set_layer_images(model(vec![
        Image::default(),
        artwork(48),
        artwork(120),
    ]));
    app.global::<CanvasThumbs>().set_mask_images(model(vec![
        Image::default(),
        mask_artwork(),
        Image::default(),
    ]));
    capture(&app, directory, "canvas-layers-mask-narrow", 900, 640)?;
    capture(&app, directory, "canvas-layers-mask-wide", 1400, 900)?;
    controls.set_picking(true);
    capture(&app, directory, "canvas-color-picker-narrow", 900, 640)?;
    controls.set_picking(false);
    controls.set_rgb_expanded(true);
    capture(&app, directory, "canvas-rgb-expanded-narrow", 900, 640)?;
    capture(&app, directory, "canvas-rgb-expanded-wide", 1400, 900)?;
    controls.set_rgb_expanded(false);
    controls.set_target("图层".into());

    app.set_canvas_open_menu(true);
    capture(&app, directory, "canvas-open-menu-wide", 1400, 900)?;
    app.set_canvas_open_menu(false);
    state.set_page(5);
    click_fixture(&app, 1400, 900, 1340.0, 252.0)?;
    if state.get_personal_menu() == 0 {
        return Err(slint::PlatformError::Other(
            "Wide asset menu did not open".into(),
        ));
    }
    capture(&app, directory, "personal-use-menu-wide", 1400, 900)?;
    state.set_personal_menu(0);
    click_fixture(&app, 900, 640, 840.0, 316.0)?;
    if state.get_personal_menu() == 0 {
        return Err(slint::PlatformError::Other(
            "Narrow asset menu did not open".into(),
        ));
    }
    capture(&app, directory, "personal-manage-menu-narrow", 900, 640)?;
    state.set_personal_menu(0);
    state.set_personal_selection_mode(true);
    let mut selected_assets = assets.clone();
    selected_assets[0].selected = true;
    state.set_personal_selected_count(1);
    state.set_personal_groups(model(vec![
        PersonalAssetGroup {
            date_label: "2026-09-21".into(),
            items: model(selected_assets[..5].to_vec()),
        },
        PersonalAssetGroup {
            date_label: "2026-09-20".into(),
            items: model(selected_assets[5..].to_vec()),
        },
    ]));
    capture(&app, directory, "personal-batch-narrow", 900, 640)?;
    state.set_personal_selection_mode(false);
    state.set_page(7);
    settle(&app)?;
    state.set_template_editor_open(true);
    state.set_template_editor_current(true);
    state.set_template_name("暖色几何海报".into());
    state.set_template_prompt(state.get_prompt());
    capture(&app, directory, "template-save-wide", 1400, 900)?;
    capture(&app, directory, "template-save-narrow", 900, 640)?;
    state.set_template_editor_open(false);
    state.set_page(1);
    settle(&app)?;
    state.set_generation_step(1);
    capture(&app, directory, "generation-results-narrow", 900, 640)?;
    state.set_generation_step(0);
    state.set_generation_parameters_open(true);
    capture(&app, directory, "generation-parameters-wide", 1400, 900)?;
    capture(&app, directory, "generation-parameters-narrow", 900, 640)?;
    state.set_generation_parameters_open(false);
    state.set_media_preview_source("task".into());
    state.set_media_preview_item(results[0].clone());
    state.set_media_preview_open(true);
    capture(&app, directory, "generation-preview-wide", 1400, 900)?;
    capture(&app, directory, "generation-preview-narrow", 900, 640)?;
    state.set_media_preview_item(results[2].clone());
    capture(&app, directory, "generation-video-preview-narrow", 900, 640)?;
    state.set_media_preview_open(false);
    state.set_selected_task(5);
    state.set_result_preview_open(true);
    capture(
        &app,
        directory,
        "generation-failure-detail-narrow",
        900,
        640,
    )?;
    state.set_result_preview_open(false);
    state.set_selected_task(0);
    state.set_mode(1);
    state.set_model_names(model(vec!["演示视频模型 · 多模态参考".into()]));
    state.set_resolutions(model(vec!["720p".into(), "1080p".into()]));
    state.set_quantities(model(vec!["1 个".into()]));
    state.set_generate_audio(true);
    state.set_reference_max(9);
    state.set_reference_limit("最多 9 项参考素材".into());
    let mut nine_refs = assets.clone();
    nine_refs.push(assets[0].clone());
    for (i, reference) in nine_refs.iter_mut().enumerate() {
        reference.id = format!("reference-{i}").into();
        reference.kind = if i == 1 {
            "video"
        } else if i == 2 {
            "audio"
        } else {
            "image"
        }
        .into();
        if i == 2 {
            reference.preview = Image::default();
        }
        reference.detail = if i == 1 {
            "@[视频1]".into()
        } else if i == 2 {
            "@[音频1]".into()
        } else {
            format!("@[图片{}]", if i == 0 { 1 } else { i - 1 }).into()
        };
        reference.status = "已上传".into();
    }
    state.set_references(model(nine_refs.clone()));
    state.set_durations(model(
        (4..=15)
            .map(|seconds| format!("{seconds} 秒").into())
            .collect(),
    ));
    state.set_duration_index(1);
    state.set_supports_generation_audio(true);
    capture(&app, directory, "generation-video-nine-wide", 1400, 900)?;
    capture(&app, directory, "generation-video-nine-narrow", 900, 640)?;
    let mut missing_refs = nine_refs.clone();
    missing_refs[1].missing = true;
    missing_refs[1].ready = false;
    missing_refs[1].preview = Image::default();
    missing_refs[1].status = "缺少文件，可替换".into();
    state.set_references(model(missing_refs));
    state.set_recovery_warning("历史素材已丢失，请替换缺失项后继续。引用顺序已保留。".into());
    capture(
        &app,
        directory,
        "generation-missing-reference-narrow",
        900,
        640,
    )?;
    state.set_model_index(-1);
    state.set_recovery_warning("历史模型已不可用，请选择模型并替换缺失素材。".into());
    capture(&app, directory, "generation-missing-model-narrow", 900, 640)?;
    state.set_model_index(0);
    state.set_recovery_warning("".into());
    state.set_references(model(nine_refs));
    state.set_generation_parameters_open(true);
    capture(
        &app,
        directory,
        "generation-video-parameters-narrow",
        900,
        640,
    )?;
    state.set_generation_parameters_open(false);
    state.set_mode(0);
    state.set_model_names(model(vec!["演示图像模型".into()]));
    state.set_resolutions(model(vec!["1024".into(), "2048".into()]));
    state.set_quantities(model(vec!["1 张".into(), "2 张".into()]));
    state.set_durations(model(vec![]));
    state.set_supports_generation_audio(false);
    state.set_references(model(vec![]));
    state.set_reference_max(4);
    state.set_reference_limit("最多 4 张参考图片".into());
    state.set_tasks(model(vec![]));
    state.set_selected_task(-1);
    capture(&app, directory, "generation-empty-wide", 1400, 900)?;
    state.set_signed_in(false);
    state.set_model_names(model(vec![]));
    state.set_references(model(vec![]));
    for (width, height, suffix) in [(1400, 900, "wide"), (900, 640, "narrow")] {
        capture(
            &app,
            directory,
            &format!("generation-signedout-{suffix}"),
            width,
            height,
        )?;
    }
    state.set_auth_open(true);
    capture(&app, directory, "login-modal-narrow", 900, 640)?;
    capture(&app, directory, "login-modal-wide", 1400, 900)?;
    app.global::<Theme>().set_dark(false);
    capture(&app, directory, "login-modal-light-narrow", 900, 640)?;
    app.global::<Theme>().set_dark(false);
    state.set_auth_open(false);
    state.set_signed_in(true);
    state.set_error("模型加载失败，请重试。".into());
    for (width, height, suffix) in [(1400, 900, "wide"), (900, 640, "narrow")] {
        capture(
            &app,
            directory,
            &format!("generation-model-error-{suffix}"),
            width,
            height,
        )?;
    }
    // Window-level overlays are intentionally captured away from generation
    // and canvas. This catches dialogs accidentally scoped to their old page.
    state.set_error("".into());
    state.set_page(5);
    settle(&app)?;
    editor.set_canvas_modified(true);
    editor.set_canvas_exit_confirm(true);
    capture(&app, directory, "canvas-exit-from-assets-narrow", 900, 640)?;
    editor.set_canvas_exit_error("工程保存失败，请检查目标路径后重试。".into());
    capture(&app, directory, "canvas-exit-save-error-narrow", 900, 640)?;
    editor.set_canvas_exit_confirm(false);
    editor.set_canvas_exit_error("".into());
    for (source, item) in [
        ("personal", assets[0].clone()),
        ("asset", assets[0].clone()),
        ("reference", assets[1].clone()),
    ] {
        state.set_media_preview_source(source.into());
        state.set_media_preview_item(item);
        state.set_media_preview_open(true);
        capture(
            &app,
            directory,
            &format!("{source}-preview-narrow"),
            900,
            640,
        )?;
        state.set_media_preview_open(false);
    }
    state.set_media_preview_source("asset".into());
    state.set_media_preview_item(assets[0].clone());
    state.set_media_preview_open(true);
    state.set_media_preview_loading(true);
    capture(&app, directory, "asset-preview-loading-narrow", 900, 640)?;
    state.set_media_preview_loading(false);
    state.set_media_preview_error("素材读取失败，请重试。".into());
    capture(&app, directory, "asset-preview-error-narrow", 900, 640)?;
    state.set_media_preview_open(false);
    state.set_media_preview_error("".into());
    // Long real-world names exercise card elision and the shared viewer title.
    let mut long_assets = assets.clone();
    long_assets[0].name = "2026秋冬新品发布_设计方案第十二版_客户确认后再次修改_保留全部细节与透明背景_超长文件名验证最终版.png".into();
    long_assets[0].detail = "/Users/creator/Movies/SeeCut/客户项目/2026秋冬新品发布/设计方案第十二版/客户确认后再次修改/保留全部细节与透明背景/最终交付/超长素材文件.png".into();
    state.set_assets(model(long_assets.clone()));
    state.set_page(2);
    capture(&app, directory, "team-long-name-narrow", 900, 640)?;
    state.set_assets(model(assets.clone()));
    state.set_page(5);
    state.set_personal_groups(model(vec![PersonalAssetGroup {
        date_label: "2026-09-21".into(),
        items: model(long_assets.clone()),
    }]));
    capture(&app, directory, "personal-long-name-narrow", 900, 640)?;
    state.set_media_preview_source("personal".into());
    state.set_media_preview_item(long_assets[0].clone());
    state.set_media_preview_open(true);
    capture(&app, directory, "asset-preview-long-name-narrow", 900, 640)?;
    state.set_media_preview_item(CloudItem {
        id: "synthetic-audio".into(),
        name: "城市夜景氛围音乐_无对白_立体声.wav".into(),
        detail: "音频 · 本地导入 · 48 kHz · 立体声 · 02:36".into(),
        kind: "audio".into(),
        ready: true,
        local: true,
        ..Default::default()
    });
    capture(&app, directory, "audio-preview-narrow", 900, 640)?;
    state.set_media_preview_open(false);
    state.set_personal_groups(model(vec![
        PersonalAssetGroup {
            date_label: "2026-09-21".into(),
            items: model(assets[..5].to_vec()),
        },
        PersonalAssetGroup {
            date_label: "2026-09-20".into(),
            items: model(assets[5..].to_vec()),
        },
    ]));
    let mut selected_assets = assets.clone();
    selected_assets[0].selected = true;
    selected_assets[2].selected = true;
    state.set_personal_references(model(selected_assets));
    state.set_asset_picker_source(0);
    state.set_asset_picker_purpose("reference".into());
    state.set_asset_picker_selected_count(2);
    state.set_asset_picker_open(true);
    capture(&app, directory, "asset-picker-multiple-narrow", 900, 640)?;
    let weak = app.as_weak();
    editor.on_canvas_exit_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<Editor>().set_canvas_exit_confirm(false);
        }
    });
    app.set_canvas_open_menu(true);
    settle(&app)?;
    editor.set_canvas_exit_confirm(true);
    capture(&app, directory, "canvas-exit-over-picker-narrow", 900, 640)?;
    if app.get_canvas_open_menu() {
        return Err(slint::PlatformError::Other(
            "Exit confirmation must dismiss the canvas menu".into(),
        ));
    }
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Escape.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Escape.into(),
        });
    if editor.get_canvas_exit_confirm() || !state.get_asset_picker_open() {
        return Err(slint::PlatformError::Other(
            "Escape must cancel exit before the underlying picker".into(),
        ));
    }
    std::fs::write(directory.join("keyboard-validation.json"),
        r#"{"status":"passed","backend":"Slint software fixture","checks":["Exit confirmation dismisses canvas menu","Escape cancels exit and preserves underlying picker"]}"#
    ).map_err(|error| slint::PlatformError::Other(error.to_string()))?;
    state.set_asset_picker_open(false);
    state.set_personal_folder_filter(0);
    crate::cloud::validate_preview_state(&app, directory).map_err(slint::PlatformError::Other)?;
    app.hide()?;
    Ok(())
}
