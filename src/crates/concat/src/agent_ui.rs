// SPDX-License-Identifier: AGPL-3.0-or-later
//! Managed assistant entry plus an explicitly enabled local development runtime.
//! The default desktop host uses the signed-in account and bundled components.

use crate::ui::{App, Assistant};
use slint::ComponentHandle;
use std::rc::Rc;

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
pub(crate) use desktop::Binding;

/// Bind once after Shell::install and keep the returned guard until app.run returns.
pub(crate) fn bind(app: &App, account: crate::cloud::AccountAccess) -> Rc<Binding> {
    bind_geometry(app);
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    {
        desktop::bind(app, account)
    }
    #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
    {
        let _ = account;
        let ui = app.global::<Assistant>();
        ui.set_enabled(true);
        ui.set_developer_tools(false);
        ui.set_settings_open(false);
        ui.set_can_send(false);
        ui.set_can_connect(false);
        ui.set_can_select_files(false);
        ui.set_connection("Agent 服务暂未接入".into());
        // The managed service is not connected yet. Do not bind the local-provider
        // callbacks or create a controller, polling timer, process, or credential.
        ui.on_close_requested({
            let app = app.as_weak();
            move || {
                if let Some(app) = app.upgrade() {
                    let ui = app.global::<Assistant>();
                    ui.set_open(false);
                    ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
                }
            }
        });
        Rc::new(Binding)
    }
}

#[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
pub(crate) struct Binding;

#[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
impl Binding {
    pub(crate) fn shutdown(&self) {}
}

fn bind_geometry(app: &App) {
    use crate::host::Shell;
    let ui = app.global::<Assistant>();
    Shell::with(|shell, _| {
        let studio = shell.studio.borrow();
        if let Some(geometry) = studio
            .prefs
            .assistant_geometry
            .as_ref()
            .filter(|g| g.valid())
        {
            ui.set_float_x(geometry.x);
            ui.set_float_y(geometry.y);
            ui.set_float_width(geometry.width);
            ui.set_float_height(geometry.height);
            ui.set_geometry_set(true);
        }
    });
    ui.on_geometry_changed(|x, y, width, height| {
        let geometry = crate::prefs::AssistantGeometry {
            x,
            y,
            width,
            height,
        };
        if geometry.valid() {
            Shell::with(|shell, _| {
                let mut studio = shell.studio.borrow_mut();
                studio.prefs.assistant_geometry = Some(geometry.clone());
                studio.prefs.save(&studio.host.dirs);
            });
        }
    });
}

#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
mod desktop {
    use super::*;
    use crate::agent_controller::{
        ConnectionConfig, Controller, DocumentIdentity, Phase, TaskCommand, TaskOutcome, UiError,
    };
    use crate::agent_identity::AgentScope;
    use crate::agent_process::LaunchSpec;
    use crate::agent_sessions::{
        self, AppReference, AppReferenceScope, Archive, ContextReference, DocumentReference,
        HistoryScope, Message, Record, Reference, RequestSnapshot, Store,
    };
    use crate::editor_mcp::{
        ASSISTANT_CLIENT_ID, AssistantAppSnapshot, AssistantPermission, AssistantPermissionError,
        AssistantSnapshot,
    };
    use crate::host::{Shell, spawn_detached};
    use crate::ui::AssistantMessage;
    use serde_json::json;
    use slint::{Model, ModelRc, Timer, TimerMode, VecModel};
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, HashMap};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    // Leave room for one complete 32 KiB input and bounded streaming reply.
    const NEW_TURN_HISTORY_LIMIT: usize = 128 * 1024;

    // The native bridge chooses one real current scope. Credentials stay outside Slint/history.
    struct ScopeSnapshot {
        scope: Option<AgentScope>,
        project_name: String,
        project_key: String,
        read_token: Option<String>,
        write_token: Option<String>,
        app_control_token: Option<String>,
        read_status: String,
        write_status: String,
        can_grant_read: bool,
        can_revoke_read: bool,
        can_grant_write: bool,
        can_revoke_write: bool,
    }
    impl ScopeSnapshot {
        fn choose(document: AssistantSnapshot, app: AssistantAppSnapshot) -> Self {
            if let Some(identity) = app.identity {
                Self {
                    scope: Some(AgentScope::App(identity)),
                    project_name: app.page_label,
                    project_key: String::new(),
                    read_token: app.read_token,
                    write_token: None,
                    app_control_token: app.control_token,
                    read_status: app.read_status,
                    write_status: app.control_status,
                    can_grant_read: app.can_grant_read,
                    can_revoke_read: app.can_revoke_read,
                    can_grant_write: app.can_grant_control,
                    can_revoke_write: app.can_revoke_control,
                }
            } else {
                Self {
                    scope: document.document.map(AgentScope::Document),
                    project_name: document.project_name,
                    project_key: document.project_key,
                    read_token: document.read_token,
                    write_token: document.write_token,
                    app_control_token: None,
                    read_status: document.read_status,
                    write_status: document.write_status,
                    can_grant_read: document.can_grant_read,
                    can_revoke_read: document.can_revoke_read,
                    can_grant_write: document.can_grant_write,
                    can_revoke_write: document.can_revoke_write,
                }
            }
        }
        fn document(&self) -> Option<&DocumentIdentity> {
            self.scope.as_ref().and_then(AgentScope::document)
        }
        fn is_app(&self) -> bool {
            self.scope
                .as_ref()
                .is_some_and(|scope| scope.app().is_some())
        }
        fn history_scope(&self) -> Option<HistoryScope> {
            match self.scope.as_ref()? {
                AgentScope::Document(_) if !self.project_key.is_empty() => {
                    Some(HistoryScope::Document(self.project_key.clone()))
                }
                AgentScope::App(app) => Some(HistoryScope::App(app.account_scope.clone())),
                _ => None,
            }
        }
        fn reference(&self) -> Option<ContextReference> {
            match self.scope.as_ref()? {
                AgentScope::Document(_) => Some(ContextReference::Document(DocumentReference {
                    project_key: self.project_key.clone(),
                    project_name: self.project_name.clone(),
                })),
                AgentScope::App(app) => Some(ContextReference::App(AppReference {
                    app_scope: AppReferenceScope {
                        instance_id: app.instance_id.clone(),
                        account_scope: app.account_scope.clone(),
                        app_session_id: app.app_session_id.clone(),
                    },
                    page_name: self.project_name.clone(),
                })),
            }
        }
    }

    // Never Debug/serialize this: comparison includes real lease tokens.
    #[derive(Clone, PartialEq, Eq)]
    struct Lease {
        scope: Option<AgentScope>,
        read: Option<String>,
        write: Option<String>,
        app_control: Option<String>,
    }
    impl Lease {
        fn from_snapshot(snapshot: &ScopeSnapshot) -> Self {
            Self {
                scope: snapshot.scope.clone(),
                read: snapshot.read_token.clone(),
                write: snapshot.write_token.clone(),
                app_control: snapshot.app_control_token.clone(),
            }
        }
    }
    fn lease_change_notice(old: &Lease, next: &Lease) -> &'static str {
        if old.scope != next.scope {
            if old
                .scope
                .as_ref()
                .is_some_and(|scope| scope.app().is_some())
                || next
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.app().is_some())
            {
                return "账户或页面会话已变化，请重新授权并恢复会话";
            }
            return "工程已变化，请重新连接；旧确认不再有效";
        }
        if old.read != next.read {
            return "读取权限已变化，请检查授权并重新连接；旧确认不再有效";
        }
        if next.write.is_some() {
            "已允许页面操作，当前会话有效"
        } else {
            "已切换为允许读取；页面操作与旧确认已撤销"
        }
    }

    #[cfg(feature = "assistant-local-dev")]
    #[derive(Clone, Copy)]
    enum FileKind {
        Runtime,
        Entrypoint,
        McpClient,
    }
    enum WorkerResult {
        Models {
            session_id: String,
            generation: u64,
            account_epoch: u64,
            ticket: u64,
            result: Result<crate::agent_models::Directory, &'static str>,
        },
        #[cfg(feature = "assistant-local-dev")]
        File {
            session_id: String,
            epoch: u64,
            kind: FileKind,
            path: Option<Result<PathBuf, &'static str>>,
        },
        Prepared {
            session_id: String,
            epoch: u64,
            account_epoch: Option<u64>,
            config: Result<Box<ConnectionConfig>, &'static str>,
        },
    }

    #[derive(Clone, PartialEq)]
    struct FrozenApproval {
        tool: String,
        arguments: serde_json::Value,
        id: String,
        turn_id: String,
        object_id: Option<String>,
        delta_x: Option<f64>,
        delta_y: Option<f64>,
    }
    struct State {
        record: Record,
        generation: u64,
        submitting: bool,
        messages: Rc<VecModel<AssistantMessage>>,
        controller: Controller,
        lease: Option<Lease>,
        runtime: Option<PathBuf>,
        entrypoint: Option<PathBuf>,
        mcp_client: Option<PathBuf>,
        selecting: bool,
        preparing: bool,
        epoch: u64,
        account_epoch: Option<u64>,
        runtime_account_epoch: Option<u64>,
        billing_revision: u64,
        notice: String,
        provider_label: String,
        catalog: crate::agent_models::Catalog,
        assistant_row: Option<usize>,
        approval: Option<FrozenApproval>,
        editor_recovery: Option<crate::agent_controller::EditorRecovery>,
        app_recovery: Option<crate::agent_controller::AppRecovery>,
        task_budget_text: String,
        task_budget_policy: Option<serde_json::Value>,
        task_permits: Vec<(u64, Instant)>,
        task_display_turn: Option<String>,
        task_grant: Option<crate::agent_task_grant::TaskGrant>,
        update_grant: Option<crate::agent_task_grant::UpdateGrant>,
        grant_seen_turn: Option<String>,
        auto_approving: bool,
        auto_attempted: Option<FrozenApproval>,
        task_permit_revision: u64,
    }
    impl State {
        fn task_policy(&self) -> Option<serde_json::Value> {
            let choice = self.catalog.current()?;
            if self.record.selected_effort_id.is_some() {
                self.selected_effort()?.policy_for(&choice.id)
            } else {
                Some(choice.policy.clone())
            }
        }
        fn sync_task_budget(&mut self) {
            let policy = self.task_policy();
            if policy != self.task_budget_policy {
                self.task_budget_text = policy
                    .as_ref()
                    .and_then(crate::agent_models::next_turn_cap)
                    .and_then(crate::agent_tasks::default_task_budget)
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                if self.task_budget_policy.is_some() {
                    self.notice = "模型或强度已变化，请核对已更新的任务积分上限".into();
                }
                self.task_budget_policy = policy;
            }
        }
        fn task_budget(&self) -> Result<u64, &'static str> {
            let cap = self
                .task_budget_policy
                .as_ref()
                .and_then(crate::agent_models::next_turn_cap)
                .ok_or("当前模型的单轮额度暂不可用，请刷新模型")?;
            crate::agent_tasks::parse_task_budget(&self.task_budget_text, cap)
        }
        fn can_select_model(&self) -> bool {
            !cfg!(feature = "assistant-local-dev")
                && !self.catalog.loading
                && self.catalog.metadata_reason().is_empty()
                && !self.preparing
                && !self.submitting
                && (!self.controller.process_present()
                    || self.controller.supports_model_selection())
                && !self.model_indices().is_empty()
        }
        fn model_indices(&self) -> Vec<usize> {
            let effort = self.selected_effort();
            self.catalog
                .choices
                .iter()
                .enumerate()
                .filter(|(_, choice)| {
                    if self.record.selected_effort_id.is_some() {
                        effort.is_some_and(|effort| effort.allows(&choice.id))
                    } else {
                        self.catalog.efforts.is_empty()
                    }
                })
                .map(|(index, _)| index)
                .collect()
        }
        fn selected_effort(&self) -> Option<&crate::agent_models::Effort> {
            let effort = self.catalog.effort(
                self.record.selected_effort_id.as_deref()?,
                self.record.selected_policy_revision.as_deref()?,
            )?;
            let policy = effort.policy_for(self.record.selected_model_id.as_deref()?)?;
            (self.record.selected_effort_policy.as_ref() == Some(&policy)).then_some(effort)
        }
        fn restore_catalog_selection(&mut self) -> bool {
            if !self.catalog.metadata_reason().is_empty() {
                return false;
            }
            let mut initialized_effort = false;
            if !self.record.selection_initialized {
                initialized_effort = self.select_effort_draft(0);
                self.record.selection_initialized = true;
            }
            if let Some(id) = &self.record.selected_model_id {
                if let Some(index) = self
                    .catalog
                    .choices
                    .iter()
                    .position(|choice| &choice.id == id)
                {
                    self.catalog.select(index as i32);
                }
            } else {
                self.record.selected_model_id =
                    self.catalog.current().map(|choice| choice.id.clone());
            }
            self.provider_label = self
                .record
                .selected_model_id
                .as_ref()
                .and_then(|id| self.catalog.choices.iter().find(|choice| &choice.id == id))
                .map(|choice| choice.label.clone())
                .unwrap_or_default();
            initialized_effort
        }
        fn selection_reason(&self) -> &'static str {
            if cfg!(feature = "assistant-local-dev") {
                return "";
            }
            let metadata = self.catalog.metadata_reason();
            if !metadata.is_empty() {
                return metadata;
            }
            if self.record.selected_effort_id.is_some() {
                let Some(effort) = self.selected_effort() else {
                    return "原强度策略已变更，请重新选择";
                };
                if !self
                    .record
                    .selected_model_id
                    .as_deref()
                    .is_some_and(|id| effort.allows(id))
                {
                    return "请选择该强度支持的模型";
                }
                if !effort
                    .model_available(self.record.selected_model_id.as_deref().unwrap_or_default())
                {
                    return "该模型在当前强度暂不可用";
                }
                if !effort.available {
                    return effort.reason();
                }
                if self.controller.view().phase == Phase::Ready
                    && !self.controller.supports_model_selection()
                {
                    return "Agent 组件尚不支持强度选择";
                }
            } else if !self.catalog.efforts.is_empty() {
                return "请选择强度";
            }
            if self
                .record
                .selected_model_id
                .as_ref()
                .is_some_and(|id| !self.catalog.choices.iter().any(|choice| &choice.id == id))
            {
                return "原模型暂不可用，请重新选择";
            }
            self.catalog.reason()
        }
        fn can_select_effort(&self) -> bool {
            !cfg!(feature = "assistant-local-dev")
                && !self.catalog.loading
                && self.catalog.metadata_reason().is_empty()
                && !self.catalog.efforts.is_empty()
                && !self.preparing
                && !self.submitting
        }
        // This changes only the next explicit request. The existing controller,
        // running turn, reconciliation and frozen request history keep ownership.
        fn select_effort_draft(&mut self, index: usize) -> bool {
            let Some(effort) = self.catalog.efforts.get(index).cloned() else {
                return false;
            };
            let model_id = self
                .record
                .selected_model_id
                .as_deref()
                .filter(|id| {
                    effort.allows(id) && self.catalog.choices.iter().any(|choice| choice.id == *id)
                })
                .unwrap_or(&effort.model_id)
                .to_owned();
            let Some(policy) = effort.policy_for(&model_id) else {
                return false;
            };
            let Some(index) = self
                .catalog
                .choices
                .iter()
                .position(|choice| choice.id == model_id)
            else {
                return false;
            };
            self.record.selected_effort_id = Some(effort.id);
            self.record.selected_policy_revision = Some(effort.revision);
            self.record.selected_effort_policy = Some(policy);
            self.record.selected_model_id = Some(model_id);
            self.record.selection_initialized = true;
            self.catalog.selected = index;
            self.provider_label = self.catalog.choices[index].label.clone();
            true
        }
        fn select_model_draft(&mut self, visible_index: usize) -> bool {
            let Some(index) = self.model_indices().get(visible_index).copied() else {
                return false;
            };
            let choice = self.catalog.choices[index].clone();
            let policy = if let Some(effort) = self.selected_effort() {
                let Some(policy) = effort.policy_for(&choice.id) else {
                    return false;
                };
                Some(policy)
            } else {
                None
            };
            self.catalog.selected = index;
            self.record.selected_model_id = Some(choice.id);
            self.record.selected_effort_policy = policy;
            self.record.selection_initialized = true;
            self.provider_label = choice.label;
            true
        }
    }
    /// Owns the controller, stable transcript model and nonblocking UI timer.
    struct Retiring {
        states: Vec<State>,
        store: Option<Store>,
        active_id: String,
    }
    pub(crate) struct Binding {
        timer: Timer,
        account: crate::cloud::AccountAccess,
        components: Option<PathBuf>,
        state: RefCell<State>,
        parked: RefCell<Vec<State>>,
        retiring: RefCell<Vec<Retiring>>,
        scope: RefCell<Option<(String, HistoryScope)>>,
        data: Option<PathBuf>,
        store: RefCell<Option<Store>>,
        history_error: RefCell<Option<&'static str>>,
        last_saved: RefCell<Vec<u8>>,
        last_persist: Cell<Instant>,
        sender: SyncSender<WorkerResult>,
        receiver: Receiver<WorkerResult>,
    }
    impl Drop for Binding {
        fn drop(&mut self) {
            Self::fence_app_control_native(true);
            self.timer.stop();
            self.state.get_mut().controller.shutdown_now();
            for state in self.parked.get_mut() {
                state.controller.shutdown_now();
            }
            for group in self.retiring.get_mut() {
                for state in &mut group.states {
                    state.controller.shutdown_now();
                }
            }
        }
    }
    impl Binding {
        /// Start bounded process shutdown before the app's exit-only reaping hook.
        pub(crate) fn shutdown(&self, app: &App) {
            Self::fence_app_control_native(true);
            self.state.borrow_mut().record.draft =
                app.global::<Assistant>().get_draft().to_string();
            let _ = self.persist(true);
            self.state.borrow_mut().controller.shutdown_now();
            for state in self.parked.borrow_mut().iter_mut() {
                state.controller.shutdown_now();
            }
        }

        fn drain_retiring(&self) {
            self.retiring.borrow_mut().retain_mut(|group| {
                for state in &mut group.states {
                    state.controller.tick(None);
                    capture(state);
                }
                if group
                    .states
                    .iter()
                    .any(|state| state.controller.process_present())
                {
                    return true;
                }
                if let Some(store) = &group.store {
                    let archive = Archive {
                        version: agent_sessions::ARCHIVE_VERSION,
                        active_id: group.active_id.clone(),
                        sessions: group
                            .states
                            .iter()
                            .map(|state| state.record.clone())
                            .collect(),
                    };
                    // The old archive already contains a conservative pending intent.
                    // A failed final checkpoint cannot permit a second runtime.
                    if let Err(error) = store.save(&archive) {
                        *self.history_error.borrow_mut() = Some(error);
                        return true;
                    }
                }
                false
            });
        }
        fn sync_scope(&self, app: &App, snapshot: &ScopeSnapshot) {
            let account = self.account.history_scope().or_else(|| {
                cfg!(feature = "assistant-local-dev").then(|| "local-development".into())
            });
            let next =
                account.and_then(|account| snapshot.history_scope().map(|scope| (account, scope)));
            if *self.scope.borrow() == next
                && self.state.borrow().account_epoch == self.account.epoch()
            {
                return;
            }
            if !self.retiring.borrow().is_empty() {
                return;
            }
            self.state.borrow_mut().record.draft =
                app.global::<Assistant>().get_draft().to_string();
            let _ = self.persist(true);
            let old = std::mem::replace(
                &mut *self.state.borrow_mut(),
                new_state(new_record(), self.account.epoch()),
            );
            let active_id = old.record.id.clone();
            let mut states = vec![old];
            states.append(&mut self.parked.borrow_mut());
            for state in &mut states {
                revoke_task_permits(state);
                state.task_grant = None;
                state.update_grant = None;
                state.catalog.clear();
            }
            let old_store = self.store.borrow_mut().take();
            let needs_retirement = states
                .iter()
                .any(|state| state.controller.process_present());
            if needs_retirement {
                for state in &mut states {
                    state.controller.disconnect();
                    state.preparing = false;
                }
                self.retiring.borrow_mut().push(Retiring {
                    states,
                    store: old_store,
                    active_id,
                });
                *self.scope.borrow_mut() = None;
                self.show_session(app);
                return;
            }
            *self.history_error.borrow_mut() = None;
            self.last_saved.borrow_mut().clear();
            let mut records = Vec::new();
            let mut active = String::new();
            if let Some((account, project)) = &next {
                let loaded = (|| {
                    let data = self.data.as_deref().ok_or("Agent 数据目录不可用")?;
                    let store =
                        Store::open_scope(&session_directory(data)?, account, project.clone())?;
                    let archive = store.load()?;
                    *self.store.borrow_mut() = Some(store);
                    Ok::<_, &'static str>(archive)
                })();
                match loaded {
                    Ok(Some(archive)) => {
                        active = archive.active_id;
                        records = archive.sessions;
                    }
                    Ok(None) => {}
                    Err(message) => *self.history_error.borrow_mut() = Some(message),
                }
            }
            if records.is_empty() {
                records.push(new_record());
            }
            let index = records
                .iter()
                .position(|record| record.id == active)
                .unwrap_or(0);
            let record = records.remove(index);
            *self.state.borrow_mut() = new_state(record, self.account.epoch());
            *self.parked.borrow_mut() = records
                .into_iter()
                .map(|record| new_state(record, self.account.epoch()))
                .collect();
            *self.scope.borrow_mut() = next;
            self.show_session(app);
        }
        fn show_session(&self, app: &App) {
            let state = self.state.borrow();
            let ui = app.global::<Assistant>();
            ui.set_messages(ModelRc::from(state.messages.clone()));
            ui.set_draft(state.record.draft.as_str().into());
            ui.set_settings_open(false);
            ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
            ui.set_scroll_token(ui.get_scroll_token().wrapping_add(1));
            drop(state);
        }

        fn persist(&self, force: bool) -> Result<(), &'static str> {
            if let Some(error) = *self.history_error.borrow() {
                return Err(error);
            }
            if !force && self.last_persist.get().elapsed() < Duration::from_millis(500) {
                return Ok(());
            }
            let store = self.store.borrow();
            let Some(store) = store.as_ref() else {
                return Ok(());
            };
            let mut state = self.state.borrow_mut();
            let mut parked = self.parked.borrow_mut();
            capture(&mut state);
            for state in parked.iter_mut() {
                capture(state);
            }
            let archive = Archive {
                version: agent_sessions::ARCHIVE_VERSION,
                active_id: state.record.id.clone(),
                sessions: std::iter::once(&*state)
                    .chain(parked.iter())
                    .map(|state| state.record.clone())
                    .collect(),
            };
            let bytes = serde_json::to_vec(&archive).map_err(|_| "无法保存会话")?;
            if *self.last_saved.borrow() != bytes {
                if let Err(error) = store.save(&archive) {
                    *self.history_error.borrow_mut() = Some(error);
                    return Err(error);
                }
                *self.last_saved.borrow_mut() = bytes;
            }
            self.last_persist.set(Instant::now());
            Ok(())
        }
        fn save_selection(
            &self,
            active_id: String,
            extra: Option<Record>,
        ) -> Result<(), &'static str> {
            let current = self.state.borrow();
            let parked = self.parked.borrow();
            let mut sessions = std::iter::once(&*current)
                .chain(parked.iter())
                .map(|state| state.record.clone())
                .collect::<Vec<_>>();
            if let Some(record) = extra {
                sessions.push(record);
            }
            let archive = Archive {
                version: agent_sessions::ARCHIVE_VERSION,
                active_id,
                sessions,
            };
            let store = self.store.borrow();
            let store = store.as_ref().ok_or("会话目录不可用")?;
            if let Err(error) = store.save(&archive) {
                *self.history_error.borrow_mut() = Some(error);
                return Err(error);
            }
            *self.last_saved.borrow_mut() =
                serde_json::to_vec(&archive).map_err(|_| "无法保存会话")?;
            self.last_persist.set(Instant::now());
            Ok(())
        }
        fn new_session(&self, app: &App) {
            Self::fence_app_control_native(true);
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_create_session()
            {
                return;
            }
            if self.persist(true).is_err() {
                return;
            }
            let mut record = new_record();
            record.title = format!("新会话 {}", self.parked.borrow().len() + 2);
            if self
                .save_selection(record.id.clone(), Some(record.clone()))
                .is_err()
            {
                self.refresh(app);
                return;
            }
            let next = new_state(record, self.account.epoch());
            let mut old = std::mem::replace(&mut *self.state.borrow_mut(), next);
            // A not-yet-launched preparation may be cancelled; submitted work is retained.
            old.epoch = next_epoch();
            old.preparing = false;
            old.selecting = false;
            self.parked.borrow_mut().push(old);
            self.show_session(app);
            self.refresh(app);
            let _ = self.persist(true);
        }
        fn rename_session(&self, app: &App, id: &str, title: &str) {
            self.refresh(app);
            let title = title.trim();
            if !app.global::<Assistant>().get_open()
                || title.is_empty()
                || title.chars().count() > 64
                || id != self.state.borrow().record.id
                || self.persist(true).is_err()
            {
                return;
            }
            let mut archive = Archive {
                version: agent_sessions::ARCHIVE_VERSION,
                active_id: id.into(),
                sessions: std::iter::once(self.state.borrow().record.clone())
                    .chain(
                        self.parked
                            .borrow()
                            .iter()
                            .map(|state| state.record.clone()),
                    )
                    .collect(),
            };
            archive.sessions[0].rename(title);
            let result = self
                .store
                .borrow()
                .as_ref()
                .ok_or("会话目录不可用")
                .and_then(|store| store.save(&archive));
            match result {
                Ok(()) => {
                    self.state.borrow_mut().record.rename(title);
                    *self.last_saved.borrow_mut() =
                        serde_json::to_vec(&archive).unwrap_or_default();
                }
                Err(error) => *self.history_error.borrow_mut() = Some(error),
            }
            self.refresh(app);
        }
        fn edit_message(&self, app: &App, id: &str, index: i32) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || app.global::<Assistant>().get_busy()
                || app.global::<Assistant>().get_unknown()
                || index < 0
            {
                return;
            }
            let mut state = self.state.borrow_mut();
            if state.record.id != id {
                return;
            }
            let Some(message) = state
                .messages
                .row_data(index as usize)
                .filter(|m| m.role == "你")
            else {
                return;
            };
            state.record.draft = message.text.to_string();
            state.record.draft_references = state
                .record
                .message_references
                .get(&(index as usize))
                .cloned()
                .unwrap_or_default();
            app.global::<Assistant>().set_draft(message.text);
            drop(state);
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn add_references(&self, app: &App, references: Vec<Reference>) {
            let mut state = self.state.borrow_mut();
            let mut next = state.record.draft_references.clone();
            for reference in references {
                if !next.contains(&reference) {
                    next.push(reference);
                }
            }
            if next.len() > 4 || next.iter().filter(|r| r.kind == "asset").count() > 2 {
                state.notice = "最多引用两项素材与四项上下文".into();
            } else {
                state.record.draft_references = next;
                state.notice.clear();
            }
            drop(state);
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn attach(self: &Rc<Self>, app: &App) {
            self.refresh(app);
            if !app.global::<Assistant>().get_history_ready()
                || app.global::<Assistant>().get_busy()
                || app.global::<Assistant>().get_unknown()
            {
                return;
            }
            let id = self.state.borrow().record.id.clone();
            let scope = self.scope.borrow().clone();
            let weak = Rc::downgrade(self);
            use seecut_flow_core::graph::MediaKind;
            self.account.pick_flow_asset(
                app,
                vec![MediaKind::Image, MediaKind::Video, MediaKind::Audio],
                Rc::new(move |app, asset| {
                    let Some(binding) = weak.upgrade() else {
                        return;
                    };
                    binding.refresh(app);
                    if binding.state.borrow().record.id != id || *binding.scope.borrow() != scope {
                        return;
                    }
                    match crate::agent_references::asset(&asset) {
                        Ok(reference) => binding.add_references(app, vec![reference]),
                        Err(error) => {
                            binding.state.borrow_mut().notice = error;
                            binding.refresh(app);
                        }
                    }
                }),
            );
        }
        fn remove_reference(&self, app: &App, key: &str) {
            self.state
                .borrow_mut()
                .record
                .draft_references
                .retain(|r| serde_json::to_string(r).is_ok_and(|s| s != key));
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn retained_reference(&self, key: &str) -> Option<Reference> {
            let state = self.state.borrow();
            state
                .record
                .draft_references
                .iter()
                .chain(state.record.message_references.values().flatten())
                .find(|r| serde_json::to_string(r).is_ok_and(|s| s == key))
                .cloned()
        }
        fn reuse_reference(&self, app: &App, key: &str) {
            if let Some(reference) = self.retained_reference(key) {
                self.add_references(app, vec![reference]);
            }
        }
        fn open_reference(&self, app: &App, key: &str, destination: &str) {
            if let Some(reference) = self.retained_reference(key)
                && let Err(error) =
                    crate::agent_references::open(app, &self.account, &reference, destination)
            {
                self.state.borrow_mut().notice = error;
                self.refresh(app);
            }
        }
        fn select_session(&self, app: &App, index: i32) {
            Self::fence_app_control_native(true);
            self.refresh(app);
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            if !app.global::<Assistant>().get_open()
                || index == 0
                || index > self.parked.borrow().len()
            {
                return;
            }
            if self.persist(true).is_err() {
                return;
            }
            let id = self.parked.borrow()[index - 1].record.id.clone();
            if self.save_selection(id, None).is_err() {
                self.refresh(app);
                return;
            }
            let next = self.parked.borrow_mut().remove(index - 1);
            let mut old = std::mem::replace(&mut *self.state.borrow_mut(), next);
            old.epoch = next_epoch();
            old.preparing = false;
            old.selecting = false;
            self.parked.borrow_mut().push(old);
            self.show_session(app);
            self.refresh(app);
            let _ = self.persist(true);
        }
        fn other_session_reason(&self) -> &'static str {
            parked_connection_reason(&self.parked.borrow(), !self.retiring.borrow().is_empty())
        }
        fn snapshot(app: &App) -> Option<ScopeSnapshot> {
            let mut result = None;
            Shell::with(|shell, _| {
                // Native file dialogs may run a nested event loop while their
                // caller holds Studio. Retry on the next tick in that case.
                let Ok(studio) = shell.studio.try_borrow() else {
                    return;
                };
                let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
                    return;
                };
                // Bridge and studio borrows end before any setter or callback runs.
                let document = bridge.assistant_snapshot(&studio, app);
                let app_snapshot = bridge.assistant_app_snapshot(&studio, app);
                result = Some(ScopeSnapshot::choose(document, app_snapshot));
            });
            result
        }
        fn refresh(&self, app: &App) {
            let account_epoch = self.account.epoch();
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            self.drain_retiring();
            self.sync_scope(app, &snapshot);
            let owner =
                (self.account.signed_in() || cfg!(feature = "assistant-local-dev")).then(|| {
                    format!(
                        "{:?}:{}",
                        self.account.epoch(),
                        self.state.borrow().record.id
                    )
                });
            Shell::with(|shell, _| {
                let studio = shell.studio.borrow();
                let mut bridge = studio.editor_mcp.borrow_mut();
                bridge.bind_assistant_session(owner);
                bridge.bind_app_control_session(if snapshot.is_app() && self.account.signed_in() {
                    Some(self.state.borrow().record.id.clone())
                } else {
                    None
                });
            });
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            self.state.borrow_mut().record.draft =
                app.global::<Assistant>().get_draft().to_string();
            self.load_models(app);
            if self.sync_lease(&snapshot) {
                app.global::<Assistant>().set_draft("".into());
            }
            {
                let mut state = self.state.borrow_mut();
                state.controller.tick(snapshot.scope.as_ref());
                if state.task_permit_revision != state.controller.task_permit_revision() {
                    revoke_task_permits(&mut state);
                    state.task_permit_revision = state.controller.task_permit_revision();
                }
                capture(&mut state);
            }
            for state in self.parked.borrow_mut().iter_mut() {
                state.controller.tick(snapshot.scope.as_ref());
                capture(state);
                // Idle background sessions release the one execution connection.
                // Running and uncertain work keeps its original owner and callbacks.
                if state.controller.process_present()
                    && !session_busy(state)
                    && !session_uncertain(state)
                    && !matches!(
                        state.controller.view().phase,
                        Phase::Initializing | Phase::OpeningSession | Phase::Closing
                    )
                {
                    state.controller.disconnect();
                }
            }
            let mut catalog_expired = None;
            for _ in 0..4 {
                let Ok(result) = self.receiver.try_recv() else {
                    break;
                };
                let target = match &result {
                    WorkerResult::Models { session_id, .. }
                    | WorkerResult::Prepared { session_id, .. } => session_id,
                    #[cfg(feature = "assistant-local-dev")]
                    WorkerResult::File { session_id, .. } => session_id,
                };
                let mut current = self.state.borrow_mut();
                let mut parked = self.parked.borrow_mut();
                let is_current = current.record.id == *target;
                let state = if is_current {
                    &mut *current
                } else if let Some(state) =
                    parked.iter_mut().find(|state| state.record.id == *target)
                {
                    state
                } else {
                    continue;
                };
                match result {
                    WorkerResult::Models {
                        session_id: _,
                        generation,
                        account_epoch,
                        ticket,
                        result,
                    } if state.account_epoch == Some(account_epoch)
                        && state.generation == generation =>
                    {
                        let expired =
                            result.as_ref().err() == Some(&crate::agent_models::ACCOUNT_EXPIRED);
                        if state.catalog.finish(ticket, result) {
                            if expired {
                                catalog_expired = Some(account_epoch);
                            }
                            state.restore_catalog_selection();
                        }
                    }
                    #[cfg(feature = "assistant-local-dev")]
                    WorkerResult::File {
                        session_id: _,
                        epoch,
                        kind,
                        path,
                    } if state.epoch == epoch => {
                        state.selecting = false;
                        if let Some(path) = path {
                            match path {
                                Ok(path)
                                    if state.controller.view().can_connect && !state.preparing =>
                                {
                                    match kind {
                                        FileKind::Runtime => state.runtime = Some(path),
                                        FileKind::Entrypoint => state.entrypoint = Some(path),
                                        FileKind::McpClient => state.mcp_client = Some(path),
                                    }
                                    state.notice.clear();
                                }
                                Err(message) => state.notice = message.into(),
                                _ => {}
                            }
                        }
                    }
                    WorkerResult::Prepared {
                        session_id: _,
                        epoch,
                        account_epoch,
                        config,
                    } if state.epoch == epoch && state.account_epoch == account_epoch => {
                        state.preparing = false;
                        match config {
                            Ok(config)
                                if snapshot.scope.as_ref() == Some(&config.scope)
                                    && snapshot.read_token.as_deref()
                                        == Some(config.read_token.as_str())
                                    && snapshot.write_token == config.write_token
                                    && snapshot.app_control_token == config.app_control_token =>
                            {
                                state.notice.clear();
                                match state.controller.connect(*config) {
                                    Ok(()) => state.runtime_account_epoch = account_epoch,
                                    Err(error) => state.notice = error.message().into(),
                                }
                            }
                            Err(message) => state.notice = message.into(),
                            _ => state.notice = "工程或权限已变化，请重新连接".into(),
                        }
                    }
                    _ => {}
                }
            }
            if let Some(epoch) = catalog_expired {
                self.account.expire_if_current(app, epoch);
                return;
            }
            let (expired, billing_changed) = {
                let mut state = self.state.borrow_mut();
                let mut parked = self.parked.borrow_mut();
                let mut expired = false;
                let mut changed = false;
                for state in std::iter::once(&mut *state).chain(parked.iter_mut()) {
                    let view = state.controller.view();
                    let owns_account = state.runtime_account_epoch.is_some()
                        && state.runtime_account_epoch == account_epoch;
                    expired |= view.account_expired && owns_account;
                    changed |= owns_account && state.billing_revision != view.billing_revision;
                    state.billing_revision = view.billing_revision;
                }
                (expired, changed)
            };
            if expired && let Some(epoch) = account_epoch {
                self.account.expire_if_current(app, epoch);
                return;
            }
            if billing_changed && self.account.signed_in() {
                self.account.refresh_wallet(app);
            }
            self.drain_app_control(app);
            self.sync_task_grant(&snapshot);
            self.publish(app, &snapshot);
            self.try_auto_approve(app);
            let _ = self.persist(false);
        }
        fn fence_app_control_native(revoke: bool) {
            Shell::with(|shell, _| {
                if let Ok(studio) = shell.studio.try_borrow()
                    && let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut()
                {
                    if revoke {
                        bridge.revoke_app_control();
                    } else {
                        bridge.fence_app_control();
                    }
                }
            });
        }
        fn drain_app_control(&self, app: &App) {
            let (fence, proposals) = {
                let mut state = self.state.borrow_mut();
                (
                    state.controller.app_control_should_fence(),
                    state.controller.take_app_control_proposals(),
                )
            };
            if fence {
                Self::fence_app_control_native(false);
            }
            for proposal in proposals {
                let current = self
                    .state
                    .borrow()
                    .controller
                    .app_control_proposal_current(&proposal);
                let mut registered = false;
                if current {
                    Shell::with(|shell, _| {
                        if let Ok(studio) = shell.studio.try_borrow()
                            && let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut()
                        {
                            registered = bridge
                                .register_app_control(&studio, app, &shell.models, proposal.clone())
                                .is_ok();
                        }
                    });
                }
                if !registered {
                    let _ = self
                        .state
                        .borrow_mut()
                        .controller
                        .authorize_app_control(&proposal, false);
                }
            }
            let mut decisions = Vec::new();
            Shell::with(|shell, _| {
                if let Ok(studio) = shell.studio.try_borrow()
                    && let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut()
                {
                    // Keep ownership of each Prepared inside this same UI/Bridge borrow.
                    for registration in bridge.take_ready_app_control() {
                        let proposal = registration.proposal.clone();
                        let current = self
                            .state
                            .borrow()
                            .controller
                            .app_control_proposal_current(&proposal);
                        let allowed = bridge.finish_app_control_registration(
                            registration,
                            current,
                            &studio,
                            app,
                            &shell.models,
                        );
                        decisions.push((proposal, allowed));
                    }
                }
            });
            for (proposal, allowed) in decisions {
                let sent = self
                    .state
                    .borrow_mut()
                    .controller
                    .authorize_app_control(&proposal, allowed)
                    .is_ok();
                if !sent || !allowed {
                    Shell::with(|shell, _| {
                        if let Ok(studio) = shell.studio.try_borrow()
                            && let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut()
                        {
                            bridge.cancel_app_control_proposal(&proposal.intent.operation_id);
                        }
                    });
                }
            }
        }
        fn grant_scope(
            &self,
            state: &State,
            snapshot: &ScopeSnapshot,
        ) -> Option<crate::agent_task_grant::GrantScope> {
            let account_epoch = self.account.epoch()?;
            if state.runtime_account_epoch != Some(account_epoch) || !self.account.signed_in() {
                return None;
            }
            let lease = state.lease.as_ref()?;
            if lease.scope != snapshot.scope
                || lease.read != snapshot.read_token
                || lease.write != snapshot.write_token
            {
                return None;
            }
            Some(crate::agent_task_grant::GrantScope {
                task_id: state.controller.current_task_id()?.into(),
                document: snapshot.document().cloned()?,
                ipc_generation: state.controller.ipc_generation(),
                account_epoch,
                read_lease: snapshot.read_token.clone()?,
                write_lease: snapshot.write_token.clone()?,
            })
        }
        fn sync_task_grant(&self, snapshot: &ScopeSnapshot) {
            let mut state = self.state.borrow_mut();
            let scope = self.grant_scope(&state, snapshot);
            let view = state.controller.view();
            let blocked = view.unknown_or_blocked();
            let disconnected = matches!(
                view.phase,
                Phase::Disconnected
                    | Phase::Failed
                    | Phase::Closing
                    | Phase::Initializing
                    | Phase::OpeningSession
            );
            if blocked || disconnected || scope.is_none() {
                revoke_task_permits(&mut state);
                state.task_grant = None;
                state.update_grant = None;
                return;
            }
            let scope = scope.unwrap();
            if state
                .task_grant
                .as_ref()
                .is_some_and(|g| !g.remains_bound(&scope, state.controller.current_task()))
            {
                revoke_task_permits(&mut state);
                state.task_grant = None;
            }
            if state
                .update_grant
                .as_ref()
                .is_some_and(|g| !g.remains_bound(&scope, state.controller.current_task()))
            {
                revoke_task_permits(&mut state);
                state.update_grant = None;
            }
            if let Some(admitted) = state.controller.task_turn_binding().cloned()
                && let Some(turn) = state.controller.current_turn_id().map(str::to_owned)
                && state.controller.is_task_turn()
                && state.grant_seen_turn.as_deref() != Some(turn.as_str())
            {
                state.grant_seen_turn = Some(turn.clone());
                if state
                    .update_grant
                    .as_ref()
                    .is_some_and(|g| admitted.control_epoch >= g.expected_epoch())
                {
                    let candidate = state.update_grant.take().unwrap();
                    let previous_ended = candidate
                        .previous_turn()
                        .is_none_or(|old| state.controller.task_turn_ended(old));
                    state.task_grant =
                        candidate.next_turn(&scope, &admitted, &turn, previous_ended, blocked);
                }
            }
        }
        fn auto_allowed(&self, app: &App) -> bool {
            let Some(snapshot) = Self::snapshot(app) else {
                return false;
            };
            let state = self.state.borrow();
            let Some(scope) = self.grant_scope(&state, &snapshot) else {
                return false;
            };
            let view = state.controller.view();
            let (Some(grant), Some(current), Some(admitted), Some(turn), Some(approval)) = (
                state.task_grant.as_ref(),
                state.controller.current_task(),
                state.controller.task_turn_binding(),
                state.controller.current_turn_id(),
                state.approval.as_ref(),
            ) else {
                return false;
            };
            view.can_approve
                && state.controller.is_task_turn()
                && approval.turn_id == turn
                && grant.allows(
                    &scope,
                    current,
                    admitted,
                    turn,
                    &approval.tool,
                    &approval.arguments,
                    view.unknown_or_blocked(),
                )
        }
        fn try_auto_approve(&self, app: &App) {
            {
                let state = self.state.borrow();
                if state.auto_approving
                    || state.approval.is_none()
                    || state.auto_attempted == state.approval
                {
                    return;
                }
            }
            if !self.auto_allowed(app) {
                return;
            }
            {
                let mut state = self.state.borrow_mut();
                state.auto_attempted = state.approval.clone();
                state.auto_approving = true;
            }
            self.approve_checked(app, true, false);
            self.state.borrow_mut().auto_approving = false;
        }
        fn schedule(self: &Rc<Self>, app: &App) {
            let phase = self.state.borrow().controller.view().phase;
            let hot = self.parked.borrow().iter().any(|state| {
                session_busy(state) || state.controller.view().phase == Phase::Closing
            }) || self.state.borrow().catalog.loading
                || self.state.borrow().preparing
                || matches!(
                    phase,
                    Phase::Initializing
                        | Phase::OpeningSession
                        | Phase::Running
                        | Phase::AwaitingApproval
                        | Phase::Stopping
                        | Phase::Closing
                );
            // Visibility alone does not create work. Idle sessions keep only
            // the low-frequency account/document check; user actions wake the
            // timer immediately when they start an active phase.
            let delay = if hot { 40 } else { 1000 };
            self.timer
                .start(TimerMode::SingleShot, Duration::from_millis(delay), {
                    let binding = Rc::downgrade(self);
                    let app = app.as_weak();
                    move || {
                        if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                            binding.refresh(&app);
                            binding.schedule(&app);
                        }
                    }
                });
        }
        fn sync_lease(&self, snapshot: &ScopeSnapshot) -> bool {
            let next = Lease::from_snapshot(snapshot);
            let sync = |state: &mut State, current: bool| {
                if !current || state.lease.as_ref().is_some_and(|old| old != &next) {
                    revoke_task_permits(state);
                    state.task_grant = None;
                    state.update_grant = None;
                }
                if let Some(old) = state.lease.as_ref().filter(|old| *old != &next) {
                    let hot = current
                        && old.scope == next.scope
                        && old.read == next.read
                        && next.read.is_some()
                        && !state.preparing
                        && state.controller.process_present();
                    let notice = lease_change_notice(old, &next);
                    let updated = hot
                        && if snapshot.is_app() {
                            state
                                .controller
                                .update_app_control_permissions(next.app_control.clone())
                                .is_ok()
                        } else {
                            state
                                .controller
                                .update_permissions(next.write.clone())
                                .is_ok()
                        };
                    if !updated {
                        state.controller.disconnect();
                        state.preparing = false;
                        state.selecting = false;
                        state.epoch = next_epoch();
                    }
                    state.approval = None;
                    state.notice = notice.into();
                }
                // Parked conversations must never inherit another conversation's tokens.
                state.lease = if current { Some(next.clone()) } else { None };
            };
            sync(&mut self.state.borrow_mut(), true);
            for state in self.parked.borrow_mut().iter_mut() {
                sync(state, false);
            }
            false
        }
        fn publish(&self, app: &App, snapshot: &ScopeSnapshot) {
            let mut state = self.state.borrow_mut();
            state.sync_task_budget();
            let view = state.controller.view();
            let phase = view.phase;
            let task_mode = snapshot.write_token.is_some() && state.controller.supports_tasks();
            let document_observation_unavailable = !snapshot.is_app()
                && !task_mode
                && state.controller.document_observation_unavailable();
            let task_observation_unavailable =
                !snapshot.is_app() && state.controller.task_observation_unavailable();
            let pending_update = state.record.task_updates.iter().find(|intent| {
                !intent.confirmed
                    && Some(intent.task_id.as_str()) == state.controller.current_task_id()
            });
            let task_update_observation_missing =
                pending_update.is_some_and(|intent| intent.observation_binding.is_none());
            let task_observation_reason = if task_observation_unavailable {
                UiError::TaskObservationUnavailable.message()
            } else if task_update_observation_missing {
                UiError::TaskObservationMissing.message()
            } else {
                ""
            };
            let unknown = view.outcome == Some(TaskOutcome::OutcomeUnknown)
                || phase == Phase::OutcomeUnknown
                || state.record.pending == "unknown";
            let busy = state.preparing
                || matches!(
                    phase,
                    Phase::Initializing
                        | Phase::OpeningSession
                        | Phase::Running
                        | Phase::AwaitingApproval
                        | Phase::Stopping
                        | Phase::Closing
                )
                || (phase == Phase::Ready
                    && !view.can_send
                    && view.error.is_none()
                    && !state.controller.task_idle());
            let outcome = outcome_label(view.outcome);
            let history_error = *self.history_error.borrow();
            let status = if let Some(error) = history_error {
                error
            } else if !state.notice.is_empty() {
                &state.notice
            } else if state.controller.requires_app_restore() {
                "原会话已保留。允许读取后，点击恢复原会话；旧问题不会自动重发。"
            } else if unknown && view.can_connect {
                "恢复连接后核实原请求"
            } else if snapshot.is_app()
                && let Some(recovery) = state.controller.app_read_recovery()
                && !recovery.message().is_empty()
            {
                recovery.message()
            } else if unknown {
                view.error
                    .map(|error| scope_error_message(error, snapshot.is_app()))
                    .unwrap_or("操作结果待核实")
            } else if let Some(error) = view.error {
                scope_error_message(error, snapshot.is_app())
            } else if view.outcome == Some(TaskOutcome::Stopped) {
                stopped_hint()
            } else if !outcome.is_empty() {
                outcome
            } else {
                match phase {
                    Phase::Running => "正在处理",
                    Phase::AwaitingApproval => "等待你确认当前操作",
                    Phase::Stopping => "正在停止，等待结果",
                    Phase::Ready if busy => "已提交，等待 Agent 回应",
                    Phase::Ready if task_mode && task_observation_unavailable => {
                        UiError::TaskObservationUnavailable.message()
                    }
                    Phase::Ready if document_observation_unavailable => {
                        UiError::ObservationUnavailable.message()
                    }
                    Phase::Ready => "可以发送问题",
                    _ => "",
                }
            };
            let ui = app.global::<Assistant>();
            let names = std::iter::once(&*state)
                .chain(self.parked.borrow().iter())
                .map(|state| {
                    let suffix = if session_uncertain(state) {
                        " · 待核实"
                    } else if session_busy(state) || state.record.pending == "running" {
                        " · 处理中"
                    } else {
                        ""
                    };
                    slint::SharedString::from(format!("{}{suffix}", state.record.title))
                })
                .collect::<Vec<_>>();
            ui.set_session_names(ModelRc::new(VecModel::from(names)));
            ui.set_session_index(0);
            ui.set_history_ready(self.store.borrow().is_some() && history_error.is_none());
            ui.set_can_create_session(
                self.store.borrow().is_some()
                    && history_error.is_none()
                    && self.parked.borrow().len() + 1 < agent_sessions::MAX_SESSIONS,
            );
            ui.set_app_scope(snapshot.is_app());
            ui.set_current_project(snapshot.project_name.as_str().into());
            ui.set_provider_label(state.provider_label.as_str().into());
            let model_indices = state.model_indices();
            ui.set_model_names(ModelRc::new(VecModel::from(
                model_indices
                    .iter()
                    .map(|index| {
                        slint::SharedString::from(state.catalog.choices[*index].label.as_str())
                    })
                    .collect::<Vec<_>>(),
            )));
            ui.set_model_index(
                model_indices
                    .iter()
                    .position(|index| *index == state.catalog.selected)
                    .map(|index| index as i32)
                    .unwrap_or(-1),
            );
            ui.set_effort_names(ModelRc::new(VecModel::from(
                state
                    .catalog
                    .efforts
                    .iter()
                    .map(|effort| slint::SharedString::from(effort.label.as_str()))
                    .collect::<Vec<_>>(),
            )));
            let effort_index = state
                .selected_effort()
                .and_then(|selected| {
                    state
                        .catalog
                        .efforts
                        .iter()
                        .position(|effort| effort.id == selected.id)
                })
                .map(|index| index as i32)
                .unwrap_or(-1);
            ui.set_effort_index(effort_index);
            ui.set_effort_visible(
                !state.catalog.efforts.is_empty() || state.record.selected_effort_id.is_some(),
            );
            ui.set_effort_label(
                state
                    .selected_effort()
                    .map(|effort| effort.label.as_str())
                    .unwrap_or(if state.record.selected_effort_id.is_some() {
                        "原强度已失效"
                    } else {
                        "选择强度"
                    })
                    .into(),
            );
            let selection_reason = state.selection_reason();
            ui.set_effort_reason(
                if ui.get_effort_visible() {
                    selection_reason
                } else {
                    ""
                }
                .into(),
            );
            ui.set_can_select_effort(history_error.is_none() && state.can_select_effort());
            ui.set_connection(
                match phase {
                    Phase::Disconnected => "尚未连接",
                    Phase::Initializing | Phase::OpeningSession => "正在初始化 Agent",
                    Phase::Closing => "正在断开 Agent",
                    Phase::Failed => "连接失败，请稍后重试",
                    Phase::OutcomeUnknown => "Agent 已暂停",
                    _ => "Agent 已连接",
                }
                .into(),
            );
            ui.set_status(status.into());
            ui.set_details("".into());
            ui.set_busy(busy);
            ui.set_expression(if unknown || phase == Phase::AwaitingApproval {
                4
            } else if busy {
                1
            } else if phase == Phase::Failed || view.outcome == Some(TaskOutcome::Failed) {
                3
            } else if view.outcome == Some(TaskOutcome::Completed) {
                2
            } else {
                0
            });
            ui.set_unknown(unknown);
            let task_budget_reason = if task_mode && !state.controller.task_active() {
                state.task_budget().err().unwrap_or("")
            } else {
                ""
            };
            ui.set_task_mode(task_mode);
            ui.set_task_budget_text(state.task_budget_text.as_str().into());
            ui.set_task_budget_reason(task_budget_reason.into());
            ui.set_can_edit_task_budget(task_mode && !state.controller.task_active() && !busy);
            let task = state.controller.current_task();
            let task_cap = state
                .record
                .request_snapshots
                .iter()
                .rev()
                .filter_map(|v| v.task.as_ref())
                .find(|v| Some(v.task_id.as_str()) == state.controller.current_task_id())
                .map(|v| v.max_task_credits);
            ui.set_task_visible(!snapshot.is_app() && state.controller.current_task_id().is_some());
            ui.set_task_status(
                task.map(|v| {
                    format!(
                        "{} · 步骤 {}/{} · {}轮",
                        v.status.label(),
                        v.verified_steps,
                        v.total_steps,
                        v.rounds
                    )
                })
                .unwrap_or_else(|| "等待查询原任务状态".into())
                .into(),
            );
            ui.set_task_reason(
                task.and_then(|v| v.blocked_reason.as_deref())
                    .map(crate::agent_tasks::blocked_reason_label)
                    .unwrap_or("")
                    .into(),
            );
            ui.set_task_limit_label(
                task_cap
                    .map(|v| format!("本任务推理上限 {} 积分，生成费用另计", v))
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_can_task_pause(state.controller.can_task_command(TaskCommand::Pause));
            ui.set_can_task_stop(state.controller.can_task_command(TaskCommand::Stop));
            ui.set_can_task_resume(
                snapshot.write_token.is_some()
                    && state.controller.can_task_command(TaskCommand::Resume),
            );
            ui.set_can_task_status(state.controller.can_task_command(TaskCommand::Status));
            ui.set_can_task_update(
                state.controller.can_task_command(TaskCommand::Update)
                    && state.controller.supports_task_observation()
                    && !task_update_observation_missing,
            );
            ui.set_task_update_label(
                if pending_update.is_some() {
                    "核实原补充"
                } else {
                    "补充要求"
                }
                .into(),
            );
            ui.set_task_compatibility(
                if !task_observation_reason.is_empty() {
                    task_observation_reason
                } else if snapshot.write_token.is_some()
                    && !state.controller.supports_tasks()
                    && state.controller.session_opened()
                {
                    "当前 Agent 版本使用单轮执行，操作仍逐项确认"
                } else {
                    ""
                }
                .into(),
            );
            ui.set_can_send(
                view.can_send
                    && (!task_mode || !state.controller.task_active())
                    && (!task_mode || !task_observation_unavailable)
                    && !document_observation_unavailable
                    && task_budget_reason.is_empty()
                    && !state.preparing
                    && snapshot.read_token.is_some()
                    && history_error.is_none()
                    && !history_full(&state)
                    && selection_reason.is_empty(),
            );
            ui.set_current_session_id(state.record.id.as_str().into());
            let keys = state
                .record
                .draft_references
                .iter()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .collect::<Vec<_>>();
            if ui
                .get_draft_references()
                .iter()
                .map(|r| r.key.to_string())
                .collect::<Vec<_>>()
                != keys
            {
                ui.set_draft_references(crate::agent_references::rows(
                    &state.record.draft_references,
                    true,
                ));
            }
            ui.set_can_stop(view.can_stop);
            ui.set_can_reconcile(view.can_reconcile);
            let app_recovery_visible = snapshot.is_app()
                && state
                    .controller
                    .app_read_recovery()
                    .is_some_and(|r| r.reason != "nothing_pending");
            ui.set_app_recovery_visible(app_recovery_visible);
            ui.set_can_abandon_app_reads(
                app_recovery_visible && state.controller.can_abandon_app_reads(),
            );
            ui.set_can_query_app_reads(
                app_recovery_visible && state.controller.can_query_app_reads(),
            );
            ui.set_reconcile_label(
                if state.controller.app_recovery().is_some() {
                    "核实原页面操作"
                } else if state
                    .controller
                    .editor_recovery()
                    .is_some_and(|operation| operation.retry_registration)
                {
                    "恢复入库"
                } else {
                    "核实原请求"
                }
                .into(),
            );

            ui.set_budget_label(view.budget_label.into());
            let connected = matches!(
                phase,
                Phase::Ready | Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            );
            ui.set_connected(connected);
            ui.set_connecting(
                state.preparing
                    || matches!(
                        phase,
                        Phase::Initializing | Phase::OpeningSession | Phase::Closing
                    ),
            );
            ui.set_can_select_model(history_error.is_none() && state.can_select_model());
            ui.set_can_disconnect(connected && !busy);
            ui.set_connect_label(if state.preparing || matches!(phase, Phase::Initializing | Phase::OpeningSession) { "正在连接…" } else if phase == Phase::Closing { "正在断开…" } else if connected { "断开" } else if snapshot.is_app() && (state.record.runtime_created || state.controller.requires_app_restore()) { "恢复原会话" } else { "连接" }.into());
            let other = self.other_session_reason();
            let missing_model = state.record.selected_model_id.as_ref().is_some_and(|id| {
                !state.catalog.loading
                    && !state.catalog.choices.is_empty()
                    && !state.catalog.choices.iter().any(|choice| &choice.id == id)
            });
            let reason = if let Some(error) = history_error {
                error
            } else if self.store.borrow().is_none() {
                if snapshot.document().is_some() && snapshot.project_key.is_empty() {
                    "工程保存后即可开始会话"
                } else {
                    "请登录并打开支持的页面或工程"
                }
            } else if !other.is_empty() {
                other
            } else if missing_model && !session_uncertain(&state) {
                "原模型暂不可用，请重新选择"
            } else if unknown && !view.can_connect {
                "操作结果待核实，连接已锁定"
            } else if !cfg!(feature = "assistant-local-dev") && !self.account.signed_in() {
                "请先登录账户"
            } else if !cfg!(feature = "assistant-local-dev")
                && !self.components.as_ref().is_some_and(|path| path.is_file())
            {
                "此应用尚未包含 Agent 运行组件"
            } else if snapshot.scope.is_none() {
                "当前页面暂不支持读取"
            } else if snapshot.read_token.is_none() {
                "请先允许读取与预览"
            } else if state.selecting {
                "请先完成文件选择"
            } else if cfg!(feature = "assistant-local-dev") && state.runtime.is_none() {
                "请选择运行环境文件"
            } else if cfg!(feature = "assistant-local-dev") && state.entrypoint.is_none() {
                "请选择 Agent 入口文件"
            } else if cfg!(feature = "assistant-local-dev") && state.mcp_client.is_none() {
                "请选择 MCP 客户端文件"
            } else if busy {
                "请求正在处理，请稍后操作"
            } else if !cfg!(feature = "assistant-local-dev") && !session_uncertain(&state) {
                selection_reason
            } else {
                ""
            };
            ui.set_can_connect(view.can_connect && reason.is_empty());
            ui.set_connect_disabled_reason(reason.into());
            ui.set_send_disabled_reason(
                if let Some(error) = history_error {
                    error
                } else if state.preparing {
                    "正在准备连接"
                } else if history_full(&state) {
                    "当前会话已满，请新建会话"
                } else if unknown {
                    "核实操作结果前已暂停后续请求"
                } else if busy {
                    "请求正在处理，可等待或停止"
                } else if snapshot.read_token.is_none() {
                    "请先允许读取与预览并连接 Agent"
                } else if !selection_reason.is_empty() {
                    selection_reason
                } else if task_mode && task_observation_unavailable {
                    UiError::TaskObservationUnavailable.message()
                } else if document_observation_unavailable {
                    UiError::ObservationUnavailable.message()
                } else if !view.can_send {
                    "请先连接 Agent"
                } else {
                    ""
                }
                .into(),
            );
            ui.set_read_granted(snapshot.read_token.is_some());
            ui.set_write_granted(
                snapshot.write_token.is_some() || snapshot.app_control_token.is_some(),
            );
            ui.set_read_permission_status(snapshot.read_status.as_str().into());
            ui.set_write_permission_status(
                if snapshot.is_app()
                    && state.controller.session_opened()
                    && !state.controller.supports_app_control()
                {
                    "当前 Agent 版本仅支持读取 App 页面".into()
                } else {
                    snapshot.write_status.as_str().into()
                },
            );
            ui.set_can_grant_read(
                (snapshot.can_grant_read || snapshot.can_revoke_write)
                    && (cfg!(feature = "assistant-local-dev") || self.account.signed_in()),
            );
            ui.set_can_revoke_read(snapshot.can_revoke_read);
            ui.set_can_grant_write(
                snapshot.can_grant_write
                    && (!snapshot.is_app() || state.controller.supports_app_control())
                    && !busy
                    && (cfg!(feature = "assistant-local-dev") || self.account.signed_in()),
            );
            ui.set_can_revoke_write(snapshot.can_revoke_write);
            ui.set_runtime_filename(filename(state.runtime.as_deref()).into());
            ui.set_entrypoint_filename(filename(state.entrypoint.as_deref()).into());
            ui.set_mcp_client_filename(filename(state.mcp_client.as_deref()).into());
            ui.set_can_select_files(
                cfg!(feature = "assistant-local-dev")
                    && view.can_connect
                    && !busy
                    && !state.selecting
                    && !unknown,
            );
            let approval = view.approval;
            let description = approval
                .map(|a| {
                    if a.tool != "move_selected_image" {
                        crate::editor_mcp::canvas_approval_view(&a.tool, a.arguments.clone())
                            .map(|(_, description)| description)
                            .unwrap_or_else(|| "操作参数无效".into())
                    } else {
                        match (&a.object_id, a.delta_x, a.delta_y) {
                            (Some(object), Some(dx), Some(dy))
                                if dx.is_finite() && dy.is_finite() =>
                            {
                                format!("对象：{object}\n水平移动：{dx} px\n垂直移动：{dy} px")
                            }
                            _ => "移动参数不完整，无法允许此操作".into(),
                        }
                    }
                })
                .unwrap_or_default();
            let valid_approval = approval.is_some_and(|a| {
                if a.tool != "move_selected_image" {
                    return crate::editor_mcp::canvas_approval_view(&a.tool, a.arguments.clone())
                        .is_some();
                }
                a.object_id.is_some()
                    && a.delta_x.is_some_and(f64::is_finite)
                    && a.delta_y.is_some_and(f64::is_finite)
            });
            let editor_recovery = state.controller.editor_recovery().cloned();
            let app_recovery = state.controller.app_recovery().cloned();
            let frozen_approval = approval.map(|a| FrozenApproval {
                tool: a.tool.clone(),
                arguments: a.arguments.clone(),
                id: a.id.clone(),
                turn_id: a.turn_id.clone(),
                object_id: a.object_id.clone(),
                delta_x: a.delta_x,
                delta_y: a.delta_y,
            });
            ui.set_approval_visible(approval.is_some());
            ui.set_approval_title(
                approval
                    .and_then(|a| {
                        crate::editor_mcp::canvas_approval_view(&a.tool, a.arguments.clone())
                            .map(|(title, _)| title)
                    })
                    .unwrap_or_else(|| {
                        if approval.is_some_and(|a| a.tool == "move_selected_image") {
                            "移动画布对象".into()
                        } else {
                            "核对操作".into()
                        }
                    })
                    .into(),
            );
            ui.set_approval_description(description.into());
            ui.set_approval_details(
                approval
                    .map(|a| crate::editor_mcp::approval_details(&a.tool, &a.arguments))
                    .unwrap_or_default()
                    .into(),
            );
            if state.approval != frozen_approval {
                ui.set_approval_revision(ui.get_approval_revision().wrapping_add(1));
            }
            ui.set_can_allow(view.can_approve && valid_approval && !unknown);
            ui.set_can_reject(view.can_approve);
            let next_row = update_streaming_reply(
                &state.messages,
                state.assistant_row,
                phase,
                view.outcome,
                view.text,
                view.progress,
            );
            state.assistant_row = next_row;
            state.approval = frozen_approval;
            state.editor_recovery = editor_recovery;
            state.app_recovery = app_recovery;
        }
        fn capture_document_observation(
            app: &App,
            snapshot: &ScopeSnapshot,
        ) -> Result<crate::agent_observation::DocumentObservationBinding, UiError> {
            let (Some(identity), Some(read_token)) =
                (snapshot.document(), snapshot.read_token.as_deref())
            else {
                return Err(UiError::DocumentChanged);
            };
            let mut binding = Err(AssistantPermissionError::StaleContext);
            Shell::with(|shell, _| {
                let studio = shell.studio.borrow();
                binding = studio
                    .editor_mcp
                    .borrow_mut()
                    .assistant_document_observation(&studio, app, identity, read_token);
            });
            binding.map_err(|error| match error {
                AssistantPermissionError::NotAllowed => UiError::AuthorizationRequired,
                _ => UiError::ObservationChanged,
            })
        }
        fn send(&self, app: &App, text: &str) {
            if app.global::<Assistant>().get_can_send() {
                Self::fence_app_control_native(false);
                self.state.borrow_mut().controller.fence_app_control();
            }
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_send()
                || (text.trim().is_empty()
                    && self.state.borrow().record.draft_references.is_empty())
                || text.len() > 32_000
            {
                return;
            }
            let mut state = self.state.borrow_mut();
            if state.preparing || state.selecting {
                return;
            }
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            if snapshot.scope.is_none()
                || snapshot.read_token.is_none()
                || state.lease.as_ref().is_none_or(|lease| {
                    lease.scope != snapshot.scope || lease.read != snapshot.read_token
                })
            {
                state.notice = "页面或读取权限已变化，请重新核对后发送".into();
                return;
            }
            let Some(context) = snapshot.reference() else {
                return;
            };
            let task_mode = snapshot.write_token.is_some() && state.controller.supports_tasks();
            let app_input = if snapshot.is_app() {
                let mut binding = Err(AssistantPermissionError::NotAllowed);
                Shell::with(|shell, _| {
                    let studio = shell.studio.borrow();
                    binding = studio.editor_mcp.borrow_mut().assistant_app_view(
                        &studio,
                        app,
                        &shell.models,
                    );
                });
                match binding {
                    Ok(view) => Some((
                        format!("input-{}", uuid::Uuid::new_v4()),
                        crate::agent_app_control_input::InputBinding::new(
                            snapshot.scope.as_ref().unwrap().app().unwrap(),
                            crate::editor_mcp::ASSISTANT_CLIENT_ID,
                            &view,
                        ),
                    )),
                    Err(error) => {
                        state.notice = error.message().into();
                        return;
                    }
                }
            } else {
                None
            };
            let document_input = if !snapshot.is_app() {
                if task_mode && !state.controller.supports_task_observation() {
                    state.notice = UiError::TaskObservationUnavailable.message().into();
                    return;
                }
                if !task_mode && !state.controller.supports_document_observation() {
                    state.notice = UiError::ObservationUnavailable.message().into();
                    return;
                }
                match Self::capture_document_observation(app, &snapshot) {
                    Ok(binding) => Some((
                        format!(
                            "{}-{}",
                            if task_mode { "task-request" } else { "input" },
                            uuid::Uuid::new_v4()
                        ),
                        binding,
                    )),
                    Err(error) => {
                        state.notice = error.message().into();
                        return;
                    }
                }
            } else {
                None
            };
            let previous = state.record.clone();
            let references = state.record.draft_references.clone();
            let include_images = state.catalog.current().is_some_and(|choice| choice.images);
            let content = match crate::agent_references::content(text, &references, include_images)
            {
                Ok(content) => content,
                Err(error) => {
                    state.notice = error;
                    drop(state);
                    self.refresh(app);
                    return;
                }
            };
            let mut visible = text.to_owned();
            if let Some(lease) = &state.lease {
                for token in [lease.read.as_ref(), lease.write.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    visible = visible.replace(token, "[已隐藏]");
                }
            }
            if let Some(account) = self.account.snapshot() {
                visible = visible.replace(&account.token, "[已隐藏]");
            }
            let choice = state.catalog.current();
            let effort = state.selected_effort();
            let selection = if !cfg!(feature = "assistant-local-dev")
                && state.controller.supports_model_selection()
            {
                let mut selection = json!({"modelId": state.record.selected_model_id.as_deref().unwrap_or("default")});
                if let Some(effort) = effort {
                    selection["effortId"] = json!(effort.id);
                    selection["policyRevision"] = json!(effort.revision);
                }
                Some(selection)
            } else {
                None
            };
            let task_intent = if task_mode {
                let max_task_credits = match state.task_budget() {
                    Ok(value) => value,
                    Err(error) => {
                        state.notice = error.into();
                        drop(state);
                        self.refresh(app);
                        return;
                    }
                };
                let Some((client_request_id, binding)) = document_input.as_ref() else {
                    state.notice = UiError::ObservationInvalid.message().into();
                    return;
                };
                Some(crate::agent_tasks::TaskIntent {
                    task_id: crate::agent_tasks::task_id_for(client_request_id),
                    client_request_id: client_request_id.clone(),
                    goal: visible.clone(),
                    max_task_credits,
                    initial_observation_binding: Some(binding.clone()),
                })
            } else {
                None
            };
            let request = RequestSnapshot {
                input_id: app_input.as_ref().map(|(id, _)| id.clone()).or_else(|| {
                    document_input
                        .as_ref()
                        .filter(|_| !task_mode)
                        .map(|(id, _)| id.clone())
                }),
                app_view_binding: app_input.as_ref().map(|(_, view)| view.page()),
                app_input_binding: app_input.as_ref().map(|(_, view)| view.clone()),
                observation_binding: document_input
                    .as_ref()
                    .filter(|_| !task_mode)
                    .map(|(_, binding)| binding.clone()),
                task: task_intent.clone(),
                references: state.record.draft_references.clone(),
                session_id: state.record.id.clone(),
                model_id: choice
                    .map(|choice| choice.id.clone())
                    .unwrap_or_else(|| state.provider_label.clone()),
                effort_id: effort.map(|effort| effort.id.clone()),
                policy: effort
                    .and_then(|effort| {
                        effort.policy_for(
                            state
                                .record
                                .selected_model_id
                                .as_deref()
                                .unwrap_or_default(),
                        )
                    })
                    .or_else(|| choice.map(|choice| choice.policy.clone()))
                    .unwrap_or_else(|| json!({})),
                context,
            };
            if let Some(task) = &task_intent {
                state.record.current_task_id = Some(task.task_id.clone());
                state.task_display_turn = None;
            }
            state.record.request_snapshots.push(request);
            state.record.name_first_input(&visible);
            state.messages.push(AssistantMessage {
                references: crate::agent_references::rows(&references, true),
                role: "你".into(),
                text: visible.into(),
                state: "".into(),
                progress: "".into(),
            });
            let user_index = state.messages.row_count() - 1;
            if !references.is_empty() {
                state
                    .record
                    .message_references
                    .insert(user_index, references.clone());
            }
            state.messages.push(AssistantMessage {
                references: Default::default(),
                role: "Agent".into(),
                text: "".into(),
                state: "".into(),
                progress: "".into(),
            });
            state.assistant_row = Some(state.messages.row_count() - 1);
            state.record.draft.clear();
            state.record.draft_references.clear();
            state.submitting = true;
            state.record.pending = "running".into();
            drop(state);
            // Persist the frozen intent before IPC acceptance. A crash can only
            // require recovery; opening this file never resends the input.
            let saved = self.persist(true);
            let mut state = self.state.borrow_mut();
            let result = match saved {
                Ok(()) => (if let Some(task) = &task_intent {
                    state.controller.create_task(
                        task,
                        content,
                        selection
                            .clone()
                            .unwrap_or_else(|| json!({"modelId":"default"})),
                    )
                } else if let Some((id, view)) = app_input {
                    state
                        .controller
                        .send_app_content(&id, content, selection, view)
                } else if let Some((id, binding)) = document_input {
                    state
                        .controller
                        .send_document_content(&id, content, selection, binding)
                } else {
                    Err(UiError::Configuration)
                })
                .map_err(|error| error.message()),
                Err(error) => Err(error),
            };
            state.submitting = false;
            match result {
                Ok(()) => {
                    if task_intent.is_some()
                        && let Some(snapshot) = Self::snapshot(app)
                        && let Some(scope) = self.grant_scope(&state, &snapshot)
                    {
                        state.task_grant = crate::agent_task_grant::TaskGrant::from_human(scope, 1);
                        state.update_grant = None;
                        state.auto_attempted = None;
                        state.grant_seen_turn = None;
                    }
                    state.notice.clear();
                    let ui = app.global::<Assistant>();
                    ui.set_draft("".into());
                    ui.set_scroll_token(ui.get_scroll_token().wrapping_add(1));
                    ui.set_busy(true);
                    ui.set_can_send(false);
                }
                Err(error) => {
                    state.record = previous;
                    state.messages.set_vec(
                        state
                            .record
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(index, row)| AssistantMessage {
                                references: crate::agent_references::rows(
                                    state
                                        .record
                                        .message_references
                                        .get(&index)
                                        .map(Vec::as_slice)
                                        .unwrap_or(&[]),
                                    true,
                                ),
                                role: row.role.as_str().into(),
                                text: row.text.as_str().into(),
                                state: row.state.as_str().into(),
                                progress: row.progress.as_str().into(),
                            })
                            .collect::<Vec<_>>(),
                    );
                    state.assistant_row = None;
                    state.notice = error.into();
                }
            }
            drop(state);
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn task_action(&self, app: &App, action: TaskCommand) {
            if !app.global::<Assistant>().get_open() {
                return;
            }
            if matches!(action, TaskCommand::Pause | TaskCommand::Stop) {
                let mut state = self.state.borrow_mut();
                state.task_grant = None;
                state.update_grant = None;
                state.controller.withdraw_task_approvals();
                revoke_task_permits(&mut state);
            }
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if !state.controller.can_task_command(action) {
                return;
            }
            let resume_epoch = state
                .controller
                .current_task()
                .and_then(|v| v.control_epoch.checked_add(1));
            if matches!(action, TaskCommand::Pause | TaskCommand::Stop) {
                state.update_grant = None;
                state.task_grant = None;
                revoke_task_permits(&mut state);
            }
            match state.controller.task_command(action) {
                Err(error) => state.notice = error.message().into(),
                Ok(()) if action == TaskCommand::Resume => {
                    if let Some(snapshot) = Self::snapshot(app)
                        && let Some(scope) = self.grant_scope(&state, &snapshot)
                        && let Some(epoch) = resume_epoch
                    {
                        state.task_grant =
                            crate::agent_task_grant::TaskGrant::from_human(scope, epoch);
                        state.auto_attempted = None;
                    }
                }
                _ => {}
            }
            drop(state);
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn task_update(&self, app: &App, text: &str) {
            if !app.global::<Assistant>().get_open() {
                return;
            }
            // Withdraw before refresh can drain a pending card or auto-approve
            // it. Keep only the prior scope for the one-shot update candidate.
            let human_grant = {
                let mut state = self.state.borrow_mut();
                if !state.controller.supports_task_observation() {
                    state.notice = UiError::TaskObservationUnavailable.message().into();
                    return;
                }
                if state.record.task_updates.iter().any(|intent| {
                    !intent.confirmed
                        && Some(intent.task_id.as_str()) == state.controller.current_task_id()
                        && intent.observation_binding.is_none()
                }) {
                    state.notice = UiError::TaskObservationMissing.message().into();
                    return;
                }
                if state.record.task_updates.iter().all(|v| {
                    v.confirmed || Some(v.task_id.as_str()) != state.controller.current_task_id()
                }) && (text.trim().is_empty()
                    || text.len() > 32000
                    || !state.record.draft_references.is_empty())
                {
                    state.notice = "补充要求需为文字；当前输入与附件已保留".into();
                    return;
                }
                let grant = state.task_grant.take();
                state.controller.withdraw_task_approvals();
                revoke_task_permits(&mut state);
                grant
            };
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if !state.controller.can_task_command(TaskCommand::Update) {
                return;
            }
            let Some(task) = state.controller.current_task().cloned() else {
                return;
            };
            let pending = state
                .record
                .task_updates
                .iter()
                .position(|v| !v.confirmed && v.task_id == task.id);
            let new_input = pending.is_none();
            let intent = if let Some(index) = pending {
                let mut intent = state.record.task_updates[index].clone();
                // Only this explicit zero-mutation rejection permits a new CAS
                // precondition. Network uncertainty retains the original one.
                if state.controller.rejected_task_update()
                    == Some(intent.client_request_id.as_str())
                {
                    if intent.retries >= 2 {
                        state.notice = "任务修订持续变化，原补充已保留，请先查询任务状态".into();
                        return;
                    }
                    intent.expected_revision = task.task_revision;
                    intent.retries += 1;
                }
                state.record.task_updates[index] = intent.clone();
                intent
            } else {
                if !state.record.draft_references.is_empty() {
                    state.notice = "补充要求目前支持文字；附件已保留，可用于新任务".into();
                    return;
                }
                if state.record.task_updates.len() >= 64
                    || state
                        .record
                        .task_updates
                        .iter()
                        .filter(|v| v.task_id == task.id)
                        .count()
                        >= 16
                {
                    state.notice = "本会话补充要求已达保存上限，请创建新会话".into();
                    return;
                }
                let Some(snapshot) = Self::snapshot(app) else {
                    state.notice = UiError::DocumentChanged.message().into();
                    return;
                };
                if snapshot.is_app()
                    || snapshot.read_token.is_none()
                    || state.lease.as_ref().is_none_or(|lease| {
                        lease.scope != snapshot.scope || lease.read != snapshot.read_token
                    })
                {
                    state.notice = UiError::DocumentChanged.message().into();
                    return;
                }
                let binding = match Self::capture_document_observation(app, &snapshot) {
                    Ok(binding) => binding,
                    Err(error) => {
                        state.notice = error.message().into();
                        return;
                    }
                };
                let mut message = text.to_owned();
                if let Some(lease) = &state.lease {
                    for token in [lease.read.as_ref(), lease.write.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        message = message.replace(token, "[已隐藏]");
                    }
                }
                if let Some(account) = self.account.snapshot() {
                    message = message.replace(&account.token, "[已隐藏]");
                }
                let intent = crate::agent_tasks::TaskUpdateIntent {
                    client_request_id: format!("update-{}", uuid::Uuid::new_v4()),
                    task_id: task.id,
                    message,
                    expected_revision: task.task_revision,
                    retries: 0,
                    confirmed: false,
                    observation_binding: Some(binding),
                };
                if !intent.valid() {
                    state.notice = "补充要求需为不超过32000字节的文字".into();
                    return;
                }
                state.record.task_updates.push(intent.clone());
                intent
            };
            state.record.updated_at_ms = now_ms();
            drop(state);
            // Persist the exact original update before any command is queued.
            if let Err(error) = self.persist(true) {
                self.state.borrow_mut().notice = error.into();
                self.refresh(app);
                return;
            }
            let mut state = self.state.borrow_mut();
            let candidate = if new_input {
                let previous = state
                    .controller
                    .current_turn_id()
                    .map(str::to_owned)
                    .or_else(|| {
                        state
                            .controller
                            .current_task()
                            .and_then(|v| v.active_turn_id.clone())
                    });
                human_grant.and_then(|g| g.for_update(intent.client_request_id.clone(), previous))
            } else {
                state.update_grant.take()
            };
            revoke_task_permits(&mut state);
            state.update_grant = candidate;
            match state.controller.retry_task_update(&intent) {
                Ok(()) => {
                    if new_input {
                        state.messages.push(AssistantMessage {
                            references: Default::default(),
                            role: "你".into(),
                            text: intent.message.as_str().into(),
                            state: "补充要求".into(),
                            progress: "".into(),
                        });
                        state.record.draft.clear();
                        app.global::<Assistant>().set_draft("".into());
                    }
                    state.notice = "补充要求已发送，等待原请求确认".into();
                }
                Err(error) => state.notice = error.message().into(),
            }
            drop(state);
            let _ = self.persist(true);
            self.refresh(app);
        }
        fn close(&self, app: &App) {
            let ui = app.global::<Assistant>();
            ui.set_open(false);
            ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
            // The conversation, draft, active request and pending approval survive hiding.
        }
        fn disconnect(&self, app: &App) {
            {
                let mut state = self.state.borrow_mut();
                if !app.global::<Assistant>().get_can_disconnect() {
                    return;
                }
                state.controller.disconnect();
                state.epoch = next_epoch();
                state.notice.clear();
            }
            let ui = app.global::<Assistant>();
            ui.set_secret_clear_token(ui.get_secret_clear_token().wrapping_add(1));
            self.refresh(app);
        }
        fn permission(&self, app: &App, action: AssistantPermission) {
            self.refresh(app);
            let ui = app.global::<Assistant>();
            let allowed = match action {
                AssistantPermission::GrantRead => ui.get_can_grant_read(),
                AssistantPermission::RevokeRead => ui.get_can_revoke_read(),
                AssistantPermission::GrantWrite => ui.get_can_grant_write() && !ui.get_busy(),
                AssistantPermission::RevokeWrite => ui.get_can_revoke_write(),
            };
            if !ui.get_open() {
                return;
            }
            if !allowed {
                self.state.borrow_mut().notice =
                    AssistantPermissionError::NotAllowed.message().into();
                self.refresh(app);
                return;
            }
            let mut result = Err(AssistantPermissionError::NotAllowed);
            Shell::with(|shell, _| {
                let studio = shell.studio.borrow();
                let mut bridge = studio.editor_mcp.borrow_mut();
                let app_snapshot = bridge.assistant_app_snapshot(&studio, app);
                result = if let Some(identity) = app_snapshot.identity {
                    match action {
                        AssistantPermission::GrantRead => {
                            if app_snapshot.read_token.is_some() {
                                bridge.revoke_app_control();
                                Ok(())
                            } else {
                                bridge.grant_app_read(&studio, app, &identity, true)
                            }
                        }
                        AssistantPermission::RevokeRead => {
                            bridge.revoke_app_read();
                            Ok(())
                        }
                        AssistantPermission::GrantWrite => {
                            bridge.grant_app_control(&studio, app, &shell.models, &identity)
                        }
                        AssistantPermission::RevokeWrite => {
                            bridge.revoke_app_control();
                            Ok(())
                        }
                    }
                } else {
                    bridge.assistant_permission(&studio, app, action)
                };
            });
            self.state.borrow_mut().notice = result
                .err()
                .map(|error| error.message())
                .unwrap_or_default()
                .into();
            self.refresh(app);
        }
        fn approve(&self, app: &App, allowed: bool) {
            self.approve_checked(app, allowed, true);
        }
        fn approve_checked(&self, app: &App, allowed: bool, require_open: bool) {
            // Freeze the target the user actually saw before draining new IPC.
            let frozen = self.state.borrow().approval.clone();
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if (require_open && !ui.get_open())
                || (!require_open && !self.auto_allowed(app))
                || !(if allowed {
                    ui.get_can_allow()
                } else {
                    ui.get_can_reject()
                })
            {
                return;
            }
            let mut state = self.state.borrow_mut();
            if frozen != state.approval {
                state.notice = "待确认操作已变化，请重新核对当前动作、对象和参数".into();
            } else if let Some(approval) = frozen {
                let mut permit = None;
                let authorization = if allowed {
                    let mut result = Err("当前工程无法授权此操作");
                    Shell::with(|shell, _| {
                        let studio = shell.studio.borrow();
                        studio.sync_mcp_selection();
                        result = studio
                            .editor_mcp
                            .borrow_mut()
                            .assistant_tool_permit(
                                &studio,
                                app,
                                &approval.tool,
                                approval.arguments.clone(),
                            )
                            .map(|id| {
                                permit = Some(id);
                            })
                            .map_err(|error| error.message());
                    });
                    result
                } else {
                    Ok(())
                };
                match authorization {
                    Err(error) => state.notice = error.into(),
                    Ok(()) => {
                        if let Some(id) = permit
                            && state.controller.is_task_turn()
                        {
                            state
                                .task_permits
                                .retain(|(_, at)| at.elapsed() < Duration::from_secs(300));
                            state.task_permits.push((id, Instant::now()));
                        }
                        if let Err(error) = state.controller.approve(&approval.id, allowed) {
                            if let Some(id) = permit {
                                Shell::with(|shell, _| {
                                    shell
                                        .studio
                                        .borrow()
                                        .editor_mcp
                                        .borrow_mut()
                                        .revoke_tool_permit(id)
                                });
                            }
                            state.notice = error.message().into();
                        }
                    }
                }
            }
            drop(state);
            self.refresh(app);
        }
        #[cfg(feature = "assistant-local-dev")]
        fn choose(&self, app: &App, kind: FileKind) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_select_files()
            {
                return;
            }
            let epoch = {
                let mut state = self.state.borrow_mut();
                state.selecting = true;
                state.epoch = next_epoch();
                state.epoch
            };
            let sender = self.sender.clone();
            let session_id = self.state.borrow().record.id.clone();
            spawn_detached(move || {
                let title = match kind {
                    FileKind::Runtime => "选择 Node 运行环境文件",
                    FileKind::Entrypoint => "选择 Agent 入口文件",
                    FileKind::McpClient => "选择 MCP 客户端文件",
                };
                // The shared async wrapper skips its callback on desktop cancel.
                // This worker always reports completion, including cancellation.
                let paths = crate::platform::pick_files(title, None);
                let path = match paths.as_deref().unwrap_or_default() {
                    [] => None,
                    [path] => Some(ordinary_file(path).map(|()| path.clone())),
                    _ => Some(Err("请只选择一个普通文件")),
                };
                let _ = sender.send(WorkerResult::File {
                    session_id,
                    epoch,
                    kind,
                    path,
                });
            });
            self.refresh(app);
        }
        fn reconcile(&self, app: &App) {
            let frozen = self.state.borrow().editor_recovery.clone();
            let app_frozen = self.state.borrow().app_recovery.clone();
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if let Some(recovery) = app_frozen {
                state.notice = if state.app_recovery.as_ref() != Some(&recovery) {
                    "待核实的原页面操作已变化".into()
                } else {
                    state
                        .controller
                        .reconcile_app(&recovery)
                        .err()
                        .map(|error| error.message())
                        .unwrap_or_default()
                        .into()
                };
                drop(state);
                self.refresh(app);
                return;
            }
            if frozen != state.editor_recovery {
                state.notice = "待核对原操作已变化，请检查当前任务".into();
            } else if let Some(operation) = frozen {
                let mut permission = Ok(());
                let mut permit = None;
                if !operation.is_flow {
                    Shell::with(|shell, _| {
                        let studio = shell.studio.borrow();
                        permission = studio
                            .editor_mcp
                            .borrow_mut()
                            .assistant_tool_permit(
                                &studio,
                                app,
                                if operation.retry_registration {
                                    "retry_canvas_registration"
                                } else if operation.is_clip {
                                    "clip_operation"
                                } else {
                                    "canvas_operation"
                                },
                                operation.query_arguments(),
                            )
                            .map(|id| {
                                permit = Some(id);
                            })
                            .map_err(|error| error.message());
                    });
                }
                if let Err(error) = permission {
                    state.notice = error.into();
                } else if let Err(error) = state.controller.reconcile_editor(&operation) {
                    if let Some(id) = permit {
                        Shell::with(|shell, _| {
                            shell
                                .studio
                                .borrow()
                                .editor_mcp
                                .borrow_mut()
                                .revoke_tool_permit(id)
                        });
                    }
                    state.notice = error.message().into();
                } else {
                    state.notice.clear();
                }
            } else {
                state.notice = state
                    .controller
                    .reconcile()
                    .err()
                    .map(|e| e.message())
                    .unwrap_or_default()
                    .into();
            }
            drop(state);
            self.refresh(app);
        }
        fn app_read_recovery_action(&self, app: &App, abandon: bool) {
            let (session_id, recovery) = {
                let state = self.state.borrow();
                (
                    state.record.id.clone(),
                    state.controller.app_read_recovery().cloned(),
                )
            };
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if state.record.id != session_id
                || state.controller.app_read_recovery() != recovery.as_ref()
            {
                state.notice = "旧问题状态已变化，请检查当前会话".into();
            } else {
                let result = if abandon {
                    state.controller.abandon_app_reads()
                } else {
                    state.controller.query_app_reads()
                };
                state.notice = result
                    .err()
                    .map(|e| scope_error_message(e, true))
                    .unwrap_or("")
                    .into();
            }
            drop(state);
            self.refresh(app);
        }
        fn load_models(&self, app: &App) {
            if cfg!(feature = "assistant-local-dev") || !app.global::<Assistant>().get_open() {
                return;
            }
            let Some(account) = self.account.snapshot() else {
                return;
            };
            let Some(ticket) = self.state.borrow_mut().catalog.begin() else {
                return;
            };
            let sender = self.sender.clone();
            let session_id = self.state.borrow().record.id.clone();
            let generation = self.state.borrow().generation;
            spawn_detached(move || {
                let result = crate::agent_models::fetch(&account.base, &account.token);
                let _ = sender.send(WorkerResult::Models {
                    session_id,
                    generation,
                    account_epoch: account.epoch,
                    ticket,
                    result,
                });
            });
        }
        fn refresh_models(&self, app: &App) {
            self.refresh(app);
            if self.state.borrow().catalog.loading || self.state.borrow().preparing {
                return;
            }
            self.state.borrow_mut().catalog.retry();
            self.refresh(app);
        }
        fn select_model(&self, app: &App, index: i32) {
            self.refresh(app);
            if !app.global::<Assistant>().get_can_select_model() {
                return;
            }
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            let previous = self.state.borrow().record.clone();
            let previous_index = self.state.borrow().catalog.selected;
            if !self.state.borrow_mut().select_model_draft(index) {
                return;
            }
            if self.persist(true).is_err() {
                let mut state = self.state.borrow_mut();
                state.record = previous;
                state.catalog.selected = previous_index;
                state.provider_label = state
                    .catalog
                    .current()
                    .map(|choice| choice.label.clone())
                    .unwrap_or_default();
            }
            self.refresh(app);
        }
        fn select_effort(&self, app: &App, index: i32) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_select_effort()
            {
                return;
            }
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            let previous = self.state.borrow().record.clone();
            let previous_index = self.state.borrow().catalog.selected;
            {
                let mut state = self.state.borrow_mut();
                if !state.select_effort_draft(index) {
                    return;
                }
                state.notice.clear();
            }
            if self.persist(true).is_err() {
                {
                    let mut state = self.state.borrow_mut();
                    state.record = previous;
                    state.catalog.selected = previous_index;
                    state.provider_label = state
                        .catalog
                        .current()
                        .map(|choice| choice.label.clone())
                        .unwrap_or_default();
                }
            }
            self.refresh(app);
        }
        fn connect_managed(&self, app: &App) {
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if !ui.get_open() || !ui.get_can_connect() {
                return;
            }
            if self.persist(true).is_err() {
                self.refresh(app);
                return;
            }
            let Some(account) = self.account.snapshot() else {
                return;
            };
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            let (Some(scope), Some(read_token), Some(manifest)) =
                (snapshot.scope, snapshot.read_token, self.components.clone())
            else {
                return;
            };
            let mut data = None;
            Shell::with(|shell, _| data = Some(shell.studio.borrow().host.dirs.data.clone()));
            let Some(data) = data else {
                return;
            };
            let (epoch, model_id) = {
                let mut state = self.state.borrow_mut();
                let model_id = if session_uncertain(&state) {
                    state
                        .record
                        .request_snapshots
                        .last()
                        .map(|request| request.model_id.clone())
                        .or_else(|| state.record.selected_model_id.clone())
                        .unwrap_or_else(|| "default".into())
                } else {
                    let Some(choice) = state.catalog.current().filter(|choice| choice.available)
                    else {
                        return;
                    };
                    choice.id.clone()
                };
                state.provider_label = state
                    .catalog
                    .choices
                    .iter()
                    .find(|choice| choice.id == model_id)
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| model_id.clone());
                state.epoch = next_epoch();
                state.preparing = true;
                state.notice.clear();
                (state.epoch, model_id)
            };
            let sender = self.sender.clone();
            let environment = app_environment();
            let session_id = self.state.borrow().record.id.clone();
            let restore = self.state.borrow().record.runtime_created
                || self.state.borrow().controller.requires_app_restore();
            spawn_detached(move || {
                let config = (|| {
                    let files = crate::agent_components::load(&manifest)?;
                    let environment = environment?;
                    let mcp_environment = environment
                        .iter()
                        .filter(|(name, _)| matches!(name.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR"))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    let mut provider = json!({"protocol":"seecut-managed","baseURL":account.base,"sessionToken":account.token});
                    if model_id != "default" {
                        provider["modelId"] = json!(model_id);
                    }
                    Ok(ConnectionConfig {
                        local_session_id: session_id.clone(),
                        restore,
                        launch: LaunchSpec {
                            executable: files.runtime,
                            entrypoint: files.entrypoint,
                            environment,
                        },
                        scope,
                        client_id: ASSISTANT_CLIENT_ID.into(),
                        mcp_client_path: files.mcp_client,
                        mcp_environment,
                        session_directory: session_directory(&data)?,
                        provider,
                        read_token,
                        write_token: snapshot.write_token,
                        app_control_token: snapshot.app_control_token,
                    })
                })();
                let _ = sender.send(WorkerResult::Prepared {
                    session_id,
                    epoch,
                    account_epoch: Some(account.epoch),
                    config: config.map(Box::new),
                });
            });
            self.refresh(app);
        }
        #[cfg(feature = "assistant-local-dev")]
        fn connect(&self, app: &App, base: &str, model: &str, key: &str, images: bool) {
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if !ui.get_open() || !ui.get_can_connect() {
                return;
            }
            if self.persist(true).is_err() {
                self.refresh(app);
                return;
            }
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
            let validated = provider_url(base).and_then(|base| {
                if model.trim().is_empty()
                    || model.len() > 256
                    || key.trim().is_empty()
                    || key.len() > 16 * 1024
                {
                    Err("请填写有效的模型名称和本次会话密钥")
                } else {
                    Ok(base)
                }
            });
            let base = match validated {
                Ok(base) => base,
                Err(message) => {
                    self.state.borrow_mut().notice = message.into();
                    self.refresh(app);
                    return;
                }
            };
            let Some(scope) = snapshot.scope else {
                return;
            };
            let Some(read_token) = snapshot.read_token else {
                return;
            };
            let mut data = None;
            Shell::with(|shell, _| data = Some(shell.studio.borrow().host.dirs.data.clone()));
            let Some(data) = data else { return };
            let (epoch, runtime, entrypoint, mcp_client) = {
                let mut state = self.state.borrow_mut();
                let (Some(runtime), Some(entrypoint), Some(client)) =
                    (&state.runtime, &state.entrypoint, &state.mcp_client)
                else {
                    return;
                };
                let files = (runtime.clone(), entrypoint.clone(), client.clone());
                state.epoch = next_epoch();
                state.preparing = true;
                state.notice.clear();
                state.provider_label = model.trim().to_owned();
                (state.epoch, files.0, files.1, files.2)
            };
            // Only same-App necessary environment, never ambient credential discovery.
            let environment = app_environment();
            let provider = json!({"protocol":"openai-chat","baseURL":base,"model":model.trim(),"apiKey":key,"images":images});
            let sender = self.sender.clone();
            let account_epoch = self.account.epoch();
            let session_id = self.state.borrow().record.id.clone();
            let restore = self.state.borrow().record.runtime_created
                || self.state.borrow().controller.requires_app_restore();
            spawn_detached(move || {
                let config = (|| {
                    ordinary_file(&runtime)?;
                    ordinary_file(&entrypoint)?;
                    ordinary_file(&mcp_client)?;
                    let environment = environment?;
                    let mcp_environment = environment
                        .iter()
                        .filter(|(name, _)| matches!(name.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR"))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    let session_directory = session_directory(&data)?;
                    Ok(ConnectionConfig {
                        local_session_id: session_id.clone(),
                        restore,
                        launch: LaunchSpec {
                            executable: runtime,
                            entrypoint,
                            environment,
                        },
                        scope,
                        client_id: ASSISTANT_CLIENT_ID.into(),
                        mcp_client_path: mcp_client,
                        mcp_environment,
                        session_directory,
                        provider,
                        read_token,
                        write_token: snapshot.write_token,
                        app_control_token: snapshot.app_control_token,
                    })
                })();
                let _ = sender.send(WorkerResult::Prepared {
                    session_id,
                    epoch,
                    account_epoch,
                    config: config.map(Box::new),
                });
            });
            self.refresh(app);
        }
    }

    fn parked_connection_reason(parked: &[State], retiring: bool) -> &'static str {
        if retiring {
            "正在结束原工程的连接"
        } else if parked.iter().any(session_uncertain) {
            "请先回到待核实会话确认原操作"
        } else if parked
            .iter()
            .any(|state| session_busy(state) || state.record.pending == "running")
        {
            "另一会话正在处理，请稍后连接"
        } else if parked
            .iter()
            .any(|state| state.preparing || state.controller.process_present())
        {
            "正在释放上一会话的连接"
        } else {
            ""
        }
    }

    fn next_epoch() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
    fn new_record() -> Record {
        Record {
            id: uuid::Uuid::new_v4().to_string(),
            title: "新会话".into(),
            title_edited: false,
            draft: String::new(),
            draft_references: Vec::new(),
            message_references: BTreeMap::new(),
            messages: Vec::new(),
            selected_model_id: None,
            selected_effort_id: None,
            selected_policy_revision: None,
            selected_effort_policy: None,
            generation_model_ids: HashMap::new(),
            selection_initialized: false,
            runtime_created: false,
            current_task_id: None,
            task_updates: Vec::new(),
            request_snapshots: Vec::new(),
            pending: String::new(),
            updated_at_ms: now_ms(),
        }
    }
    fn new_state(record: Record, account_epoch: Option<u64>) -> State {
        let messages = Rc::new(VecModel::from(
            record
                .messages
                .iter()
                .enumerate()
                .map(|(index, message)| AssistantMessage {
                    references: crate::agent_references::rows(
                        record
                            .message_references
                            .get(&index)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        true,
                    ),
                    role: message.role.as_str().into(),
                    text: message.text.as_str().into(),
                    state: message.state.as_str().into(),
                    progress: message.progress.as_str().into(),
                })
                .collect::<Vec<_>>(),
        ));
        let mut controller = Controller::default();
        controller.restore_task(record.current_task_id.clone());
        State {
            record,
            generation: next_epoch(),
            messages,
            submitting: false,
            controller,
            lease: None,
            runtime: None,
            entrypoint: None,
            mcp_client: None,
            selecting: false,
            preparing: false,
            epoch: next_epoch(),
            account_epoch,
            runtime_account_epoch: None,
            billing_revision: 0,
            notice: String::new(),
            provider_label: String::new(),
            catalog: crate::agent_models::Catalog::default(),
            assistant_row: None,
            approval: None,
            editor_recovery: None,
            app_recovery: None,
            task_budget_text: String::new(),
            task_budget_policy: None,
            task_permits: Vec::new(),
            task_display_turn: None,
            task_grant: None,
            update_grant: None,
            grant_seen_turn: None,
            auto_approving: false,
            auto_attempted: None,
            task_permit_revision: 0,
        }
    }
    fn history_full(state: &State) -> bool {
        serde_json::to_vec(&state.record)
            .map_or(true, |bytes| bytes.len() >= NEW_TURN_HISTORY_LIMIT)
    }
    fn session_busy(state: &State) -> bool {
        let view = state.controller.view();
        state.preparing
            || state.submitting
            || matches!(
                view.phase,
                Phase::Initializing
                    | Phase::OpeningSession
                    | Phase::Running
                    | Phase::AwaitingApproval
                    | Phase::Stopping
            )
            || (view.phase == Phase::Ready
                && !view.can_send
                && view.error.is_none()
                && !state.controller.task_idle())
    }
    fn session_uncertain(state: &State) -> bool {
        let view = state.controller.view();
        state.record.pending == "unknown"
            || view.phase == Phase::OutcomeUnknown
            || view.outcome == Some(TaskOutcome::OutcomeUnknown)
    }
    fn revoke_task_permits(state: &mut State) {
        let permits = std::mem::take(&mut state.task_permits);
        state.approval = None;
        Shell::with(|shell, _| {
            let studio = shell.studio.borrow();
            let mut bridge = studio.editor_mcp.borrow_mut();
            for (id, _) in permits {
                bridge.revoke_tool_permit(id);
            }
        });
    }
    fn capture(state: &mut State) {
        if state.controller.is_task_turn()
            && let Some(turn) = state.controller.current_turn_id().map(str::to_owned)
            && state.task_display_turn.as_deref() != Some(turn.as_str())
        {
            let reuse = state.task_display_turn.is_none()
                && state
                    .assistant_row
                    .and_then(|i| state.messages.row_data(i))
                    .is_some_and(|r| r.text.is_empty() && r.state.is_empty());
            if !reuse {
                state.messages.push(AssistantMessage {
                    references: Default::default(),
                    role: "Agent".into(),
                    text: "".into(),
                    state: "".into(),
                    progress: "".into(),
                });
                state.assistant_row = Some(state.messages.row_count() - 1);
            }
            state.task_display_turn = Some(turn);
        }
        let view = state.controller.view();
        if !state.submitting {
            state.assistant_row = update_streaming_reply(
                &state.messages,
                state.assistant_row,
                view.phase,
                view.outcome,
                view.text,
                view.progress,
            );
        }
        if !state.submitting {
            state.record.current_task_id = state.controller.current_task_id().map(str::to_owned);
            if let Some(id) = state.controller.acknowledged_task_update()
                && let Some(update) = state
                    .record
                    .task_updates
                    .iter_mut()
                    .find(|v| v.client_request_id == id)
            {
                update.confirmed = true;
            }
        }
        let previous_pending = state.record.pending.clone();
        if view.phase == Phase::OutcomeUnknown || view.outcome == Some(TaskOutcome::OutcomeUnknown)
        {
            state.record.pending = "unknown".into();
        } else if state.submitting
            || matches!(
                view.phase,
                Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            )
            || (view.phase == Phase::Ready
                && !view.can_send
                && view.error.is_none()
                && !state.controller.task_idle())
        {
            state.record.pending = "running".into();
        } else if state.controller.session_opened()
            && matches!(
                view.phase,
                Phase::Ready | Phase::Failed | Phase::Disconnected
            )
        {
            state.record.pending.clear();
        }
        // A recovered interrupted reply stays as history; it is never replayed or
        // replaced with the new process's empty stream buffer.
        if !previous_pending.is_empty()
            && state.record.pending.is_empty()
            && state.assistant_row.is_none()
            && let Some(index) = state.messages.row_count().checked_sub(1)
            && let Some(mut row) = state.messages.row_data(index)
            && row.role == "Agent"
            && (row.state.is_empty()
                || (previous_pending == "unknown" && row.state == "操作结果待核实"))
        {
            row.state = if state.controller.app_scope() {
                "旧问题已结束"
            } else if previous_pending == "unknown" {
                "已核实"
            } else {
                "旧请求已结束"
            }
            .into();
            row.progress = "".into();
            state.messages.set_row_data(index, row);
        }
        let messages = state
            .messages
            .iter()
            .map(|row| Message {
                role: row.role.to_string(),
                text: row.text.to_string(),
                state: row.state.to_string(),
                progress: row.progress.to_string(),
            })
            .collect::<Vec<_>>();
        let created = state.record.runtime_created || state.controller.session_opened();
        if messages != state.record.messages
            || previous_pending != state.record.pending
            || created != state.record.runtime_created
        {
            state.record.updated_at_ms = now_ms();
        }
        state.record.messages = messages;
        state.record.runtime_created = created;
    }

    pub(super) fn bind(app: &App, account: crate::cloud::AccountAccess) -> Rc<Binding> {
        let (sender, receiver) = sync_channel(4);
        let mut data = None;
        Shell::with(|shell, _| data = Some(shell.studio.borrow().host.dirs.data.clone()));
        let binding = Rc::new(Binding {
            timer: Timer::default(),
            account: account.clone(),
            components: crate::agent_components::manifest_path(),
            state: RefCell::new(new_state(new_record(), account.epoch())),
            parked: RefCell::new(Vec::new()),
            retiring: RefCell::new(Vec::new()),
            scope: RefCell::new(None),
            data,
            store: RefCell::new(None),
            history_error: RefCell::new(None),
            last_saved: RefCell::new(Vec::new()),
            last_persist: Cell::new(Instant::now()),
            sender,
            receiver,
        });
        let ui = app.global::<Assistant>();
        ui.set_enabled(true);
        ui.set_developer_tools(cfg!(feature = "assistant-local-dev"));
        ui.set_messages(ModelRc::from(binding.state.borrow().messages.clone()));
        macro_rules! callback {
            ($name:ident, $method:ident $(, $argument:expr)*) => {
                ui.$name({
                    let binding = Rc::downgrade(&binding);
                    let app = app.as_weak();
                    move || if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) { binding.$method(&app $(, $argument)*); binding.schedule(&app); }
                });
            };
        }
        callback!(on_close_requested, close);
        callback!(on_new_session, new_session);
        callback!(on_attach, attach);
        ui.on_reuse_reference({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |key| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.reuse_reference(&app, &key);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_open_reference({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |key, destination| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.open_reference(&app, &key, &destination);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_remove_reference({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |key| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.remove_reference(&app, &key);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_rename_session({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |id, title| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.rename_session(&app, &id, &title);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_edit_message({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |id, index| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.edit_message(&app, &id, index);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_select_session({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |index| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.select_session(&app, index);
                    binding.schedule(&app);
                }
            }
        });
        callback!(on_disconnect, disconnect);
        callback!(on_task_pause, task_action, TaskCommand::Pause);
        callback!(on_task_stop, task_action, TaskCommand::Stop);
        callback!(on_task_resume, task_action, TaskCommand::Resume);
        callback!(on_task_status_requested, task_action, TaskCommand::Status);
        ui.on_task_budget_edited({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |text| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    if app.global::<Assistant>().get_can_edit_task_budget() {
                        binding.state.borrow_mut().task_budget_text = text.to_string();
                    }
                    binding.refresh(&app);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_task_update({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |text| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.task_update(&app, &text);
                    binding.schedule(&app);
                }
            }
        });
        callback!(on_connect_managed, connect_managed);
        callback!(on_refresh_models, refresh_models);
        ui.on_select_effort({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |index| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.select_effort(&app, index);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_select_model({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |index| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.select_model(&app, index);
                    binding.schedule(&app);
                }
            }
        });
        callback!(on_reconcile, reconcile);
        callback!(on_abandon_app_reads, app_read_recovery_action, true);
        callback!(on_query_app_reads, app_read_recovery_action, false);
        callback!(on_grant_read, permission, AssistantPermission::GrantRead);
        callback!(on_revoke_read, permission, AssistantPermission::RevokeRead);
        callback!(on_grant_write, permission, AssistantPermission::GrantWrite);
        callback!(
            on_revoke_write,
            permission,
            AssistantPermission::RevokeWrite
        );
        callback!(on_allow, approve, true);
        callback!(on_reject, approve, false);
        #[cfg(feature = "assistant-local-dev")]
        callback!(on_choose_runtime, choose, FileKind::Runtime);
        #[cfg(feature = "assistant-local-dev")]
        callback!(on_choose_entrypoint, choose, FileKind::Entrypoint);
        #[cfg(feature = "assistant-local-dev")]
        callback!(on_choose_mcp_client, choose, FileKind::McpClient);
        ui.on_send({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |text| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.send(&app, &text);
                    binding.schedule(&app);
                }
            }
        });
        #[cfg(feature = "assistant-local-dev")]
        ui.on_connect({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move |base, model, key, images| {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    binding.connect(&app, &base, &model, &key, images);
                    binding.schedule(&app);
                }
            }
        });
        ui.on_stop({
            let binding = Rc::downgrade(&binding);
            let app = app.as_weak();
            move || {
                if let (Some(binding), Some(app)) = (binding.upgrade(), app.upgrade()) {
                    let (task_turn, frozen_turn) = {
                        let state = binding.state.borrow();
                        (
                            state.controller.is_task_turn(),
                            state.controller.current_turn_id().map(str::to_owned),
                        )
                    };
                    if task_turn {
                        // This human action withdraws task permits before IPC is drained.
                        binding.task_action(&app, TaskCommand::Stop);
                    } else {
                        Binding::fence_app_control_native(false);
                        binding.state.borrow_mut().controller.fence_app_control();
                        binding.refresh(&app);
                        if app.global::<Assistant>().get_open() {
                            let mut state = binding.state.borrow_mut();
                            // A late result or new task must not redirect the old chat stop.
                            if frozen_turn.is_some()
                                && state.controller.current_turn_id() == frozen_turn.as_deref()
                                && !state.controller.is_task_turn()
                                && let Err(error) = state.controller.stop()
                            {
                                state.notice = error.message().into();
                            }
                            drop(state);
                            binding.refresh(&app);
                        }
                    }
                    binding.schedule(&app);
                }
            }
        });
        binding.refresh(app);
        binding.schedule(app);
        binding
    }

    fn scope_error_message(
        error: crate::agent_controller::UiError,
        app_scope: bool,
    ) -> &'static str {
        use crate::agent_controller::UiError;
        match (app_scope, error) {
            (true, UiError::AuthorizationRequired) => "请允许读取当前页面文字和媒体预览",
            (true, UiError::DocumentChanged) => "账户或 App 会话已变化，请重新授权并恢复原会话",
            (true, UiError::SessionRecoveryRequired) => "旧问题需要结束或核实，请查看会话状态",
            (true, UiError::VerifyRequired) => "旧问题的读取状态待核实，已暂停后续问题",
            _ => error.message(),
        }
    }
    fn stopped_hint() -> &'static str {
        "本轮已停止；单轮上限 120 秒。重新发起需新输入，编辑动作需新确认"
    }

    fn outcome_label(outcome: Option<TaskOutcome>) -> &'static str {
        match outcome {
            Some(TaskOutcome::Completed) => "已完成",
            Some(TaskOutcome::SubmittedPendingVerification) => "已提交，待验证",
            Some(TaskOutcome::Failed) => "处理失败",
            Some(TaskOutcome::Stopped) => "已停止",
            Some(TaskOutcome::OutcomeUnknown) => "操作结果待核实",
            Some(TaskOutcome::OperationVerified) => "原操作已核对",
            None => "",
        }
    }

    // Once a reply is terminal, the host detaches its streaming row. Failed or
    // disconnected transport cannot replace retained text with a reset buffer.
    // Closing is not final: process exit can still establish an unknown result.
    fn reply_row_update(
        phase: Phase,
        outcome: Option<TaskOutcome>,
        text: &str,
    ) -> (Option<&str>, &'static str, bool) {
        if outcome.is_some() {
            // A tool receipt can establish pending verification before the
            // model finishes speaking. Keep the same row live until turn end.
            let terminal = outcome != Some(TaskOutcome::SubmittedPendingVerification)
                || !matches!(
                    phase,
                    Phase::Running | Phase::AwaitingApproval | Phase::Stopping
                );
            return (Some(text), outcome_label(outcome), terminal);
        }
        match phase {
            Phase::Failed => (None, "连接失败", true),
            Phase::Disconnected => (None, "已断开", true),
            Phase::OutcomeUnknown => (Some(text), "操作结果待核实", true),
            _ => (Some(text), "", false),
        }
    }

    fn update_streaming_reply(
        messages: &VecModel<AssistantMessage>,
        index: Option<usize>,
        phase: Phase,
        outcome: Option<TaskOutcome>,
        text: &str,
        progress: &str,
    ) -> Option<usize> {
        let index = index?;
        let mut row = messages.row_data(index)?;
        let (reply_text, reply_state, terminal) = reply_row_update(phase, outcome, text);
        let progress = if !terminal && progress.is_empty() {
            match phase {
                Phase::Running => "正在处理…",
                Phase::Stopping => "正在停止，等待结果",
                Phase::Ready => "等待 Agent 回应…",
                _ => progress,
            }
        } else {
            progress
        };
        if reply_text.is_some_and(|text| row.text.as_str() != text)
            || row.state.as_str() != reply_state
            || row.progress.as_str() != progress
        {
            if let Some(text) = reply_text {
                row.text = text.into();
            }
            row.state = reply_state.into();
            row.progress = progress.into();
            messages.set_row_data(index, row);
        }
        if terminal { None } else { Some(index) }
    }
    fn filename(path: Option<&Path>) -> String {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    #[cfg(any(test, feature = "assistant-local-dev"))]
    fn provider_url(raw: &str) -> Result<String, &'static str> {
        let raw = raw.trim();
        let url = url::Url::parse(raw).map_err(|_| "服务地址格式无效")?;
        let has_userinfo = raw.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || has_userinfo
        {
            return Err("服务地址需为 HTTP(S)，且不能包含账号、密码或片段");
        }
        Ok(url.into())
    }
    #[cfg(feature = "assistant-local-dev")]
    fn ordinary_file(path: &Path) -> Result<(), &'static str> {
        if !path.is_absolute() {
            return Err("请选择绝对路径的普通文件");
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "所选文件无法读取")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("请选择普通文件，不能使用软链接或目录");
        }
        Ok(())
    }
    fn app_environment() -> Result<BTreeMap<String, String>, &'static str> {
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        let temporary = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let temporary = temporary.to_str().ok_or("App 临时目录编码无效")?;
        values.insert("TMPDIR".into(), temporary.into());
        for name in ["XDG_RUNTIME_DIR", "LANG", "LC_ALL", "TZ"] {
            if let Some(value) = std::env::var_os(name) {
                values.insert(
                    name.into(),
                    value.into_string().map_err(|_| "App 环境值编码无效")?,
                );
            }
        }
        for (name, value) in &values {
            if value.contains('\0') || value.len() > 4096 {
                return Err("App 环境值无效");
            }
            if matches!(name.as_str(), "TMPDIR" | "XDG_RUNTIME_DIR")
                && (!Path::new(value).is_absolute() || !Path::new(value).is_dir())
            {
                return Err("App 临时目录或运行目录不可用");
            }
        }
        Ok(values)
    }
    fn session_directory(data: &Path) -> Result<PathBuf, &'static str> {
        if !data.is_absolute() {
            return Err("App 数据目录必须是绝对路径");
        }
        let mut path = PathBuf::new();
        for component in data.components() {
            if matches!(component, Component::ParentDir | Component::CurDir) {
                return Err("App 数据目录路径无效");
            }
            path.push(component);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Each parent was checked before creating the next component.
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&path)
                        .map_err(|_| "无法创建 App 数据目录")?;
                    std::fs::symlink_metadata(&path).map_err(|_| "App 数据目录不可用")?
                }
                Err(_) => return Err("App 数据目录不可用"),
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("App 数据目录不能经过软链接");
            }
        }
        let directory = data.join("assistant-sessions");
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .map_err(|_| "无法创建 Agent 会话目录")?;
            }
            Err(_) => return Err("Agent 会话目录不可用"),
        }
        let metadata = std::fs::symlink_metadata(&directory).map_err(|_| "Agent 会话目录不可用")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("Agent 会话目录必须由当前用户拥有、权限为 0700，且不能是软链接");
        }
        Ok(directory)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn effort_catalog(available: bool) -> crate::agent_models::Directory {
            crate::agent_models::parse(json!({
                "protocol_version":1,"default_model_id":"model-one",
                "models":[{"model_id":"model-one","label":"模型一","available":true,"protocol_version":1}],
                "effort_policy_version":1,"default_effort_id":"balanced",
                "efforts":[{"id":"balanced","label":"均衡","policy_revision":"rev-one","available":available,"reason_code":if available { serde_json::Value::Null } else { json!("unavailable") },
                    "agent":{"model_id":"model-one","max_output_tokens":2048,"max_steps":6,"max_turn_credits":20,"pricing_version":"prices-1","context_window":8192,"input_rate":1,"output_rate":2},
                    "generation":{"image_model_ids":["image-one"],"video_model_ids":[]}}]
            })).unwrap()
        }
        fn apply_catalog(state: &mut State, directory: crate::agent_models::Directory) -> bool {
            state.catalog.retry();
            let ticket = state.catalog.begin().unwrap();
            state.catalog.finish(ticket, Ok(directory));
            state.restore_catalog_selection()
        }
        #[test]
        fn task_budget_preserves_same_policy_input_and_reconfirms_changed_model_cap() {
            let mut state = new_state(new_record(), Some(1));
            assert!(apply_catalog(&mut state, effort_catalog(true)));
            state.sync_task_budget();
            assert_eq!(state.task_budget_text, "80");
            state.task_budget_text = "60".into();
            state.sync_task_budget();
            assert_eq!(state.task_budget().unwrap(), 60);
            state.record.selected_effort_id = None;
            state.catalog.choices[0].policy = json!({"max_turn_credits":30});
            state.sync_task_budget();
            assert_eq!(state.task_budget_text, "120");
            assert!(state.notice.contains("模型或强度已变化"));
            state.task_budget_text = "29".into();
            assert!(state.task_budget().is_err());
            assert_eq!(state.task_budget_text, "29");
            state.task_budget_text = "9007199254740992".into();
            assert!(state.task_budget().is_err());
        }
        #[test]
        fn approval_refresh_compares_every_frozen_field_not_display_text() {
            let original = FrozenApproval {
                tool: "flow_update_node".into(),
                arguments: json!({"nodeId":"n","text":"one"}),
                id: "a".into(),
                turn_id: "t".into(),
                object_id: Some("layer:1".into()),
                delta_x: Some(1.0),
                delta_y: Some(2.0),
            };
            for field in 0..7 {
                let mut changed = original.clone();
                match field {
                    0 => changed.tool = "flow_delete_nodes".into(),
                    1 => changed.arguments["text"] = json!("two"),
                    2 => changed.id = "other".into(),
                    3 => changed.turn_id = "other".into(),
                    4 => changed.object_id = Some("layer:2".into()),
                    5 => changed.delta_x = Some(3.0),
                    _ => changed.delta_y = Some(4.0),
                };
                assert!(original != changed);
            }
        }
        #[test]
        fn permission_change_preserves_read_connection_notice() {
            let old = Lease {
                scope: None,
                read: Some("synthetic-read".into()),
                write: Some("synthetic-write".into()),
                app_control: None,
            };
            let mut next = old.clone();
            next.write = None;
            assert!(lease_change_notice(&old, &next).contains("撤销"));
            assert!(!lease_change_notice(&old, &next).contains("重新连接"));
            next.read = None;
            assert!(lease_change_notice(&old, &next).contains("重新连接"));
            assert!(stopped_hint().contains("120 秒"));
        }

        #[test]
        fn only_new_sessions_adopt_default_effort_and_unavailable_defaults_stay_unavailable() {
            let mut new = new_state(new_record(), Some(1));
            assert!(apply_catalog(&mut new, effort_catalog(false)));
            assert_eq!(new.record.selected_effort_id.as_deref(), Some("balanced"));
            assert!(!new.selected_effort().unwrap().available);
            let mut legacy = new_state(new_record(), Some(1));
            legacy.record.selection_initialized = true;
            assert!(!apply_catalog(&mut legacy, effort_catalog(true)));
            assert!(legacy.record.selected_effort_id.is_none());
            assert!(legacy.record.selected_effort_policy.is_none());
        }
        #[test]
        fn restored_policy_rejects_same_revision_drift_but_not_availability_changes() {
            let mut state = new_state(new_record(), Some(1));
            apply_catalog(&mut state, effort_catalog(true));
            let original = state.record.clone();
            apply_catalog(&mut state, effort_catalog(false));
            assert!(state.selected_effort().is_some());
            assert!(!state.selected_effort().unwrap().available);
            let mut changed = effort_catalog(true);
            changed.efforts[0].public["agent"]["max_steps"] = json!(7);
            apply_catalog(&mut state, changed);
            assert!(state.selected_effort().is_none());
            assert_eq!(
                state.record.selected_effort_policy,
                original.selected_effort_policy
            );
            assert_eq!(
                state.record.selected_policy_revision,
                original.selected_policy_revision
            );
            assert!(!state.selection_reason().is_empty());
            let mut restored = new_state(state.record.clone(), Some(1));
            let mut changed = effort_catalog(true);
            changed.efforts[0].public["generation"]["image_model_ids"] = json!(["another-image"]);
            apply_catalog(&mut restored, changed);
            assert!(restored.selected_effort().is_none());
            assert_eq!(
                restored.record.selected_effort_policy,
                original.selected_effort_policy
            );
        }
        #[test]
        fn catalog_refresh_error_blocks_a_new_request_without_rewriting_policy() {
            let mut state = new_state(new_record(), Some(1));
            apply_catalog(&mut state, effort_catalog(true));
            let original = state.record.selected_effort_policy.clone();
            state.catalog.retry();
            let ticket = state.catalog.begin().unwrap();
            assert!(!state.selection_reason().is_empty());
            state.catalog.finish(ticket, Err("强度目录暂不可用"));
            assert!(!state.selection_reason().is_empty());
            assert_eq!(state.record.selected_effort_policy, original);
            apply_catalog(&mut state, effort_catalog(true));
            assert!(state.selection_reason().is_empty());
        }
        #[test]
        fn pending_sessions_can_change_next_effort_without_rewriting_original_request() {
            for pending in ["running", "unknown"] {
                let mut state = new_state(new_record(), Some(1));
                apply_catalog(&mut state, effort_catalog(true));
                state.record.pending = pending.into();
                state.record.runtime_created = true;
                let original = RequestSnapshot {
                    input_id: None,
                    app_view_binding: None,
                    app_input_binding: None,
                    observation_binding: None,
                    task: None,
                    references: Vec::new(),
                    session_id: state.record.id.clone(),
                    model_id: "model-one".into(),
                    effort_id: Some("balanced".into()),
                    policy: state.selected_effort().unwrap().policy(),
                    context: ContextReference::Document(DocumentReference {
                        project_key: "project".into(),
                        project_name: "测试工程".into(),
                    }),
                };
                state.record.request_snapshots.push(original.clone());
                let mut second = state.catalog.efforts[0].clone();
                second.id = "quality".into();
                second.revision = "rev-two".into();
                second.public["id"] = json!(second.id);
                second.public["policy_revision"] = json!(second.revision);
                second.public["agent"]["max_steps"] = json!(9);
                state.catalog.efforts.push(second);
                assert_eq!(
                    state.can_select_effort(),
                    !cfg!(feature = "assistant-local-dev")
                );
                assert!(state.select_effort_draft(1));
                assert_eq!(state.record.selected_effort_id.as_deref(), Some("quality"));
                assert_eq!(state.record.request_snapshots, vec![original]);
                assert_eq!(state.record.pending, pending);
                assert!(!state.controller.view().can_send);
                assert!(state.controller.send_text_with_selection("must not send", Some(json!({"modelId":"model-one","effortId":"quality","policyRevision":"rev-two"}))).is_err());
                assert_eq!(state.record.pending, pending);
            }
        }
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicU64, Ordering};

        #[test]
        fn persisting_a_submitting_question_keeps_its_new_reply_row_attached() {
            let mut state = new_state(new_record(), Some(1));
            state.messages.push(AssistantMessage {
                references: Default::default(),
                role: "Agent".into(),
                text: "上一轮已经完成".into(),
                state: "已完成".into(),
                progress: "".into(),
            });
            state.messages.push(AssistantMessage {
                references: Default::default(),
                role: "Agent".into(),
                text: "".into(),
                state: "".into(),
                progress: "".into(),
            });
            state.assistant_row = Some(1);
            state.submitting = true;
            // No subprocess is needed: even a stale disconnected Controller would
            // detach this row if capture observed it before the new IPC send.
            capture(&mut state);
            assert_eq!(state.assistant_row, Some(1));
            assert_eq!(state.record.pending, "running");
            assert_eq!(state.record.messages[0].text, "上一轮已经完成");
            assert_eq!(state.record.messages[0].state, "已完成");
            assert_eq!(state.record.messages[1].text, "");
            assert_eq!(state.record.messages[1].state, "");
            assert_eq!(state.record.messages[1].progress, "");
        }

        #[test]
        fn rebuilding_the_same_session_cannot_reuse_worker_tickets() {
            let record = new_record();
            let old = new_state(record.clone(), Some(7));
            let rebuilt = new_state(record, Some(7));
            assert_eq!(old.record.id, rebuilt.record.id);
            assert_eq!(old.account_epoch, rebuilt.account_epoch);
            assert_ne!(old.epoch, rebuilt.epoch);
            assert_ne!(old.generation, rebuilt.generation);
            assert_ne!(old.epoch, rebuilt.generation);
            assert_ne!(old.generation, rebuilt.epoch);
        }
        #[test]
        fn a_blank_view_cannot_release_another_sessions_pending_work() {
            for (pending, reason) in [
                ("running", "另一会话正在处理，请稍后连接"),
                ("unknown", "请先回到待核实会话确认原操作"),
            ] {
                let blank = new_state(new_record(), Some(1));
                let mut owner = new_state(new_record(), Some(1));
                owner.record.runtime_created = true;
                owner.record.pending = pending.into();
                let original_id = owner.record.id.clone();
                capture(&mut owner);
                assert!(blank.controller.view().can_connect);
                assert_eq!(owner.record.id, original_id);
                assert_eq!(owner.record.pending, pending);
                assert_eq!(parked_connection_reason(&[owner], false), reason);
                // Selecting the owner should allow its explicit restoration; the
                // empty background view cannot itself claim unfinished work.
                assert_eq!(parked_connection_reason(&[blank], false), "");
            }
            assert_eq!(parked_connection_reason(&[], true), "正在结束原工程的连接");
        }
        #[test]
        fn terminal_replies_detach_and_transport_failure_preserves_text() {
            for outcome in [
                TaskOutcome::Completed,
                TaskOutcome::SubmittedPendingVerification,
                TaskOutcome::Failed,
                TaskOutcome::Stopped,
                TaskOutcome::OutcomeUnknown,
            ] {
                let (text, state, terminal) =
                    reply_row_update(Phase::Ready, Some(outcome), "最终回复");
                assert_eq!(text, Some("最终回复"));
                assert!(!state.is_empty() && terminal);
            }
            for phase in [Phase::Failed, Phase::Disconnected] {
                let (text, state, terminal) = reply_row_update(phase, None, "");
                assert!(
                    text.is_none(),
                    "transport state must not erase the retained row"
                );
                assert!(!state.is_empty() && terminal);
            }
            assert_eq!(
                reply_row_update(Phase::Closing, None, "处理中"),
                (Some("处理中"), "", false)
            );
            assert_eq!(
                reply_row_update(
                    Phase::Closing,
                    Some(TaskOutcome::OutcomeUnknown),
                    "保留回复"
                ),
                (Some("保留回复"), "操作结果待核实", true)
            );
        }

        #[test]
        fn pending_tool_receipt_does_not_detach_streaming_body() {
            let messages = VecModel::from(vec![AssistantMessage {
                references: Default::default(),
                role: "Agent".into(),
                text: "".into(),
                state: "".into(),
                progress: "".into(),
            }]);
            let pending = Some(TaskOutcome::SubmittedPendingVerification);
            let mut index = update_streaming_reply(
                &messages,
                Some(0),
                Phase::Running,
                pending,
                "",
                "移动请求已返回",
            );
            assert_eq!(index, Some(0));
            index = update_streaming_reply(
                &messages,
                index,
                Phase::Running,
                pending,
                "仍需核对画布",
                "移动请求已返回",
            );
            assert_eq!(messages.row_data(0).unwrap().text.as_str(), "仍需核对画布");
            assert_eq!(
                messages.row_data(0).unwrap().state.as_str(),
                "已提交，待验证"
            );
            assert_eq!(index, Some(0));
            index = update_streaming_reply(
                &messages,
                index,
                Phase::Ready,
                pending,
                "最终回复，待验证",
                "移动请求已返回",
            );
            assert!(index.is_none());
            assert_eq!(
                messages.row_data(0).unwrap().text.as_str(),
                "最终回复，待验证"
            );
            assert_eq!(
                messages.row_data(0).unwrap().progress.as_str(),
                "移动请求已返回"
            );
            assert_eq!(
                messages.row_data(0).unwrap().state.as_str(),
                "已提交，待验证"
            );
        }
        #[test]
        fn completed_reply_survives_new_connection_buffer_reset() {
            let messages = VecModel::from(vec![AssistantMessage {
                references: Default::default(),
                role: "Agent".into(),
                text: "".into(),
                state: "".into(),
                progress: "".into(),
            }]);
            let mut index = update_streaming_reply(
                &messages,
                Some(0),
                Phase::Running,
                None,
                "流式回复",
                "读取当前画布…",
            );
            assert_eq!(messages.row_data(0).unwrap().text.as_str(), "流式回复");
            index = update_streaming_reply(
                &messages,
                index,
                Phase::Ready,
                Some(TaskOutcome::Completed),
                "最终回复",
                "读取当前画布 · 已返回",
            );
            assert!(index.is_none());
            // Controller.connect resets text/outcome before initialize/create.
            index = update_streaming_reply(&messages, index, Phase::Initializing, None, "", "");
            index = update_streaming_reply(&messages, index, Phase::Ready, None, "", "");
            assert!(index.is_none());
            let retained = messages.row_data(0).unwrap();
            assert_eq!(retained.text.as_str(), "最终回复");
            assert_eq!(retained.state.as_str(), "已完成");
            assert_eq!(retained.progress.as_str(), "读取当前画布 · 已返回");
        }

        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                // macOS's ambient /var temp path is a symlink; fixture root itself
                // uses the actual directory so tests can isolate our own symlinks.
                let temporary = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                let root = temporary.join(format!(
                    "seecut-assistant-host-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&root)
                    .unwrap();
                Self(root)
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn session_storage_creates_missing_parents_and_rejects_symlinks() {
            let fixture = Fixture::new();
            let existing = fixture.0.join("existing");
            std::fs::create_dir(&existing).unwrap();
            std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
            let sessions = session_directory(&existing).unwrap();
            assert_eq!(
                std::fs::metadata(&sessions).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(session_directory(&existing).unwrap(), sessions);

            let missing = fixture.0.join("fresh/nested-data");
            assert!(session_directory(&missing).is_ok());
            assert_eq!(
                std::fs::metadata(&missing).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let linked = fixture.0.join("linked");
            symlink(&existing, &linked).unwrap();
            assert!(session_directory(&linked.join("must-not-create")).is_err());
            assert!(!existing.join("must-not-create").exists());

            std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(session_directory(&existing).is_err());
            let leaf_data = fixture.0.join("leaf-data");
            std::fs::create_dir(&leaf_data).unwrap();
            symlink(&sessions, leaf_data.join("assistant-sessions")).unwrap();
            assert!(session_directory(&leaf_data).is_err());
        }

        #[test]
        fn provider_requires_http_without_userinfo_or_fragment() {
            assert!(provider_url("https://example.invalid/v1").is_ok());
            for invalid in [
                "file:///tmp/x",
                "https://user@example.invalid/v1",
                "https://user:password@example.invalid",
                "https://@example.invalid",
                "https://example.invalid/#fragment",
            ] {
                assert!(provider_url(invalid).is_err());
            }
        }
    }
}
