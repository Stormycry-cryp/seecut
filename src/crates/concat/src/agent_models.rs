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
#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) choices: Vec<Choice>,
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
    pub(crate) fn finish(
        &mut self,
        ticket: u64,
        result: Result<Vec<Choice>, &'static str>,
    ) -> bool {
        if ticket != self.ticket || !self.loading {
            return false;
        }
        self.loading = false;
        self.loaded = true;
        match result {
            Ok(choices) => {
                let old = self.current().map(|choice| choice.id.clone());
                self.selected = old
                    .and_then(|id| choices.iter().position(|choice| choice.id == id))
                    .unwrap_or(0);
                self.choices = choices;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
    pub(crate) fn current(&self) -> Option<&Choice> {
        self.choices.get(self.selected)
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
pub(crate) fn parse(value: Value) -> Result<Vec<Choice>, &'static str> {
    let invalid = "模型目录暂不可用";
    if value["protocol_version"] != 1 {
        return Err(invalid);
    }
    let Some(models) = value.get("models") else {
        let available = value["available"].as_bool().ok_or(invalid)?;
        return Ok(vec![Choice {
            id: "default".into(),
            label: "账户模型".into(),
            available,
            policy: policy_snapshot(&value),
        }]);
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
    Ok(result)
}

/// A bounded, read-only metadata request. Credentials stay on this worker.
pub(crate) fn fetch(base: &str, token: &str) -> Result<Vec<Choice>, &'static str> {
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
        assert_eq!(choices[0].id, "fast");
        assert!(choices[0].available);
        assert_eq!(
            parse(json!({"protocol_version":1,"available":false})).unwrap()[0].id,
            "default"
        );
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
        catalog.finish(first, Ok(vec![]));
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
        assert_eq!(fetch(&url, "fixture-session").unwrap()[0].id, "fast");
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
