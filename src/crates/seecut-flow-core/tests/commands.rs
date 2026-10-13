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
