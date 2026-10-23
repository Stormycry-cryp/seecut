// SPDX-License-Identifier: AGPL-3.0-or-later
//! Initial human-input observation references, never document or edit authority.

use crate::agent_identity::DocumentIdentity;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const INVALID: &str = "文档输入观察字段无效";
const INVALID_CONTEXT: &str = "编辑器当前状态或视图字段无效";

/// The closed wire projection of an existing native Context observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DocumentObservationBinding {
    pub(crate) app_instance_id: String,
    pub(crate) client_id: String,
    pub(crate) kind: String,
    pub(crate) project_id: String,
    pub(crate) document_session_id: String,
    pub(crate) revision: u64,
    pub(crate) context_revision: u64,
    pub(crate) selection_revision: u64,
    pub(crate) selected_object_ids: Vec<String>,
    /// Required on the wire, including when no supported native view exists.
    pub(crate) view_state_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flow: Option<FlowObservationBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FlowObservationBinding {
    pub(crate) canvas_id: String,
    pub(crate) account_scope: String,
    pub(crate) session_id: String,
}

// Value preserves required-nullable presence; Option alone would accept a
// missing viewStateId and collapse a forbidden flow:null into an absent field.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireBinding {
    app_instance_id: String,
    client_id: String,
    kind: String,
    project_id: String,
    document_session_id: String,
    revision: serde_json::Number,
    context_revision: serde_json::Number,
    selection_revision: serde_json::Number,
    selected_object_ids: Vec<String>,
    view_state_id: RequiredValue,
    #[serde(default)]
    flow: PresentValue,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct RequiredValue(Value);

#[derive(Default)]
enum PresentValue {
    #[default]
    Absent,
    Present(Value),
}

impl<'de> Deserialize<'de> for PresentValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

impl<'de> Deserialize<'de> for DocumentObservationBinding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = WireBinding::deserialize(deserializer)?;
        let invalid = || serde::de::Error::custom(INVALID);
        let view_state_id = match raw.view_state_id.0 {
            Value::Null => None,
            Value::String(value) => Some(value),
            _ => return Err(invalid()),
        };
        let flow = match raw.flow {
            PresentValue::Absent => None,
            PresentValue::Present(value) => {
                // Reject explicit null as well as extra or missing Flow axes.
                Some(serde_json::from_value(value).map_err(serde::de::Error::custom)?)
            }
        };
        let value = Self {
            app_instance_id: raw.app_instance_id,
            client_id: raw.client_id,
            kind: raw.kind,
            project_id: raw.project_id,
            document_session_id: raw.document_session_id,
            revision: epoch_number(&raw.revision).ok_or_else(invalid)?,
            context_revision: epoch_number(&raw.context_revision).ok_or_else(invalid)?,
            selection_revision: epoch_number(&raw.selection_revision).ok_or_else(invalid)?,
            selected_object_ids: raw.selected_object_ids,
            view_state_id,
            flow,
        };
        if !value.valid() {
            return Err(invalid());
        }
        Ok(value)
    }
}

impl DocumentObservationBinding {
    /// Validate a historical reference by its own original scope and fields.
    pub(crate) fn valid(&self) -> bool {
        let mut selected = HashSet::new();
        text(&self.app_instance_id, 100)
            && text(&self.client_id, 64)
            && text(&self.project_id, 1024)
            && text(&self.document_session_id, 64)
            && self.revision <= MAX_SAFE_INTEGER
            && self.context_revision <= MAX_SAFE_INTEGER
            && self.selection_revision <= MAX_SAFE_INTEGER
            && self
                .selected_object_ids
                .iter()
                .all(|id| text(id, 400) && id.encode_utf16().count() <= 100 && selected.insert(id))
            && self
                .view_state_id
                .as_deref()
                .is_none_or(|id| fingerprint(id, "view-v1-"))
            && match (self.kind.as_str(), self.flow.as_ref()) {
                ("canvas" | "clip", None) => true,
                ("flow", Some(flow)) => {
                    self.view_state_id.is_none()
                        && text(&flow.canvas_id, 512)
                        && text(&flow.account_scope, 512)
                        && text(&flow.session_id, 512)
                        && flow.session_id == self.document_session_id
                }
                _ => false,
            }
    }

    pub(crate) fn matches_identity(&self, identity: &DocumentIdentity, client_id: &str) -> bool {
        self.valid()
            && self.app_instance_id == identity.instance_id
            && self.client_id == client_id
            && self.project_id == identity.project_id
            && self.document_session_id == identity.document_session_id
    }

    /// Project only the unique exact document in a real native response.data.
    /// The caller retains the native read grant; this function performs no I/O.
    pub(crate) fn from_native_context(
        data: &Value,
        identity: &DocumentIdentity,
        client_id: &str,
    ) -> Result<Self, String> {
        if data.get("appInstanceId").and_then(Value::as_str) != Some(&identity.instance_id) {
            return Err(INVALID_CONTEXT.into());
        }
        let documents = data
            .get("documents")
            .and_then(Value::as_array)
            .ok_or(INVALID_CONTEXT)?;
        let mut matches = documents.iter().filter(|doc| {
            doc.get("projectId").and_then(Value::as_str) == Some(&identity.project_id)
                && doc.get("documentSessionId").and_then(Value::as_str)
                    == Some(&identity.document_session_id)
        });
        let doc = matches.next().ok_or(INVALID_CONTEXT)?;
        if matches.next().is_some() || doc.get("kind") != data.get("activeWorkspace") {
            return Err(INVALID_CONTEXT.into());
        }
        let kind = field(doc, "kind")?.as_str().ok_or(INVALID_CONTEXT)?;
        let selected_object_ids = field(doc, "selectedObjectIds")?
            .as_array()
            .ok_or(INVALID_CONTEXT)?
            .iter()
            .map(|value| value.as_str().map(str::to_owned).ok_or(INVALID_CONTEXT))
            .collect::<Result<Vec<_>, _>>()?;
        let flow = if kind == "flow" {
            Some(FlowObservationBinding {
                canvas_id: string_field(doc, "canvasId")?,
                account_scope: string_field(doc, "accountScope")?,
                session_id: string_field(doc, "sessionId")?,
            })
        } else {
            None
        };
        let mut value = Self {
            app_instance_id: identity.instance_id.clone(),
            client_id: client_id.into(),
            kind: kind.into(),
            project_id: identity.project_id.clone(),
            document_session_id: identity.document_session_id.clone(),
            revision: epoch_value(field(doc, "revision")?).ok_or(INVALID_CONTEXT)?,
            context_revision: epoch_value(field(doc, "contextRevision")?).ok_or(INVALID_CONTEXT)?,
            selection_revision: epoch_value(field(doc, "selectionRevision")?)
                .ok_or(INVALID_CONTEXT)?,
            selected_object_ids,
            view_state_id: None,
            flow,
        };
        if !value.valid() {
            return Err(INVALID_CONTEXT.into());
        }
        value.view_state_id = parsed_view_state_id(doc, &value)?;
        if !value.valid() {
            return Err(INVALID_CONTEXT.into());
        }
        Ok(value)
    }
}

fn text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn fingerprint(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn epoch_number(value: &serde_json::Number) -> Option<u64> {
    let number = value.as_f64()?;
    (number.is_finite()
        && number >= 0.0
        && number <= MAX_SAFE_INTEGER as f64
        && number.fract() == 0.0)
        .then_some(number as u64)
}

fn epoch_value(value: &Value) -> Option<u64> {
    value.as_number().and_then(epoch_number)
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value.get(key).ok_or_else(|| INVALID_CONTEXT.into())
}

fn string_field(value: &Value, key: &str) -> Result<String, String> {
    field(value, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| INVALID_CONTEXT.into())
}

fn ensure(condition: bool) -> Result<(), String> {
    condition
        .then_some(())
        .ok_or_else(|| INVALID_CONTEXT.into())
}

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value.as_object().ok_or_else(|| INVALID_CONTEXT.into())
}

fn finite(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| INVALID_CONTEXT.into())
}

fn nonnegative(value: &Value) -> Result<(), String> {
    ensure(finite(value)? >= 0.0)
}

fn positive(value: &Value) -> Result<(), String> {
    ensure(finite(value)? > 0.0)
}

fn size(value: &Value) -> Result<(), String> {
    object(value)?;
    nonnegative(field(value, "width")?)?;
    nonnegative(field(value, "height")?)
}

fn nullable_size(value: &Value) -> Result<(), String> {
    if value.is_null() { Ok(()) } else { size(value) }
}

fn rect(value: &Value) -> Result<(), String> {
    object(value)?;
    finite(field(value, "x")?)?;
    finite(field(value, "y")?)?;
    size(value)
}

fn nullable_rect(value: &Value) -> Result<(), String> {
    if value.is_null() { Ok(()) } else { rect(value) }
}

fn object_id(value: &Value, prefix: &str) -> Result<(), String> {
    let value = value.as_str().ok_or(INVALID_CONTEXT)?;
    let suffix = value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix(':'));
    ensure(suffix.is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 100
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }))
}

fn object_ids(value: &Value, prefix: &str) -> Result<Vec<String>, String> {
    let ids = value.as_array().ok_or(INVALID_CONTEXT)?;
    let mut unique = HashSet::new();
    ids.iter()
        .map(|id| {
            object_id(id, prefix)?;
            let id = id.as_str().ok_or(INVALID_CONTEXT)?;
            ensure(unique.insert(id))?;
            Ok(id.to_owned())
        })
        .collect()
}

/// Mirror private parseView's whitelist validation before retaining its hash.
/// Unknown view versions deliberately project null, as the private parser does.
fn parsed_view_state_id(
    doc: &Value,
    observation: &DocumentObservationBinding,
) -> Result<Option<String>, String> {
    let Some(source) = doc.get("view").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    object(source)?;
    if source.get("version").and_then(Value::as_f64) != Some(1.0) {
        return Ok(None);
    }
    ensure(matches!(observation.kind.as_str(), "canvas" | "clip"))?;
    ensure(source.get("kind").and_then(Value::as_str) == Some(&observation.kind))?;
    let view_state_id = string_field(source, "viewStateId")?;
    ensure(fingerprint(&view_state_id, "view-v1-"))?;
    let spaces = field(source, "coordinateSpaces")?;
    object(spaces)?;
    for (key, expected) in [
        ("viewport", "slintLogicalPixelsTopLeftYDown"),
        ("document", "documentPixelsTopLeftYDown"),
        ("timeline", "secondsFromTimelineStart"),
    ] {
        ensure(field(spaces, key)?.as_str() == Some(expected))?;
    }
    let allowed: &[&str] = if observation.kind == "canvas" {
        &[
            "canvas.document",
            "canvas.viewport",
            "canvas.stageRect",
            "canvas.visibleDocumentRect",
        ]
    } else {
        &[
            "clip.timeline.viewport",
            "clip.timeline.visibleEndSeconds",
            "clip.timeline.verticalScroll",
            "clip.preview.viewport",
            "clip.preview.visibleDocumentRect",
            "clip.preview.zoom",
            "clip.preview.pan",
            "clip.preview.renderedFrameTime",
            "clip.preview.renderedImageSize",
        ]
    };
    let mut unknown = HashSet::new();
    for key in field(source, "unavailable")?
        .as_array()
        .ok_or(INVALID_CONTEXT)?
    {
        let key = key.as_str().ok_or(INVALID_CONTEXT)?;
        ensure(allowed.contains(&key) && unknown.insert(key))?;
    }
    let binding = field(source, "binding")?;
    object(binding)?;
    ensure(field(binding, "appInstanceId")?.as_str() == Some(&observation.app_instance_id))?;
    for key in ["projectId", "documentSessionId"] {
        ensure(field(binding, key)? == field(doc, key)?)?;
    }
    for key in ["revision", "contextRevision", "selectionRevision"] {
        ensure(epoch_value(field(binding, key)?) == epoch_value(field(doc, key)?))?;
    }
    if observation.kind == "canvas" {
        let canvas = field(source, "canvas")?;
        if canvas.is_null() && unknown.contains("canvas.document") {
            return Ok(Some(view_state_id));
        }
        object(canvas)?;
        nullable_size(field(canvas, "viewport")?)?;
        let zoom = field(canvas, "zoom")?;
        object(zoom)?;
        positive(field(zoom, "percent")?)?;
        positive(field(zoom, "logicalPixelsPerDocumentPixel")?)?;
        let pan = field(canvas, "pan")?;
        object(pan)?;
        finite(field(pan, "x")?)?;
        finite(field(pan, "y")?)?;
        for key in ["stageRect", "visibleDocumentRect", "pixelSelectionBounds"] {
            nullable_rect(field(canvas, key)?)?;
        }
        size(field(canvas, "documentSize")?)?;
        let selected = field(canvas, "selectedLayerId")?;
        let selected = if selected.is_null() {
            Vec::new()
        } else {
            object_id(selected, "layer")?;
            vec![selected.as_str().ok_or(INVALID_CONTEXT)?.to_owned()]
        };
        ensure(selected == observation.selected_object_ids)?;
        let mask = field(canvas, "pixelSelectionStateId")?;
        ensure(mask.is_null() || mask.as_str().is_some_and(|id| fingerprint(id, "mask-v1-")))?;
        ensure(field(canvas, "paintingLayerMask")?.is_boolean())?;
    } else {
        let clip = field(source, "clip")?;
        object(clip)?;
        object_id(field(clip, "timelineId")?, "timeline")?;
        nonnegative(field(clip, "playheadSeconds")?)?;
        ensure(field(clip, "playing")?.is_boolean())?;
        let timeline = field(clip, "timeline")?;
        object(timeline)?;
        finite(field(timeline, "visibleStartSeconds")?)?;
        positive(field(timeline, "secondsPerLogicalPixel")?)?;
        ensure(field(timeline, "visibleEndSeconds")?.is_null())?;
        ensure(field(timeline, "viewport")?.is_null())?;
        let preview = field(clip, "preview")?;
        object(preview)?;
        size(field(preview, "outputSize")?)?;
        nullable_size(field(preview, "renderedImageSize")?)?;
        ensure(field(preview, "viewport")?.is_null())?;
        ensure(field(preview, "visibleDocumentRect")?.is_null())?;
        let selected = object_ids(field(clip, "selectedClipIds")?, "clip")?;
        object_ids(field(clip, "selectedMediaIds")?, "media")?;
        // Both sets are already unique: equality here has parseView's sorted
        // membership semantics without changing the original selection order.
        let expected = observation
            .selected_object_ids
            .iter()
            .collect::<HashSet<_>>();
        ensure(
            selected.len() == expected.len() && selected.iter().all(|id| expected.contains(id)),
        )?;
    }
    Ok(Some(view_state_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> DocumentIdentity {
        DocumentIdentity {
            instance_id: "instance".into(),
            project_id: "project".into(),
            document_session_id: "session".into(),
        }
    }

    fn wire() -> Value {
        json!({"appInstanceId":"instance","clientId":"client","kind":"canvas",
            "projectId":"project","documentSessionId":"session","revision":1,
            "contextRevision":2,"selectionRevision":3,"selectedObjectIds":["layer:1"],
            "viewStateId":null})
    }

    fn context(kind: &str) -> Value {
        let mut doc = json!({"kind":kind,"projectId":"project","documentSessionId":"session",
            "revision":1,"contextRevision":2,"selectionRevision":3,"selectedObjectIds":["layer:1"]});
        if kind == "flow" {
            doc["canvasId"] = json!("canvas");
            doc["accountScope"] = json!("account");
            doc["sessionId"] = json!("session");
            doc["selectedObjectIds"] = json!(["node:1"]);
        }
        json!({"appInstanceId":"instance","activeWorkspace":kind,"documents":[doc]})
    }

    fn canvas_view(doc: &Value) -> Value {
        json!({"version":1,"kind":"canvas","viewStateId":format!("view-v1-{}", "a".repeat(64)),
            "binding":{"appInstanceId":"instance","projectId":doc["projectId"],
                "documentSessionId":doc["documentSessionId"],"revision":doc["revision"],
                "contextRevision":doc["contextRevision"],"selectionRevision":doc["selectionRevision"]},
            "coordinateSpaces":{"viewport":"slintLogicalPixelsTopLeftYDown",
                "document":"documentPixelsTopLeftYDown","timeline":"secondsFromTimelineStart"},
            "canvas":{"viewport":null,"zoom":{"percent":100,"logicalPixelsPerDocumentPixel":1},
                "pan":{"x":0,"y":0},"stageRect":null,"visibleDocumentRect":null,
                "documentSize":{"width":100,"height":200},"selectedLayerId":"layer:1",
                "pixelSelectionBounds":null,"pixelSelectionStateId":null,"paintingLayerMask":false},
            "unavailable":["canvas.viewport","canvas.stageRect","canvas.visibleDocumentRect"]})
    }

    fn decode(value: Value) -> Result<DocumentObservationBinding, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn required_nullable_view_and_forbidden_flow_presence() {
        let value = decode(wire()).unwrap();
        assert!(value.valid());
        assert_eq!(serde_json::to_value(value).unwrap(), wire());
        let mut missing = wire();
        missing.as_object_mut().unwrap().remove("viewStateId");
        assert!(decode(missing).is_err());
        for bad in [json!(0), json!(false), json!("view-v1-fake")] {
            let mut value = wire();
            value["viewStateId"] = bad;
            assert!(decode(value).is_err());
        }
        for kind in ["canvas", "clip"] {
            for flow in [
                Value::Null,
                json!({"canvasId":"c","accountScope":"a","sessionId":"session"}),
            ] {
                let mut value = wire();
                value["kind"] = json!(kind);
                value["flow"] = flow;
                assert!(decode(value).is_err());
            }
        }
    }

    #[test]
    fn closed_fields_and_semantic_numbers() {
        for key in ["revision", "viewStateId", "selectedObjectIds", "projectId"] {
            let mut value = wire();
            value.as_object_mut().unwrap().remove(key);
            assert!(decode(value).is_err(), "missing {key}");
        }
        for bad in [
            json!(-1),
            json!(1.25),
            json!(9_007_199_254_740_992u64),
            Value::Null,
            json!("1"),
        ] {
            for key in ["revision", "contextRevision", "selectionRevision"] {
                let mut value = wire();
                value[key] = bad.clone();
                assert!(decode(value).is_err(), "{key}: {bad}");
            }
        }
        let mut value = wire();
        value["revision"] = json!(9_007_199_254_740_991u64);
        value["contextRevision"] = json!(1.0);
        assert!(decode(value).is_ok());
        let mut value = wire();
        value["extra"] = Value::Null;
        assert!(decode(value).is_err());
        let duplicate =
            serde_json::to_string(&wire())
                .unwrap()
                .replacen("{", "{\"revision\":1,", 1);
        assert!(serde_json::from_str::<DocumentObservationBinding>(&duplicate).is_err());
    }

    #[test]
    fn flow_requires_three_axes_and_its_document_session() {
        let value = DocumentObservationBinding::from_native_context(
            &context("flow"),
            &identity(),
            "client",
        )
        .unwrap();
        assert!(value.matches_identity(&identity(), "client"));
        let original = serde_json::to_value(value).unwrap();
        assert_eq!(
            original["flow"],
            json!({"canvasId":"canvas","accountScope":"account","sessionId":"session"})
        );
        for axis in ["canvasId", "accountScope", "sessionId"] {
            for bad in [
                Value::Null,
                json!(""),
                json!("\u{0085}"),
                json!("x".repeat(513)),
            ] {
                let mut value = original.clone();
                value["flow"][axis] = bad;
                assert!(decode(value).is_err());
            }
            let mut value = original.clone();
            value["flow"].as_object_mut().unwrap().remove(axis);
            assert!(decode(value).is_err());
        }
        for mutate in ["missing", "null", "session", "extra", "view"] {
            let mut value = original.clone();
            match mutate {
                "missing" => {
                    value.as_object_mut().unwrap().remove("flow");
                }
                "null" => value["flow"] = Value::Null,
                "session" => value["flow"]["sessionId"] = json!("other"),
                "extra" => value["flow"]["extra"] = Value::Null,
                _ => value["viewStateId"] = json!(format!("view-v1-{}", "a".repeat(64))),
            }
            assert!(decode(value).is_err(), "{mutate}");
        }
    }

    #[test]
    fn preserves_large_ordered_selection_and_utf16_id_limit() {
        let ids = (0..1000)
            .rev()
            .map(|id| format!("clip:{id}"))
            .collect::<Vec<_>>();
        let mut data = context("clip");
        data["documents"][0]["selectedObjectIds"] = json!(ids);
        let value =
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").unwrap();
        assert_eq!(value.selected_object_ids, ids);
        let mut value = wire();
        value["selectedObjectIds"] = json!(["😀".repeat(50)]);
        assert!(decode(value.clone()).is_ok());
        value["selectedObjectIds"] = json!(["😀".repeat(51)]);
        assert!(decode(value).is_err());
        for ids in [json!(["a", "a"]), json!([""]), json!(["a\u{007f}"])] {
            let mut value = wire();
            value["selectedObjectIds"] = ids;
            assert!(decode(value).is_err());
        }
    }

    #[test]
    fn accepts_full_project_byte_limit_and_requires_exact_scope() {
        let mut identity = identity();
        identity.project_id = "界".repeat(341) + "x";
        assert_eq!(identity.project_id.len(), 1024);
        let mut data = context("canvas");
        data["documents"][0]["projectId"] = json!(identity.project_id);
        let value =
            DocumentObservationBinding::from_native_context(&data, &identity, "client").unwrap();
        assert!(value.matches_identity(&identity, "client"));
        assert!(!value.matches_identity(&identity, "other-client"));
        let mut other = identity.clone();
        other.project_id.push('x');
        assert!(!value.matches_identity(&other, "client"));
        data["documents"][0]["projectId"] = json!(other.project_id);
        assert!(DocumentObservationBinding::from_native_context(&data, &other, "client").is_err());
        for axis in ["appInstanceId", "projectId", "documentSessionId"] {
            let mut value = serde_json::to_value(&value).unwrap();
            value[axis] = json!("different");
            assert!(!decode(value).unwrap().matches_identity(&identity, "client"));
        }
    }

    #[test]
    fn context_requires_unique_exact_document_and_active_workspace() {
        let original = context("canvas");
        for mutate in [
            "instance",
            "project",
            "session",
            "workspace",
            "duplicate",
            "no-docs",
        ] {
            let mut data = original.clone();
            match mutate {
                "instance" => data["appInstanceId"] = json!("other"),
                "project" => data["documents"][0]["projectId"] = json!("other"),
                "session" => data["documents"][0]["documentSessionId"] = json!("other"),
                "workspace" => data["activeWorkspace"] = json!("clip"),
                "duplicate" => {
                    let doc = data["documents"][0].clone();
                    data["documents"].as_array_mut().unwrap().push(doc);
                }
                _ => {
                    data.as_object_mut().unwrap().remove("documents");
                }
            }
            assert!(
                DocumentObservationBinding::from_native_context(&data, &identity(), "client")
                    .is_err(),
                "{mutate}"
            );
        }
    }

    #[test]
    fn accepts_only_existing_supported_validated_view_hash() {
        let mut data = context("canvas");
        data["documents"][0]["view"] = canvas_view(&data["documents"][0]);
        let value =
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").unwrap();
        assert_eq!(
            value.view_state_id,
            Some(format!("view-v1-{}", "a".repeat(64)))
        );
        for pointer in [
            "/binding/projectId",
            "/binding/appInstanceId",
            "/binding/revision",
            "/coordinateSpaces/viewport",
            "/canvas/zoom/percent",
            "/canvas/selectedLayerId",
            "/canvas/paintingLayerMask",
            "/unavailable",
        ] {
            let mut changed = data.clone();
            *changed["documents"][0]["view"]
                .pointer_mut(pointer)
                .unwrap() = json!("wrong");
            assert!(
                DocumentObservationBinding::from_native_context(&changed, &identity(), "client")
                    .is_err(),
                "{pointer}"
            );
        }
        for version in [json!(2), json!("1"), Value::Null] {
            let mut changed = data.clone();
            changed["documents"][0]["view"]["version"] = version;
            let value =
                DocumentObservationBinding::from_native_context(&changed, &identity(), "client")
                    .unwrap();
            assert_eq!(value.view_state_id, None);
        }
        data["documents"][0]["view"]["canvas"] = Value::Null;
        data["documents"][0]["view"]["unavailable"] = json!(["canvas.document"]);
        assert!(
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").is_ok()
        );
    }

    #[test]
    fn clip_view_checks_geometry_ids_and_keeps_native_selection_order() {
        let mut data = context("clip");
        data["documents"][0]["selectedObjectIds"] = json!(["clip:b", "clip:a"]);
        let mut view = canvas_view(&data["documents"][0]);
        view["kind"] = json!("clip");
        view.as_object_mut().unwrap().remove("canvas");
        view["unavailable"] = json!(["clip.timeline.viewport", "clip.preview.viewport"]);
        view["clip"] = json!({"timelineId":"timeline:main","playheadSeconds":0,"playing":false,
            "timeline":{"visibleStartSeconds":-1,"secondsPerLogicalPixel":0.1,
                "visibleEndSeconds":null,"viewport":null},
            "preview":{"outputSize":{"width":1920,"height":1080},
                "renderedImageSize":null,"viewport":null,"visibleDocumentRect":null},
            "selectedClipIds":["clip:a","clip:b"],"selectedMediaIds":["media:source"]});
        data["documents"][0]["view"] = view;
        let value =
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").unwrap();
        assert_eq!(value.selected_object_ids, ["clip:b", "clip:a"]);
        assert!(value.view_state_id.is_some());
        for (pointer, bad) in [
            ("/clip/playheadSeconds", json!(-1)),
            ("/clip/timeline/secondsPerLogicalPixel", json!(0)),
            ("/clip/timeline/visibleEndSeconds", json!(1)),
            ("/clip/preview/viewport", json!({"width":1,"height":1})),
            ("/clip/preview/outputSize/width", json!(-1)),
            ("/clip/timelineId", json!("timeline:bad space")),
            ("/clip/selectedClipIds", json!(["clip:a"])),
            ("/clip/selectedMediaIds", json!(["media:a", "media:a"])),
            ("/unavailable", json!(["canvas.viewport"])),
        ] {
            let mut changed = data.clone();
            *changed["documents"][0]["view"]
                .pointer_mut(pointer)
                .unwrap() = bad;
            assert!(
                DocumentObservationBinding::from_native_context(&changed, &identity(), "client")
                    .is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn native_flow_axes_and_view_cannot_be_fabricated() {
        for axis in ["canvasId", "accountScope", "sessionId"] {
            let mut data = context("flow");
            data["documents"][0].as_object_mut().unwrap().remove(axis);
            assert!(
                DocumentObservationBinding::from_native_context(&data, &identity(), "client")
                    .is_err(),
                "{axis}"
            );
        }
        let mut data = context("flow");
        data["documents"][0]["sessionId"] = json!("other");
        assert!(
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").is_err()
        );
        let mut data = context("flow");
        data["documents"][0]["view"] = canvas_view(&data["documents"][0]);
        assert!(
            DocumentObservationBinding::from_native_context(&data, &identity(), "client").is_err()
        );
        data["documents"][0]["view"]["version"] = json!(2);
        assert_eq!(
            DocumentObservationBinding::from_native_context(&data, &identity(), "client")
                .unwrap()
                .view_state_id,
            None
        );
    }

    #[test]
    fn all_scope_text_limits_are_utf8_bytes_without_control_characters() {
        for (key, limit) in [
            ("appInstanceId", 100),
            ("clientId", 64),
            ("projectId", 1024),
            ("documentSessionId", 64),
        ] {
            let mut value = wire();
            value[key] = json!("x".repeat(limit));
            assert!(decode(value.clone()).is_ok(), "{key}");
            for bad in ["x".repeat(limit + 1), String::new(), "x\u{0085}".into()] {
                value[key] = json!(bad);
                assert!(decode(value.clone()).is_err(), "{key}");
            }
        }
        let mut value = wire();
        value["kind"] = json!("app");
        assert!(decode(value).is_err());
    }
}
