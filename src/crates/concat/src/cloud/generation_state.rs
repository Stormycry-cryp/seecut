//! Draft preferences and the current submission's trusted usage, independent of UI indices.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
    #[serde(default)]
    pub effort_identity: Option<String>,
    pub models: HashMap<String, String>,
}
pub(super) struct Resolved {
    pub mode: i32,
    pub effort: Option<Value>,
    pub needs_confirmation: bool,
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
            && self
                .effort_identity
                .as_ref()
                .is_none_or(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
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
        let pristine = self.effort_id.is_none() && self.models.is_empty();
        let mut needs_confirmation = false;
        let effort = if let Some(id) = &self.effort_id {
            match efforts.iter().find(|e| e["id"] == *id) {
                Some(current)
                    if available(&current)
                        && self.effort_identity.as_deref()
                            == Some(policy_identity(current).as_str()) =>
                {
                    Some(current.clone())
                }
                _ => {
                    // Retain the chosen identity. The user must explicitly accept a new
                    // policy; a refreshed catalogue is not consent to a paid substitute.
                    needs_confirmation = true;
                    Some(json!({"id": id}))
                }
            }
        } else if pristine {
            efforts
                .iter()
                .filter(available)
                .find(|e| e["id"] == default_effort)
                .or_else(|| efforts.iter().find(available))
                .cloned()
        } else {
            needs_confirmation = required;
            None
        };
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
            if let Some(id) = self.models.get(kind) {
                selected.insert(kind.into(), id.clone());
            } else if pristine && let Some(id) = candidates.first().and_then(|m| m["id"].as_str()) {
                selected.insert(kind.into(), id.into());
            }
        }
        Some(Resolved {
            mode: self.mode,
            effort,
            needs_confirmation,
            models: selected,
        })
    }
}
/// Only a fingerprint is persisted; refreshed prices and policies are never accepted by identity alone.
pub(super) fn policy_identity(effort: &Value) -> String {
    let policy = json!({
        "id": effort["id"], "revision": effort["policy_revision"],
        "agent": effort["agent"], "agent_models": effort["agent_models"],
        "generation": effort["generation"],
    });
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&policy).expect("JSON policy"))
    )
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
                effort_identity: Some(policy_identity(
                    &json!({"id":"high","available":true,"policy_revision":"new-high","generation":{"image_model_ids":["image-a","image-b"],"video_model_ids":["video-a"]}}),
                )),
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
        assert!(!selected.needs_confirmation);
        assert_eq!(selected.mode, 0);
        assert_eq!(selected.effort.unwrap()["policy_revision"], "new-high");
    }
    #[test]
    fn removed_model_and_effort_keep_original_identity_until_explicit_reselection() {
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
        assert_eq!(selected.models["image"], "image-b");
        assert!(!selected.needs_confirmation);
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
        assert_eq!(selected.effort.unwrap()["id"], "high");
        assert_eq!(selected.models, choice.models);
        assert!(selected.needs_confirmation);
        let empty = choice
            .resolve(&choice.service, &choice.account, &[], &efforts, "low", true)
            .unwrap();
        assert_eq!(empty.models, choice.models);
        assert!(empty.needs_confirmation);
    }
    #[test]
    fn refreshed_or_restored_policy_revision_requires_explicit_reselection() {
        let (choice, models, mut efforts) = fixture();
        let saved: Choices =
            serde_json::from_value(serde_json::to_value(&choice).unwrap()).unwrap();
        efforts[1]["policy_revision"] = json!("changed-high");
        for original in [&choice, &saved] {
            let resolved = original
                .resolve(
                    &choice.service,
                    &choice.account,
                    &models,
                    &efforts,
                    "low",
                    true,
                )
                .unwrap();
            assert!(resolved.needs_confirmation);
            assert_eq!(resolved.effort.unwrap(), json!({"id":"high"}));
            assert_eq!(resolved.models, choice.models);
        }
        let mut accepted = saved;
        accepted.effort_identity = Some(policy_identity(&efforts[1]));
        assert!(
            !accepted
                .resolve(
                    &choice.service,
                    &choice.account,
                    &models,
                    &efforts,
                    "low",
                    true
                )
                .unwrap()
                .needs_confirmation
        );
    }
    #[test]
    fn old_preferences_without_policy_proof_require_reselection_and_only_new_drafts_default() {
        let (mut choice, models, efforts) = fixture();
        choice.effort_identity = None;
        assert!(
            choice
                .resolve(
                    &choice.service,
                    &choice.account,
                    &models,
                    &efforts,
                    "low",
                    true
                )
                .unwrap()
                .needs_confirmation
        );
        choice.effort_id = None;
        assert!(
            choice
                .resolve(
                    &choice.service,
                    &choice.account,
                    &models,
                    &efforts,
                    "low",
                    true
                )
                .unwrap()
                .needs_confirmation
        );
        choice.models.clear();
        let initial = choice
            .resolve(
                &choice.service,
                &choice.account,
                &models,
                &efforts,
                "low",
                true,
            )
            .unwrap();
        assert!(!initial.needs_confirmation);
        assert_eq!(initial.effort.unwrap()["id"], "low");
        assert_eq!(initial.models["image"], "image-a");
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
