//! Draft preferences and the current submission's trusted usage, independent of UI indices.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Default)]
pub(super) struct ActualCost {
    key: String,
    tasks: HashSet<String>,
    credits: HashMap<String, i64>,
}
impl ActualCost {
    pub(super) fn key(&self) -> &str {
        &self.key
    }
    pub(super) fn current(&self, key: &str) -> bool {
        !key.is_empty() && self.key == key
    }
    pub(super) fn begin(&mut self, key: &str) {
        if self.key != key {
            *self = Self {
                key: key.into(),
                ..Self::default()
            };
        }
    }
    pub(super) fn bind(&mut self, key: &str, tasks: &[Value]) -> bool {
        if !self.current(key) {
            return false;
        }
        self.tasks.extend(
            tasks
                .iter()
                .filter_map(|task| task["id"].as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
        );
        self.observe(key, tasks);
        true
    }
    pub(super) fn observe(&mut self, key: &str, tasks: &[Value]) {
        if !self.current(key) {
            return;
        }
        for task in tasks {
            let Some(id) = task["id"].as_str().filter(|id| self.tasks.contains(*id)) else {
                continue;
            };
            if let Some(actual) = task["captured_credits"].as_i64().filter(|n| *n >= 0) {
                self.credits
                    .entry(id.into())
                    .and_modify(|prior| *prior = (*prior).max(actual))
                    .or_insert(actual);
            }
        }
    }
    pub(super) fn display(&self) -> String {
        if self.tasks.iter().any(|id| !self.credits.contains_key(id)) {
            return "—".into();
        }
        self.credits
            .values()
            .fold(0_i64, |sum, n| sum.saturating_add(*n))
            .to_string()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Choices {
    pub version: u32,
    pub service: String,
    pub account: String,
    pub mode: i32,
    pub effort_id: Option<String>,
    pub models: HashMap<String, String>,
}
pub(super) struct Resolved {
    pub mode: i32,
    pub effort: Option<Value>,
    pub models: HashMap<String, String>,
}
fn identity(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && id.bytes().all(|b| b.is_ascii_graphic())
}
impl Choices {
    pub(super) fn valid(&self) -> bool {
        self.version == 1
            && !self.service.is_empty()
            && identity(&self.account)
            && matches!(self.mode, 0 | 1)
            && self.effort_id.as_ref().is_none_or(|id| identity(id))
            && self.models.len() <= 2
            && self
                .models
                .iter()
                .all(|(kind, id)| matches!(kind.as_str(), "image" | "video") && identity(id))
    }
    /// Called only after the host has validated a fresh catalogue. Never restores prices or tasks.
    pub(super) fn resolve(
        &self,
        service: &str,
        account: &str,
        models: &[Value],
        efforts: &[Value],
        default_effort: &str,
        required: bool,
    ) -> Option<Resolved> {
        if !self.valid() || self.service != service || self.account != account {
            return None;
        }
        let kind = if self.mode == 0 { "image" } else { "video" };
        let available = |effort: &&Value| {
            effort["generation_available"]
                .as_bool()
                .unwrap_or(effort["available"] == true)
                && models.iter().any(|model| {
                    model["kind"] == kind
                        && model["available"] == true
                        && allowed(effort, kind, model["id"].as_str().unwrap_or(""))
                })
        };
        let effort = efforts
            .iter()
            .filter(available)
            .find(|e| e["id"].as_str() == self.effort_id.as_deref())
            .or_else(|| {
                efforts
                    .iter()
                    .filter(available)
                    .find(|e| e["id"] == default_effort)
            })
            .or_else(|| efforts.iter().find(available))
            .cloned();
        let mut selected = HashMap::new();
        for kind in ["image", "video"] {
            let candidates: Vec<&Value> = models
                .iter()
                .filter(|m| {
                    m["kind"] == kind
                        && m["available"] == true
                        && effort.as_ref().map_or(!required, |e| {
                            allowed(e, kind, m["id"].as_str().unwrap_or(""))
                        })
                })
                .collect();
            if let Some(model) = candidates
                .iter()
                .find(|m| m["id"].as_str() == self.models.get(kind).map(String::as_str))
                .or_else(|| candidates.first())
                && let Some(id) = model["id"].as_str()
            {
                selected.insert(kind.into(), id.into());
            }
        }
        Some(Resolved {
            mode: self.mode,
            effort,
            models: selected,
        })
    }
}
fn allowed(effort: &Value, kind: &str, id: &str) -> bool {
    effort["generation"][format!("{kind}_model_ids")]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|v| v == id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn next_submission_starts_at_zero_and_old_poll_cannot_fill_it() {
        let mut cost = ActualCost::default();
        cost.begin("a");
        assert!(cost.bind("a", &[json!({"id":"a1","captured_credits":7})]));
        assert_eq!(cost.display(), "7");
        cost.begin("b");
        assert_eq!(cost.display(), "0");
        cost.observe("a", &[json!({"id":"a1","captured_credits":12})]);
        assert!(!cost.bind("a", &[json!({"id":"a1","captured_credits":12})]));
        assert_eq!(cost.display(), "0");
        assert!(cost.current("b"));
    }
    #[test]
    fn same_unknown_key_preserves_usage_and_replies_merge_monotonically() {
        let mut cost = ActualCost::default();
        cost.begin("a");
        cost.bind(
            "a",
            &[json!({"id":"1","captured_credits":3}), json!({"id":"2"})],
        );
        assert_eq!(cost.display(), "—");
        cost.begin("a");
        cost.bind(
            "a",
            &[
                json!({"id":"1","captured_credits":2}),
                json!({"id":"2","captured_credits":1}),
            ],
        );
        assert_eq!(cost.display(), "4");
        cost.bind(
            "a",
            &[
                json!({"id":"1","captured_credits":3}),
                json!({"id":"2","captured_credits":1}),
            ],
        );
        assert_eq!(cost.display(), "4");
        assert_eq!(cost.key(), "a");
    }
    fn fixture() -> (Choices, Vec<Value>, Vec<Value>) {
        (
            Choices {
                version: 1,
                service: "https://service.test".into(),
                account: "account1".into(),
                mode: 0,
                effort_id: Some("high".into()),
                models: HashMap::from([
                    ("image".into(), "image-b".into()),
                    ("video".into(), "video-a".into()),
                ]),
            },
            vec![
                json!({"id":"image-a","kind":"image","available":true}),
                json!({"id":"video-a","kind":"video","available":true}),
                json!({"id":"image-b","kind":"image","available":true}),
            ],
            vec![
                json!({"id":"low","available":true,"policy_revision":"new-low","generation":{"image_model_ids":["image-a"],"video_model_ids":[]}}),
                json!({"id":"high","available":true,"policy_revision":"new-high","generation":{"image_model_ids":["image-a","image-b"],"video_model_ids":["video-a"]}}),
            ],
        )
    }
    #[test]
    fn saved_ids_survive_reopen_and_reordering_with_fresh_policy() {
        let (choice, mut models, mut efforts) = fixture();
        let saved: Choices =
            serde_json::from_str(&serde_json::to_string(&choice).unwrap()).unwrap();
        models.reverse();
        efforts.reverse();
        let selected = saved
            .resolve(
                &choice.service,
                &choice.account,
                &models,
                &efforts,
                "low",
                true,
            )
            .unwrap();
        assert_eq!(selected.models, choice.models);
        assert_eq!(selected.mode, 0);
        assert_eq!(selected.effort.unwrap()["policy_revision"], "new-high");
    }
    #[test]
    fn removed_model_and_effort_fall_back_only_inside_current_allowed_set() {
        let (choice, mut models, mut efforts) = fixture();
        models.retain(|m| m["id"] != "image-b");
        let selected = choice
            .resolve(
                &choice.service,
                &choice.account,
                &models,
                &efforts,
                "low",
                true,
            )
            .unwrap();
        assert_eq!(selected.models["image"], "image-a");
        efforts.retain(|e| e["id"] != "high");
        let selected = choice
            .resolve(
                &choice.service,
                &choice.account,
                &models,
                &efforts,
                "low",
                true,
            )
            .unwrap();
        assert_eq!(selected.effort.unwrap()["id"], "low");
        assert!(!selected.models.contains_key("video"));
        assert!(
            choice
                .resolve(&choice.service, &choice.account, &[], &efforts, "low", true)
                .unwrap()
                .models
                .is_empty()
        );
    }
    #[test]
    fn preferences_do_not_cross_account_or_service_and_invalid_version_is_ignored() {
        let (mut choice, models, efforts) = fixture();
        assert!(
            choice
                .resolve(&choice.service, "other", &models, &efforts, "low", true)
                .is_none()
        );
        assert!(
            choice
                .resolve(
                    "https://other.test",
                    &choice.account,
                    &models,
                    &efforts,
                    "low",
                    true
                )
                .is_none()
        );
        choice.version = 2;
        assert!(!choice.valid());
    }
}
