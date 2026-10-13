// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetVersion {
    pub sha256: String,
    pub registration_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
}

impl AssetVersion {
    pub(crate) fn matches_json(&self, encoded: &str) -> bool {
        serde_json::from_str::<Self>(encoded).is_ok_and(|version| version == *self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_round_trip_preserves_identity_independent_of_json_key_order() {
        let version = AssetVersion {
            sha256: "a".repeat(64),
            registration_id: "registered-output".into(),
            previous_id: Some("prior-output".into()),
        };
        let mut value = serde_json::to_value(&version).unwrap();
        assert!(version.matches_json(&value.to_string()));
        assert!(version.matches_json(&serde_json::to_string_pretty(&value).unwrap()));
        value["registration_id"] = "different-registration".into();
        assert!(!version.matches_json(&value.to_string()));
        value["registration_id"] = "registered-output".into();
        value["sha256"] = "b".repeat(64).into();
        assert!(!version.matches_json(&value.to_string()));
        assert!(!version.matches_json("null"));
    }
}
