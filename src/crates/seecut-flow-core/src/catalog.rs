//! Recoverable project catalogue. Listing never opens a writer or recovers runs.
use crate::{Result, graph::Document, runner::Runtime};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
    pub document_id: String,
    pub project_id: String,
    pub title: String,
    pub modified: u64,
    pub nodes: usize,
    pub read_only: bool,
}
impl Project {
    pub fn key(&self) -> String {
        crate::digest(&self.path)
    }
}
#[derive(Serialize, Deserialize)]
struct Record {
    format: u32,
    project: Project,
}
pub struct Listing {
    pub projects: Vec<Project>,
    pub warnings: Vec<String>,
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("工程文件过大，原文件已保留".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
/// Validate complete legacy/native projects without taking their writer lock.
pub fn inspect(path: &Path) -> Result<Project> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let document: Document = read(&path.join("document.json"))?;
    document.check_envelope()?;
    let runtime: Runtime = read(&path.join("runtime.json"))?;
    if runtime.format != 1 || runtime.document_id != document.id {
        return Err("工程运行记录身份或版本不匹配".into());
    }
    runtime.validate()?;
    let title = document
        .title
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    let modified = fs::metadata(path.join("document.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |v| v.as_secs());
    Ok(Project {
        path,
        document_id: document.id,
        project_id: document.project_id,
        title,
        modified,
        nodes: document.graphs.values().map(|g| g.nodes.len()).sum(),
        read_only: document
            .graphs
            .values()
            .any(|g| g.nodes.values().any(|n| n.ports().is_err())),
    })
}
/// Each external path has its own atomic record; owned projects can rebuild the index.
pub fn remember(root: &Path, project: &Project) -> Result<()> {
    let index = root.join(".project-catalog");
    fs::create_dir_all(&index).map_err(|e| e.to_string())?;
    crate::store::atomic(
        &index,
        &format!("{}.json", project.key()),
        &Record {
            format: 1,
            project: project.clone(),
        },
    )
}
pub fn list(root: &Path) -> Listing {
    let mut projects = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut replaced = BTreeSet::new();
    // Read remembered identities first. A replaced path must not silently adopt another project.
    let index = root.join(".project-catalog");
    if index.exists() {
        match fs::read_dir(&index) {
            Ok(entries) => {
                for entry in entries {
                    let result = (|| {
                        let entry = entry.map_err(|e| e.to_string())?;
                        if entry.path().extension().is_none_or(|e| e != "json") {
                            return Ok(());
                        }
                        let record: Record = read(&entry.path())?;
                        if record.format != 1 {
                            return Err("工程索引版本不支持".into());
                        }
                        let project = inspect(&record.project.path)?;
                        if project.document_id != record.project.document_id
                            || project.project_id != record.project.project_id
                        {
                            replaced.insert(project.path.clone());
                            return Err(format!(
                                "{} 的工程身份已改变，请重新选择文件夹",
                                record.project.title
                            ));
                        }
                        projects.insert(project.path.clone(), project);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        warnings.push(error);
                    }
                }
            }
            Err(error) => warnings.push(error.to_string()),
        }
    }
    if root.exists() {
        match fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.join("document.json").is_file() {
                        continue;
                    }
                    match inspect(&path) {
                        Ok(project) if replaced.contains(&project.path) => {}
                        Ok(project) => {
                            projects.entry(project.path.clone()).or_insert(project);
                        }
                        Err(error) => warnings.push(format!(
                            "{}：{}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            error
                        )),
                    }
                }
            }
            Err(error) => warnings.push(error.to_string()),
        }
    }
    let mut projects = projects.into_values().collect::<Vec<_>>();
    projects.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.title.cmp(&b.title))
    });
    Listing { projects, warnings }
}
