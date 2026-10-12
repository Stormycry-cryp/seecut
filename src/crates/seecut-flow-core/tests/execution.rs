use seecut_flow_core::{
    execution::{Execution, Phase},
    graph::{Document, Graph, MediaKind},
    host::{Context, GenerationDraft},
    runner::{Asset, Scope},
    store::Store,
};
use serde_json::json;

fn scope() -> Scope {
    Scope {
        account: "account-one".into(),
        service: "service-one".into(),
    }
}
fn draft(document: &Document) -> GenerationDraft {
    GenerationDraft {
        context: Context {
            document_id: document.id.clone(),
            graph_id: "graph".into(),
            node_id: "node".into(),
            revision: 1,
            scope: scope(),
        },
        model_id: "model".into(),
        capability_version: "catalog-revision".into(),
        media_kind: MediaKind::Image,
        prompt: "synthetic".into(),
        parameters: json!({"model":"model","count":1}),
        references: vec![],
    }
}
fn intent(store: &mut Store) -> String {
    let mut e = store.execution().clone();
    let id = e.begin(draft(store.document()), "a".repeat(64)).unwrap();
    e.intent(
        &id,
        json!({"model":"model","prompt":"synthetic","quote_id":"quote"}),
    )
    .unwrap();
    store.save_execution(e).unwrap();
    id
}
fn task(id: &str, credits: u64) -> serde_json::Value {
    json!({"id":id,"kind":"image","model":"model","request":{"model":"model","prompt":"synthetic"},
        "status":"succeeded","captured_credits":credits,"outputs":[{"id":"out_one","sha256":"b".repeat(64)}]})
}
fn asset() -> Asset {
    Asset {
        id: "library-id".into(),
        version: json!({"sha256":"b".repeat(64),"registration_id":"operation:original"})
            .to_string(),
        kind: MediaKind::Image,
        simulated: false,
    }
}
#[test]
fn old_simulator_runtime_is_unchanged_and_real_intent_without_task_reopens() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    assert!(!path.join("execution.json").exists());
    let runtime = std::fs::read(path.join("runtime.json")).unwrap();
    let id = intent(&mut store);
    drop(store);
    let store = Store::open(&path).unwrap();
    let a = &store.execution().attempts[0];
    assert_eq!(a.id, id);
    assert_eq!(a.phase, Phase::Intent);
    assert!(a.tasks.is_empty());
    assert_eq!(runtime, std::fs::read(path.join("runtime.json")).unwrap());
    assert!(store.runtime().tasks.is_empty());
    assert_eq!(store.runtime().virtual_spent, 0);
}
#[test]
fn duplicate_click_and_view_revision_change_reuse_frozen_inflight_attempt() {
    let d = Document::new(Graph::default());
    let mut e = Execution::new(d.id.clone(), d.project_id.clone());
    let mut input = draft(&d);
    let first = e.begin(input.clone(), "a".repeat(64)).unwrap();
    input.context.revision += 1;
    assert_eq!(first, e.begin(input, "a".repeat(64)).unwrap());
    assert_eq!(e.attempts.len(), 1);
    e.paused = true;
    assert!(e.begin(draft(&d), "a".repeat(64)).is_err());
}
#[test]
fn changed_graph_and_paused_execution_still_accept_original_late_task() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let mut next = store.document().clone();
    next.title = Some("changed".into());
    store.save_document(next).unwrap();
    let mut e = store.execution().clone();
    e.paused = true;
    e.observe(&id, &scope(), &task("gen_original", 7)).unwrap();
    e.registered(&id, "gen_original", "out_one", asset())
        .unwrap();
    store.save_execution(e).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.execution().paused);
    assert_eq!(store.execution().attempts[0].phase, Phase::Finished);
    assert_eq!(store.execution().actual_credits(&scope()).unwrap(), 7);
}
#[test]
fn account_scope_and_frozen_request_mismatch_leave_snapshot_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let before = std::fs::read(path.join("execution.json")).unwrap();
    let mut e = store.execution().clone();
    let wrong = Scope {
        account: "other".into(),
        ..scope()
    };
    assert!(e.observe(&id, &wrong, &task("gen_original", 7)).is_err());
    let mut wrong = task("gen_original", 7);
    wrong["request"]["prompt"] = json!("changed");
    assert!(e.observe(&id, &scope(), &wrong).is_err());
    assert_eq!(before, std::fs::read(path.join("execution.json")).unwrap());
    assert!(store.execution().attempts[0].tasks.is_empty());
}
#[test]
fn registration_selection_and_restart_keep_same_asset_and_version() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let mut e = store.execution().clone();
    e.observe(&id, &scope(), &task("gen_original", 7)).unwrap();
    assert!(e.select(&id, "gen_original", "out_one", &scope()).is_err());
    let mut bad = asset();
    bad.version =
        json!({"sha256":"c".repeat(64),"registration_id":"operation:original"}).to_string();
    assert!(e.registered(&id, "gen_original", "out_one", bad).is_err());
    e.registered(&id, "gen_original", "out_one", asset())
        .unwrap();
    e.registered(&id, "gen_original", "out_one", asset())
        .unwrap();
    assert_eq!(
        e.select(&id, "gen_original", "out_one", &scope()).unwrap(),
        asset()
    );
    assert!(
        e.select("other-batch", "gen_original", "out_one", &scope())
            .is_err()
    );
    store.save_execution(e).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.execution().attempts[0].selected,
        Some(("gen_original".into(), "out_one".into()))
    );
}
#[test]
fn repeated_queries_batch_aliases_and_multiple_nodes_do_not_double_count_charges() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let mut e = store.execution().clone();
    for _ in 0..3 {
        e.observe(&id, &scope(), &task("gen_one", 7)).unwrap();
    }
    let mut second = e.attempts[0].clone();
    second.id = "second-node".into();
    second.key.clone_from(&second.id);
    second.draft.context.node_id = "second".into();
    second.fingerprint = seecut_flow_core::execution::fingerprint(&second.draft);
    e.attempts.push(second);
    assert_eq!(e.actual_credits(&scope()).unwrap(), 7);
    e.observe("second-node", &scope(), &task("gen_two", 12))
        .unwrap();
    assert_eq!(e.actual_credits(&scope()).unwrap(), 19);
    let mut stale = task("gen_one", 0);
    stale["status"] = json!("pending_reconcile");
    stale["outputs"] = json!([]);
    e.observe(&id, &scope(), &stale).unwrap();
    assert_eq!(e.actual_credits(&scope()).unwrap(), 19);
    assert_eq!(
        e.actual_credits(&Scope {
            account: "other".into(),
            ..scope()
        })
        .unwrap(),
        0
    );
    store.save_execution(e).unwrap();
    drop(store);
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .execution()
            .actual_credits(&scope())
            .unwrap(),
        19
    );
}
#[test]
fn missing_or_changed_output_checksums_and_nonactual_credit_fields_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let mut e = store.execution().clone();
    let mut bad = task("gen_one", 7);
    bad["outputs"][0]["sha256"] = serde_json::Value::Null;
    e.observe(&id, &scope(), &bad).unwrap();
    assert_eq!(e.actual_credits(&scope()).unwrap(), 7);
    assert!(e.attempts[0].tasks["gen_one"].outputs.is_empty());
    assert!(e.attempts[0].tasks["gen_one"].output_error);
    bad = task("gen_one", 7);
    bad.as_object_mut().unwrap().remove("captured_credits");
    bad["quoted_credits"] = json!(7);
    assert!(e.observe(&id, &scope(), &bad).is_err());
    e.observe(&id, &scope(), &task("gen_one", 7)).unwrap();
    bad = task("gen_one", 7);
    bad["outputs"][0]["sha256"] = json!("c".repeat(64));
    e.observe(&id, &scope(), &bad).unwrap();
    assert_eq!(e.actual_credits(&scope()).unwrap(), 7);
    assert!(e.attempts[0].tasks["gen_one"].output_error);
    assert_eq!(
        e.attempts[0].tasks["gen_one"].outputs["out_one"].sha256,
        "b".repeat(64)
    );
}
#[test]
fn failed_first_save_never_publishes_an_intent_authorizing_external_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    std::fs::create_dir(path.join("execution.json")).unwrap();
    let mut e = store.execution().clone();
    e.begin(draft(store.document()), "a".repeat(64)).unwrap();
    assert!(store.save_execution(e).is_err());
    assert!(store.execution().attempts.is_empty());
    assert!(
        store
            .rename_project("cannot continue poisoned writer")
            .is_err()
    );
}

#[test]
fn unknown_original_key_is_reused_and_changed_inputs_cannot_create_another() {
    let d = Document::new(Graph::default());
    let mut e = Execution::new(d.id.clone(), d.project_id.clone());
    let input = draft(&d);
    let id = e.begin(input.clone(), "a".repeat(64)).unwrap();
    e.intent(
        &id,
        json!({"model":"model","prompt":"synthetic","quote_id":"quote"}),
    )
    .unwrap();
    assert_eq!(e.begin(input.clone(), "a".repeat(64)).unwrap(), id);
    let mut changed = input.clone();
    changed.prompt = "changed".into();
    assert!(e.begin(changed.clone(), "a".repeat(64)).is_err());
    let mut unknown = task("gen_one", 0);
    unknown["status"] = json!("pending_reconcile");
    unknown["outputs"] = json!([]);
    e.observe(&id, &scope(), &unknown).unwrap();
    assert_eq!(e.begin(input, "a".repeat(64)).unwrap(), id);
    assert!(e.begin(changed, "a".repeat(64)).is_err());
    assert_eq!(e.attempts.len(), 1);
}

#[test]
fn downstream_requires_selected_current_semantic_upstream_version() {
    use seecut_flow_core::{
        graph::{Edge, Node},
        host::image_draft,
    };
    let root = tempfile::tempdir().unwrap();
    let (mut doc, g, _) = seecut_flow_core::sample::document();
    let graph = doc.graphs.get_mut(&g).unwrap();
    for id in ["video", "pick_video", "delivery"] {
        graph.nodes.remove(id);
    }
    graph.edges.retain(|e| {
        !["video", "pick_video", "delivery"].contains(&e.from.as_str())
            && !["video", "pick_video", "delivery"].contains(&e.to.as_str())
    });
    graph.nodes.get_mut("image").unwrap().params =
        json!({"model":"model","capability_version":"catalog-revision","count":1});
    let mut downstream = Node::new(
        "image_generate",
        json!({"model":"model","capability_version":"catalog-revision","count":1}),
    );
    downstream.id = "downstream".into();
    graph.nodes.insert(downstream.id.clone(), downstream);
    for (from, input) in [("prompt", "prompt"), ("pick_image", "references")] {
        graph
            .connect(Edge {
                from: from.into(),
                output: "out".into(),
                to: "downstream".into(),
                input: input.into(),
                order: 0,
            })
            .unwrap();
    }
    let mut store = Store::create(&root.path().join("project"), doc).unwrap();
    let upstream = image_draft(&store, &scope(), &g, "image").unwrap();
    let mut e = store.execution().clone();
    let id = e.begin(upstream.clone(), "a".repeat(64)).unwrap();
    e.intent(
        &id,
        json!({"quote_id":"quote","model":"model","prompt":upstream.prompt}),
    )
    .unwrap();
    let mut receipt = task("gen_one", 7);
    receipt["request"]["prompt"] = json!(upstream.prompt);
    e.observe(&id, &scope(), &receipt).unwrap();
    e.registered(&id, "gen_one", "out_one", asset()).unwrap();
    e.select(&id, "gen_one", "out_one", &scope()).unwrap();
    store.save_execution(e).unwrap();
    assert_eq!(
        image_draft(&store, &scope(), &g, "downstream")
            .unwrap()
            .references,
        vec![asset()]
    );
    let mut changed = store.document().clone();
    let graph = changed.graphs.get_mut(&g).unwrap();
    graph.nodes.get_mut("image").unwrap().position = [250., 100.];
    graph.revision += 1;
    store.save_document(changed).unwrap();
    assert!(image_draft(&store, &scope(), &g, "downstream").is_ok());
    let mut changed = store.document().clone();
    changed
        .graphs
        .get_mut(&g)
        .unwrap()
        .nodes
        .get_mut("prompt")
        .unwrap()
        .params["text"] = json!("new prompt");
    changed.graphs.get_mut(&g).unwrap().revision += 1;
    store.save_document(changed).unwrap();
    assert!(image_draft(&store, &scope(), &g, "downstream").is_err());
    assert_eq!(
        store.execution().attempts[0].selected,
        Some(("gen_one".into(), "out_one".into()))
    );
}

#[test]
fn remote_expiry_does_not_skip_known_output_local_registration_recovery() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("project");
    let mut store = Store::create(&path, Document::new(Graph::default())).unwrap();
    let id = intent(&mut store);
    let mut e = store.execution().clone();
    e.observe(&id, &scope(), &task("gen_one", 7)).unwrap();
    let mut expired = task("gen_one", 7);
    expired["status"] = json!("expired");
    expired["outputs"] = json!([]);
    e.observe(&id, &scope(), &expired).unwrap();
    assert_eq!(e.attempts[0].phase, Phase::Accepted);
    store.save_execution(e).unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    let mut e = store.execution().clone();
    e.registered(&id, "gen_one", "out_one", asset()).unwrap();
    assert_eq!(e.attempts[0].phase, Phase::Finished);
    assert_eq!(e.actual_credits(&scope()).unwrap(), 7);
    store.save_execution(e).unwrap();
}
