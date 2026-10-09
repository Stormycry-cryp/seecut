use seecut_flow_core::{
    runner::{Fault, Output, Runner, Scope, Status},
    sample,
    store::Store,
};
use serde_json::{Value, json};
use std::fs;

fn fixture() -> (tempfile::TempDir, Runner, String) {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, graph, _) = sample::document();
    let store = Store::create(&tmp.path().join("doc"), doc).unwrap();
    (tmp, Runner::new(store, Scope::synthetic()).unwrap(), graph)
}
fn complete(runner: &mut Runner, graph: &str) {
    for (source, target) in [("image", "pick_image"), ("video", "pick_video")] {
        runner.run_to(graph, source).unwrap();
        let Output::Candidates { assets, .. } = runner.current_output(graph, source).unwrap()
        else {
            panic!("candidates missing")
        };
        runner.choose(graph, target, &assets[0].id).unwrap();
    }
    runner.run_to(graph, "delivery").unwrap();
}

#[test]
fn parseable_runtime_corruption_is_rejected_without_repair() {
    let (tmp, mut runner, graph) = fixture();
    complete(&mut runner, &graph);
    let baseline = serde_json::to_value(runner.store.runtime()).unwrap();
    let document = fs::read(tmp.path().join("doc/document.json")).unwrap();
    let image = baseline["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .position(|a| a["node_id"] == "image")
        .unwrap();
    let delivery = baseline["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .position(|a| a["node_id"] == "delivery")
        .unwrap();
    let key = baseline["attempts"][image]["key"].as_str().unwrap();
    let delivery_key = baseline["attempts"][delivery]["key"].as_str().unwrap();
    for case in [
        "orphan_task",
        "task_scope",
        "task_fingerprint",
        "task_output",
        "attempt_output",
        "matching_forged_outputs",
        "attempt_scope",
        "missing_task",
        "spent_low",
        "spent_high",
        "quote",
        "orphan_delivery",
        "wrong_delivery",
        "missing_delivery",
        "non_delivery",
        "cancelled_delivery",
        "intent_with_task",
        "failed_with_task",
        "unknown_without_task",
    ] {
        let mut bad = baseline.clone();
        // Validation must fail before the normal running -> interrupted recovery can write.
        bad["runs"][0]["status"] = json!("running");
        match case {
            "orphan_task" => {
                bad["tasks"]["orphan"] = bad["tasks"][key].clone();
            }
            "task_scope" => bad["tasks"][key]["scope"]["account"] = json!("other"),
            "task_fingerprint" => bad["tasks"][key]["fingerprint"] = json!("wrong"),
            "task_output" | "matching_forged_outputs" => {
                bad["tasks"][key]["output"]["Candidates"]["assets"][0]["version"] = json!("wrong");
                if case == "matching_forged_outputs" {
                    bad["attempts"][image]["output"] = bad["tasks"][key]["output"].clone();
                }
            }
            "attempt_output" => bad["attempts"][image]["output"] = json!({"Text":"wrong"}),
            "attempt_scope" => bad["attempts"][image]["scope"]["service"] = json!("other"),
            "missing_task" | "unknown_without_task" => {
                bad["tasks"].as_object_mut().unwrap().remove(key);
                if case == "unknown_without_task" {
                    bad["attempts"][image]["status"] = json!("Unknown");
                    bad["attempts"][image]["output"] = Value::Null;
                    bad["virtual_spent"] = json!(10);
                }
            }
            "spent_low" => bad["virtual_spent"] = json!(29),
            "spent_high" => bad["virtual_spent"] = json!(31),
            "quote" => bad["attempts"][image]["virtual_quote"] = json!(u64::MAX),
            "orphan_delivery" => {
                bad["deliveries"]["orphan"] = bad["deliveries"][delivery_key].clone()
            }
            "wrong_delivery" => bad["deliveries"][delivery_key]["version"] = json!("wrong"),
            "missing_delivery" => {
                bad["deliveries"]
                    .as_object_mut()
                    .unwrap()
                    .remove(delivery_key);
            }
            "non_delivery" => bad["deliveries"][key] = bad["deliveries"][delivery_key].clone(),
            "cancelled_delivery" => bad["attempts"][delivery]["status"] = json!("Cancelled"),
            "intent_with_task" | "failed_with_task" => {
                bad["attempts"][image]["status"] = json!(if case == "intent_with_task" {
                    "Intent"
                } else {
                    "Failed"
                });
                bad["attempts"][image]["output"] = Value::Null;
            }
            _ => unreachable!(),
        }
        let root = tmp.path().join(case);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("document.json"), &document).unwrap();
        let bytes = serde_json::to_vec_pretty(&bad).unwrap();
        // This is semantic corruption, not a JSON parser failure.
        serde_json::from_slice::<seecut_flow_core::runner::Runtime>(&bytes).unwrap();
        fs::write(root.join("runtime.json"), &bytes).unwrap();
        assert!(Store::open(&root).is_err(), "accepted {case}");
        assert_eq!(
            fs::read(root.join("runtime.json")).unwrap(),
            bytes,
            "rewrote {case}"
        );
        assert_eq!(fs::read(root.join("document.json")).unwrap(), document);
    }
}

#[test]
fn accepted_and_unaccepted_cancellation_boundaries_survive_reopen() {
    for accepted in [false, true] {
        for stage in [0, 1, 2] {
            let (tmp, mut runner, graph) = fixture();
            runner
                .run_node(&graph, "prompt", false, Fault::None)
                .unwrap();
            runner
                .run_node(&graph, "asset", false, Fault::None)
                .unwrap();
            let fault = if accepted {
                Fault::AfterAccept
            } else {
                Fault::BeforeSubmit
            };
            assert!(runner.run_node(&graph, "image", false, fault).is_err());
            let id = runner.store.runtime().attempts.last().unwrap().id.clone();
            if stage > 0 {
                runner.cancel(&id).unwrap();
            }
            if stage > 1 {
                assert_eq!(runner.reconcile(&id).unwrap(), Status::Cancelled);
            }
            let expected = serde_json::to_value(runner.store.runtime()).unwrap();
            drop(runner);
            let store = Store::open(&tmp.path().join("doc")).unwrap();
            assert_eq!(serde_json::to_value(store.runtime()).unwrap(), expected);
            assert_eq!(store.runtime().virtual_spent, if accepted { 20 } else { 0 });
        }
    }
}

#[test]
fn historical_tasks_and_deliveries_survive_deleted_nodes_and_graphs() {
    let (tmp, mut runner, graph) = fixture();
    complete(&mut runner, &graph);
    let expected = serde_json::to_value(runner.store.runtime()).unwrap();
    let mut document = runner.store.document().clone();
    let old = document.graphs.get_mut(&graph).unwrap();
    old.nodes.clear();
    old.edges.clear();
    old.revision += 1;
    runner.store.save_document(document).unwrap();
    drop(runner);
    let mut store = Store::open(&tmp.path().join("doc")).unwrap();
    assert_eq!(serde_json::to_value(store.runtime()).unwrap(), expected);
    let mut document = store.document().clone();
    document.graphs.clear();
    store.save_document(document).unwrap();
    drop(store);
    let store = Store::open(&tmp.path().join("doc")).unwrap();
    assert_eq!(serde_json::to_value(store.runtime()).unwrap(), expected);
}
