use crate::graph::{Document, Edge, Graph, Node};
use serde_json::json;
/// Synthetic P2 scenario. Final image/video candidates both require explicit adoption.
pub fn document() -> (Document, String, String) {
    let mut graph = Graph::default();
    for (id, kind, params) in [
        ("prompt", "prompt", json!({"text":"合成商品的柔光展示"})),
        (
            "asset",
            "asset",
            json!({"asset_id":"synthetic:product", "content_version":"1", "media_kind":"Image"}),
        ),
        (
            "image",
            "image_generate",
            json!({"model":"deterministic-v1", "count":2}),
        ),
        ("pick_image", "select", json!({"media_kind":"Image"})),
        (
            "video",
            "video_generate",
            json!({"model":"deterministic-v1", "count":1}),
        ),
        ("pick_video", "select", json!({"media_kind":"Video"})),
        (
            "delivery",
            "deliver",
            json!({"media_kind":"Video", "target":"synthetic-assets"}),
        ),
    ] {
        let mut node = Node::new(kind, params);
        node.id = id.into();
        graph.nodes.insert(id.into(), node);
    }
    for (from, to, input) in [
        ("prompt", "image", "prompt"),
        ("asset", "image", "references"),
        ("image", "pick_image", "candidates"),
        ("prompt", "video", "prompt"),
        ("pick_image", "video", "references"),
        ("video", "pick_video", "candidates"),
        ("pick_video", "delivery", "media"),
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
    let graph_id = graph.id.clone();
    (Document::new(graph), graph_id, "delivery".into())
}
