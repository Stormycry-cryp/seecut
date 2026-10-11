// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public model aliases only. Account credentials never enter this state.
use serde_json::Value;
pub(crate) const ACCOUNT_EXPIRED: &str = "登录已过期，请重新登录";

#[derive(Clone)]
pub(crate) struct Choice {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) available: bool,
    pub(crate) policy: Value,
}
#[derive(Clone)]
pub(crate) struct Effort {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) revision: String,
    pub(crate) model_id: String,
    pub(crate) available: bool,
    pub(crate) public: Value,
}
impl Effort {
    pub(crate) fn reason(&self) -> &'static str {
        if self.available {
            ""
        } else {
            "该强度暂不可用"
        }
    }
    pub(crate) fn policy(&self) -> Value {
        let mut policy = self.public.clone();
        if let Some(object) = policy.as_object_mut() {
            object.remove("available");
            object.remove("reason_code");
        }
        policy
    }
}
#[derive(Default)]
pub(crate) struct Directory {
    pub(crate) choices: Vec<Choice>,
    pub(crate) efforts: Vec<Effort>,
}
#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) choices: Vec<Choice>,
    pub(crate) efforts: Vec<Effort>,
    pub(crate) selected: usize,
    pub(crate) loading: bool,
    loaded: bool,
    ticket: u64,
    error: Option<&'static str>,
}
impl Catalog {
    pub(crate) fn clear(&mut self) {
        let ticket = self.ticket.wrapping_add(1);
        *self = Self {
            ticket,
            ..Self::default()
        };
    }
    pub(crate) fn retry(&mut self) {
        self.ticket = self.ticket.wrapping_add(1);
        self.loaded = false;
        self.loading = false;
        self.error = None;
    }
    pub(crate) fn begin(&mut self) -> Option<u64> {
        if self.loaded || self.loading {
            return None;
        }
        self.loading = true;
        Some(self.ticket)
    }
    pub(crate) fn finish(&mut self, ticket: u64, result: Result<Directory, &'static str>) -> bool {
        if ticket != self.ticket || !self.loading {
            return false;
        }
        self.loading = false;
        self.loaded = true;
        match result {
            Ok(directory) => {
                let choices = directory.choices;
                let old = self.current().map(|choice| choice.id.clone());
                self.selected = old
                    .and_then(|id| choices.iter().position(|choice| choice.id == id))
                    .unwrap_or(0);
                self.choices = choices;
                self.efforts = directory.efforts;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
    pub(crate) fn current(&self) -> Option<&Choice> {
        self.choices.get(self.selected)
    }
    pub(crate) fn effort(&self, id: &str, revision: &str) -> Option<&Effort> {
        self.efforts
            .iter()
            .find(|effort| effort.id == id && effort.revision == revision)
    }
    pub(crate) fn metadata_reason(&self) -> &'static str {
        if self.loading {
            "正在获取模型…"
        } else {
            self.error.unwrap_or("")
        }
    }
    pub(crate) fn select(&mut self, index: i32) {
        if let Ok(index) = usize::try_from(index)
            && index < self.choices.len()
        {
            self.selected = index;
        }
    }
    pub(crate) fn reason(&self) -> &'static str {
        if self.loading {
            "正在获取模型…"
        } else if let Some(error) = self.error {
            error
        } else if self.current().is_some_and(|choice| choice.available) {
            ""
        } else {
            "该模型暂未开放"
        }
    }
}
fn alias(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}
// Only public server policy metadata is frozen into local request history.
fn policy_snapshot(value: &Value) -> Value {
    let mut policy = serde_json::Map::new();
    for name in [
        "protocol_version",
        "max_output_tokens",
        "max_steps",
        "max_turn_credits",
        "pricing_version",
        "policy_revision",
        "effort_id",
    ] {
        if let Some(field) = value
            .get(name)
            .filter(|field| field.is_number() || field.is_string())
        {
            policy.insert(name.into(), field.clone());
        }
    }
    Value::Object(policy)
}
pub(crate) fn parse(value: Value) -> Result<Directory, &'static str> {
    let invalid = "模型目录暂不可用";
    if value["protocol_version"] != 1 {
        return Err(invalid);
    }
    let Some(models) = value.get("models") else {
        let available = value["available"].as_bool().ok_or(invalid)?;
        return Ok(Directory {
            choices: vec![Choice {
                id: "default".into(),
                label: "账户模型".into(),
                available,
                policy: policy_snapshot(&value),
            }],
            efforts: parse_efforts(&value, &[])?,
        });
    };
    let models = models
        .as_array()
        .filter(|models| !models.is_empty() && models.len() <= 17)
        .ok_or(invalid)?;
    let default = value["default_model_id"]
        .as_str()
        .filter(|id| alias(id))
        .ok_or(invalid)?;
    let mut result = Vec::<Choice>::new();
    for model in models {
        let id = model["model_id"]
            .as_str()
            .filter(|id| alias(id))
            .ok_or(invalid)?;
        let label = model["label"]
            .as_str()
            .filter(|label| {
                !label.trim().is_empty()
                    && label.chars().count() <= 64
                    && !label.chars().any(char::is_control)
            })
            .ok_or(invalid)?;
        let available = model["available"].as_bool().ok_or(invalid)?;
        if model["protocol_version"] != 1 || result.iter().any(|choice| choice.id == id) {
            return Err(invalid);
        }
        result.push(Choice {
            id: id.into(),
            label: label.into(),
            available,
            policy: policy_snapshot(model),
        });
    }
    let index = result
        .iter()
        .position(|choice| choice.id == default)
        .ok_or(invalid)?;
    result.swap(0, index);
    let efforts = parse_efforts(&value, &result)?;
    Ok(Directory {
        choices: result,
        efforts,
    })
}

fn parse_efforts(value: &Value, models: &[Choice]) -> Result<Vec<Effort>, &'static str> {
    let invalid = "强度目录暂不可用";
    if value.get("effort_policy_version").is_none() {
        return if value.get("efforts").is_none() && value.get("default_effort_id").is_none() {
            Ok(Vec::new())
        } else {
            Err(invalid)
        };
    }
    if value["effort_policy_version"] != 1 {
        return Err(invalid);
    }
    let entries = value["efforts"]
        .as_array()
        .filter(|entries| entries.len() <= 8)
        .ok_or(invalid)?;
    if entries.is_empty() {
        return if value["default_effort_id"].is_null() {
            Ok(Vec::new())
        } else {
            Err(invalid)
        };
    }
    let mut efforts: Vec<Effort> = Vec::new();
    for entry in entries {
        let id = entry["id"].as_str().filter(|id| alias(id)).ok_or(invalid)?;
        let label = entry["label"]
            .as_str()
            .filter(|label| {
                !label.trim().is_empty()
                    && label.chars().count() <= 64
                    && !label.chars().any(char::is_control)
            })
            .ok_or(invalid)?;
        let revision = entry["policy_revision"]
            .as_str()
            .filter(|id| alias(id))
            .ok_or(invalid)?;
        let available = entry["available"].as_bool().ok_or(invalid)?;
        let reason = &entry["reason_code"];
        if !(reason.as_str().is_some_and(alias) || available && reason.is_null()) {
            return Err(invalid);
        }
        let model_id = entry["agent"]["model_id"]
            .as_str()
            .filter(|id| models.iter().any(|model| model.id == *id))
            .ok_or(invalid)?;
        let mut agent = serde_json::Map::new();
        agent.insert("model_id".into(), Value::String(model_id.into()));
        for name in ["max_output_tokens", "max_steps", "max_turn_credits"] {
            let limit = entry["agent"][name]
                .as_u64()
                .filter(|value| *value > 0 && *value <= i64::MAX as u64)
                .ok_or(invalid)?;
            agent.insert(name.into(), Value::from(limit));
        }
        for name in [
            "protocol_version",
            "pricing_version",
            "context_window",
            "input_rate",
            "output_rate",
        ] {
            if let Some(field) = entry["agent"].get(name) {
                if !field.is_number() && !field.is_string() {
                    return Err(invalid);
                }
                agent.insert(name.into(), field.clone());
            }
        }
        let mut generation = serde_json::Map::new();
        let mut reference_count = 0;
        for name in ["image_model_ids", "video_model_ids"] {
            let values = entry["generation"][name]
                .as_array()
                .filter(|values| values.len() <= 32)
                .ok_or(invalid)?;
            let mut ids = Vec::<Value>::new();
            for value in values {
                let id = value
                    .as_str()
                    .filter(|id| {
                        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                    })
                    .ok_or(invalid)?;
                let value = Value::String(id.into());
                if ids.contains(&value) {
                    return Err(invalid);
                }
                ids.push(value);
            }
            reference_count += ids.len();
            generation.insert(name.into(), Value::Array(ids));
        }
        if efforts.iter().any(|effort| effort.id == id) || (available && reference_count == 0) {
            return Err(invalid);
        }
        let public = serde_json::json!({"id":id,"label":label,"policy_revision":revision,"available":available,"reason_code":reason,"agent":agent,"generation":generation});
        efforts.push(Effort {
            id: id.into(),
            label: label.into(),
            revision: revision.into(),
            model_id: model_id.into(),
            available,
            public,
        });
    }
    if value["default_effort_id"].as_str() != efforts.first().map(|effort| effort.id.as_str()) {
        return Err(invalid);
    }
    Ok(efforts)
}

/// A bounded, read-only metadata request. Credentials stay on this worker.
pub(crate) fn fetch(base: &str, token: &str) -> Result<Directory, &'static str> {
    use std::io::Read;
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .redirects(0)
        .build()
        .get(&format!("{base}/api/assistant/capabilities"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| {
            if matches!(error, ureq::Error::Status(401, _)) {
                ACCOUNT_EXPIRED
            } else {
                "模型目录暂不可用"
            }
        })?;
    if response.status() != 200 {
        return Err("模型目录暂不可用");
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "模型目录暂不可用")?;
    if bytes.len() > 65536 {
        return Err("模型目录暂不可用");
    }
    parse(serde_json::from_slice(&bytes).map_err(|_| "模型目录暂不可用")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn response() -> Value {
        json!({"protocol_version":1,"default_model_id":"fast","models":[
            {"model_id":"quality","label":"高质量","available":false,"protocol_version":1},
            {"model_id":"fast","label":"快速","available":true,"protocol_version":1}
        ]})
    }
    #[test]
    fn default_and_legacy_catalog() {
        let choices = parse(response()).unwrap();
        assert_eq!(choices.choices[0].id, "fast");
        assert!(choices.choices[0].available);
        assert_eq!(
            parse(json!({"protocol_version":1,"available":false}))
                .unwrap()
                .choices[0]
                .id,
            "default"
        );
    }
    fn effort_response() -> Value {
        let mut value = response();
        value["effort_policy_version"] = json!(1);
        value["default_effort_id"] = json!("balanced");
        value["efforts"] = json!([{
            "id":"balanced","label":"均衡","policy_revision":"fixture-1","available":true,"reason_code":null,
            "agent":{"model_id":"fast","max_output_tokens":2048,"max_steps":6,"max_turn_credits":20,"pricing_version":"prices-1","context_window":8192,"input_rate":1,"output_rate":2},
            "generation":{"image_model_ids":["image/one"],"video_model_ids":[]}
        }]);
        value
    }
    #[test]
    fn effort_policy_is_public_complete_and_not_fabricated_for_legacy() {
        assert!(parse(response()).unwrap().efforts.is_empty());
        let mut value = effort_response();
        value["efforts"][0]["token"] = json!("must-not-persist");
        value["efforts"][0]["agent"]["api_key"] = json!("must-not-persist");
        let directory = parse(value).unwrap();
        let effort = &directory.efforts[0];
        assert_eq!(effort.id, "balanced");
        assert_eq!(effort.model_id, "fast");
        assert_eq!(effort.policy()["policy_revision"], "fixture-1");
        assert_eq!(effort.policy()["agent"]["input_rate"], 1);
        assert_eq!(
            effort.policy()["generation"]["image_model_ids"],
            json!(["image/one"])
        );
        assert!(effort.policy().get("available").is_none());
        assert!(effort.policy().get("reason_code").is_none());
        assert!(!effort.public.to_string().contains("must-not-persist"));
        let mut empty = response();
        empty["effort_policy_version"] = json!(1);
        empty["default_effort_id"] = Value::Null;
        empty["efforts"] = json!([]);
        assert!(parse(empty).unwrap().efforts.is_empty());
    }
    #[test]
    fn effort_catalog_rejects_ambiguous_ids_revisions_and_mappings() {
        for (path, replacement) in [
            ("/effort_policy_version", json!(2)),
            ("/default_effort_id", json!("missing")),
            ("/efforts/0/id", json!("unsafe/id")),
            ("/efforts/0/policy_revision", json!("")),
            ("/efforts/0/agent/model_id", json!("not-listed")),
            ("/efforts/0/agent/max_steps", json!(0)),
            ("/efforts/0/generation/image_model_ids", json!([])),
            (
                "/efforts/0/generation/image_model_ids",
                json!(["same", "same"]),
            ),
        ] {
            let mut value = effort_response();
            *value.pointer_mut(path).unwrap() = replacement;
            assert!(parse(value).is_err(), "{path}");
        }
        let mut value = effort_response();
        let duplicate = value["efforts"][0].clone();
        value["efforts"].as_array_mut().unwrap().push(duplicate);
        assert!(parse(value).is_err());
    }
    #[test]
    fn unavailable_effort_and_expired_revision_are_not_replaced() {
        let mut value = effort_response();
        value["efforts"][0]["available"] = json!(false);
        value["efforts"][0]["reason_code"] = json!("provider_unavailable");
        value["efforts"][0]["generation"]["image_model_ids"] = json!([]);
        let mut catalog = Catalog::default();
        let ticket = catalog.begin().unwrap();
        catalog.finish(ticket, Ok(parse(value).unwrap()));
        assert!(catalog.effort("balanced", "previous-revision").is_none());
        let effort = catalog.effort("balanced", "fixture-1").unwrap();
        assert!(!effort.available);
        assert_eq!(effort.reason(), "该强度暂不可用");
    }
    #[test]
    fn malformed_or_ambiguous_catalog_is_rejected() {
        for value in [
            json!({"protocol_version":2}),
            json!({"protocol_version":1,"models":[]}),
            json!({"protocol_version":1,"available":"yes"}),
        ] {
            assert!(parse(value).is_err());
        }
        let mut value = response();
        value["models"][1]["model_id"] = json!("quality");
        assert!(parse(value).is_err());
        let mut value = response();
        value["models"][0]["model_id"] = json!("https://gateway");
        assert!(parse(value).is_err());
        let mut value = response();
        value["models"][0]["label"] = json!("hidden\ntext");
        assert!(parse(value).is_err());
        let mut value = response();
        value["default_model_id"] = json!("removed");
        assert!(parse(value).is_err());
    }
    #[test]
    fn account_change_rejects_old_response() {
        let mut catalog = Catalog::default();
        let old = catalog.begin().unwrap();
        catalog.clear();
        let new = catalog.begin().unwrap();
        assert!(!catalog.finish(old, Ok(parse(response()).unwrap())));
        assert!(catalog.choices.is_empty());
        assert!(catalog.loading);
        assert!(catalog.finish(new, Ok(parse(response()).unwrap())));
        assert_eq!(catalog.current().unwrap().id, "fast");
    }
    #[test]
    fn refresh_fences_old_response_preserves_alias_and_never_connects_on_error() {
        let mut catalog = Catalog::default();
        let first = catalog.begin().unwrap();
        assert!(catalog.begin().is_none());
        catalog.finish(first, Ok(parse(response()).unwrap()));
        catalog.select(1);
        assert_eq!(catalog.reason(), "该模型暂未开放");
        catalog.select(-1);
        catalog.select(99);
        assert_eq!(catalog.current().unwrap().id, "quality");
        catalog.retry();
        let next = catalog.begin().unwrap();
        catalog.finish(first, Ok(Directory::default()));
        assert!(catalog.loading);
        catalog.finish(next, Err("模型目录暂不可用"));
        assert_eq!(catalog.reason(), "模型目录暂不可用");
        catalog.retry();
        let next = catalog.begin().unwrap();
        catalog.finish(next, Ok(parse(response()).unwrap()));
        assert_eq!(catalog.current().unwrap().id, "quality");
    }
    fn http_fixture(status: &str, body: String) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_owned();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nLocation: http://127.0.0.1:9/forbidden\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, worker)
    }
    #[test]
    fn metadata_request_uses_existing_account_only() {
        let (url, worker) = http_fixture("200 OK", response().to_string());
        assert_eq!(
            fetch(&url, "fixture-session").unwrap().choices[0].id,
            "fast"
        );
        let request = worker.join().unwrap();
        assert!(request.starts_with("GET /api/assistant/capabilities HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer fixture-session\r\n"));
    }
    #[test]
    fn redirects_and_expired_account_do_not_open_catalog() {
        for (status, message) in [
            ("302 Found", "模型目录暂不可用"),
            ("401 Unauthorized", "登录已过期，请重新登录"),
        ] {
            let (url, worker) = http_fixture(status, response().to_string());
            assert_eq!(fetch(&url, "fixture-session").err(), Some(message));
            worker.join().unwrap();
        }
    }
    #[test]
    fn metadata_response_is_bounded_and_validated() {
        for body in ["x".repeat(65537), "invalid JSON".into()] {
            let (url, worker) = http_fixture("200 OK", body);
            assert_eq!(
                fetch(&url, "fixture-session").err(),
                Some("模型目录暂不可用")
            );
            worker.join().unwrap();
        }
    }
}
