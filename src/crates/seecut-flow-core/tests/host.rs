use seecut_flow_core::{
    graph::MediaKind,
    host::{self, LibraryAsset},
    runner::{Asset, Fault, Runner, Scope},
    sample,
    store::Store,
};
fn fixture() -> (tempfile::TempDir, Runner, String) {
    let tmp = tempfile::tempdir().unwrap();
    let (doc, g, _) = sample::document();
    let store = Store::create(&tmp.path().join("doc"), doc).unwrap();
    (tmp, Runner::new(store, Scope::synthetic()).unwrap(), g)
}
fn library_asset() -> LibraryAsset {
    LibraryAsset {
        asset: Asset {
            id: "synthetic:library-2".into(),
            version: "sha256-version-2".into(),
            kind: MediaKind::Image,
            simulated: true,
        },
        name: "合成资产库素材".into(),
        available: true,
    }
}
#[test]
fn library_selection_is_versioned_and_stale_callback_is_rejected() {
    let (_tmp, mut r, g) = fixture();
    let request = host::picker_request(&r, &g, "asset").unwrap();
    let scope = r.scope().clone();
    host::accept_asset(&mut r.store, &scope, &request, library_asset()).unwrap();
    assert_eq!(
        r.store.document().graphs[&g].nodes["asset"].params["asset_id"],
        "synthetic:library-2"
    );
    let mut different = library_asset();
    different.asset.id = "synthetic:late".into();
    assert!(host::accept_asset(&mut r.store, &scope, &request, different).is_err());
}
#[test]
fn library_unavailable_or_wrong_account_never_changes_graph() {
    let (_tmp, mut r, g) = fixture();
    let request = host::picker_request(&r, &g, "asset").unwrap();
    let mut scope = r.scope().clone();
    scope.account = "other".into();
    let before = r.store.document().clone();
    assert!(host::accept_asset(&mut r.store, &scope, &request, library_asset()).is_err());
    let scope = r.scope().clone();
    let mut missing = library_asset();
    missing.available = false;
    assert!(host::accept_asset(&mut r.store, &scope, &request, missing).is_err());
    assert_eq!(r.store.document(), &before);
}
#[test]
fn generation_handoff_preserves_ordered_versions_without_submit() {
    let (_tmp, mut r, g) = fixture();
    for node in ["prompt", "asset"] {
        r.run_node(&g, node, false, Fault::None).unwrap();
    }
    let draft = host::generation_draft(&r, &g, "image").unwrap();
    assert_eq!(draft.references[0].id, "synthetic:product");
    assert_eq!(draft.references[0].version, "1");
    assert_eq!(draft.prompt, "合成商品的柔光展示");
    assert_eq!(r.store.runtime().virtual_spent, 0);
}

#[test]
fn library_reference_is_one_atomic_edit_with_idempotent_replay() {
    let (_tmp, mut r, g) = fixture();
    let request = host::picker_request(&r, &g, "image").unwrap();
    let scope = r.scope().clone();
    let before = r.store.document().graphs[&g].clone();
    let first = host::accept_asset(&mut r.store, &scope, &request, library_asset()).unwrap();
    let saved = r.store.document().clone();
    let replay = host::accept_asset(&mut r.store, &scope, &request, library_asset()).unwrap();
    assert_eq!(first, replay);
    assert_eq!(r.store.document(), &saved);
    let graph = &saved.graphs[&g];
    assert_eq!(graph.nodes.len(), before.nodes.len() + 1);
    assert_eq!(graph.edges.len(), before.edges.len() + 1);
    assert_eq!(graph.revision, before.revision + 1);
    let edge = graph.edges.last().unwrap();
    assert_eq!(edge.to, "image");
    assert_eq!(edge.input, "references");
    assert_eq!(edge.order, 1);
    assert_eq!(
        graph.nodes[&edge.from].params["content_version"],
        "sha256-version-2"
    );
    assert!(r.store.runtime().attempts.is_empty());
}

#[test]
fn stale_library_return_cannot_leave_an_orphan_reference() {
    let (_tmp, mut r, g) = fixture();
    let request = host::picker_request(&r, &g, "image").unwrap();
    let scope = r.scope().clone();
    let mut doc = r.store.document().clone();
    doc.graphs.get_mut(&g).unwrap().revision += 1;
    r.store.save_document(doc).unwrap();
    let before = r.store.document().clone();
    assert!(host::accept_asset(&mut r.store, &scope, &request, library_asset()).is_err());
    assert_eq!(r.store.document(), &before);
}

#[test]
fn shared_model_is_serializable_but_never_run_by_simulator() {
    let (_tmp, mut r, g) = fixture();
    let mut doc = r.store.document().clone();
    let graph = doc.graphs.get_mut(&g).unwrap();
    graph.nodes.get_mut("image").unwrap().params = serde_json::json!({
        "model":"host-catalogue-fixture", "capability_version":"catalogue-42", "count":4
    });
    graph.revision += 1;
    r.store.save_document(doc).unwrap();
    assert!(
        r.run_node(&g, "image", false, Fault::None)
            .unwrap_err()
            .contains("shared host")
    );
    assert!(r.store.runtime().attempts.is_empty());
    for node in ["prompt", "asset"] {
        r.run_node(&g, node, false, Fault::None).unwrap();
    }
    let draft = host::generation_draft(&r, &g, "image").unwrap();
    assert_eq!(draft.model_id, "host-catalogue-fixture");
    assert_eq!(draft.capability_version, "catalogue-42");
    assert_eq!(r.store.runtime().virtual_spent, 0);
}

#[test]
fn callback_replay_after_target_deletion_returns_original_receipt() {
    use seecut_flow_core::commands::{self, Command, Request};
    let (_tmp, mut r, g) = fixture();
    let request = host::picker_request(&r, &g, "image").unwrap();
    let scope = r.scope().clone();
    let first = host::accept_asset(&mut r.store, &scope, &request, library_asset()).unwrap();
    let deletion = Request {
        document_id: r.store.document().id.clone(),
        graph_id: g.clone(),
        expected_revision: r.store.document().graphs[&g].revision,
        operation: "remove-target".into(),
        command: Command::Remove {
            nodes: vec!["image".into()],
        },
    };
    commands::apply(&mut r.store, deletion).unwrap();
    let before = r.store.document().clone();
    assert_eq!(
        host::accept_asset(&mut r.store, &scope, &request, library_asset()).unwrap(),
        first
    );
    assert_eq!(r.store.document(), &before);
}

#[test]
fn asset_callback_shares_undo_history_and_discards_old_redo() {
    use seecut_flow_core::{
        commands::Command,
        protocol::{self, Session},
    };
    use serde_json::json;
    let (_tmp, mut r, g) = fixture();
    let mut session = Session::default();
    let rename = protocol::command_request(
        &r,
        &g,
        Command::Rename {
            node: "prompt".into(),
            title: "Edited title".into(),
        },
    );
    session.request(&mut r, rename).unwrap();
    let selection = host::picker_request(&r, &g, "image").unwrap();
    let callback = json!({"op":"accept_asset", "selection":selection, "asset":library_asset()});
    session.request(&mut r, callback.clone()).unwrap();
    session.request(&mut r, callback).unwrap(); // replay is not another history entry
    assert_eq!(r.store.document().graphs[&g].nodes.len(), 8);
    session.request(&mut r, json!({"op":"undo"})).unwrap();
    assert_eq!(r.store.document().graphs[&g].nodes.len(), 7);
    assert_eq!(
        r.store.document().graphs[&g].nodes["prompt"].title,
        "Edited title"
    );
    let selection = host::picker_request(&r, &g, "image").unwrap();
    session
        .request(
            &mut r,
            json!({"op":"accept_asset", "selection":selection, "asset":library_asset()}),
        )
        .unwrap();
    assert!(session.request(&mut r, json!({"op":"redo"})).is_err());
    assert_eq!(r.store.document().graphs[&g].nodes.len(), 8);
}

#[test]
fn no_op_move_does_not_invalidate_pending_asset_callback() {
    use seecut_flow_core::{
        commands::{self, Command, Request},
        id,
    };
    let (_tmp, mut r, g) = fixture();
    let selection = host::picker_request(&r, &g, "image").unwrap();
    let graph = &r.store.document().graphs[&g];
    let request = Request {
        document_id: r.store.document().id.clone(),
        graph_id: g.clone(),
        expected_revision: graph.revision,
        operation: id(),
        command: Command::Move {
            positions: std::collections::BTreeMap::from([(
                "image".into(),
                graph.nodes["image"].position,
            )]),
        },
    };
    commands::apply(&mut r.store, request).unwrap();
    let scope = r.scope().clone();
    assert!(host::accept_asset(&mut r.store, &scope, &selection, library_asset()).is_ok());
}
