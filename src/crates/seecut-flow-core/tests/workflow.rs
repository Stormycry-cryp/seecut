//! Service-receipt contract fixtures, not real supplier or media-generation evidence.
use seecut_flow_core::{
    digest,
    execution::{Execution, Phase},
    graph::{Document, Edge, Graph, MediaKind, Node},
    runner::{Asset, Scope},
    store::Store,
    workflow::{Choice, State},
};
use serde_json::{Value, json};

fn scope() -> Scope {
    Scope {
        account: "account-one".into(),
        service: "service-one".into(),
    }
}
fn node(id: &str, kind: &str, params: Value) -> Node {
    let mut n = Node::new(kind, params);
    n.id = id.into();
    n.title = id.into();
    n
}
fn graph() -> Graph {
    let mut graph = Graph {
        id: "workflow".into(),
        ..Graph::default()
    };
    for n in [
        node("prompt", "prompt", json!({"text":"A short scene"})),
        node(
            "image1",
            "image_generate",
            json!({"model":"image-model","capability_version":"image-cap","count":1,"prompt":"first"}),
        ),
        node("pick1", "select", json!({"media_kind":"Image"})),
        node(
            "image2",
            "image_generate",
            json!({"model":"image-model","capability_version":"image-cap","count":1,"prompt":"second"}),
        ),
        node("pick2", "select", json!({"media_kind":"Image"})),
        node(
            "video",
            "video_generate",
            json!({"model":"video-model","capability_version":"video-cap","count":1,"prompt":"animate","duration":5,"generate_audio":true}),
        ),
        node("pick-video", "select", json!({"media_kind":"Video"})),
        node(
            "deliver",
            "deliver",
            json!({"media_kind":"Video","target":"library"}),
        ),
        node(
            "unrelated",
            "image_generate",
            json!({"model":"image-model","capability_version":"image-cap","count":1,"prompt":"unrelated"}),
        ),
    ] {
        graph.nodes.insert(n.id.clone(), n);
    }
    for (from, to, input) in [
        ("prompt", "image1", "prompt"),
        ("image1", "pick1", "candidates"),
        ("pick1", "image2", "references"),
        ("image2", "pick2", "candidates"),
        ("pick2", "video", "references"),
        ("video", "pick-video", "candidates"),
        ("pick-video", "deliver", "media"),
        ("prompt", "unrelated", "prompt"),
    ] {
        graph
            .connect(Edge {
                from: from.into(),
                output: "out".into(),
                to: to.into(),
                input: input.into(),
                order: 0,
            })
            .unwrap();
    }
    graph
}
fn execution() -> Execution {
    Execution::new("document-one".into(), "project-one".into())
}
fn run(e: &mut Execution, g: Graph, target: &str) -> String {
    e.begin_run(g, scope(), target.into(), "a".repeat(64), false)
        .unwrap()
}
fn finish(e: &mut Execution, node: &str, outputs: usize) -> Vec<Choice> {
    let a = e
        .attempts
        .iter()
        .rev()
        .find(|a| a.draft.context.node_id == node)
        .unwrap()
        .clone();
    let kind = if a.draft.media_kind == MediaKind::Video {
        "video"
    } else {
        "image"
    };
    let task = format!("gen_{}", a.id);
    let body = json!({"quote_id":"quote-original","model":a.draft.model_id,"prompt":a.draft.prompt,"quantity":1});
    e.intent(&a.id, body.clone()).unwrap();
    let rows: Vec<_> = (0..outputs)
        .map(|n| json!({"id":format!("out_{n}"),"sha256":digest(&(a.id.clone(),n))}))
        .collect();
    e.observe(&a.id,&scope(),&json!({"id":task,"kind":kind,"model":a.draft.model_id,"request":body,"status":"succeeded","captured_credits":3,"outputs":rows})).unwrap();
    (0..outputs)
        .map(|n| Choice {
            attempt: a.id.clone(),
            task: task.clone(),
            output: format!("out_{n}"),
        })
        .collect()
}
fn register(e: &mut Execution, c: &Choice) -> Asset {
    let a = e.attempts.iter().find(|a| a.id == c.attempt).unwrap();
    let asset=Asset { id:format!("asset_{}_{}",a.id,c.output),version:json!({"sha256":a.tasks[&c.task].outputs[&c.output].sha256,"registration_id":format!("{}:{}",c.task,c.output)}).to_string(),kind:a.draft.media_kind.clone(),simulated:false };
    e.registered(&c.attempt, &c.task, &c.output, asset.clone())
        .unwrap();
    asset
}
fn select(e: &mut Execution, id: &str, node: &str, c: &Choice) {
    assert!(
        e.choose_run_output("workflow", node, c.clone(), &scope())
            .unwrap()
    );
    assert_eq!(
        e.runs.iter().find(|r| r.id == id).unwrap().state,
        State::Running
    );
}
fn completed_graph(e: &Execution, g: &Graph) -> Graph {
    let mut g = g.clone();
    for (node, choice) in &e.runs.last().unwrap().choices {
        g.nodes.get_mut(node).unwrap().params["selected_output"] =
            serde_json::to_value(choice).unwrap();
    }
    g
}
fn complete_chain(e: &mut Execution, g: Graph) -> (String, Graph) {
    let id = run(e, g.clone(), "deliver");
    for (generation, pick) in [
        ("image1", "pick1"),
        ("image2", "pick2"),
        ("video", "pick-video"),
    ] {
        assert!(e.advance_run(&id, &scope()).unwrap().is_none());
        let choices = finish(e, generation, 1);
        register(e, &choices[0]);
        e.advance_run(&id, &scope()).unwrap();
        assert_eq!(e.runs.last().unwrap().state, State::AwaitingSelection);
        select(e, &id, pick, &choices[0]);
    }
    let delivery = e.advance_run(&id, &scope()).unwrap().unwrap();
    assert_eq!(delivery.asset.kind, MediaKind::Video);
    e.complete_delivery(&delivery, &scope()).unwrap();
    e.complete_delivery(&delivery, &scope()).unwrap(); // stable no-op receipt replay
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.runs.last().unwrap().state, State::Complete);
    (id, completed_graph(e, &g))
}
#[test]
fn dependency_chain_waits_for_each_adoption_and_delivers_the_registered_video_version() {
    let mut e = execution();
    let (id, _) = complete_chain(&mut e, graph());
    assert_eq!(e.attempts.len(), 3);
    assert!(
        e.attempts
            .iter()
            .all(|a| a.draft.context.node_id != "unrelated")
    );
    assert_eq!(e.attempts[1].draft.references.len(), 1);
    assert_eq!(e.attempts[2].draft.references.len(), 1);
    let run = e.runs.iter().find(|r| r.id == id).unwrap();
    assert_eq!(run.actual_credits(&e), 9);
    assert_eq!(
        run.selected_asset("deliver"),
        run.selected_asset("pick-video")
    );
    e.validate("document-one", "project-one").unwrap();
}
#[test]
fn successful_dependencies_reuse_fingerprints_and_only_changed_downstream_runs_again() {
    let mut e = execution();
    let (_, mut g) = complete_chain(&mut e, graph());
    g.nodes.get_mut("image1").unwrap().position = [300., 200.];
    g.revision += 1;
    let same = run(&mut e, g.clone(), "deliver");
    let delivery = e.advance_run(&same, &scope()).unwrap().unwrap();
    e.complete_delivery(&delivery, &scope()).unwrap();
    e.advance_run(&same, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 3);
    assert_eq!(e.runs.last().unwrap().actual_credits(&e), 0);
    g.nodes.get_mut("image2").unwrap().params["prompt"] = json!("only this branch changes");
    let changed = run(&mut e, g, "deliver");
    e.advance_run(&changed, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 4);
    assert_eq!(e.attempts.last().unwrap().draft.context.node_id, "image2");
    assert!(e.runs.last().unwrap().steps["image1"].reused);
}
#[test]
fn unknown_and_duplicate_click_keep_run_attempt_and_original_key() {
    let mut e = execution();
    let mut g = graph();
    let id = run(&mut e, g.clone(), "image1");
    e.advance_run(&id, &scope()).unwrap();
    let a = e.attempts[0].clone();
    e.intent(
        &a.id,
        json!({"quote_id":"q","model":a.draft.model_id,"prompt":a.draft.prompt}),
    )
    .unwrap();
    g.nodes.get_mut("image1").unwrap().position = [40., 60.];
    g.revision += 1;
    assert_eq!(run(&mut e, g.clone(), "image1"), id);
    e.advance_run(&id, &scope()).unwrap();
    assert!(e.retry_run(&id, &scope()).is_err());
    assert_eq!(e.attempts.len(), 1);
    assert_eq!(e.attempts[0].key, a.key);
    g.nodes.get_mut("image1").unwrap().params["prompt"] = json!("new prompt");
    assert!(
        e.begin_run(g, scope(), "image1".into(), "b".repeat(64), false)
            .is_err()
    );
}
#[test]
fn paused_restart_keeps_frozen_graph_and_partial_registration_only_retries_registration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("flow");
    let g = graph();
    let mut store = Store::create(&path, Document::new(g.clone())).unwrap();
    let mut e = store.execution().clone();
    let id = run(&mut e, g, "deliver");
    e.advance_run(&id, &scope()).unwrap();
    let choices = finish(&mut e, "image1", 2);
    register(&mut e, &choices[0]);
    assert_eq!(e.attempts[0].phase, Phase::Accepted);
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 1);
    e.paused = true;
    register(&mut e, &choices[1]);
    store.save_execution(e).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let mut e = store.execution().clone();
    assert_eq!(e.runs[0].id, id);
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 1);
    e.paused = false;
    e.advance_run(&id, &scope()).unwrap();
    select(&mut e, &id, "pick1", &choices[1]);
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 2);
    assert!(e.attempts[1].draft.references[0].id.ends_with("out_1"));
}
#[test]
fn changing_visible_graph_cannot_change_an_active_runs_frozen_inputs_or_delivery_identity() {
    let mut e = execution();
    let mut visible = graph();
    let id = run(&mut e, visible.clone(), "deliver");
    visible.nodes.get_mut("image1").unwrap().params["prompt"] = json!("later edit");
    e.advance_run(&id, &scope()).unwrap();
    assert!(e.attempts[0].draft.prompt.ends_with("first"));
    assert_eq!(e.runs[0].graph.nodes["image1"].params["prompt"], "first");
    let mut wrong = scope();
    wrong.account = "other".into();
    assert!(e.advance_run(&id, &wrong).is_err());
}

#[test]
fn live_legacy_intent_wins_over_a_previous_successful_batch() {
    let mut e = execution();
    let g = graph();
    let old = run(&mut e, g.clone(), "image1");
    e.advance_run(&old, &scope()).unwrap();
    let choices = finish(&mut e, "image1", 1);
    register(&mut e, &choices[0]);
    e.advance_run(&old, &scope()).unwrap();
    let draft = e.attempts[0].draft.clone();
    let live = e.begin(draft.clone(), "b".repeat(64)).unwrap();
    e.intent(
        &live,
        json!({"quote_id":"new-quote","model":draft.model_id,"prompt":draft.prompt}),
    )
    .unwrap();
    let next = run(&mut e, g, "pick1");
    e.advance_run(&next, &scope()).unwrap();
    let run = e.runs.last().unwrap();
    assert_eq!(run.steps["image1"].attempt.as_deref(), Some(live.as_str()));
    assert!(!run.steps["image1"].reused);
    assert!(run.steps["image1"].output.is_none());
    assert_eq!(e.attempts.len(), 2);
}
#[test]
fn replaced_unsubmitted_plan_does_not_recapture_superseded_preparation() {
    let mut e = execution();
    let g = graph();
    let old = run(&mut e, g.clone(), "image1");
    e.advance_run(&old, &scope()).unwrap();
    let old_attempt = e.attempts[0].id.clone();
    e.attempts[0].error = Some("preparation failed".into());
    assert!(e.advance_run(&old, &scope()).is_err());
    let next = run(&mut e, g, "pick1");
    e.advance_run(&next, &scope()).unwrap();
    assert!(e.attempts[0].superseded);
    assert!(!e.allows_scheduling(&old_attempt));
    assert_ne!(
        e.runs.last().unwrap().steps["image1"].attempt.as_deref(),
        Some(old_attempt.as_str())
    );
    assert_eq!(e.runs.last().unwrap().state, State::Running);
    e.validate("document-one", "project-one").unwrap();
}

#[test]
fn single_node_does_not_schedule_missing_upstream_generation() {
    let mut e = execution();
    let g = graph();
    let single = e
        .begin_run(g.clone(), scope(), "image2".into(), "a".repeat(64), true)
        .unwrap();
    assert!(e.advance_run(&single, &scope()).is_err());
    assert!(e.attempts.is_empty());
    let full = run(&mut e, g, "image2");
    assert_ne!(full, single);
    e.advance_run(&full, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 1);
    assert_eq!(e.attempts[0].draft.context.node_id, "image1");
}

fn two_batches(e: &mut Execution, g: &Graph) -> (Choice, Asset, Choice, Asset) {
    let first = run(e, g.clone(), "image1");
    e.advance_run(&first, &scope()).unwrap();
    let a = finish(e, "image1", 1).remove(0);
    let asset_a = register(e, &a);
    e.advance_run(&first, &scope()).unwrap();
    let draft = e.attempts[0].draft.clone();
    e.begin(draft, "b".repeat(64)).unwrap();
    let b = finish(e, "image1", 1).remove(0);
    let asset_b = register(e, &b);
    (a, asset_a, b, asset_b)
}
#[test]
fn persisted_historical_choice_survives_reopen_and_is_the_actual_downstream_reference() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("flow"), Document::new(graph())).unwrap();
    let mut e = store.execution().clone();
    let mut g = graph();
    let (a, asset_a, _, _) = two_batches(&mut e, &g);
    g.nodes.get_mut("pick1").unwrap().params["selected_output"] = serde_json::to_value(&a).unwrap();
    let mut document = store.document().clone();
    g.revision += 1;
    document.graphs.insert(g.id.clone(), g.clone());
    store.save_document(document).unwrap();
    store.save_execution(e).unwrap();
    drop(store);
    let store = Store::open(&temp.path().join("flow")).unwrap();
    let g = store.document().graphs["workflow"].clone();
    let mut e = store.execution().clone();
    let id = run(&mut e, g, "image2");
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.attempts.len(), 3);
    assert_eq!(e.attempts[2].draft.references, vec![asset_a]);
    assert_eq!(e.runs.last().unwrap().choices["pick1"], a);
}
#[test]
fn consumers_independently_keep_historical_and_latest_batches() {
    let mut e = execution();
    let mut g = graph();
    let (a, asset_a, b, asset_b) = two_batches(&mut e, &g);
    g.nodes.get_mut("pick1").unwrap().params["selected_output"] = serde_json::to_value(&a).unwrap();
    g.nodes.insert(
        "pick-other".into(),
        node(
            "pick-other",
            "select",
            json!({"media_kind":"Image","selected_output":b}),
        ),
    );
    for (from, to, input, order) in [
        ("image1", "pick-other", "candidates", 0),
        ("pick-other", "image2", "references", 1),
    ] {
        g.connect(Edge {
            from: from.into(),
            output: "out".into(),
            to: to.into(),
            input: input.into(),
            order,
        })
        .unwrap();
    }
    let id = run(&mut e, g, "image2");
    e.advance_run(&id, &scope()).unwrap();
    assert_eq!(e.attempts[2].draft.references, vec![asset_a, asset_b]);
    assert_eq!(e.runs.last().unwrap().choices["pick1"], a);
    assert_eq!(e.runs.last().unwrap().choices["pick-other"], b);
}
#[test]
fn changed_input_and_unresolved_submission_cannot_reuse_an_old_choice() {
    for unresolved in [false, true] {
        let mut e = execution();
        let mut g = graph();
        let (a, _, _, _) = two_batches(&mut e, &g);
        g.nodes.get_mut("pick1").unwrap().params["selected_output"] =
            serde_json::to_value(&a).unwrap();
        if unresolved {
            let draft = e.attempts[0].draft.clone();
            let pending = e.begin(draft, "c".repeat(64)).unwrap();
            e.intent(&pending, json!({"quote_id":"pending"})).unwrap();
        } else {
            g.nodes.get_mut("image1").unwrap().params["prompt"] = json!("changed input");
        }
        let id = run(&mut e, g, "image2");
        e.advance_run(&id, &scope()).unwrap();
        assert!(e.runs.last().unwrap().choices.is_empty());
        assert!(
            e.attempts
                .iter()
                .all(|a| a.draft.context.node_id != "image2")
        );
        assert!(
            !e.choose_run_output("workflow", "pick1", a, &scope())
                .unwrap_or(false)
        );
    }
}
