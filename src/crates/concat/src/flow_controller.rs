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
        let before = self.graph().clone();
        let viewport_only = matches!(&command, Command::Viewport { .. });
        if let Command::Viewport { viewport } = &command {
            if before.viewport == *viewport {
                return Ok(vec![]);
            }
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
    New(PathBuf),
    Open(PathBuf),
    Close,
    Select(String),
    Navigate(i32),
    Quit,
    Edit(Command),
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
        session.edit(Command::Viewport { viewport: view })?;
        self.pending_view = None;
        Ok(())
    }
    pub fn change_draft(&mut self, generation: &str, text: String) -> Result<()> {
        let session = self.session.as_ref().ok_or("工程已关闭")?;
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
        if node.params["text"].as_str() != Some(draft.original.as_str()) {
            return Err("节点内容已改变，请先保留当前草稿并重新打开".into());
        }
        let mut params = node.params.clone();
        params["text"] = draft.text.clone().into();
        session.edit(Command::Parameters {
            node: draft.node,
            params,
        })?;
        self.draft.as_mut().expect("draft exists").original = draft.text;
        Ok(())
    }
    pub fn request(&mut self, intent: Intent) -> Result<Option<Effect>> {
        if self.pending.is_some() {
            return Ok(None);
        }
        self.flush_viewport()?;
        if let Intent::Select(node) = &intent {
            if self.session.as_ref().is_some_and(|s| s.selected == *node) {
                return Ok(Some(Effect::None));
            }
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
            Intent::New(path) => {
                let graph = Graph {
                    viewport: [60., 64., 0.8],
                    ..Graph::default()
                };
                let store = Store::create(&path, Document::new(graph))?;
                self.session = Some(Session::from_store(store, path)?);
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
            }
            Intent::Close => self.session = None,
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
                let created = session.edit(command)?;
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
            if node.kind != "prompt" {
                return None;
            }
            let text = node.params["text"].as_str()?.to_owned();
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
        c.request(Intent::Edit(Command::Add { node: n })).unwrap();
        (dir, c, id)
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
        c.request(Intent::Edit(Command::Add { node: second }))
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
