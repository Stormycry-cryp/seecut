// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable graph editing and identity-bound prompt drafts. No execution authority.
use seecut_flow_core::{
    Result,
    commands::{self, Command, Request},
    graph::{Document, Graph},
    id,
    store::Store,
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct History {
    undo: Vec<Graph>,
    redo: Vec<Graph>,
}
pub(crate) struct Session {
    pub store: Store,
    pub path: PathBuf,
    pub graph_id: String,
    pub token: String,
    pub selected: String,
    history: BTreeMap<String, History>,
}
impl Session {
    fn from_store(store: Store, path: PathBuf) -> Result<Self> {
        let graph_id = store
            .document()
            .graphs
            .keys()
            .next()
            .ok_or("工程没有图")?
            .clone();
        Ok(Self {
            store,
            path,
            graph_id,
            token: id(),
            selected: String::new(),
            history: BTreeMap::new(),
        })
    }
    pub fn graph(&self) -> &Graph {
        &self.store.document().graphs[&self.graph_id]
    }
    pub fn accept_asset(
        &mut self,
        scope: &seecut_flow_core::runner::Scope,
        request: &seecut_flow_core::host::AssetPickerRequest,
        asset: seecut_flow_core::host::LibraryAsset,
    ) -> Result<()> {
        let before = self.graph().clone();
        seecut_flow_core::host::accept_asset(&mut self.store, scope, request, asset)?;
        if before != *self.graph() {
            let history = self.history.entry(self.graph_id.clone()).or_default();
            history.undo.push(before);
            if history.undo.len() > 100 {
                history.undo.remove(0);
            }
            history.redo.clear();
        }
        Ok(())
    }
    pub fn can_undo(&self) -> bool {
        self.history
            .get(&self.graph_id)
            .is_some_and(|h| !h.undo.is_empty())
    }
    pub fn can_redo(&self) -> bool {
        self.history
            .get(&self.graph_id)
            .is_some_and(|h| !h.redo.is_empty())
    }
    pub fn edit(&mut self, command: Command) -> Result<Vec<String>> {
        if self.store.is_read_only() {
            return Err("此工程含不支持的节点，当前为只读".into());
        }
        let before = self.graph().clone();
        let viewport_only = matches!(&command, Command::Viewport { .. });
        if let Command::Viewport { viewport } = &command
            && before.viewport == *viewport
        {
            return Ok(vec![]);
        }
        let request = Request {
            document_id: self.store.document().id.clone(),
            graph_id: self.graph_id.clone(),
            expected_revision: before.revision,
            operation: id(),
            command,
        };
        let receipt = commands::apply(&mut self.store, request)?;
        if !viewport_only && before != *self.graph() {
            let history = self.history.entry(self.graph_id.clone()).or_default();
            history.undo.push(before);
            if history.undo.len() > 100 {
                history.undo.remove(0);
            }
            history.redo.clear();
        }
        if !self.selected.is_empty() && !self.graph().nodes.contains_key(&self.selected) {
            self.selected.clear();
        }
        Ok(receipt.created)
    }
    /// History is committed only after the durable graph snapshot succeeds.
    pub fn history(&mut self, redo: bool) -> Result<bool> {
        let Some(history) = self.history.get(&self.graph_id) else {
            return Ok(false);
        };
        let Some(mut next) = (if redo {
            history.redo.last()
        } else {
            history.undo.last()
        })
        .cloned() else {
            return Ok(false);
        };
        let before = self.graph().clone();
        next.revision = before.revision.checked_add(1).ok_or("图版本已耗尽")?;
        next.viewport = before.viewport;
        let mut document = self.store.document().clone();
        document.graphs.insert(self.graph_id.clone(), next);
        self.store.save_document(document)?;
        let history = self
            .history
            .get_mut(&self.graph_id)
            .expect("history exists");
        if redo {
            history.redo.pop();
            history.undo.push(before);
        } else {
            history.undo.pop();
            history.redo.push(before);
        }
        if !self.graph().nodes.contains_key(&self.selected) {
            self.selected.clear();
        }
        Ok(true)
    }
}
#[derive(Clone, serde::Serialize)]
struct Draft {
    generation: String,
    token: String,
    document: String,
    graph: String,
    node: String,
    original: String,
    text: String,
}
#[derive(Clone)]
pub(crate) enum Intent {
    #[cfg(test)]
    New(PathBuf),
    NewNamed(PathBuf, String),
    Open(PathBuf),
    OpenProject(PathBuf, String, String),
    RenameProject(String),
    Close,
    Select(String),
    Navigate(i32),
    Quit,
    Edit(Box<Command>),
    Undo,
    Redo,
}
#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    None,
    Navigate(i32),
    Quit,
}
#[derive(Default)]
pub(crate) struct Controller {
    pub session: Option<Session>,
    draft: Option<Draft>,
    pending: Option<Intent>,
    pending_view: Option<(String, [f64; 3])>,
}
impl Controller {
    /// Does not touch the possibly poisoned Store, nor consume any pending edit.
    pub fn export_recovery(&self, path: &Path) -> Result<()> {
        let s = self.session.as_ref().ok_or("工程已关闭")?;
        let data = serde_json::to_vec_pretty(&serde_json::json!({
            "format": "seecut-flow-recovery-v1",
            "source_path": s.path,
            "document": s.store.document(),
            "graph_id": s.graph_id,
            "selected_node": s.selected,
            "session_token": s.token,
            "draft": self.draft,
            "pending_viewport": self.pending_view,
        }))
        .map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| format!("无法新建备份，内存内容仍保留：{e}"))?;
        file.write_all(&data)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|e| format!("备份未完成，内存内容仍保留：{e}"))?;
        file.persist_noclobber(path)
            .map_err(|e| format!("无法完成备份，请选择未使用的文件名：{e}"))?;
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("备份已写入，但目录同步未确认；内存内容仍保留：{e}"))?;
        Ok(())
    }
    /// Only the explicit recovery-dialog discard action may bypass failed writes.
    pub fn discard_and_close(&mut self) {
        self.pending = None;
        self.pending_view = None;
        self.draft = None;
        self.session = None;
    }
    pub fn dirty(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| d.text != d.original)
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn prompt(&self) -> &str {
        self.draft.as_ref().map_or("", |d| d.text.as_str())
    }
    pub fn token_matches(&self, token: &str) -> bool {
        self.session.as_ref().is_some_and(|s| s.token == token)
    }
    pub fn draft_token(&self) -> &str {
        self.draft.as_ref().map_or("", |d| d.generation.as_str())
    }
    pub fn viewport(&self) -> Option<[f64; 3]> {
        self.pending_view
            .as_ref()
            .map(|v| v.1)
            .or_else(|| self.session.as_ref().map(|s| s.graph().viewport))
    }
    pub fn preview_viewport(&mut self, token: &str, view: [f64; 3]) -> Result<()> {
        if !self.token_matches(token) {
            return Err("工程已切换，忽略旧视口".into());
        }
        if view.iter().any(|v| !v.is_finite()) || view[2] <= 0. {
            return Err("无效视口".into());
        }
        self.pending_view = Some((token.to_owned(), view));
        Ok(())
    }
    pub fn flush_viewport(&mut self) -> Result<()> {
        let Some((token, view)) = self.pending_view.clone() else {
            return Ok(());
        };
        let session = self.session.as_mut().ok_or("工程已关闭")?;
        if session.token != token {
            return Err("视口归属已改变，未写入工程".into());
        }
        if session.store.is_read_only() {
            return Ok(()); // ephemeral navigation is safe; do not downgrade the original.
        }
        session.edit(Command::Viewport { viewport: view })?;
        self.pending_view = None;
        Ok(())
    }
    pub fn change_draft(&mut self, generation: &str, text: String) -> Result<()> {
        let session = self.session.as_ref().ok_or("工程已关闭")?;
        if session.store.is_read_only() {
            return Err("此工程为只读".into());
        }
        let draft = self.draft.as_mut().ok_or("未选择提示词节点")?;
        if generation != draft.generation
            || draft.token != session.token
            || draft.document != session.store.document().id
            || draft.graph != session.graph_id
            || draft.node != session.selected
        {
            return Err("草稿归属已失效，忽略旧输入".into());
        }
        draft.text = text;
        Ok(())
    }
    pub fn save_draft(&mut self) -> Result<()> {
        self.flush_viewport()?;
        if !self.dirty() {
            return Ok(());
        }
        let draft = self.draft.as_ref().ok_or("没有草稿")?.clone();
        let session = self.session.as_mut().ok_or("工程已关闭")?;
        if draft.token != session.token
            || draft.document != session.store.document().id
            || draft.graph != session.graph_id
            || draft.node != session.selected
        {
            return Err("草稿归属已改变，未写入工程".into());
        }
        let node = session
            .graph()
            .nodes
            .get(&draft.node)
            .ok_or("提示词节点不存在")?;
        let field = if node.kind == "prompt" {
            "text"
        } else {
            "prompt"
        };
        if node.params[field].as_str().unwrap_or("") != draft.original {
            return Err("节点内容已改变，请先保留当前草稿并重新打开".into());
        }
        let mut params = node.params.clone();
        params[field] = draft.text.clone().into();
        session.edit(Command::Parameters {
            node: draft.node,
            params,
        })?;
        self.draft.as_mut().expect("draft exists").original = draft.text;
        Ok(())
    }
    /// A delayed save only belongs to the draft that scheduled it.
    pub fn autosave_draft(&mut self, generation: &str) -> Result<bool> {
        if self.pending() || generation != self.draft_token() || !self.dirty() {
            return Ok(false);
        }
        self.save_draft()?;
        Ok(true)
    }
    pub fn request(&mut self, intent: Intent) -> Result<Option<Effect>> {
        if self.pending.is_some() {
            return Ok(None);
        }
        self.flush_viewport()?;
        if let Intent::Open(path) | Intent::OpenProject(path, _, _) = &intent
            && self
                .session
                .as_ref()
                .is_some_and(|s| same_path(&s.path, path))
        {
            if let Intent::OpenProject(_, document, project) = &intent {
                let current = self
                    .session
                    .as_ref()
                    .expect("session exists")
                    .store
                    .document();
                if current.id != *document || current.project_id != *project {
                    return Err("工程身份已改变".into());
                }
            }
            return Ok(Some(Effect::None));
        }
        if let Intent::Select(node) = &intent
            && self.session.as_ref().is_some_and(|s| s.selected == *node)
        {
            return Ok(Some(Effect::None));
        }
        if self.dirty() {
            self.pending = Some(intent);
            return Ok(None);
        }
        self.perform(intent).map(Some)
    }
    /// 0 cancels, 1 discards, 2 saves. A failed save leaves both draft and intent intact.
    pub fn resolve(&mut self, choice: i32) -> Result<Effect> {
        if choice == 0 {
            self.pending = None;
            return Ok(Effect::None);
        }
        if !matches!(choice, 1 | 2) {
            return Err("无效的草稿处理选项".into());
        }
        if choice == 2 {
            self.save_draft()?;
        }
        let Some(intent) = self.pending.take() else {
            return Ok(Effect::None);
        };
        // Retain the original draft if opening a different document fails.
        let backup = self.draft.clone();
        if choice == 1 {
            self.reload_draft();
        }
        match self.perform(intent.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.draft = backup;
                self.pending = Some(intent);
                Err(error)
            }
        }
    }
    fn perform(&mut self, intent: Intent) -> Result<Effect> {
        match intent {
            #[cfg(test)]
            Intent::New(path) => {
                let graph = Graph {
                    viewport: [60., 64., 0.8],
                    ..Graph::default()
                };
                let store = Store::create(&path, Document::new(graph))?;
                self.session = Some(Session::from_store(store, path)?);
                self.pending_view = None;
            }
            Intent::NewNamed(path, name) => {
                let title = seecut_flow_core::store::project_name(&name)?;
                let mut document = Document::new(Graph {
                    viewport: [60., 64., 0.8],
                    ..Graph::default()
                });
                document.title = Some(title);
                let store = Store::create(&path, document)?;
                self.session = Some(Session::from_store(store, path)?);
                self.pending_view = None;
            }
            Intent::Open(path) => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| same_path(&s.path, &path))
                {
                    return Ok(Effect::None);
                }
                let store = Store::open(&path)?;
                let next = Session::from_store(store, path)?;
                self.session = Some(next);
                self.pending_view = None;
            }
            Intent::OpenProject(path, document, project) => {
                let store = Store::open_project(&path, &document, &project)?;
                self.session = Some(Session::from_store(store, path)?);
                self.pending_view = None;
            }
            Intent::RenameProject(name) => {
                self.session
                    .as_mut()
                    .ok_or("工程已关闭")?
                    .store
                    .rename_project(&name)?;
            }
            Intent::Close => {
                self.session = None;
                self.pending_view = None;
            }
            Intent::Select(node) => {
                let session = self.session.as_mut().ok_or("请先打开工程")?;
                if !node.is_empty() && !session.graph().nodes.contains_key(&node) {
                    return Err("节点已不存在".into());
                }
                session.selected = node;
            }
            Intent::Navigate(page) => return Ok(Effect::Navigate(page)),
            Intent::Quit => return Ok(Effect::Quit),
            Intent::Edit(command) => {
                let session = self.session.as_mut().ok_or("请先打开工程")?;
                let created = session.edit(*command)?;
                if let Some(node) = created.first() {
                    session.selected = node.clone();
                }
            }
            Intent::Undo | Intent::Redo => {
                let session = self.session.as_mut().ok_or("请先打开工程")?;
                if !session.history(matches!(intent, Intent::Redo))? {
                    return Err("没有可撤销或重做的操作".into());
                }
            }
        }
        self.reload_draft();
        Ok(Effect::None)
    }
    fn reload_draft(&mut self) {
        self.draft = self.session.as_ref().and_then(|s| {
            let node = s.graph().nodes.get(&s.selected)?;
            if !matches!(
                node.kind.as_str(),
                "prompt" | "image_generate" | "video_generate"
            ) {
                return None;
            }
            let field = if node.kind == "prompt" {
                "text"
            } else {
                "prompt"
            };
            let text = node.params[field].as_str().unwrap_or("").to_owned();
            Some(Draft {
                generation: id(),
                token: s.token.clone(),
                document: s.store.document().id.clone(),
                graph: s.graph_id.clone(),
                node: node.id.clone(),
                original: text.clone(),
                text,
            })
        });
    }
}
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seecut_flow_core::graph::Node;
    use serde_json::json;
    fn open() -> (tempfile::TempDir, Controller, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Controller::default();
        c.request(Intent::New(dir.path().join("one"))).unwrap();
        let n = Node::new("prompt", json!({"text":"原文"}));
        let id = n.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node: n })))
            .unwrap();
        (dir, c, id)
    }
    #[test]
    fn connected_new_target_is_one_undo_and_persists_on_reopen() {
        let (dir, mut c, prompt) = open();
        let mut node = Node::new(
            "image_generate",
            json!({"model":"unconfigured","count":1,"prompt":""}),
        );
        node.position = [460., 240.];
        let id = node.id.clone();
        let edge = seecut_flow_core::graph::Edge {
            from: prompt,
            output: "out".into(),
            to: id.clone(),
            input: "prompt".into(),
            order: 0,
        };
        c.request(Intent::Edit(Box::new(Command::AddConnected {
            node,
            edge: edge.clone(),
        })))
        .unwrap();
        assert_eq!(c.session.as_ref().unwrap().graph().nodes.len(), 2);
        assert_eq!(
            c.session.as_ref().unwrap().graph().edges,
            vec![edge.clone()]
        );
        c.request(Intent::Undo).unwrap();
        assert_eq!(c.session.as_ref().unwrap().graph().nodes.len(), 1);
        assert!(c.session.as_ref().unwrap().graph().edges.is_empty());
        c.request(Intent::Redo).unwrap();
        c.request(Intent::Close).unwrap();
        c.request(Intent::Open(dir.path().join("one"))).unwrap();
        let g = c.session.as_ref().unwrap().graph();
        assert_eq!(g.nodes[&id].position, [460., 240.]);
        assert_eq!(g.edges, vec![edge]);
    }
    #[test]
    fn autosave_persists_latest_text_once_and_survives_reopen() {
        let (dir, mut c, node) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "输入中".into()).unwrap();
        c.change_draft(&token, "自动保存\n最新正文".into()).unwrap();
        assert!(c.autosave_draft(&token).unwrap());
        let revision = c.session.as_ref().unwrap().graph().revision;
        assert!(!c.autosave_draft(&token).unwrap());
        assert_eq!(c.session.as_ref().unwrap().graph().revision, revision);
        assert!(!c.dirty());
        c.request(Intent::Close).unwrap();
        c.request(Intent::Open(dir.path().join("one"))).unwrap();
        c.request(Intent::Select(node)).unwrap();
        assert_eq!(c.prompt(), "自动保存\n最新正文");
    }
    #[test]
    fn autosave_from_previous_node_never_saves_the_new_draft() {
        let (_dir, mut c, _) = open();
        let old = c.draft_token().to_owned();
        let node = Node::new(
            "image_generate",
            json!({"model":"unconfigured","count":1,"prompt":"独立正文"}),
        );
        c.request(Intent::Edit(Box::new(Command::Add { node })))
            .unwrap();
        let current = c.draft_token().to_owned();
        c.change_draft(&current, "新节点草稿".into()).unwrap();
        assert!(!c.autosave_draft(&old).unwrap());
        assert!(c.dirty());
        assert!(c.autosave_draft(&current).unwrap());
        // Closing before a pending debounce uses this same synchronous flush.
        c.change_draft(&current, "退出立即保留".into()).unwrap();
        c.save_draft().unwrap();
        assert_eq!(c.request(Intent::Quit).unwrap(), Some(Effect::Quit));
    }
    #[test]
    fn autosave_failure_preserves_draft_and_original_file() {
        let (dir, mut c, _) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "保存失败也要保留".into()).unwrap();
        let file = dir.path().join("one/document.json");
        let original = std::fs::read(&file).unwrap();
        let backup = dir.path().join("one/backup.json");
        std::fs::rename(&file, &backup).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(c.autosave_draft(&token).is_err());
        assert!(c.dirty());
        assert_eq!(c.prompt(), "保存失败也要保留");
        assert_eq!(std::fs::read(backup).unwrap(), original);
    }
    #[test]
    fn generation_drafts_remain_independent_through_history_and_reopen() {
        let (dir, mut c, _) = open();
        let first = Node::new(
            "image_generate",
            json!({"model":"unconfigured","count":1,"prompt":"第一份"}),
        );
        let first_id = first.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node: first })))
            .unwrap();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "中文\n自己的要求".into()).unwrap();
        c.save_draft().unwrap();
        let second = Node::new(
            "image_generate",
            json!({"model":"unconfigured","count":1,"prompt":"第二份"}),
        );
        let second_id = second.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node: second })))
            .unwrap();
        assert_eq!(c.prompt(), "第二份");
        assert!(c.change_draft(&token, "迟到输入".into()).is_err());
        c.request(Intent::Select(first_id.clone())).unwrap();
        assert_eq!(c.prompt(), "中文\n自己的要求");
        c.request(Intent::Undo).unwrap();
        assert_eq!(c.prompt(), "中文\n自己的要求");
        c.request(Intent::Redo).unwrap();
        c.request(Intent::Select(second_id)).unwrap();
        assert_eq!(c.prompt(), "第二份");
        c.request(Intent::Close).unwrap();
        c.request(Intent::Open(dir.path().join("one"))).unwrap();
        c.request(Intent::Select(first_id)).unwrap();
        assert_eq!(c.prompt(), "中文\n自己的要求");
    }
    #[test]
    fn navigation_and_quit_keep_drafts_until_the_original_intent_is_confirmed() {
        let (dir, mut c, node) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "导航前保存".into()).unwrap();
        assert_eq!(c.request(Intent::Navigate(3)).unwrap(), None);
        assert_eq!(c.resolve(0).unwrap(), Effect::None);
        assert!(!c.pending());
        assert!(c.dirty());
        assert_eq!(c.prompt(), "导航前保存");
        assert_eq!(c.session.as_ref().unwrap().selected, node);
        assert_eq!(c.request(Intent::Navigate(3)).unwrap(), None);
        assert_eq!(c.resolve(2).unwrap(), Effect::Navigate(3));
        assert!(!c.dirty());

        let token = c.draft_token().to_owned();
        c.change_draft(&token, "退出前保存".into()).unwrap();
        assert_eq!(c.request(Intent::Quit).unwrap(), None);
        assert_eq!(c.resolve(0).unwrap(), Effect::None);
        assert!(c.dirty());
        assert!(!c.pending());
        assert_eq!(c.prompt(), "退出前保存");
        assert_eq!(c.request(Intent::Quit).unwrap(), None);
        assert_eq!(c.resolve(2).unwrap(), Effect::Quit);
        assert!(!c.pending());
        assert!(!c.dirty());
        let saved: Document =
            serde_json::from_slice(&std::fs::read(dir.path().join("one/document.json")).unwrap())
                .unwrap();
        let graph_id = &c.session.as_ref().unwrap().graph_id;
        assert_eq!(
            saved.graphs[graph_id].nodes[&node].params["text"],
            "退出前保存"
        );
        assert_eq!(c.request(Intent::Quit).unwrap(), Some(Effect::Quit));
    }
    #[test]
    fn redo_intent_restores_the_node_in_memory_and_on_disk() {
        let (dir, mut c, node) = open();
        assert_eq!(c.request(Intent::Undo).unwrap(), Some(Effect::None));
        assert!(
            !c.session
                .as_ref()
                .unwrap()
                .graph()
                .nodes
                .contains_key(&node)
        );
        assert_eq!(c.request(Intent::Redo).unwrap(), Some(Effect::None));
        let session = c.session.as_ref().unwrap();
        assert_eq!(session.graph().nodes[&node].params["text"], "原文");
        assert!(session.selected.is_empty());
        let saved: Document =
            serde_json::from_slice(&std::fs::read(dir.path().join("one/document.json")).unwrap())
                .unwrap();
        let graph_id = &c.session.as_ref().unwrap().graph_id;
        assert_eq!(saved.graphs[graph_id].nodes[&node].params["text"], "原文");
    }
    #[test]
    fn draft_cancel_save_and_reopen_preserve_identity() {
        let (dir, mut c, node) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "中文\n第二行".into()).unwrap();
        assert_eq!(
            c.request(Intent::New(dir.path().join("two"))).unwrap(),
            None
        );
        c.resolve(0).unwrap();
        assert_eq!(c.prompt(), "中文\n第二行");
        assert_eq!(c.session.as_ref().unwrap().selected, node);
        c.request(Intent::Close).unwrap();
        c.resolve(2).unwrap();
        assert!(c.session.is_none());
        c.request(Intent::Open(dir.path().join("one"))).unwrap();
        c.request(Intent::Select(node)).unwrap();
        assert_eq!(c.prompt(), "中文\n第二行");
        assert!(c.change_draft(&token, "旧回调".into()).is_err());
    }
    #[test]
    fn pending_edit_cancel_save_and_undo_preserve_saved_prompt() {
        let (dir, mut c, original) = open();
        let generation = c.draft_token().to_owned();
        c.change_draft(&generation, "已保存的修改".into()).unwrap();
        let added = Node::new("prompt", json!({"text":"新节点提示词"}));
        let added_id = added.id.clone();
        assert_eq!(
            c.request(Intent::Edit(Box::new(Command::Add {
                node: added.clone(),
            })))
            .unwrap(),
            None
        );
        c.resolve(0).unwrap();
        assert!(!c.pending());
        assert!(c.dirty());
        assert_eq!(c.prompt(), "已保存的修改");
        assert_eq!(c.session.as_ref().unwrap().selected, original);
        assert_eq!(c.session.as_ref().unwrap().graph().nodes.len(), 1);
        assert!(
            !c.session
                .as_ref()
                .unwrap()
                .graph()
                .nodes
                .contains_key(&added_id)
        );
        assert_eq!(
            c.request(Intent::Edit(Box::new(Command::Add { node: added })))
                .unwrap(),
            None
        );
        c.resolve(2).unwrap();
        assert!(!c.pending());
        assert!(!c.dirty());
        let session = c.session.as_ref().unwrap();
        assert_eq!(session.graph().nodes.len(), 2);
        assert!(session.graph().nodes.contains_key(&added_id));
        let saved: Document =
            serde_json::from_slice(&std::fs::read(dir.path().join("one/document.json")).unwrap())
                .unwrap();
        let saved_graph = &saved.graphs[&session.graph_id];
        assert_eq!(saved_graph.nodes.len(), 2);
        assert_eq!(saved_graph.nodes[&original].params["text"], "已保存的修改");
        c.resolve(2).unwrap();
        assert_eq!(c.session.as_ref().unwrap().graph().nodes.len(), 2);
        c.request(Intent::Undo).unwrap();
        let graph = c.session.as_ref().unwrap().graph();
        assert_eq!(graph.nodes.len(), 1);
        assert!(!graph.nodes.contains_key(&added_id));
        assert_eq!(graph.nodes[&original].params["text"], "已保存的修改");
    }
    #[test]
    fn history_persists_before_moving_stacks() {
        let (_dir, mut c, _node) = open();
        let s = c.session.as_mut().unwrap();
        assert!(s.history(false).unwrap());
        assert!(s.graph().nodes.is_empty());
        assert!(!s.history(false).unwrap());
        assert!(s.history(true).unwrap());
        assert_eq!(s.graph().nodes.len(), 1);
        let path = s.path.clone();
        std::fs::rename(path.join("document.json"), path.join("document.saved.json")).unwrap();
        std::fs::create_dir(path.join("document.json")).unwrap();
        assert!(s.history(false).is_err());
        assert!(s.can_undo());
        assert!(!s.can_redo());
        assert_eq!(s.graph().nodes.len(), 1);
    }
    #[test]
    fn failed_save_retains_pending_draft_and_document() {
        let (dir, mut c, _node) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "未保存".into()).unwrap();
        c.request(Intent::New(dir.path().join("two"))).unwrap();
        let file = dir.path().join("one/document.json");
        std::fs::rename(&file, dir.path().join("one/document.saved.json")).unwrap();
        std::fs::create_dir(file).unwrap();
        assert!(c.resolve(2).is_err());
        assert!(c.pending());
        assert!(c.dirty());
        assert_eq!(c.prompt(), "未保存");
        assert_eq!(c.draft_token(), token);
        c.resolve(0).unwrap();
        assert!(c.dirty());
        assert!(!dir.path().join("two").exists());
    }
    #[test]
    fn failed_open_after_discard_keeps_original_draft() {
        let (dir, mut c, _) = open();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "草稿".into()).unwrap();
        c.request(Intent::Open(dir.path().join("missing"))).unwrap();
        assert!(c.resolve(1).is_err());
        assert_eq!(c.prompt(), "草稿");
        assert!(c.dirty());
    }
    #[test]
    fn old_prompt_generation_cannot_modify_new_selection() {
        let (_dir, mut c, first) = open();
        let old = c.draft_token().to_owned();
        let second = Node::new("prompt", json!({"text":"另一个节点"}));
        c.request(Intent::Edit(Box::new(Command::Add { node: second })))
            .unwrap();
        assert!(c.change_draft(&old, "旧组合输入".into()).is_err());
        assert_eq!(c.prompt(), "另一个节点");
        c.request(Intent::Select(first)).unwrap();
        assert!(c.change_draft(&old, "延迟回调".into()).is_err());
    }
    #[test]
    fn immediate_close_flushes_viewport_and_save_error_blocks_close() {
        let (dir, mut c, _) = open();
        let token = c.session.as_ref().unwrap().token.clone();
        c.preview_viewport(&token, [123., -42., 1.25]).unwrap();
        c.request(Intent::Close).unwrap();
        c.request(Intent::Open(dir.path().join("one"))).unwrap();
        assert_eq!(
            c.session.as_ref().unwrap().graph().viewport,
            [123., -42., 1.25]
        );
        assert!(c.preview_viewport(&token, [1., 2., 1.]).is_err());
        let token = c.session.as_ref().unwrap().token.clone();
        c.preview_viewport(&token, [456., 0., 1.]).unwrap();
        let file = dir.path().join("one/document.json");
        std::fs::rename(&file, dir.path().join("one/backup.json")).unwrap();
        std::fs::create_dir(file).unwrap();
        assert!(c.request(Intent::Close).is_err());
        assert!(c.session.is_some());
        assert_eq!(c.viewport(), Some([456., 0., 1.]));
    }
    #[test]
    fn failed_viewport_can_export_discard_and_reopen_from_disk() {
        let (dir, mut c, node) = open();
        let old_token = c.session.as_ref().unwrap().token.clone();
        let generation = c.draft_token().to_owned();
        c.change_draft(&generation, "恢复草稿\n第二行".into())
            .unwrap();
        c.preview_viewport(&old_token, [456., 0., 1.]).unwrap();
        let project = dir.path().join("one");
        let file = project.join("document.json");
        std::fs::rename(&file, project.join("backup.json")).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(c.request(Intent::Close).is_err());
        let recovery = dir.path().join("recovery.json");
        c.export_recovery(&recovery).unwrap();
        let snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(recovery).unwrap()).unwrap();
        assert_eq!(snapshot["draft"]["text"], "恢复草稿\n第二行");
        assert_eq!(snapshot["draft"]["node"], node);
        assert_eq!(snapshot["pending_viewport"][1], json!([456., 0., 1.]));
        assert!(c.dirty());
        c.discard_and_close();
        assert!(c.session.is_none());
        assert!(!c.dirty());
        assert!(!c.pending());
        assert_eq!(c.viewport(), None);
        // The broken disk remains an error; recovery must not reset or overwrite it.
        assert!(c.request(Intent::Open(project.clone())).is_err());
        assert!(c.session.is_none());
        std::fs::remove_dir(&file).unwrap();
        std::fs::rename(project.join("backup.json"), file).unwrap();
        c.request(Intent::Open(project)).unwrap();
        c.request(Intent::Select(node)).unwrap();
        assert_eq!(c.prompt(), "原文");
        assert_ne!(c.session.as_ref().unwrap().token, old_token);
        assert!(c.change_draft(&generation, "旧回调".into()).is_err());
    }
    #[test]
    fn failed_recovery_export_keeps_draft_pending_intent_and_existing_file() {
        let (dir, mut c, _) = open();
        let generation = c.draft_token().to_owned();
        c.change_draft(&generation, "尚未保存".into()).unwrap();
        c.request(Intent::Close).unwrap();
        let token = c.session.as_ref().unwrap().token.clone();
        c.preview_viewport(&token, [30., 42., 1.]).unwrap();
        let target = dir.path().join("existing.json");
        std::fs::write(&target, b"existing content").unwrap();
        assert!(c.export_recovery(&target).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"existing content");
        assert!(c.pending());
        assert_eq!(c.prompt(), "尚未保存");
        assert_eq!(c.draft_token(), generation);
        assert_eq!(c.viewport(), Some([30., 42., 1.]));
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use seecut_flow_core::graph::{Edge, Node};
    use serde_json::json;
    fn new(c: &mut Controller, path: PathBuf, name: &str) {
        c.request(Intent::NewNamed(path, name.into())).unwrap();
    }
    #[test]
    fn project_switch_cancel_save_discard_and_same_path_reenter_keep_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let mut c = Controller::default();
        new(&mut c, b.clone(), "B");
        let bd = c.session.as_ref().unwrap().store.document().clone();
        new(&mut c, a.clone(), "A");
        let node = Node::new("prompt", json!({"text":"原文"}));
        let id = node.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node })))
            .unwrap();
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "保存到 A".into()).unwrap();
        let ad = c.session.as_ref().unwrap().store.document().clone();
        assert_eq!(
            c.request(Intent::OpenProject(
                a.clone(),
                ad.id.clone(),
                ad.project_id.clone()
            ))
            .unwrap(),
            Some(Effect::None)
        );
        assert_eq!(c.draft_token(), token);
        assert!(c.dirty());
        let open_b = || Intent::OpenProject(b.clone(), bd.id.clone(), bd.project_id.clone());
        assert_eq!(c.request(open_b()).unwrap(), None);
        c.resolve(0).unwrap();
        assert_eq!(c.prompt(), "保存到 A");
        assert_eq!(c.session.as_ref().unwrap().path, a);
        c.request(open_b()).unwrap();
        c.resolve(2).unwrap();
        assert_eq!(c.session.as_ref().unwrap().store.document().id, bd.id);
        c.request(Intent::Open(a.clone())).unwrap();
        c.request(Intent::Select(id.clone())).unwrap();
        assert_eq!(c.prompt(), "保存到 A");
        let token = c.draft_token().to_owned();
        c.change_draft(&token, "放弃内容".into()).unwrap();
        c.request(open_b()).unwrap();
        c.resolve(1).unwrap();
        c.request(Intent::Open(a)).unwrap();
        c.request(Intent::Select(id)).unwrap();
        assert_eq!(c.prompt(), "保存到 A");
    }
    #[test]
    fn names_viewport_and_graph_survive_fresh_controller_without_crossed_view() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let mut c = Controller::default();
        new(&mut c, a.clone(), "A");
        let before = c.session.as_ref().unwrap().store.document().clone();
        let token = c.session.as_ref().unwrap().token.clone();
        c.preview_viewport(&token, [-230., 72., 1.2]).unwrap();
        c.request(Intent::RenameProject("重命名 A".into())).unwrap();
        new(&mut c, b, "B");
        assert_eq!(c.viewport(), Some([60., 64., 0.8]));
        c.request(Intent::Close).unwrap();
        drop(c);
        let mut c = Controller::default();
        c.request(Intent::OpenProject(a, before.id.clone(), before.project_id))
            .unwrap();
        assert_eq!(c.viewport(), Some([-230., 72., 1.2]));
        let doc = c.session.as_ref().unwrap().store.document();
        assert_eq!(doc.id, before.id);
        assert_eq!(doc.title.as_deref(), Some("重命名 A"));
    }
    #[test]
    fn future_node_navigation_is_ephemeral_and_old_writer_never_reserializes_raw_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future");
        let mut node = Node::new("prompt", json!({"text":"opaque"}));
        node.version = 999;
        let mut graph = Graph::default();
        graph.nodes.insert(node.id.clone(), node);
        drop(Store::create(&path, Document::new(graph)).unwrap());
        let before = std::fs::read(path.join("document.json")).unwrap();
        let mut c = Controller::default();
        c.request(Intent::Open(path.clone())).unwrap();
        let token = c.session.as_ref().unwrap().token.clone();
        c.preview_viewport(&token, [1., 2., 0.4]).unwrap();
        assert!(
            c.request(Intent::RenameProject("forbidden".into()))
                .is_err()
        );
        assert!(c.request(Intent::Edit(Box::new(Command::Arrange))).is_err());
        c.request(Intent::Close).unwrap();
        assert_eq!(c.viewport(), None);
        assert_eq!(before, std::fs::read(path.join("document.json")).unwrap());
    }
    #[test]
    fn duplicate_create_and_invalid_connection_leave_existing_graph_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let mut c = Controller::default();
        new(&mut c, path.clone(), "A");
        let prompt = Node::new("prompt", json!({"text":"A"}));
        let p = prompt.id.clone();
        let image = Node::new("image_generate", json!({"model":"unconfigured","count":1}));
        let i = image.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node: prompt })))
            .unwrap();
        c.request(Intent::Edit(Box::new(Command::Add { node: image })))
            .unwrap();
        let before = c.session.as_ref().unwrap().store.document().clone();
        assert!(
            c.request(Intent::NewNamed(path.clone(), "duplicate".into()))
                .is_err()
        );
        assert!(
            c.request(Intent::Edit(Box::new(Command::Connect {
                edge: Edge {
                    from: i,
                    output: "out".into(),
                    to: p,
                    input: "prompt".into(),
                    order: 0
                }
            })))
            .is_err()
        );
        assert_eq!(&before, c.session.as_ref().unwrap().store.document());
        c.request(Intent::Close).unwrap();
        c.request(Intent::Open(path)).unwrap();
        assert_eq!(c.session.as_ref().unwrap().graph().nodes.len(), 2);
    }
    #[test]
    fn native_picker_edit_is_one_undo_and_rejects_stale_callback() {
        use seecut_flow_core::{
            graph::MediaKind,
            host::{LibraryAsset, picker_request_for_store},
            runner::{Asset, Scope},
        };
        let dir = tempfile::tempdir().unwrap();
        let mut c = Controller::default();
        new(&mut c, dir.path().join("picker"), "Picker");
        let node = Node::new(
            "asset",
            json!({"asset_id":"old","content_version":"old-version","media_kind":"Image"}),
        );
        let id = node.id.clone();
        c.request(Intent::Edit(Box::new(Command::Add { node })))
            .unwrap();
        let s = c.session.as_mut().unwrap();
        let scope = Scope::synthetic();
        let before = s.graph().clone();
        let request = picker_request_for_store(&s.store, &scope, &s.graph_id, &id).unwrap();
        let selected = LibraryAsset {
            asset: Asset {
                id: "new".into(),
                version: "new-version".into(),
                kind: MediaKind::Image,
                simulated: false,
            },
            name: "new image".into(),
            available: true,
        };
        s.accept_asset(&scope, &request, selected.clone()).unwrap();
        let changed = s.graph().clone();
        s.accept_asset(&scope, &request, selected).unwrap();
        assert_eq!(changed, *s.graph());
        s.history(false).unwrap();
        assert_eq!(s.graph().nodes[&id], before.nodes[&id]);
        s.history(true).unwrap();
        assert_eq!(s.graph().nodes[&id], changed.nodes[&id]);
        let stale = LibraryAsset {
            asset: Asset {
                id: "later".into(),
                version: "later".into(),
                kind: MediaKind::Image,
                simulated: false,
            },
            name: "late".into(),
            available: true,
        };
        assert!(s.accept_asset(&scope, &request, stale).is_err());
    }
}
