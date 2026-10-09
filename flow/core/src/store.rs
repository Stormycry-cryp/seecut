use crate::{Result, graph::Document, runner::Runtime};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// One process holds the OS lock for the entire session; a crashed process releases it.
/// Separate graph and execution snapshots: each attempt contains immutable input data.
pub struct Store {
    root: PathBuf,
    _lock: File,
    document: Document,
    runtime: Runtime,
    poisoned: bool,
}
impl Store {
    pub fn create(root: &Path, document: Document) -> Result<Self> {
        document.check_envelope()?;
        fs::create_dir(root).map_err(|e| e.to_string())?; // never overwrite a document
        let lock = lock(root)?;
        let runtime = Runtime::new(document.id.clone());
        atomic(root, "document.json", &document)?;
        atomic(root, "runtime.json", &runtime)?;
        Ok(Self {
            root: root.into(),
            _lock: lock,
            document,
            runtime,
            poisoned: false,
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let lock = lock(root)?;
        let document: Document = read(root, "document.json")?;
        document.check_envelope()?; // unknown nodes remain readable, execution validates separately
        let mut runtime: Runtime = read(root, "runtime.json")?;
        if runtime.format != 1 || runtime.document_id != document.id {
            return Err("run store identity/version mismatch".into());
        }
        runtime.validate()?;
        let mut recovered = false;
        for run in &mut runtime.runs {
            if run.status == "running" {
                run.status = "interrupted".into();
                recovered = true;
            }
        }
        if recovered {
            atomic(root, "runtime.json", &runtime)?;
        }
        Ok(Self {
            root: root.into(),
            _lock: lock,
            document,
            runtime,
            poisoned: false,
        })
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }
    pub fn save_document(&mut self, mut document: Document) -> Result<()> {
        self.ready()?;
        document.check_envelope()?;
        if document.revision != self.document.revision {
            return Err("document revision conflict".into());
        }
        if document.id != self.document.id || document.project_id != self.document.project_id {
            return Err("document identity change requires save-as".into());
        }
        // Future node versions are retained for viewing but never downgraded by an older writer.
        if self
            .document
            .graphs
            .values()
            .any(|g| g.nodes.values().any(|n| n.ports().is_err()))
        {
            return Err("unsupported node: document is read-only".into());
        }
        for (id, graph) in &document.graphs {
            graph.validate(false)?;
            if let Some(previous) = self.document.graphs.get(id)
                && graph != previous
                && graph.revision <= previous.revision
            {
                return Err("stale graph revision".into());
            }
        }
        document.revision += 1;
        self.write("document.json", &document)?;
        self.document = document;
        Ok(())
    }
    pub fn save_as(&self, root: &Path) -> Result<Self> {
        self.ready()?;
        let mut copy = self.document.clone();
        copy.source_document = Some(copy.id.clone());
        copy.id = crate::id();
        copy.operations.clear();
        // No active attempts, virtual balance, task IDs, or idempotency keys transfer.
        Self::create(root, copy)
    }
    pub(crate) fn save_runtime(&mut self, runtime: Runtime) -> Result<()> {
        self.ready()?;
        runtime.validate()?;
        self.write("runtime.json", &runtime)?;
        self.runtime = runtime;
        Ok(())
    }
    fn ready(&self) -> Result<()> {
        if self.poisoned {
            Err("storage outcome uncertain: close and reopen before further actions".into())
        } else {
            Ok(())
        }
    }
    fn write(&mut self, name: &str, value: &impl Serialize) -> Result<()> {
        if let Err(error) = atomic(&self.root, name, value) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }
}
fn lock(root: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("writer.lock"))
        .map_err(|e| e.to_string())?;
    file.try_lock()
        .map_err(|_| "Flow document is already open for writing".to_string())?;
    Ok(file)
}
fn read<T: DeserializeOwned>(root: &Path, name: &str) -> Result<T> {
    let path = root.join(name);
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("Flow snapshot exceeds 32 MiB".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("corrupt {name}; original preserved: {e}"))
}
fn atomic(root: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Flow snapshot exceeds 32 MiB".into());
    }
    let path = root.join(format!(".{name}.{}.tmp", crate::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&path, root.join(name))?;
        File::open(root)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.map_err(|e| format!("cannot persist {name}: {e}"))
}
