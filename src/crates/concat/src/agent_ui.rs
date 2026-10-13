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
        ConnectionConfig, Controller, DocumentIdentity, Phase, TaskOutcome,
    };
    use crate::agent_process::LaunchSpec;
    use crate::agent_sessions::{
        self, Archive, ContextReference, Message, Record, RequestSnapshot, Store,
    };
    use crate::editor_mcp::{
        ASSISTANT_CLIENT_ID, AssistantPermission, AssistantPermissionError, AssistantSnapshot,
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

    // Never Debug/serialize this: comparison includes real lease tokens.
    #[derive(Clone, PartialEq, Eq)]
    struct Lease {
        document: Option<DocumentIdentity>,
        read: Option<String>,
        write: Option<String>,
    }
    impl Lease {
        fn from_snapshot(snapshot: &AssistantSnapshot) -> Self {
            Self {
                document: snapshot.document.clone(),
                read: snapshot.read_token.clone(),
                write: snapshot.write_token.clone(),
            }
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
    }
    impl State {
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
        scope: RefCell<Option<(String, String)>>,
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
        fn sync_scope(&self, app: &App, snapshot: &AssistantSnapshot) {
            let account = self.account.history_scope().or_else(|| {
                cfg!(feature = "assistant-local-dev").then(|| "local-development".into())
            });
            let next = account
                .filter(|_| !snapshot.project_key.is_empty())
                .map(|account| (account, snapshot.project_key.clone()));
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
                    let store = Store::open(&session_directory(data)?, account, project)?;
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
        fn select_session(&self, app: &App, index: i32) {
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
        fn snapshot(app: &App) -> Option<AssistantSnapshot> {
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
                result = Some(bridge.assistant_snapshot(&studio, app));
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
            self.state.borrow_mut().record.draft =
                app.global::<Assistant>().get_draft().to_string();
            self.load_models(app);
            if self.sync_lease(&snapshot) {
                app.global::<Assistant>().set_draft("".into());
            }
            {
                let mut state = self.state.borrow_mut();
                state.controller.tick(snapshot.document.as_ref());
                capture(&mut state);
            }
            for state in self.parked.borrow_mut().iter_mut() {
                state.controller.tick(snapshot.document.as_ref());
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
                                if snapshot.document.as_ref() == Some(&config.document)
                                    && snapshot.read_token.as_deref()
                                        == Some(config.read_token.as_str())
                                    && snapshot.write_token == config.write_token =>
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
            self.publish(app, &snapshot);
            let _ = self.persist(false);
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
        fn sync_lease(&self, snapshot: &AssistantSnapshot) -> bool {
            let next = Lease::from_snapshot(snapshot);
            let sync = |state: &mut State| {
                if state.lease.as_ref().is_some_and(|old| old != &next) {
                    state.controller.disconnect();
                    state.preparing = false;
                    state.selecting = false;
                    state.epoch = next_epoch();
                    state.approval = None;
                    state.notice = "工程或权限已变化，请重新连接".into();
                }
                state.lease = Some(Lease::from_snapshot(snapshot));
            };
            sync(&mut self.state.borrow_mut());
            for state in self.parked.borrow_mut().iter_mut() {
                sync(state);
            }
            false
        }
        fn publish(&self, app: &App, snapshot: &AssistantSnapshot) {
            let mut state = self.state.borrow_mut();
            let view = state.controller.view();
            let phase = view.phase;
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
                || (phase == Phase::Ready && !view.can_send && view.error.is_none());
            let outcome = outcome_label(view.outcome);
            let history_error = *self.history_error.borrow();
            let status = if let Some(error) = history_error {
                error
            } else if !state.notice.is_empty() {
                &state.notice
            } else if unknown && view.can_connect {
                "恢复连接后核实原请求"
            } else if unknown {
                view.error
                    .map(|error| error.message())
                    .unwrap_or("操作结果待核实")
            } else if let Some(error) = view.error {
                error.message()
            } else if !outcome.is_empty() {
                outcome
            } else {
                match phase {
                    Phase::Running => "正在处理",
                    Phase::AwaitingApproval => "等待你确认移动操作",
                    Phase::Stopping => "正在停止，等待结果",
                    Phase::Ready if busy => "已提交，等待 Agent 回应",
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
            ui.set_can_send(
                view.can_send
                    && !state.preparing
                    && snapshot.read_token.is_some()
                    && history_error.is_none()
                    && !history_full(&state)
                    && selection_reason.is_empty(),
            );
            ui.set_can_stop(view.can_stop);
            ui.set_can_reconcile(view.can_reconcile);
            ui.set_reconcile_label(
                if state
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
            ui.set_connect_label(if state.preparing || matches!(phase, Phase::Initializing | Phase::OpeningSession) { "正在连接…" } else if phase == Phase::Closing { "正在断开…" } else if connected { "断开" } else { "连接" }.into());
            let other = self.other_session_reason();
            let missing_model = state.record.selected_model_id.as_ref().is_some_and(|id| {
                !state.catalog.loading
                    && !state.catalog.choices.is_empty()
                    && !state.catalog.choices.iter().any(|choice| &choice.id == id)
            });
            let reason = if let Some(error) = history_error {
                error
            } else if self.store.borrow().is_none() {
                if snapshot.document.is_some() && snapshot.project_key.is_empty() {
                    "工程保存后即可开始会话"
                } else {
                    "请登录并打开工程"
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
            } else if snapshot.document.is_none() {
                "请先打开工程"
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
                if history_full(&state) {
                    "当前会话已满，请新建会话"
                } else if unknown {
                    "核实操作结果前已暂停后续请求"
                } else if busy {
                    "请求正在处理，可等待或停止"
                } else if snapshot.read_token.is_none() {
                    "请先允许读取与预览并连接 Agent"
                } else if !selection_reason.is_empty() {
                    selection_reason
                } else if !view.can_send {
                    "请先连接 Agent"
                } else {
                    ""
                }
                .into(),
            );
            ui.set_read_granted(snapshot.read_token.is_some());
            ui.set_write_granted(snapshot.write_token.is_some());
            ui.set_read_permission_status(snapshot.read_status.as_str().into());
            ui.set_write_permission_status(snapshot.write_status.as_str().into());
            ui.set_can_grant_read(
                snapshot.can_grant_read
                    && !busy
                    && (cfg!(feature = "assistant-local-dev") || self.account.signed_in()),
            );
            ui.set_can_revoke_read(snapshot.can_revoke_read);
            ui.set_can_grant_write(
                snapshot.can_grant_write
                    && !busy
                    && (cfg!(feature = "assistant-local-dev") || self.account.signed_in()),
            );
            ui.set_can_renew_write(
                snapshot.can_renew_write
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
                    .unwrap_or_else(|| "移动画布对象".into())
                    .into(),
            );
            ui.set_approval_description(description.into());
            ui.set_can_allow(view.can_approve && valid_approval);
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
        }
        fn send(&self, app: &App, text: &str) {
            self.refresh(app);
            if !app.global::<Assistant>().get_open()
                || !app.global::<Assistant>().get_can_send()
                || text.trim().is_empty()
                || text.len() > 32_000
            {
                return;
            }
            let mut state = self.state.borrow_mut();
            if state.preparing || state.selecting {
                return;
            }
            let previous = state.record.clone();
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
            let Some(snapshot) = Self::snapshot(app) else {
                return;
            };
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
            let request = RequestSnapshot {
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
                context: ContextReference {
                    project_key: snapshot.project_key,
                    project_name: snapshot.project_name,
                },
            };
            state.record.request_snapshots.push(request);
            if state.record.request_snapshots.len() == 1 {
                state.record.title = visible
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(24)
                    .collect();
            }
            state.messages.push(AssistantMessage {
                role: "你".into(),
                text: visible.into(),
                state: "".into(),
                progress: "".into(),
            });
            state.messages.push(AssistantMessage {
                role: "Agent".into(),
                text: "".into(),
                state: "".into(),
                progress: "".into(),
            });
            state.assistant_row = Some(state.messages.row_count() - 1);
            state.record.draft.clear();
            state.submitting = true;
            state.record.pending = "running".into();
            drop(state);
            // Persist the frozen intent before IPC acceptance. A crash can only
            // require recovery; opening this file never resends the input.
            let saved = self.persist(true);
            let mut state = self.state.borrow_mut();
            let result = match saved {
                Ok(()) => if selection.is_some() {
                    state.controller.send_text_with_selection(text, selection)
                } else {
                    state.controller.send_text(text)
                }
                .map_err(|error| error.message()),
                Err(error) => Err(error),
            };
            state.submitting = false;
            match result {
                Ok(()) => {
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
                            .map(|row| AssistantMessage {
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
                AssistantPermission::GrantRead => ui.get_can_grant_read() && !ui.get_busy(),
                AssistantPermission::RevokeRead => ui.get_can_revoke_read(),
                AssistantPermission::GrantWrite => ui.get_can_grant_write() && !ui.get_busy(),
                AssistantPermission::RenewWrite => ui.get_can_renew_write() && !ui.get_busy(),
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
                result = studio
                    .editor_mcp
                    .borrow_mut()
                    .assistant_permission(&studio, app, action);
            });
            self.state.borrow_mut().notice = result
                .err()
                .map(|error| error.message())
                .unwrap_or_default()
                .into();
            self.refresh(app);
        }
        fn approve(&self, app: &App, allowed: bool) {
            // Freeze the target the user actually saw before draining new IPC.
            let frozen = self.state.borrow().approval.clone();
            self.refresh(app);
            let ui = app.global::<Assistant>();
            if !ui.get_open()
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
                state.notice = "待确认操作已变化，请检查当前对象和位移".into();
            } else if let Some(approval) = frozen {
                let mut permit = None;
                let authorization = if allowed && approval.tool != "move_selected_image" {
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
            self.refresh(app);
            if !app.global::<Assistant>().get_open() {
                return;
            }
            let mut state = self.state.borrow_mut();
            if frozen != state.editor_recovery {
                state.notice = "待核对原操作已变化，请检查当前任务".into();
            } else if let Some(operation) = frozen {
                let mut permission = Err("无法读取原操作回执");
                let mut permit = None;
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
            let (Some(document), Some(read_token), Some(manifest)) = (
                snapshot.document,
                snapshot.read_token,
                self.components.clone(),
            ) else {
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
            let restore = self.state.borrow().record.runtime_created;
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
                        document,
                        client_id: ASSISTANT_CLIENT_ID.into(),
                        mcp_client_path: files.mcp_client,
                        mcp_environment,
                        session_directory: session_directory(&data)?,
                        provider,
                        read_token,
                        write_token: snapshot.write_token,
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
            let Some(document) = snapshot.document else {
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
            let restore = self.state.borrow().record.runtime_created;
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
                        document,
                        client_id: ASSISTANT_CLIENT_ID.into(),
                        mcp_client_path: mcp_client,
                        mcp_environment,
                        session_directory,
                        provider,
                        read_token,
                        write_token: snapshot.write_token,
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
            draft: String::new(),
            messages: Vec::new(),
            selected_model_id: None,
            selected_effort_id: None,
            selected_policy_revision: None,
            selected_effort_policy: None,
            generation_model_ids: HashMap::new(),
            selection_initialized: false,
            runtime_created: false,
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
                .map(|message| AssistantMessage {
                    role: message.role.as_str().into(),
                    text: message.text.as_str().into(),
                    state: message.state.as_str().into(),
                    progress: message.progress.as_str().into(),
                })
                .collect::<Vec<_>>(),
        ));
        State {
            record,
            generation: next_epoch(),
            messages,
            submitting: false,
            controller: Controller::default(),
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
            || (view.phase == Phase::Ready && !view.can_send && view.error.is_none())
    }
    fn session_uncertain(state: &State) -> bool {
        let view = state.controller.view();
        state.record.pending == "unknown"
            || view.phase == Phase::OutcomeUnknown
            || view.outcome == Some(TaskOutcome::OutcomeUnknown)
    }
    fn capture(state: &mut State) {
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
        let previous_pending = state.record.pending.clone();
        if view.phase == Phase::OutcomeUnknown || view.outcome == Some(TaskOutcome::OutcomeUnknown)
        {
            state.record.pending = "unknown".into();
        } else if state.submitting
            || matches!(
                view.phase,
                Phase::Running | Phase::AwaitingApproval | Phase::Stopping
            )
            || (view.phase == Phase::Ready && !view.can_send && view.error.is_none())
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
            row.state = if previous_pending == "unknown" {
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
        callback!(on_grant_read, permission, AssistantPermission::GrantRead);
        callback!(on_revoke_read, permission, AssistantPermission::RevokeRead);
        callback!(on_grant_write, permission, AssistantPermission::GrantWrite);
        callback!(on_renew_write, permission, AssistantPermission::RenewWrite);
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
                    binding.refresh(&app);
                    if app.global::<Assistant>().get_open() {
                        let mut state = binding.state.borrow_mut();
                        if let Err(error) = state.controller.stop() {
                            state.notice = error.message().into();
                        }
                        drop(state);
                        binding.refresh(&app);
                    }
                    binding.schedule(&app);
                }
            }
        });
        binding.refresh(app);
        binding.schedule(app);
        binding
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
                    session_id: state.record.id.clone(),
                    model_id: "model-one".into(),
                    effort_id: Some("balanced".into()),
                    policy: state.selected_effort().unwrap().policy(),
                    context: ContextReference {
                        project_key: "project".into(),
                        project_name: "测试工程".into(),
                    },
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
                role: "Agent".into(),
                text: "上一轮已经完成".into(),
                state: "已完成".into(),
                progress: "".into(),
            });
            state.messages.push(AssistantMessage {
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
