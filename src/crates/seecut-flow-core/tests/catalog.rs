use seecut_flow_core::{
    catalog,
    graph::{Document, Graph, Node},
    store::Store,
};
use serde_json::json;
use std::fs;

#[test]
fn listing_is_read_only_even_with_a_live_writer_and_recovers_without_index() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy-name");
    let store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let before = fs::read(path.join("runtime.json")).unwrap();
    let p = catalog::inspect(&path).unwrap();
    assert_eq!(p.title, "legacy-name");
    catalog::remember(root.path(), &p).unwrap();
    catalog::remember(root.path(), &p).unwrap();
    assert_eq!(catalog::list(root.path()).projects.len(), 1);
    fs::remove_dir_all(root.path().join(".project-catalog")).unwrap();
    assert_eq!(catalog::list(root.path()).projects.len(), 1);
    assert_eq!(before, fs::read(path.join("runtime.json")).unwrap());
    assert!(Store::open(&path).is_err());
    drop(store);
}
#[test]
fn rename_preserves_ids_graph_receipts_runtime_and_increments_document_revision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let old = store.document().clone();
    let runtime = fs::read(path.join("runtime.json")).unwrap();
    store.rename_project("  A / 工程  ").unwrap();
    let next = store.document();
    assert_eq!(next.title.as_deref(), Some("A / 工程"));
    assert_eq!(
        (&next.id, &next.project_id, &next.graphs, &next.operations),
        (&old.id, &old.project_id, &old.graphs, &old.operations)
    );
    assert_eq!(next.revision, old.revision + 1);
    assert!(store.rename_project("\n").is_err());
    assert_eq!(runtime, fs::read(path.join("runtime.json")).unwrap());
    drop(store);
    assert_eq!(catalog::inspect(&path).unwrap().title, "A / 工程");
}
#[test]
fn stale_catalog_identity_is_rejected_before_runtime_writes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one");
    drop(Store::create(&path, Document::new(Graph::default())).unwrap());
    let before = fs::read(path.join("runtime.json")).unwrap();
    let p = catalog::inspect(&path).unwrap();
    catalog::remember(root.path(), &p).unwrap();
    assert!(Store::open_project(&path, "stale", &p.project_id).is_err());
    assert_eq!(before, fs::read(path.join("runtime.json")).unwrap());
    let mut record: serde_json::Value = serde_json::from_slice(
        &fs::read(
            root.path()
                .join(".project-catalog")
                .join(format!("{}.json", p.key())),
        )
        .unwrap(),
    )
    .unwrap();
    record["project"]["document_id"] = "stale".into();
    fs::write(
        root.path()
            .join(".project-catalog")
            .join(format!("{}.json", p.key())),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let list = catalog::list(root.path());
    assert!(list.projects.is_empty());
    assert_eq!(list.warnings.len(), 1);
}
#[test]
fn incomplete_create_and_corrupt_index_do_not_hide_complete_owned_projects() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one");
    drop(Store::create(&path, Document::new(Graph::default())).unwrap());
    assert!(Store::create(&path, Document::new(Graph::default())).is_err());
    fs::create_dir(root.path().join("partial")).unwrap();
    fs::write(
        root.path().join("partial/document.json"),
        fs::read(path.join("document.json")).unwrap(),
    )
    .unwrap();
    fs::create_dir(root.path().join(".project-catalog")).unwrap();
    fs::write(root.path().join(".project-catalog/broken.json"), b"{").unwrap();
    let listing = catalog::list(root.path());
    assert_eq!(listing.projects.len(), 1);
    assert_eq!(listing.warnings.len(), 2);
}
#[test]
fn external_legacy_and_save_as_have_distinct_entries_and_future_nodes_keep_raw_file() {
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let path = external.path().join("legacy");
    let store = Store::create(&path, Document::new(Graph::default())).unwrap();
    drop(store.save_as(&root.path().join("copy")).unwrap());
    drop(store);
    let p = catalog::inspect(&path).unwrap();
    catalog::remember(root.path(), &p).unwrap();
    let entries = catalog::list(root.path()).projects;
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].key(), entries[1].key());
    assert_eq!(entries[0].project_id, entries[1].project_id);
    let mut raw: serde_json::Value =
        serde_json::from_slice(&fs::read(path.join("document.json")).unwrap()).unwrap();
    let graph = raw["graphs"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    let mut node = Node::new("prompt", json!({"text":"legacy"}));
    node.version = 900;
    graph["nodes"][&node.id] = serde_json::to_value(&node).unwrap();
    raw["future-field"] = json!({"opaque": [1,2,3]});
    let bytes = serde_json::to_vec_pretty(&raw).unwrap();
    fs::write(path.join("document.json"), &bytes).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert!(store.is_read_only());
    assert!(store.rename_project("changed").is_err());
    assert!(catalog::inspect(&path).unwrap().read_only);
    assert_eq!(bytes, fs::read(path.join("document.json")).unwrap());
}
