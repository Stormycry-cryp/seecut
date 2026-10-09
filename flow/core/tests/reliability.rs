use seecut_flow_core::{
    graph::{Document, Edge, Graph, History, Node},
    runner::{Fault, Outcome, Output, Runner, Scope, Status},
    sample,
    store::Store,
};
use serde_json::json;
use tempfile::TempDir;
fn fixture() -> (TempDir, Runner, String) {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, graph, _) = sample::document();
    let store = Store::create(&tmp.path().join("document"), doc).unwrap();
    (tmp, Runner::new(store, Scope::synthetic()).unwrap(), graph)
}
fn choice(r: &mut Runner, g: &str, source: &str, target: &str) {
    let Output::Candidates { assets, .. } = r.current_output(g, source).unwrap() else {
        panic!()
    };
    r.choose(g, target, &assets[0].id).unwrap();
}
fn prepare_image(r: &mut Runner, g: &str) {
    assert!(matches!(
        r.run_to(g, "image").unwrap(),
        Outcome::Complete(_)
    ));
}
#[test]
fn end_to_end_requires_each_choice_and_reopens_same_output() {
    let (tmp, mut r, g) = fixture();
    assert!(
        matches!(r.run_to(&g, "delivery").unwrap(), Outcome::Paused { node, .. } if node == "pick_image")
    );
    assert_eq!(r.store.runtime().virtual_spent, 20);
    choice(&mut r, &g, "image", "pick_image");
    assert!(
        matches!(r.run_to(&g, "delivery").unwrap(), Outcome::Paused { node, .. } if node == "pick_video")
    );
    choice(&mut r, &g, "video", "pick_video");
    let result = r.run_to(&g, "delivery").unwrap();
    assert!(matches!(result, Outcome::Complete(_)));
    assert_eq!(r.store.runtime().virtual_spent, 30);
    assert_eq!(r.store.runtime().deliveries.len(), 1);
    drop(r);
    let mut reopened = Runner::new(
        Store::open(&tmp.path().join("document")).unwrap(),
        Scope::synthetic(),
    )
    .unwrap();
    assert_eq!(reopened.run_to(&g, "delivery").unwrap(), result);
    assert_eq!(reopened.store.runtime().virtual_spent, 30);
    assert_eq!(reopened.store.runtime().deliveries.len(), 1);
}
#[test]
fn single_node_never_runs_ancestors() {
    let (_tmp, mut r, g) = fixture();
    assert!(r.run_node(&g, "image", false, Fault::None).is_err());
    assert!(r.store.runtime().attempts.is_empty());
    assert_eq!(r.store.runtime().virtual_spent, 0);
}
#[test]
fn duplicate_click_and_move_reuse_but_input_edit_invalidates() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    let original = r.current_output(&g, "image").unwrap();
    let n = r.store.runtime().attempts.len();
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.nodes.get_mut("image").unwrap().position = [444., 122.];
    graph.revision += 1;
    r.store.save_document(doc).unwrap();
    assert_eq!(
        r.run_node(&g, "image", false, Fault::None).unwrap(),
        original
    );
    assert_eq!(r.store.runtime().attempts.len(), n);
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.nodes.get_mut("prompt").unwrap().params["text"] = json!("changed");
    graph.revision += 1;
    r.store.save_document(doc).unwrap();
    assert!(r.current_output(&g, "image").is_err());
    prepare_image(&mut r, &g);
    assert_ne!(r.current_output(&g, "image").unwrap(), original);
    assert_eq!(r.store.runtime().virtual_spent, 40);
}
#[test]
fn old_choice_does_not_adopt_new_batch() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    choice(&mut r, &g, "image", "pick_image");
    r.run_node(&g, "pick_image", false, Fault::None).unwrap();
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.revision += 1;
    graph.nodes.get_mut("image").unwrap().params["count"] = json!(3);
    r.store.save_document(doc).unwrap();
    r.run_node(&g, "image", false, Fault::None).unwrap();
    assert!(r.current_output(&g, "pick_image").is_err());
    assert!(r.run_node(&g, "pick_image", false, Fault::None).is_err());
}
#[test]
fn lost_response_reopens_and_reconciles_without_second_debit() {
    let (tmp, mut r, g) = fixture();
    for node in ["prompt", "asset"] {
        r.run_node(&g, node, false, Fault::None).unwrap();
    }
    assert!(r.run_node(&g, "image", false, Fault::AfterAccept).is_err());
    let attempt = r.store.runtime().attempts.last().unwrap().id.clone();
    assert_eq!(r.store.runtime().virtual_spent, 20);
    drop(r);
    let mut r = Runner::new(
        Store::open(&tmp.path().join("document")).unwrap(),
        Scope::synthetic(),
    )
    .unwrap();
    assert!(r.run_node(&g, "image", true, Fault::None).is_err());
    assert_eq!(r.reconcile(&attempt).unwrap(), Status::Succeeded);
    r.run_node(&g, "image", false, Fault::None).unwrap();
    assert_eq!(r.store.runtime().virtual_spent, 20);
}
#[test]
fn pre_submit_interruption_requires_reconcile_then_explicit_retry() {
    let (_tmp, mut r, g) = fixture();
    assert!(
        r.run_node(&g, "prompt", false, Fault::BeforeSubmit)
            .is_err()
    );
    let attempt = r.store.runtime().attempts.last().unwrap().id.clone();
    assert_eq!(r.reconcile(&attempt).unwrap(), Status::Failed);
    assert!(r.run_node(&g, "prompt", false, Fault::None).is_err());
    r.run_node(&g, "prompt", true, Fault::None).unwrap();
    assert_eq!(r.store.runtime().virtual_spent, 0);
}
#[test]
fn cancellation_keeps_late_result_and_blocks_downstream() {
    let (_tmp, mut r, g) = fixture();
    r.run_node(&g, "prompt", false, Fault::None).unwrap();
    r.run_node(&g, "asset", false, Fault::None).unwrap();
    assert!(r.run_node(&g, "image", false, Fault::AfterAccept).is_err());
    let attempt = r.store.runtime().attempts.last().unwrap().id.clone();
    r.cancel(&attempt).unwrap();
    assert!(r.run_node(&g, "image", true, Fault::None).is_err());
    assert_eq!(r.reconcile(&attempt).unwrap(), Status::Cancelled);
    assert!(r.store.runtime().attempts.last().unwrap().output.is_some());
    assert!(r.current_output(&g, "image").is_err());
    assert_eq!(r.store.runtime().virtual_spent, 20);
}
#[test]
fn changed_account_cannot_recover_or_bypass_unknown() {
    let (tmp, mut r, g) = fixture();
    assert!(r.run_node(&g, "prompt", false, Fault::AfterAccept).is_err());
    let attempt = r.store.runtime().attempts.last().unwrap().id.clone();
    drop(r);
    let mut r = Runner::new(
        Store::open(&tmp.path().join("document")).unwrap(),
        Scope {
            account: "other".into(),
            service: "other".into(),
        },
    )
    .unwrap();
    assert!(r.reconcile(&attempt).is_err());
    assert!(r.run_node(&g, "prompt", true, Fault::None).is_err());
}
#[test]
fn late_result_after_edit_is_historical_only() {
    let (_tmp, mut r, g) = fixture();
    assert!(r.run_node(&g, "prompt", false, Fault::AfterAccept).is_err());
    let attempt = r.store.runtime().attempts.last().unwrap().id.clone();
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.revision += 1;
    graph.nodes.get_mut("prompt").unwrap().params["text"] = json!("edited during execution");
    r.store.save_document(doc).unwrap();
    assert_eq!(r.reconcile(&attempt).unwrap(), Status::Succeeded);
    assert!(r.current_output(&g, "prompt").is_err());
}
#[test]
fn rejected_attempt_is_retryable_without_debit() {
    let (_tmp, mut r, g) = fixture();
    r.run_node(&g, "prompt", false, Fault::None).unwrap();
    r.run_node(&g, "asset", false, Fault::None).unwrap();
    assert!(r.run_node(&g, "image", false, Fault::Reject).is_err());
    assert_eq!(r.store.runtime().virtual_spent, 0);
    r.run_node(&g, "image", true, Fault::None).unwrap();
    assert_eq!(r.store.runtime().virtual_spent, 20);
}
#[test]
fn save_as_preserves_graph_but_drops_all_execution_authority() {
    let (tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    let copy = r.store.save_as(&tmp.path().join("copy")).unwrap();
    assert_ne!(copy.document().id, r.store.document().id);
    assert!(copy.runtime().attempts.is_empty());
    assert_eq!(copy.runtime().virtual_spent, 0);
}
#[test]
fn exclusive_writer_and_corrupt_file_are_not_overwritten() {
    let (tmp, r, _) = fixture();
    let root = tmp.path().join("document");
    assert!(Store::open(&root).is_err());
    drop(r);
    std::fs::write(root.join("runtime.json"), b"broken").unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(std::fs::read(root.join("runtime.json")).unwrap(), b"broken");
}
#[test]
fn unknown_node_kept_read_only_and_future_envelope_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = Graph::default();
    let node = Node::new("future-3d", json!({"opaque":[1,2,3]}));
    graph.nodes.insert(node.id.clone(), node);
    let doc = Document::new(graph);
    let root = tmp.path().join("future");
    drop(Store::create(&root, doc.clone()).unwrap());
    let mut store = Store::open(&root).unwrap();
    assert_eq!(store.document(), &doc);
    assert!(store.save_document(doc.clone()).is_err());
    drop(store);
    let mut future = doc;
    future.format = 99;
    std::fs::write(
        root.join("document.json"),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    assert!(Store::open(&root).is_err());
}
#[test]
fn malformed_connections_missing_required_and_cycles_block_execution() {
    let (doc, g, _) = sample::document();
    let graph = doc.graphs[&g].clone();
    for change in 0..4 {
        let mut bad = graph.clone();
        let mut edge = bad.edges[0].clone();
        match change {
            0 => edge.output = "missing".into(),
            1 => edge.from = "missing".into(),
            2 => edge.from = "image".into(),
            _ => {}
        }
        bad.edges.push(edge);
        assert!(bad.validate(false).is_err());
    }
    let mut missing = graph.clone();
    missing
        .edges
        .retain(|e| !(e.to == "image" && e.input == "prompt"));
    assert!(missing.validate(true).is_err());
    // Two image-delivery nodes yield a structurally typed cycle.
    let mut cycle = Graph::default();
    for key in ["a", "b"] {
        let mut n = Node::new(
            "deliver",
            json!({"target":"synthetic-assets", "media_kind":"Image"}),
        );
        n.id = key.into();
        cycle.nodes.insert(key.into(), n);
    }
    for (a, b) in [("a", "b"), ("b", "a")] {
        cycle.edges.push(Edge {
            from: a.into(),
            output: "out".into(),
            to: b.into(),
            input: "media".into(),
            order: 0,
        });
    }
    assert!(cycle.validate(true).unwrap_err().contains("cycle"));
}
#[test]
fn undo_retains_execution_facts_and_monotonic_revision() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    let spent = r.store.runtime().virtual_spent;
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    let mut history = History::default();
    history
        .edit(graph, |g| {
            g.nodes.get_mut("prompt").unwrap().params["text"] = json!("edited");
            Ok(())
        })
        .unwrap();
    let revision = graph.revision;
    assert!(history.undo(graph).unwrap());
    assert!(graph.revision > revision);
    r.store.save_document(doc).unwrap();
    assert!(r.current_output(&g, "image").is_ok());
    assert_eq!(r.store.runtime().virtual_spent, spent);
}
#[test]
fn stale_graph_save_is_refused() {
    let (_tmp, mut r, g) = fixture();
    let mut doc = r.store.document().clone();
    doc.graphs
        .get_mut(&g)
        .unwrap()
        .nodes
        .get_mut("prompt")
        .unwrap()
        .title = "changed".into();
    assert!(r.store.save_document(doc).is_err());
}
#[test]
fn cross_graph_identical_node_ids_do_not_share_results() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    let mut doc = r.store.document().clone();
    let mut graph = doc.graphs[&g].clone();
    graph.id = "another-graph".into();
    doc.graphs.insert(graph.id.clone(), graph);
    r.store.save_document(doc).unwrap();
    assert!(r.current_output("another-graph", "image").is_err());
}
#[test]
fn ancestor_run_leaves_unrelated_branch_untouched() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    assert!(
        !r.store
            .runtime()
            .attempts
            .iter()
            .any(|a| a.node_id == "video")
    );
}
#[test]
fn failed_persistence_prevents_new_execution_and_poisons_session() {
    let (tmp, mut r, g) = fixture();
    let root = tmp.path().join("document");
    std::fs::rename(root.join("runtime.json"), root.join("runtime.saved")).unwrap();
    std::fs::create_dir(root.join("runtime.json")).unwrap(); // rename cannot replace directory
    assert!(r.run_node(&g, "prompt", false, Fault::None).is_err());
    assert!(r.store.runtime().tasks.is_empty());
    std::fs::remove_dir(root.join("runtime.json")).unwrap();
    std::fs::rename(root.join("runtime.saved"), root.join("runtime.json")).unwrap();
    assert!(r.run_node(&g, "prompt", false, Fault::None).is_err());
    drop(r);
    let mut r = Runner::new(Store::open(&root).unwrap(), Scope::synthetic()).unwrap();
    r.run_node(&g, "prompt", false, Fault::None).unwrap();
}
#[test]
fn interrupted_delivery_readback_is_idempotent() {
    let (_tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    choice(&mut r, &g, "image", "pick_image");
    r.run_to(&g, "video").unwrap();
    choice(&mut r, &g, "video", "pick_video");
    r.run_node(&g, "pick_video", false, Fault::None).unwrap();
    assert!(
        r.run_node(&g, "delivery", false, Fault::BeforeDelivery)
            .is_err()
    );
    let id = r.store.runtime().attempts.last().unwrap().id.clone();
    r.reconcile(&id).unwrap();
    r.reconcile(&id).unwrap();
    assert_eq!(r.store.runtime().deliveries.len(), 1);
    assert_eq!(r.store.runtime().virtual_spent, 30);
}
#[test]
fn repeated_retry_click_reuses_success_without_extra_virtual_debit() {
    let (_tmp, mut r, g) = fixture();
    r.run_node(&g, "prompt", false, Fault::None).unwrap();
    r.run_node(&g, "asset", false, Fault::None).unwrap();
    assert!(r.run_node(&g, "image", false, Fault::Reject).is_err());
    let first = r.run_node(&g, "image", true, Fault::None).unwrap();
    let second = r.run_node(&g, "image", true, Fault::None).unwrap();
    assert_eq!(first, second);
    assert_eq!(r.store.runtime().virtual_spent, 20);
}
#[test]
fn invalid_geometry_cannot_corrupt_saved_document() {
    let (tmp, mut r, g) = fixture();
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.revision += 1;
    graph.nodes.get_mut("prompt").unwrap().position[0] = f64::NAN;
    assert!(r.store.save_document(doc).is_err());
    drop(r);
    assert!(Store::open(&tmp.path().join("document")).is_ok());
}
#[test]
fn stale_document_cannot_erase_new_graph() {
    let (_tmp, mut r, g) = fixture();
    let mut stale = r.store.document().clone();
    let mut fresh = stale.clone();
    let new = Graph::default();
    let new_id = new.id.clone();
    fresh.graphs.insert(new.id.clone(), new);
    r.store.save_document(fresh).unwrap();
    stale.graphs.get_mut(&g).unwrap().revision += 1;
    assert!(r.store.save_document(stale).is_err());
    assert!(r.store.document().graphs.contains_key(&new_id));
}
#[test]
fn interrupted_run_is_marked_on_reopen() {
    let (tmp, mut r, g) = fixture();
    prepare_image(&mut r, &g);
    drop(r);
    let path = tmp.path().join("document/runtime.json");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["runs"][0]["status"] = json!("running"); // exact persisted boundary before finish_run
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let store = Store::open(&tmp.path().join("document")).unwrap();
    assert_eq!(store.runtime().runs[0].status, "interrupted");
}
#[test]
fn cancelled_before_submit_is_proven_terminal_without_a_task() {
    let (_tmp, mut r, g) = fixture();
    assert!(
        r.run_node(&g, "prompt", false, Fault::BeforeSubmit)
            .is_err()
    );
    let id = r.store.runtime().attempts.last().unwrap().id.clone();
    r.cancel(&id).unwrap();
    assert_eq!(r.reconcile(&id).unwrap(), Status::Cancelled);
}
