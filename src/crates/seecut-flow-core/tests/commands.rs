use seecut_flow_core::{
    commands::{self, Command, Request},
    graph::Edge,
    id, sample,
    store::Store,
};
fn request(s: &Store, g: &str, c: Command) -> Request {
    Request {
        document_id: s.document().id.clone(),
        graph_id: g.into(),
        expected_revision: s.document().graphs[g].revision,
        operation: id(),
        command: c,
    }
}
#[test]
fn command_idempotency_conflict_and_restart_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, g, _) = sample::document();
    let root = tmp.path().join("doc");
    let mut s = Store::create(&root, doc).unwrap();
    let req = request(
        &s,
        &g,
        Command::Rename {
            node: "prompt".into(),
            title: "new".into(),
        },
    );
    let first = commands::apply(&mut s, req.clone()).unwrap();
    assert_eq!(commands::apply(&mut s, req.clone()).unwrap(), first);
    drop(s);
    let mut s = Store::open(&root).unwrap();
    assert_eq!(commands::apply(&mut s, req.clone()).unwrap(), first);
    let mut conflict = req;
    conflict.command = Command::Arrange;
    assert!(commands::apply(&mut s, conflict).is_err());
}
#[test]
fn stale_command_is_rejected_and_duplicates_have_new_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, g, _) = sample::document();
    let mut s = Store::create(&tmp.path().join("doc"), doc).unwrap();
    let stale = request(&s, &g, Command::Arrange);
    let copy = request(
        &s,
        &g,
        Command::Duplicate {
            nodes: vec!["image".into()],
            incoming: false,
        },
    );
    let receipt = commands::apply(&mut s, copy).unwrap();
    assert_ne!(receipt.created[0], "image");
    assert!(
        !s.document().graphs[&g]
            .edges
            .iter()
            .any(|e| e.to == receipt.created[0])
    );
    assert!(commands::apply(&mut s, stale).is_err());
    let copy = request(
        &s,
        &g,
        Command::Duplicate {
            nodes: vec!["image".into()],
            incoming: true,
        },
    );
    let receipt = commands::apply(&mut s, copy).unwrap();
    assert_eq!(
        s.document().graphs[&g]
            .edges
            .iter()
            .filter(|e| e.to == receipt.created[0])
            .count(),
        2
    );
}
#[test]
fn invalid_edge_transaction_leaves_original_revision_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, g, _) = sample::document();
    let mut s = Store::create(&tmp.path().join("doc"), doc).unwrap();
    let before = s.document().clone();
    let command = Command::Connect {
        edge: Edge {
            from: "image".into(),
            output: "out".into(),
            to: "video".into(),
            input: "references".into(),
            order: 1,
        },
    };
    let req = request(&s, &g, command);
    assert!(commands::apply(&mut s, req).is_err());
    assert_eq!(s.document(), &before);
}

#[test]
fn reconnect_is_atomic_and_reorder_changes_actual_input_order() {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, g, _) = sample::document();
    let mut s = Store::create(&tmp.path().join("doc"), doc).unwrap();
    let mut text = s.document().graphs[&g].nodes["prompt"].clone();
    text.id = "second-text".into();
    text.params["text"] = "节点专用".into();
    let req = request(&s, &g, Command::Add { node: text });
    commands::apply(&mut s, req).unwrap();
    let old = s.document().graphs[&g]
        .edges
        .iter()
        .find(|e| e.to == "image" && e.input == "prompt")
        .unwrap()
        .clone();
    let mut invalid = old.clone();
    invalid.from = "image".into();
    let before = s.document().clone();
    let req = request(
        &s,
        &g,
        Command::Rewire {
            old: old.clone(),
            new: invalid,
        },
    );
    assert!(commands::apply(&mut s, req).is_err());
    assert_eq!(s.document(), &before);
    let mut new = old.clone();
    new.from = "second-text".into();
    let req = request(
        &s,
        &g,
        Command::Rewire {
            old: old.clone(),
            new: new.clone(),
        },
    );
    commands::apply(&mut s, req).unwrap();
    assert!(!s.document().graphs[&g].edges.contains(&old));
    assert!(s.document().graphs[&g].edges.contains(&new));
    let mut asset = s.document().graphs[&g].nodes["asset"].clone();
    asset.id = "second-asset".into();
    let edge = Edge {
        from: asset.id.clone(),
        output: "out".into(),
        to: "image".into(),
        input: "references".into(),
        order: 1,
    };
    let req = request(
        &s,
        &g,
        Command::AddConnected {
            node: asset,
            edge: edge.clone(),
        },
    );
    commands::apply(&mut s, req).unwrap();
    let mut duplicate = edge;
    duplicate.order = 2;
    let before = s.document().clone();
    let req = request(&s, &g, Command::Connect { edge: duplicate });
    assert!(commands::apply(&mut s, req).is_err());
    assert_eq!(s.document(), &before);
    let req = request(
        &s,
        &g,
        Command::Reorder {
            node: "image".into(),
            input: "references".into(),
            sources: vec!["second-asset".into(), "asset".into()],
        },
    );
    commands::apply(&mut s, req).unwrap();
    let edges = &s.document().graphs[&g].edges;
    assert_eq!(
        edges
            .iter()
            .find(|e| e.from == "second-asset" && e.to == "image")
            .unwrap()
            .order,
        0
    );
    assert_eq!(
        edges
            .iter()
            .find(|e| e.from == "asset" && e.to == "image")
            .unwrap()
            .order,
        1
    );
    drop(s);
    let reopened = Store::open(&tmp.path().join("doc")).unwrap();
    assert!(reopened.document().graphs[&g].edges.contains(&new));
}

#[test]
fn copies_keep_frozen_results_and_independent_choices_across_restart() {
    use seecut_flow_core::{
        graph::{Document, Graph, MediaKind, Node},
        host::{image_draft, node_choice, result_attempt},
        runner::{Asset, Scope},
    };
    use serde_json::json;
    for incoming in [false, true] {
        for include_selection in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("doc");
            let mut graph = Graph::default();
            for (id, kind, params) in [
                ("text", "prompt", json!({"text":"上游"})),
                (
                    "image",
                    "image_generate",
                    json!({"model":"model", "capability_version":"v1", "prompt":"自己的要求", "count":1}),
                ),
                ("pick", "select", json!({"media_kind":"Image"})),
            ] {
                let mut n = Node::new(kind, params);
                n.id = id.into();
                graph.nodes.insert(id.into(), n);
            }
            for (from, to, input) in [("text", "image", "prompt"), ("image", "pick", "candidates")]
            {
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
            let g = graph.id.clone();
            let mut s = Store::create(&path, Document::new(graph)).unwrap();
            let scope = Scope {
                account: "account".into(),
                service: "service".into(),
            };
            let draft = image_draft(&s, &scope, &g, "image").unwrap();
            let mut e = s.execution().clone();
            let a = e.begin(draft.clone(), "a".repeat(64)).unwrap();
            e.intent(
                &a,
                json!({"model":"model","prompt":draft.prompt,"quote_id":"test-quote"}),
            )
            .unwrap();
            e.observe(&a, &scope, &json!({"id":"gen_task","kind":"image","model":"model","request":{"model":"model","prompt":draft.prompt},"status":"succeeded","captured_credits":7,"outputs":[{"id":"one","sha256":"b".repeat(64)},{"id":"two","sha256":"c".repeat(64)}]})).unwrap();
            for (out, hash) in [("one", "b"), ("two", "c")] {
                e.registered(&a,"gen_task",out,Asset{id:format!("asset-{out}"),version:json!({"sha256":hash.repeat(64),"registration_id":format!("copy-test:{out}")}).to_string(),kind:MediaKind::Image,simulated:false}).unwrap();
            }
            e.select(&a, "gen_task", "one", &scope).unwrap();
            s.save_execution(e).unwrap();
            let execution_before = std::fs::read(path.join("execution.json")).unwrap();
            let nodes = if include_selection {
                vec!["image".into(), "pick".into()]
            } else {
                vec!["image".into()]
            };
            let req = request(&s, &g, Command::Duplicate { nodes, incoming });
            let copied = commands::apply(&mut s, req).unwrap().created;
            let copy = copied[0].clone();
            assert_eq!(
                execution_before,
                std::fs::read(path.join("execution.json")).unwrap()
            );
            let graph = &s.document().graphs[&g];
            let n = &graph.nodes[&copy];
            let attempt = result_attempt(s.execution(), graph, n, &scope).unwrap();
            assert_eq!(attempt.id, a);
            assert_eq!(attempt.draft.context.node_id, "image");
            assert_eq!(
                node_choice(n, attempt),
                Some(("gen_task".into(), "one".into()))
            );
            assert_eq!(
                graph.edges.iter().filter(|e| e.to == copy).count(),
                usize::from(incoming)
            );
            if include_selection {
                assert!(
                    graph
                        .edges
                        .iter()
                        .any(|e| e.from == copy && e.to == copied[1])
                );
                let pick = &graph.nodes[&copied[1]];
                assert_eq!(
                    result_attempt(s.execution(), graph, pick, &scope)
                        .unwrap()
                        .id,
                    a
                );
                assert_eq!(
                    node_choice(pick, attempt),
                    Some(("gen_task".into(), "one".into()))
                );
            }
            let mut params = n.params.clone();
            params["selected_output"] = json!({"attempt":a,"task":"gen_task","output":"two"});
            let req = request(
                &s,
                &g,
                Command::Parameters {
                    node: copy.clone(),
                    params,
                },
            );
            commands::apply(&mut s, req).unwrap();
            assert_eq!(
                execution_before,
                std::fs::read(path.join("execution.json")).unwrap()
            );
            drop(s);
            let mut s = Store::open(&path).unwrap();
            let graph = &s.document().graphs[&g];
            let attempt =
                result_attempt(s.execution(), graph, &graph.nodes[&copy], &scope).unwrap();
            assert_eq!(
                node_choice(&graph.nodes[&copy], attempt),
                Some(("gen_task".into(), "two".into()))
            );
            assert_eq!(
                node_choice(&graph.nodes["image"], attempt),
                Some(("gen_task".into(), "one".into()))
            );
            assert!(
                result_attempt(
                    s.execution(),
                    graph,
                    &graph.nodes[&copy],
                    &Scope {
                        account: "other".into(),
                        service: scope.service.clone()
                    }
                )
                .is_none()
            );
            // A new run of the original must not move the copy to another batch.
            let original_draft = image_draft(&s, &scope, &g, "image").unwrap();
            let mut e = s.execution().clone();
            let original_next = e.begin(original_draft, "e".repeat(64)).unwrap();
            assert_ne!(original_next, a);
            s.save_execution(e).unwrap();
            let graph = &s.document().graphs[&g];
            assert_eq!(
                result_attempt(s.execution(), graph, &graph.nodes[&copy], &scope)
                    .unwrap()
                    .id,
                a
            );
            if include_selection {
                let pick = &graph.nodes[&copied[1]];
                let prior = result_attempt(s.execution(), graph, pick, &scope).unwrap();
                assert_eq!(prior.id, a);
                assert_eq!(
                    node_choice(pick, prior),
                    Some(("gen_task".into(), "one".into()))
                );
            }
            let draft = image_draft(&s, &scope, &g, &copy).unwrap();
            assert_eq!(draft.context.node_id, copy);
            assert!(draft.parameters.get("copied_results").is_none());
            assert!(draft.parameters.get("selected_output").is_none());
            let mut e = s.execution().clone();
            let own = e.begin(draft, "d".repeat(64)).unwrap();
            assert_ne!(own, a);
            s.save_execution(e).unwrap();
            let graph = &s.document().graphs[&g];
            assert_eq!(
                result_attempt(s.execution(), graph, &graph.nodes[&copy], &scope)
                    .unwrap()
                    .id,
                own
            );
            assert_eq!(
                result_attempt(s.execution(), graph, &graph.nodes["image"], &scope)
                    .unwrap()
                    .id,
                original_next
            );
        }
    }
}
