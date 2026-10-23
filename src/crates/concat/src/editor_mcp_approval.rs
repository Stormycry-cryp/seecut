//! Display projection only. Full requests remain frozen and checked by the permit path.
use concat_editor_mcp::FlowWorkspace;
use serde_json::Value;

pub(super) fn canvas_edit_summary(edit: &concat_editor_mcp::CanvasEdit) -> (String, String) {
    use concat_editor_mcp::CanvasEdit::*;
    let title = match edit {
        LayerSelect { .. } => "选择图层",
        LayerAdd => "新建图层",
        LayerDelete { .. } => "删除图层",
        LayerMove { .. } => "调整图层顺序",
        GroupAdd => "新建图层组",
        LayerMoveInto { .. } => "移动图层到组",
        GroupSetExpanded { .. } => "展开或收起图层组",
        LayerSetVisibility { .. } => "设置图层显隐",
        LayerSetOpacity { .. } => "设置图层透明度",
        SelectionRect { .. } => "设置矩形选区",
        SelectionAll => "选择全部像素",
        SelectionClear => "取消像素选区",
        SelectionFill { .. } => "填充像素选区",
        SelectionDelete { .. } => "清除选区像素",
        MaskAdd { .. } => "新增图层蒙版",
        MaskRemove { .. } => "移除图层蒙版",
        MaskSetEnabled { .. } => "设置蒙版启停",
        MaskSelect { .. } => "选择蒙版绘制",
        BrushSet { .. } => "设置画笔",
        BrushStroke { .. } => "绘制笔触",
        Undo => "撤销画布编辑",
        Redo => "重做画布编辑",
        AdjustmentAdd { .. } => "新增调整图层",
        AdjustmentSetParameter { .. } => "设置调整参数",
        CurveSet { .. } | CurveAdd { .. } | CurveRemove { .. } => "编辑颜色曲线",
    };
    let description = match edit {
        BrushStroke {
            object_id,
            target,
            points,
        } => format!(
            "{object_id} · {target:?} · {} 个文档像素坐标点，完整坐标见参数详情",
            points.len()
        ),
        _ => serde_json::to_string(edit).unwrap_or_default(),
    };
    (title.into(), description)
}

fn short(value: &str, limit: usize) -> String {
    let clean: String = value.chars().filter(|c| !c.is_control()).collect();
    let mut result: String = clean.chars().take(limit).collect();
    if clean.chars().count() > limit {
        result.push('…');
    }
    result
}
fn field(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|v| !v.trim().is_empty() && !v.chars().any(char::is_control))
        .map(|v| short(v, 64))
}
fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|k| allowed.contains(&k.as_str())))
}
fn list(value: &Value) -> Option<&Vec<Value>> {
    value.as_array().filter(|v| !v.is_empty() && v.len() <= 64)
}

pub(super) fn flow_summary(call: &FlowWorkspace) -> Option<(String, String)> {
    let a = &call.args;
    let (title, description) = match call.name.as_str() {
        "flow_add_node" if keys(a, &["type", "position", "label"]) => {
            let kind = field(a, "type")?;
            let x = a["position"]["x"].as_f64()?;
            let y = a["position"]["y"].as_f64()?;
            if !x.is_finite() || !y.is_finite() || !keys(&a["position"], &["x", "y"]) {
                return None;
            }
            if a.get("label").is_some_and(|v| !v.is_string()) {
                return None;
            }
            let label = field(a, "label")
                .map(|v| format!(" · 名称“{v}”"))
                .unwrap_or_default();
            ("新增节点", format!("类型：{kind}{label} · 位置 {x}, {y}"))
        }
        "flow_connect_nodes" if keys(a, &["connections"]) => {
            let connections = list(&a["connections"])?;
            let mut rows = Vec::new();
            for connection in connections {
                if !keys(
                    connection,
                    &["source", "sourceHandle", "target", "targetHandle"],
                ) {
                    return None;
                }
                rows.push(format!(
                    "{} / {} → {} / {}",
                    field(connection, "source")?,
                    field(connection, "sourceHandle")?,
                    field(connection, "target")?,
                    field(connection, "targetHandle")?
                ));
            }
            (
                "连接节点",
                format!(
                    "{} 条连接：{}{}",
                    rows.len(),
                    rows.iter().take(2).cloned().collect::<Vec<_>>().join("；"),
                    if rows.len() > 2 {
                        "；其余见参数详情"
                    } else {
                        ""
                    }
                ),
            )
        }
        "flow_delete_nodes" if keys(a, &["nodeIds"]) => {
            let ids = list(&a["nodeIds"])?;
            let rows = ids
                .iter()
                .map(|v| v.as_str().filter(|s| !s.is_empty()).map(|s| short(s, 64)))
                .collect::<Option<Vec<_>>>()?;
            (
                "删除节点",
                format!(
                    "删除 {} 个节点：{}{}",
                    rows.len(),
                    rows.iter().take(3).cloned().collect::<Vec<_>>().join("、"),
                    if rows.len() > 3 {
                        "；其余见参数详情"
                    } else {
                        ""
                    }
                ),
            )
        }
        "flow_switch_canvas" if keys(a, &["canvasId"]) => {
            ("切换画布", format!("目标画布：{}", field(a, "canvasId")?))
        }
        "flow_update_node" | "flow_execute_node" if keys(a, &["nodeId", "name", "args"]) => {
            let node = field(a, "nodeId")?;
            let params = &a["args"];
            match (call.name.as_str(), a["name"].as_str()?) {
                ("flow_update_node", "node:setText") if keys(params, &["text"]) => (
                    "修改文本",
                    format!(
                        "节点：{node} · {} 字符；不启动生成",
                        params["text"].as_str()?.chars().count()
                    ),
                ),
                ("flow_update_node", "node:setPrompt") if keys(params, &["prompt"]) => (
                    "修改提示词",
                    format!(
                        "节点：{node} · {} 字符；不启动生成",
                        params["prompt"].as_str()?.chars().count()
                    ),
                ),
                ("flow_update_node", "node:setConfig")
                    if keys(
                        params,
                        &[
                            "providerId",
                            "modelId",
                            "size",
                            "ratio",
                            "duration",
                            "resolution",
                            "mode",
                            "generateAudio",
                            "language",
                            "sampleRate",
                            "format",
                        ],
                    ) =>
                {
                    if params.get("providerId").is_some_and(|v| v != "seecut") {
                        return None;
                    }
                    if params.get("providerId").is_some() != params.get("modelId").is_some() {
                        return None;
                    }
                    for (key, value) in params.as_object()? {
                        let valid = match key.as_str() {
                            "generateAudio" => value.is_boolean(),
                            "duration" => value.as_f64().is_some_and(|n| n.is_finite() && n > 0.0),
                            "sampleRate" => value.as_u64().is_some_and(|n| n > 0),
                            "mode" => {
                                value.as_str().is_some_and(|v| !v.is_empty())
                                    || value.as_array().is_some_and(|v| {
                                        !v.is_empty()
                                            && v.iter()
                                                .all(|v| v.as_str().is_some_and(|v| !v.is_empty()))
                                    })
                            }
                            _ => value.as_str().is_some_and(|v| !v.is_empty()),
                        };
                        if !valid {
                            return None;
                        }
                    }
                    let fields = params
                        .as_object()?
                        .keys()
                        .map(|key| match key.as_str() {
                            "providerId" => "服务",
                            "modelId" => "模型",
                            "size" | "resolution" => "分辨率",
                            "ratio" => "比例",
                            "duration" => "时长",
                            "mode" => "模式",
                            "generateAudio" => "声音",
                            "language" => "语种",
                            "sampleRate" => "采样率",
                            "format" => "格式",
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>()
                        .join("、");
                    (
                        "修改生成参数",
                        format!("节点：{node} · {fields}；不启动生成，具体值见参数详情"),
                    )
                }
                ("flow_execute_node", "node:generateImage") if keys(params, &[]) => (
                    "生成图片",
                    format!("节点：{node} · 使用当前提示词和模型提交一次生成，结果默认进入资产库"),
                ),
                ("flow_execute_node", "node:generateVideo") if keys(params, &[]) => (
                    "生成视频",
                    format!("节点：{node} · 使用当前提示词和模型提交一次生成，结果默认进入资产库"),
                ),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some((title.into(), description))
}

// Do not publish account bindings or host configuration. Defensive redaction also
// applies to native argument details; authorization still compares the original.
pub(super) fn details(args: &Value) -> String {
    fn redact(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let k = key.to_ascii_lowercase();
                        let sensitive = [
                            "token",
                            "secret",
                            "password",
                            "apikey",
                            "api_key",
                            "authorization",
                            "cookie",
                            "headers",
                            "endpoint",
                            "serviceurl",
                            "baseurl",
                        ]
                        .iter()
                        .any(|word| k.contains(word));
                        (
                            key.clone(),
                            if sensitive {
                                Value::String("[已隐藏]".into())
                            } else {
                                redact(value)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_string(&redact(args)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn view(name: &str, args: Value) -> Option<(String, String)> {
        let call = crate::toonflow_agent::approval(
            name,
            json!({"projectId":"p","canvasId":"c","sessionId":"s","accountScope":"account","revision":1,"args":args}),
        )?;
        flow_summary(&call)
    }
    #[test]
    fn maps_only_real_whitelisted_actions_and_targets() {
        let cases = [
            (
                "flow_add_node",
                json!({"type":"textNode","position":{"x":1,"y":2}}),
                "新增节点",
            ),
            (
                "flow_connect_nodes",
                json!({"connections":[{"source":"text-1","sourceHandle":"text","target":"image-1","targetHandle":"prompt"}]}),
                "连接节点",
            ),
            (
                "flow_delete_nodes",
                json!({"nodeIds":["image-1"]}),
                "删除节点",
            ),
            (
                "flow_switch_canvas",
                json!({"canvasId":"canvas-2"}),
                "切换画布",
            ),
            (
                "flow_update_node",
                json!({"nodeId":"text-1","name":"node:setText","args":{"text":"实际文本"}}),
                "修改文本",
            ),
            (
                "flow_update_node",
                json!({"nodeId":"image-1","name":"node:setPrompt","args":{"prompt":"真实提示词"}}),
                "修改提示词",
            ),
            (
                "flow_update_node",
                json!({"nodeId":"image-1","name":"node:setConfig","args":{"ratio":"1:1"}}),
                "修改生成参数",
            ),
            (
                "flow_execute_node",
                json!({"nodeId":"image-1","name":"node:generateImage","args":{}}),
                "生成图片",
            ),
            (
                "flow_execute_node",
                json!({"nodeId":"video-1","name":"node:generateVideo","args":{}}),
                "生成视频",
            ),
        ];
        for (name, args, title) in cases {
            let v = view(name, args).unwrap();
            assert_eq!(v.0, title);
            assert!(!v.1.contains("flow_"));
            assert!(!v.1.contains("account"));
        }
    }
    #[test]
    fn unknown_or_incomplete_parameters_cannot_enable_approval() {
        for (name, args) in [
            (
                "flow_execute_node",
                json!({"nodeId":"n","name":"node:deleteAll","args":{}}),
            ),
            (
                "flow_update_node",
                json!({"nodeId":"n","name":"node:setConfig","args":{"apiKey":"synthetic"}}),
            ),
            ("flow_delete_nodes", json!({"nodeIds":[]})),
            ("flow_add_node", json!({"type":"textNode"})),
            ("flow_write", json!({})),
        ] {
            assert!(view(name, args).is_none());
        }
    }
    #[test]
    fn long_content_stays_in_details_and_never_mutates_the_request() {
        let text = "长文本".repeat(2000);
        let args = json!({"nodeId":"stable-1","name":"node:setText","args":{"text":text}});
        let before = args.clone();
        let v = view("flow_update_node", args.clone()).unwrap();
        assert!(v.1.chars().count() < 100);
        assert!(v.1.contains("stable-1"));
        assert_eq!(
            serde_json::from_str::<Value>(&details(&args)).unwrap(),
            args
        );
        assert_eq!(args, before);
    }
    #[test]
    fn details_redact_credentials_without_changing_the_frozen_request() {
        let args = json!({"prompt":"safe","nested":{"apiKey":"synthetic-hidden","writeToken":"synthetic-token"}});
        let text = details(&args);
        assert!(!text.contains("synthetic-"));
        assert!(text.contains("safe"));
        assert_eq!(args["nested"]["apiKey"], "synthetic-hidden");
    }
}
