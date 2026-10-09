//! Isolated Flow domain and deterministic simulator. No network or user project access.
pub mod commands;
pub mod graph;
pub mod host;
pub mod protocol;
pub mod runner;
pub mod sample;
pub mod store;

pub type Result<T> = std::result::Result<T, String>;
pub(crate) fn next_revision(revision: u64) -> Result<u64> {
    revision.checked_add(1).ok_or("revision exhausted".into())
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn digest(value: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable domain value"))
    )
}
