use seecut_flow_core::{
    commands::{self, Command, Request},
    graph::{Edge, History},
    protocol::{self, Session},
    runner::{Output, Runner, Scope},
    sample,
    store::Store,
};
use serde_json::json;
use std::fs;

fn rename(store: &Store, graph: &str, operation: &str) -> Request {
    Request {
        document_id: store.document().id.clone(),
        graph_id: graph.into(),
        expected_revision: store.document().graphs[graph].revision,
        operation: operation.into(),
        command: Command::Rename {
            node: "prompt".into(),
            title: operation.into(),
        },
    }
}

#[test]
fn document_and_graph_max_reopen_read_only_with_receipt_replay() {
    for document_limit in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("doc");
        let (mut doc, graph, _) = sample::document();
        if document_limit {
            doc.revision = u64::MAX - 1;
        } else {
            doc.graphs.get_mut(&graph).unwrap().revision = u64::MAX - 1;
        }
        let mut store = Store::create(&root, doc).unwrap();
        let first = rename(&store, &graph, "first");
        let receipt = commands::apply(&mut store, first.clone()).unwrap();
        assert_eq!(
            if document_limit {
                store.document().revision
            } else {
                receipt.revision
            },
            u64::MAX
        );
        drop(store);
        let mut store = Store::open(&root).unwrap();
        let before = store.document().clone();
        let bytes = fs::read(root.join("document.json")).unwrap();
        assert_eq!(commands::apply(&mut store, first).unwrap(), receipt);
        let next = rename(&store, &graph, "second");
        assert!(
            commands::apply(&mut store, next)
                .unwrap_err()
                .contains("revision exhausted")
        );
        if document_limit {
            assert!(
                store
                    .save_document(before.clone())
                    .unwrap_err()
                    .contains("revision exhausted")
            );
        }
        assert_eq!(store.document(), &before);
        assert_eq!(fs::read(root.join("document.json")).unwrap(), bytes);
        assert!(!store.document().operations.contains_key("second"));
    }
}

#[test]
fn connect_and_history_exhaustion_preserve_graph_and_stacks() {
    let (doc, graph_id, _) = sample::document();
    let mut graph = doc.graphs[&graph_id].clone();
    graph.revision = u64::MAX - 1;
    let mut edge = Edge {
        from: "asset".into(),
        output: "out".into(),
        to: "image".into(),
        input: "references".into(),
        order: 1,
    };
    graph.connect(edge.clone()).unwrap();
    assert_eq!(graph.revision, u64::MAX);
    let before = graph.clone();
    edge.order = 2;
    assert!(
        graph
            .connect(edge)
            .unwrap_err()
            .contains("revision exhausted")
    );
    let mut history = History::default();
    assert!(
        history
            .edit(&mut graph, |g| {
                g.nodes.get_mut("prompt").unwrap().title = "blocked".into();
                Ok(())
            })
            .unwrap_err()
            .contains("revision exhausted")
    );
    assert_eq!(graph, before);

    graph.revision = u64::MAX - 1;
    history
        .edit(&mut graph, |g| {
            g.nodes.get_mut("prompt").unwrap().title = "edited".into();
            Ok(())
        })
        .unwrap();
    let before = graph.clone();
    for _ in 0..2 {
        assert!(
            history
                .undo(&mut graph)
                .unwrap_err()
                .contains("revision exhausted")
        );
        assert_eq!(graph, before);
    }
    // Lower only the test fixture counter to verify the failed undo retained its entry.
    graph.revision = u64::MAX - 1;
    assert!(history.undo(&mut graph).unwrap());
    let before = graph.clone();
    for _ in 0..2 {
        assert!(
            history
                .redo(&mut graph)
                .unwrap_err()
                .contains("revision exhausted")
        );
        assert_eq!(graph, before);
    }
    graph.revision = u64::MAX - 1;
    assert!(history.redo(&mut graph).unwrap());
    assert_eq!(graph.nodes["prompt"].title, "edited");
}

#[test]
fn protocol_undo_redo_exhaustion_keeps_history_and_saved_bytes() {
    for document_limit in [false, true] {
        for redo in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("doc");
            let (mut doc, graph, _) = sample::document();
            let revision = u64::MAX - if redo { 2 } else { 1 };
            if document_limit {
                doc.revision = revision;
            } else {
                doc.graphs.get_mut(&graph).unwrap().revision = revision;
            }
            let store = Store::create(&root, doc).unwrap();
            let mut runner = Runner::new(store, Scope::synthetic()).unwrap();
            let mut session = Session::default();
            let request = protocol::command_request(
                &runner,
                &graph,
                Command::Rename {
                    node: "prompt".into(),
                    title: "edited".into(),
                },
            );
            session.request(&mut runner, request).unwrap();
            if redo {
                session.request(&mut runner, json!({"op":"undo"})).unwrap();
            }
            let before = runner.store.document().clone();
            let bytes = fs::read(root.join("document.json")).unwrap();
            for _ in 0..2 {
                assert!(
                    session
                        .request(
                            &mut runner,
                            json!({"op":if redo { "redo" } else { "undo" }})
                        )
                        .unwrap_err()
                        .contains("revision exhausted")
                );
                assert_eq!(runner.store.document(), &before);
                assert_eq!(fs::read(root.join("document.json")).unwrap(), bytes);
            }
        }
    }
}

#[test]
fn candidate_selection_at_max_preserves_graph_and_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("doc");
    let (doc, graph, _) = sample::document();
    let store = Store::create(&root, doc).unwrap();
    let mut runner = Runner::new(store, Scope::synthetic()).unwrap();
    runner.run_to(&graph, "image").unwrap();
    let Output::Candidates { assets, .. } = runner.current_output(&graph, "image").unwrap() else {
        panic!()
    };
    let mut doc = runner.store.document().clone();
    doc.graphs.get_mut(&graph).unwrap().revision = u64::MAX;
    runner.store.save_document(doc).unwrap();
    let document = fs::read(root.join("document.json")).unwrap();
    let runtime = fs::read(root.join("runtime.json")).unwrap();
    assert!(
        runner
            .choose(&graph, "pick_image", &assets[0].id)
            .unwrap_err()
            .contains("revision exhausted")
    );
    assert_eq!(fs::read(root.join("document.json")).unwrap(), document);
    assert_eq!(fs::read(root.join("runtime.json")).unwrap(), runtime);
}
