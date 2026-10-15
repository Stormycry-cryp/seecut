//! 在 Slint 已有的桌面窗口中承载完整 Flow 网页。

use crate::ui::App;

pub(crate) fn configure(app: &App, endpoint: Result<String, String>) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    native::configure(app, endpoint);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = endpoint;
        app.set_flow_host_error("内嵌 Flow 当前支持 macOS 和 Windows 桌面端。".into());
    }
}

pub(crate) fn sync() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    native::sync();
}

pub(crate) fn evaluate(script: &str) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        native::evaluate(script)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = script;
        false
    }
}

pub(crate) fn shutdown() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    native::shutdown();
}

/// 返回 true 表示网页仍在保存，调用方应暂缓原有关闭流程。
pub(crate) fn defer_close(app: &App) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        native::defer_close(app)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::*;
    use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
    use std::cell::RefCell;
    use wry::{Rect, WebView, WebViewBuilder, dpi::PhysicalPosition, dpi::PhysicalSize};

    #[derive(Default)]
    struct Host {
        url: Option<String>,
        webview: Option<WebView>,
        #[cfg(target_os = "macos")]
        overlay: Option<objc2::rc::Retained<crate::toonflow_overlay_macos::FlowContainer>>,
        attempted: bool,
        visible: bool,
        assistant_open: bool,
        bounds: Option<[i32; 4]>,
        dark: Option<bool>,
        zoom: Option<f64>,
        loaded: bool,
        app_ready: bool,
        pending_close: Option<String>,
        close_permit: bool,
    }

    thread_local! {
        // WKWebView/WebView2 的创建、更新与销毁都留在现有 UI 线程。
        static HOST: RefCell<Host> = RefCell::new(Host::default());
    }

    pub(super) fn configure(app: &App, endpoint: Result<String, String>) {
        app.on_flow_close_cancel({
            let weak = app.as_weak();
            move || {
                if let Some(app) = weak.upgrade() {
                    app.set_flow_close_error("".into());
                }
            }
        });
        app.on_flow_close_retry({
            let weak = app.as_weak();
            move || {
                if let Some(app) = weak.upgrade() {
                    app.set_flow_close_error("".into());
                    app.invoke_titlebar_close();
                }
            }
        });
        let endpoint = endpoint.and_then(|value| {
            let url = url::Url::parse(&value).map_err(|e| format!("Flow 地址无效：{e}"))?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.port().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Flow 地址需要使用带端口的本机回环地址。".into());
            }
            Ok(value)
        });
        match endpoint {
            Ok(url) => HOST.with(|host| host.borrow_mut().url = Some(url)),
            Err(error) => {
                log::warn!("Flow service: {error}");
                app.set_flow_host_error(error.into());
            }
        }
    }

    fn theme_script(dark: bool, background: slint::Color) -> String {
        let theme = if dark { "dark" } else { "light" };
        let background = crate::format::hex_of(background);
        format!(
            "window.__SEECUT_EMBEDDED__=true;window.__SEECUT_THEME__='{theme}';\
             if(document.documentElement){{document.documentElement.dataset.seecutTheme='{theme}';\
             document.documentElement.style.colorScheme='{theme}';\
             document.documentElement.style.backgroundColor='{background}';}}\
             window.dispatchEvent(new CustomEvent('seecut:theme',{{detail:{{theme:'{theme}'}}}}));"
        )
    }

    fn bounds(values: [i32; 4]) -> Rect {
        Rect {
            position: PhysicalPosition::new(values[0], values[1]).into(),
            size: PhysicalSize::new(values[2] as u32, values[3] as u32).into(),
        }
    }

    pub(super) fn sync() {
        crate::host::Shell::with(|_, app| {
            let dark = app.global::<crate::ui::Theme>().get_dark();
            let background = app.global::<crate::ui::Theme>().get_page();
            let background_rgba = (
                background.red(),
                background.green(),
                background.blue(),
                background.alpha(),
            );
            app.window().with_winit_window(|window| {
                // Slint 的布局值使用逻辑像素。按 Slint 自身的缩放转换，允许
                // SLINT_SCALE_FACTOR 与显示器默认缩放不同。
                let scale = f64::from(app.window().scale_factor());
                let zoom = scale / window.scale_factor();
                let values = [
                    (f64::from(app.get_flow_host_x()) * scale).round() as i32,
                    (f64::from(app.get_flow_host_y()) * scale).round() as i32,
                    (f64::from(app.get_flow_host_width()) * scale).round() as i32,
                    (f64::from(app.get_flow_host_height()) * scale).round() as i32,
                ];
                let visible = app.get_flow_host_active()
                    && values[2] > 0
                    && values[3] > 0
                    && window.is_visible() != Some(false)
                    && window.is_minimized() != Some(true);
                HOST.with(|state| {
                    let mut host = state.borrow_mut();
                    if host.webview.is_none() && visible && !host.attempted {
                        let Some(url) = host.url.clone() else { return };
                        let origin = url::Url::parse(&url).expect("validated Flow URL").origin();
                        let ipc_origin = origin.clone();
                        host.attempted = true;
                        match WebViewBuilder::new()
                            .with_url(url)
                            .with_bounds(bounds(values))
                            .with_visible(false)
                            .with_background_color(background_rgba)
                            .with_initialization_script(theme_script(dark, background))
                            .with_navigation_handler(move |destination| {
                                url::Url::parse(&destination)
                                    .is_ok_and(|destination| destination.origin() == origin)
                            })
                            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                            .with_ipc_handler(move |request| {
                                if !url::Url::parse(&request.uri().to_string())
                                    .is_ok_and(|url| url.origin() == ipc_origin)
                                {
                                    return;
                                }
                                let Ok(message) =
                                    serde_json::from_str::<HostMessage>(request.body())
                                else {
                                    return;
                                };
                                let _ = slint::invoke_from_event_loop(move || {
                                    handle_host_message(message)
                                });
                            })
                            .with_on_page_load_handler(|event, _| {
                                let loaded = matches!(event, wry::PageLoadEvent::Finished);
                                let _ = slint::invoke_from_event_loop(move || {
                                    if loaded {
                                        refresh_theme();
                                    }
                                    HOST.with(|host| {
                                        let mut host = host.borrow_mut();
                                        host.loaded = loaded;
                                        if !loaded {
                                            host.app_ready = false;
                                        }
                                    });
                                    sync();
                                });
                            })
                            .build_as_child(window)
                        {
                            Ok(webview) => {
                                #[cfg(target_os = "macos")]
                                {
                                    host.overlay =
                                        crate::toonflow_overlay_macos::FlowContainer::attach(
                                            &webview,
                                        );
                                }
                                host.webview = Some(webview);
                                host.bounds = Some(values);
                                host.dark = Some(dark);
                                log::info!("Flow WebView attached to the SeeCut window");
                            }
                            Err(error) => {
                                log::error!("Flow WebView: {error}");
                                app.set_flow_host_error(
                                    format!("无法创建内嵌浏览器：{error}").into(),
                                );
                            }
                        }
                    }
                    let Some(webview) = host.webview.as_ref() else {
                        return;
                    };
                    if host.bounds != Some(values)
                        && values[2] > 0
                        && values[3] > 0
                        && let Err(error) = webview.set_bounds(bounds(values))
                    {
                        log::warn!("Flow WebView bounds: {error}");
                    }
                    if host.dark != Some(dark) {
                        if let Err(error) = webview.set_background_color(background_rgba) {
                            log::warn!("Flow WebView background: {error}");
                        }
                        if let Err(error) = webview.evaluate_script(&theme_script(dark, background))
                        {
                            log::warn!("Flow WebView theme: {error}");
                        }
                    }
                    if host.zoom != Some(zoom)
                        && let Err(error) = webview.zoom(zoom)
                    {
                        log::warn!("Flow WebView zoom: {error}");
                    }
                    #[cfg(target_os = "macos")]
                    if let Some(overlay) = &host.overlay {
                        let ratio = scale / window.scale_factor();
                        overlay.update(app.get_flow_overlay_visible().then(|| {
                            [
                                f64::from(app.get_flow_overlay_x() - 8.0) * ratio,
                                f64::from(app.get_flow_overlay_y() - 8.0) * ratio,
                                f64::from(app.get_flow_overlay_width() + 16.0) * ratio,
                                f64::from(app.get_flow_overlay_height() + 16.0) * ratio,
                            ]
                        }));
                    }
                    let show = visible && host.loaded && host.app_ready;
                    let assistant_open = app.global::<crate::Assistant>().get_open();
                    if host.visible != show {
                        if let Err(error) = webview.set_visible(show) {
                            log::warn!("Flow WebView visibility: {error}");
                        }
                    }
                    if host.visible != show || host.assistant_open != assistant_open {
                        let _ = if show && !assistant_open {
                            webview.focus()
                        } else {
                            webview.focus_parent()
                        };
                    }
                    host.bounds = Some(values);
                    host.dark = Some(dark);
                    host.zoom = Some(zoom);
                    host.visible = show;
                    host.assistant_open = assistant_open;
                });
            });
        });
    }

    pub(super) fn evaluate(script: &str) -> bool {
        HOST.with(|h| {
            h.borrow()
                .webview
                .as_ref()
                .is_some_and(|w| w.evaluate_script(script).is_ok())
        })
    }
    fn refresh_theme() {
        crate::host::Shell::with(|_, app| {
            HOST.with(|host| {
                if let Some(webview) = &host.borrow().webview {
                    let dark = app.global::<crate::ui::Theme>().get_dark();
                    let background = app.global::<crate::ui::Theme>().get_page();
                    if let Err(error) = webview.evaluate_script(&theme_script(dark, background)) {
                        log::warn!("Flow WebView loaded theme: {error}");
                    }
                }
            });
        });
    }

    #[derive(serde::Deserialize)]
    #[serde(tag = "type", deny_unknown_fields)]
    enum HostMessage {
        #[serde(rename = "seecut:view-ready")]
        ViewReady,
        #[serde(rename = "seecut:flow-context")]
        FlowContext { context: serde_json::Value },
        #[serde(rename = "seecut:flow-result")]
        FlowResult {
            id: String,
            result: serde_json::Value,
            error: Option<String>,
        },
        #[serde(rename = "seecut:close-ready")]
        CloseReady {
            #[serde(rename = "requestId")]
            request_id: String,
        },
        #[serde(rename = "seecut:close-error")]
        CloseError {
            #[serde(rename = "requestId")]
            request_id: String,
            message: String,
        },
    }

    pub(super) fn defer_close(app: &App) -> bool {
        HOST.with(|state| {
            let mut host = state.borrow_mut();
            if host.close_permit {
                host.close_permit = false;
                return false;
            }
            if host.pending_close.is_some() {
                return true;
            }
            if !host.loaded || host.webview.is_none() {
                return false;
            }
            let request_id = uuid::Uuid::new_v4().to_string();
            let script = format!(
                "(async()=>{{const requestId='{}';try{{\
                if(typeof window.__SEECUT_FLUSH__==='function'){{await window.__SEECUT_FLUSH__();}}\
                else{{console.debug('SeeCut: app has no pending save hook');}}\
                window.ipc.postMessage(JSON.stringify({{type:'seecut:close-ready',requestId}}));\
                }}catch(error){{window.ipc.postMessage(JSON.stringify({{type:'seecut:close-error',\
                requestId,message:String(error?.message||error)}}));}}}})();",
                request_id
            );
            host.pending_close = Some(request_id.clone());
            let result = host
                .webview
                .as_ref()
                .expect("checked webview")
                .evaluate_script(&script);
            if let Err(error) = result {
                host.pending_close = None;
                app.set_flow_close_error(format!("无法确认 Flow 已保存：{error}").into());
            } else {
                slint::Timer::single_shot(std::time::Duration::from_secs(10), move || {
                    close_error(
                        &request_id,
                        "Flow 保存确认超时。窗口已保留，请返回检查后重试关闭。",
                    );
                });
            }
            true
        })
    }

    fn close_error(request_id: &str, message: &str) {
        let matched = HOST.with(|state| {
            let mut host = state.borrow_mut();
            if host.pending_close.as_deref() != Some(request_id) {
                return false;
            }
            host.pending_close = None;
            true
        });
        if matched {
            crate::host::Shell::with(|_, app| {
                app.set_flow_close_error(message.chars().take(600).collect::<String>().into());
            });
        }
    }

    fn handle_host_message(message: HostMessage) {
        match message {
            HostMessage::FlowContext { context } => crate::toonflow_agent::update(context),
            HostMessage::FlowResult { id, result, error } => {
                crate::toonflow_agent::complete(id, result, error)
            }
            HostMessage::ViewReady => {
                HOST.with(|host| host.borrow_mut().app_ready = true);
                sync();
            }
            HostMessage::CloseReady { request_id } => {
                let matched = HOST.with(|state| {
                    let mut host = state.borrow_mut();
                    if host.pending_close.as_ref() != Some(&request_id) {
                        return false;
                    }
                    host.pending_close = None;
                    host.close_permit = true;
                    true
                });
                if matched {
                    crate::host::Shell::with(|_, app| {
                        app.set_flow_close_error("".into());
                        app.invoke_titlebar_close();
                    });
                }
            }
            HostMessage::CloseError {
                request_id,
                message,
            } => close_error(&request_id, &format!("Flow 尚未保存：{message}")),
        }
    }

    pub(super) fn shutdown() {
        HOST.with(|host| {
            // 必须先释放子视图，再让 Slint 释放父窗口。
            let mut host = host.borrow_mut();
            host.webview.take();
            *host = Host::default();
        });
    }
}
