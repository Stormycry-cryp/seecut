// SPDX-License-Identifier: AGPL-3.0-or-later
//! App read dispatch, separate from every document and write permission.
use super::*;
use crate::editor_mcp_app_page::{
    AppIdentity, AppPages, Context, Error, FrozenPage, Navigation, PageId,
};
use crate::editor_mcp_app_preview::{self as preview, PreviewPixels};
use concat_editor_mcp::{AppPageId, AppPageQuery, AppReadScope, AppViewBinding};
use std::rc::Rc;
#[path = "editor_mcp_app_policy.rs"]
mod policy;
use policy::{Authority, Grants};

/// Trusted host projection only. Tokens never enter Slint/history/logs.
pub(crate) struct AssistantAppSnapshot {
    pub identity: Option<AppIdentity>,
    pub read_token: Option<String>,
    pub read_status: String,
    pub can_grant_read: bool,
    pub can_revoke_read: bool,
    pub page_label: String,
}

pub(super) struct AppReadState {
    navigation: Rc<Navigation>,
    grants: Grants,
    pages: HashMap<String, AppPages<slint::Image>>,
}
impl Default for AppReadState {
    fn default() -> Self {
        Self {
            navigation: crate::cloud::app_read_navigation(),
            grants: Grants::default(),
            pages: HashMap::new(),
        }
    }
}
impl AppReadState {
    fn authority(&self, instance: &str) -> Result<Authority, Error> {
        let (account_epoch, identity) = crate::cloud::app_read_identity(instance)?;
        Ok(Authority {
            instance_id: identity.instance_id,
            account_epoch,
            account_scope: identity.account_scope,
            app_session_id: identity.app_session_id,
        })
    }
    fn authorized(&self, request: &Request, authority: &Authority, media: bool) -> bool {
        request.instance_id == authority.instance_id
            && request.app_scope.as_ref().is_some_and(|scope| {
                scope.account_scope == authority.account_scope
                    && scope.app_session_id == authority.app_session_id
            })
            && self
                .grants
                .authorized(&request.client_id, &request.client_token, authority, media)
    }
    fn capture(
        &mut self,
        request: &Request,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
    ) -> Result<Context, Error> {
        if !self.pages.contains_key(&request.client_id) {
            if self.pages.len() >= 32 {
                return Err(Error::ResourceLimit);
            }
            self.pages.insert(
                request.client_id.clone(),
                AppPages::with_navigation(
                    request.instance_id.clone(),
                    request.client_id.clone(),
                    self.navigation.clone(),
                ),
            );
        }
        crate::cloud::app_read_snapshot(
            app,
            studio,
            models,
            self.pages.get_mut(&request.client_id).ok_or(Error::Busy)?,
        )
    }
    pub fn clear(&mut self) {
        self.grants.clear();
        self.pages.clear();
    }
}

pub(super) struct AppPreviewJob {
    request: Request,
    authority: Authority,
    pixels: PreviewPixels,
}

fn response(error: Error) -> Response {
    Response::error(error.status())
}
fn json_response(value: impl serde::Serialize) -> Response {
    serde_json::to_value(value)
        .map(Response::ok)
        .unwrap_or_else(|_| Response::error("ioFailure"))
}
fn proto_page(page: PageId) -> AppPageId {
    match page {
        PageId::Generation => AppPageId::Generation,
        PageId::PersonalLibrary => AppPageId::PersonalLibrary,
        PageId::TeamLibrary => AppPageId::TeamLibrary,
        PageId::Templates => AppPageId::Templates,
        PageId::Account => AppPageId::Account,
        PageId::Credits => AppPageId::Credits,
        PageId::Settings => AppPageId::Settings,
        PageId::Projects => AppPageId::Projects,
    }
}
fn local_page(page: AppPageId) -> PageId {
    match page {
        AppPageId::Generation => PageId::Generation,
        AppPageId::PersonalLibrary => PageId::PersonalLibrary,
        AppPageId::TeamLibrary => PageId::TeamLibrary,
        AppPageId::Templates => PageId::Templates,
        AppPageId::Account => PageId::Account,
        AppPageId::Credits => PageId::Credits,
        AppPageId::Settings => PageId::Settings,
        AppPageId::Projects => PageId::Projects,
    }
}
pub(super) fn current_page(studio: &Studio, app: &App) -> Option<PageId> {
    if studio.settings.open {
        return Some(PageId::Settings);
    }
    let ui = app.global::<SeeCut>();
    match ui.get_page() {
        1 => Some(PageId::Generation),
        2 => Some(PageId::TeamLibrary),
        3 => Some(PageId::Credits),
        4 => Some(PageId::Account),
        5 => Some(PageId::PersonalLibrary),
        7 => Some(PageId::Templates),
        0 if studio.on_start => Some(PageId::Projects),
        6 if ui.get_canvas_gallery_open() => Some(PageId::Projects),
        _ => None,
    }
}
fn page_label(page: PageId) -> &'static str {
    match page {
        PageId::Generation => "生成",
        PageId::PersonalLibrary => "个人素材",
        PageId::TeamLibrary => "团队素材",
        PageId::Templates => "模板",
        PageId::Account => "账户",
        PageId::Credits => "积分",
        PageId::Settings => "设置",
        PageId::Projects => "项目列表",
    }
}

impl BridgeUi {
    pub(crate) fn assistant_app_snapshot(
        &mut self,
        studio: &Studio,
        app: &App,
    ) -> AssistantAppSnapshot {
        let page = current_page(studio, app);
        let current = self
            .app_reads
            .authority(&self.instance_id)
            .ok()
            .filter(|_| page.is_some());
        let readable = crate::cloud::app_read_gate(studio, app).is_ok();
        let token = current
            .as_ref()
            .filter(|_| readable)
            .and_then(|authority| self.app_reads.grants.token(ASSISTANT_CLIENT_ID, authority));
        AssistantAppSnapshot {
            identity: current.map(|a| AppIdentity {
                instance_id: a.instance_id,
                account_scope: a.account_scope,
                app_session_id: a.app_session_id,
            }),
            read_status: if !readable && page.is_some() {
                "当前对话框或账户操作期间暂停读取"
            } else if token.is_some() {
                "已允许读取 App 页面 · 当前用户会话有效"
            } else {
                "尚未允许读取 App 页面"
            }
            .into(),
            read_token: token,
            can_grant_read: readable
                && page.is_some()
                && crate::cloud::app_read_identity(&self.instance_id).is_ok(),
            can_revoke_read: self.app_reads.grants.has(ASSISTANT_CLIENT_ID),
            page_label: page.map(page_label).unwrap_or("").into(),
        }
    }
    pub(crate) fn grant_app_read(
        &mut self,
        studio: &Studio,
        app: &App,
        expected: &AppIdentity,
        media: bool,
    ) -> Result<(), AssistantPermissionError> {
        if current_page(studio, app).is_none() || crate::cloud::app_read_gate(studio, app).is_err()
        {
            return Err(AssistantPermissionError::NotAllowed);
        }
        let authority = self
            .app_reads
            .authority(&self.instance_id)
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        if authority.instance_id != expected.instance_id
            || authority.account_scope != expected.account_scope
            || authority.app_session_id != expected.app_session_id
        {
            return Err(AssistantPermissionError::StaleContext);
        }
        self.app_reads
            .grants
            .grant(ASSISTANT_CLIENT_ID, authority, media)
            .map_err(|_| AssistantPermissionError::NotAllowed)
    }
    pub(crate) fn revoke_app_read(&mut self) {
        self.app_reads.grants.revoke(ASSISTANT_CLIENT_ID);
        self.app_reads.pages.remove(ASSISTANT_CLIENT_ID);
    }
    pub(crate) fn revoke_all_app_reads(&mut self) {
        self.app_reads.clear();
    }
    pub(crate) fn assistant_app_view(
        &mut self,
        studio: &Studio,
        app: &App,
        models: &crate::studio::Models,
    ) -> Result<AppViewBinding, AssistantPermissionError> {
        let authority = self
            .app_reads
            .authority(&self.instance_id)
            .map_err(|_| AssistantPermissionError::NotAllowed)?;
        let token = self
            .app_reads
            .grants
            .token(ASSISTANT_CLIENT_ID, &authority)
            .ok_or(AssistantPermissionError::NotAllowed)?;
        let mut request:Request=serde_json::from_value(json!({"instanceId":self.instance_id,"clientId":ASSISTANT_CLIENT_ID,"clientToken":token,
            "method":"appContext","appScope":{"accountScope":authority.account_scope,"appSessionId":authority.app_session_id},"appPage":{"appSessionId":authority.app_session_id}}))
            .map_err(|_|AssistantPermissionError::NotAllowed)?;
        // The request is internal host construction, never user/model arguments.
        request.app_scope = Some(AppReadScope {
            account_scope: authority.account_scope,
            app_session_id: authority.app_session_id,
        });
        let context = self
            .app_reads
            .capture(&request, studio, app, models)
            .map_err(|_| AssistantPermissionError::StaleContext)?;
        Ok(AppViewBinding {
            page_id: proto_page(context.page.page_id),
            page_epoch: context.page.page_epoch,
            page_state_id: context.page.page_state_id,
        })
    }
}

pub(super) fn handle(
    bridge: &mut BridgeUi,
    request: &Request,
    studio: &Studio,
    app: &App,
    models: &crate::studio::Models,
) -> UiReply {
    if concat_editor_mcp::validate_request_shape(request).is_err() {
        return UiReply::Response(Response::error("invalidInput"));
    }
    let state = &mut bridge.app_reads;
    let authority = match state.authority(&bridge.instance_id) {
        Ok(value) => value,
        Err(error) => return UiReply::Response(response(error)),
    };
    let media = request.method == Method::PagePreview;
    if !state.authorized(request, &authority, media) {
        return UiReply::Response(Response::error("notAuthorized"));
    }
    if request.method == Method::Capabilities {
        let mut granted = vec!["R"];
        if state.grants.media(&request.client_id, &authority) {
            granted.push("M");
        }
        return UiReply::Response(Response::ok(
            json!({"scopeKind":"app","appReadProtocolVersion":1,"appInstanceId":authority.instance_id,"clientId":request.client_id,
            "accountScope":authority.account_scope,"appSessionId":authority.app_session_id,"tools":["capabilities","app_context","page_items","page_preview"],
            "permissions":["R","M"],"grantedPermissions":granted,"readOnly":true,"readLifetime":"userSession","maxPageSize":50,"maxPageItems":2048,
            "maxPageSnapshotBytes":524288,"maxReadbackBytes":49152,"maxPreviewEdge":256,"maxPreviewSourcePixels":4194304}),
        ));
    }
    let context = match state.capture(request, studio, app, models) {
        Ok(value) => value,
        Err(error) => return UiReply::Response(response(error)),
    };
    let registry = match state.pages.get(&request.client_id) {
        Some(value) => value,
        None => return UiReply::Response(Response::error("busy")),
    };
    match request.app_page.as_ref() {
        Some(AppPageQuery::Context(_)) if request.method == Method::AppContext => {
            UiReply::Response(json_response(context))
        }
        Some(AppPageQuery::Items(query)) if request.method == Method::PageItems => {
            let frozen = FrozenPage {
                app_session_id: query.app_session_id.clone(),
                page_id: local_page(query.page_id),
                page_epoch: query.page_epoch,
                page_state_id: query.page_state_id.clone(),
            };
            UiReply::Response(
                match registry.page_items(&frozen, query.offset, query.limit) {
                    Ok(value) => json_response(value),
                    Err(error) => response(error),
                },
            )
        }
        Some(AppPageQuery::Preview(query)) if request.method == Method::PagePreview => {
            let frozen = FrozenPage {
                app_session_id: query.app_session_id.clone(),
                page_id: local_page(query.page_id),
                page_epoch: query.page_epoch,
                page_state_id: query.page_state_id.clone(),
            };
            match preview::capture_pixels(
                registry,
                frozen,
                query.item_id.clone(),
                query.preview_version,
            ) {
                Ok(pixels) => UiReply::AppPreview(Box::new(AppPreviewJob {
                    request: request.clone(),
                    authority,
                    pixels,
                })),
                Err(error) => UiReply::Response(response(error)),
            }
        }
        _ => UiReply::Response(Response::error("invalidInput")),
    }
}

pub(super) fn dispatch_preview(job: AppPreviewJob) -> Response {
    let Ok(_slot) = preview_slot().try_lock() else {
        return Response::error("busy");
    };
    let encoded = match preview::encode(&job.pixels) {
        Ok(value) => value,
        Err(error) => return response(error),
    };
    on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return Response::error("busy");
        };
        let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
            return Response::error("busy");
        };
        let authority = match bridge.app_reads.authority(&bridge.instance_id) {
            Ok(value) => value,
            Err(error) => return response(error),
        };
        if authority != job.authority
            || !bridge.app_reads.authorized(&job.request, &authority, true)
        {
            return Response::error("notAuthorized");
        }
        if let Err(error) = bridge
            .app_reads
            .capture(&job.request, &studio, &app, &shell.models)
        {
            return response(error);
        }
        let Some(registry) = bridge.app_reads.pages.get(&job.request.client_id) else {
            return Response::error("busy");
        };
        match preview::finish(registry, &job.pixels, encoded) {
            Ok(value) => Response::ok(value),
            Err(error) => response(error),
        }
    })
    .unwrap_or_else(|error| error)
}
