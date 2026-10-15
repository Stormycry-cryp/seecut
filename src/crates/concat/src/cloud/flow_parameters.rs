//! Catalogue-derived Flow request choices, shared with the node editor.
use serde_json::{Value, json};
pub(crate) fn generation_fields(kind: &str) -> &'static [&'static str] {
    if kind == "video" {
        &[
            "resolution",
            "duration",
            "aspect_ratio",
            "generate_audio",
            "quantity",
        ]
    } else {
        &["size", "quality", "aspect_ratio", "quantity"]
    }
}
pub(crate) fn parameter_values(parameter: &Value) -> Vec<Value> {
    parameter["values"].as_array().cloned().unwrap_or_else(|| {
        if parameter["type"] == "boolean" {
            vec![json!(true), json!(false)]
        } else {
            vec![]
        }
    })
}
pub(crate) fn parameter_default(parameter: &Value) -> Value {
    parameter.get("default").cloned().unwrap_or_else(|| {
        parameter_values(parameter)
            .into_iter()
            .next()
            .unwrap_or(Value::Null)
    })
}
pub(super) fn generation_body(
    model: &Value,
    prompt: &str,
    parameters: &Value,
    ids: &[String],
    effort: Option<&Value>,
) -> Result<Value, String> {
    let kind = model["kind"].as_str().unwrap_or_default();
    if !matches!(kind, "image" | "video") || model["available"] != true {
        return Err("该模型当前不可运行".into());
    }
    let operation = if kind == "image" && !ids.is_empty() {
        "edit"
    } else {
        "generate"
    };
    if !model["operations"]
        .as_array()
        .is_some_and(|v| v.iter().any(|op| op == operation))
    {
        return Err("当前模型不支持此生成方式".into());
    }
    let mut body = json!({"model":model["id"],"prompt":prompt.trim(),"operation":operation});
    for &field in generation_fields(kind) {
        let parameter = &model["parameters"][field];
        if parameter.is_null() {
            continue;
        }
        let selected = parameters
            .get(field)
            .cloned()
            .or_else(|| {
                (field == "quantity")
                    .then(|| parameters["count"].clone())
                    .filter(|v| !v.is_null())
            })
            .unwrap_or_else(|| parameter_default(parameter));
        if !parameter_values(parameter).contains(&selected) {
            return Err(format!("{field} 不在当前模型能力中"));
        }
        body[field] = selected;
    }
    if let Some(groups) = model["duration_resolution_map"].as_array() {
        let allowed = groups.iter().any(|group| {
            group["resolution"]
                .as_array()
                .is_some_and(|v| v.contains(&body["resolution"]))
                && group["duration"]
                    .as_array()
                    .is_some_and(|v| v.contains(&body["duration"]))
        });
        if !allowed {
            return Err("当前分辨率不支持所选时长".into());
        }
    }
    if !ids.is_empty() {
        body["reference_asset_ids"] = json!(ids);
    }
    if let Some(effort) = effort {
        let field = if kind == "video" {
            "video_model_ids"
        } else {
            "image_model_ids"
        };
        if !effort["generation"][field]
            .as_array()
            .is_some_and(|ids| ids.contains(&model["id"]))
        {
            return Err("当前强度不支持此模型".into());
        }
        body["effort_id"] = effort["id"].clone();
        body["policy_revision"] = effort["policy_revision"].clone();
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn video() -> Value {
        json!({"id":"video-model","kind":"video","available":true,"operations":["generate"],"parameters":{
            "resolution":{"values":["720p"],"default":"720p"},"duration":{"values":[4,5,6],"default":5},
            "aspect_ratio":{"values":["16:9","3:4"]},"generate_audio":{"type":"boolean","default":true},"quantity":{"values":[1,2],"default":1}
        }})
    }
    #[test]
    fn video_request_uses_typed_catalog_defaults_and_multimedia_references() {
        let body = generation_body(
            &video(),
            " animate ",
            &json!({}),
            &["image-ref".into(), "video-ref".into(), "audio-ref".into()],
            None,
        )
        .unwrap();
        assert_eq!(body["duration"], 5);
        assert_eq!(body["generate_audio"], true);
        assert_eq!(body["aspect_ratio"], "16:9");
        assert_eq!(body["operation"], "generate");
        assert_eq!(body["reference_asset_ids"].as_array().unwrap().len(), 3);
        let body = generation_body(
            &video(),
            "p",
            &json!({"duration":6,"generate_audio":false,"aspect_ratio":"3:4","quantity":2}),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(body["generate_audio"], false);
        assert_eq!(body["quantity"], 2);
        assert!(generation_body(&video(), "p", &json!({"duration":"6"}), &[], None).is_err());
    }
    #[test]
    fn video_effort_freezes_policy_and_rejects_disallowed_model() {
        let mut effort = json!({"id":"standard","policy_revision":"r2","generation":{"video_model_ids":["video-model"]}});
        let body = generation_body(&video(), "p", &json!({}), &[], Some(&effort)).unwrap();
        assert_eq!(body["effort_id"], "standard");
        assert_eq!(body["policy_revision"], "r2");
        effort["generation"]["video_model_ids"] = json!([]);
        assert!(generation_body(&video(), "p", &json!({}), &[], Some(&effort)).is_err());
    }
    #[test]
    fn image_edit_keeps_reference_order_and_current_parameters() {
        let model = json!({"id":"image-model","kind":"image","available":true,"operations":["generate","edit"],"parameters":{"size":{"values":["auto","1024x1024"],"default":"auto"},"quality":{"values":["high"],"default":"high"},"quantity":{"values":[1,2],"default":1}}});
        let body = generation_body(
            &model,
            "p",
            &json!({"count":2}),
            &["second".into(), "first".into()],
            None,
        )
        .unwrap();
        assert_eq!(body["operation"], "edit");
        assert_eq!(body["reference_asset_ids"], json!(["second", "first"]));
        assert_eq!(body["quantity"], 2);
        assert_eq!(
            generation_body(&model, "p", &json!({}), &[], None).unwrap()["operation"],
            "generate"
        );
    }

    #[test]
    fn nano_image_keeps_independent_ratio_without_inventing_quality() {
        let model = json!({"id":"nano-banana-2.1","kind":"image","available":true,"operations":["generate"],"parameters":{
            "size":{"values":["1K","2K","4K"],"default":"1K"},
            "aspect_ratio":{"values":["auto","9:16"],"default":"auto"}}});
        let body = generation_body(
            &model,
            "p",
            &json!({"size":"4K","aspect_ratio":"9:16"}),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(body["aspect_ratio"], "9:16");
        assert_eq!(body["size"], "4K");
        assert!(body.get("quality").is_none());
        assert!(generation_body(&model, "p", &json!({"aspect_ratio":"8:1"}), &[], None).is_err());
    }

    #[test]
    fn resolution_duration_combination_is_checked_before_submission() {
        let mut model = video();
        model["parameters"]["resolution"]["values"] = json!(["720p", "1080p"]);
        model["duration_resolution_map"] = json!([
            {"resolution":["720p"],"duration":[4,5,6]},
            {"resolution":["1080p"],"duration":[4,5]}
        ]);
        assert!(
            generation_body(
                &model,
                "p",
                &json!({"resolution":"1080p","duration":6}),
                &[],
                None
            )
            .is_err()
        );
        assert!(
            generation_body(
                &model,
                "p",
                &json!({"resolution":"720p","duration":6}),
                &[],
                None
            )
            .is_ok()
        );
    }
}
