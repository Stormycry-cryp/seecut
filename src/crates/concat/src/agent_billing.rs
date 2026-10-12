//! Public billing metadata validation. No provider secrets or monetary floats.
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const FIELDS: [&str; 7] = [
    "billing_mode",
    "usage_contract_id",
    "provider_id",
    "upstream_model",
    "pricing_snapshot",
    "pricing_digest",
    "context_preflight",
];
fn identity(value: &Value) -> bool {
    value.as_str().is_some_and(|v| {
        !v.is_empty()
            && v.len() <= 128
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    })
}
fn integer_text(value: &Value, length: usize, positive: bool) -> bool {
    value.as_str().is_some_and(|v| {
        !v.is_empty()
            && v.len() <= length
            && v.bytes().all(|b| b.is_ascii_digit())
            && (v == "0" || !v.starts_with('0'))
            && (!positive || v != "0")
    })
}
fn decimal(value: &Value) -> bool {
    value.as_str().is_some_and(|v| {
        let mut parts = v.split('.');
        let whole = parts.next().unwrap_or("");
        let fraction = parts.next();
        !whole.is_empty()
            && whole.len() <= 9
            && whole.bytes().all(|b| b.is_ascii_digit())
            && (whole == "0" || !whole.starts_with('0'))
            && parts.next().is_none()
            && fraction.is_none_or(|s| {
                !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit())
            })
    })
}
fn fraction(mut numerator: u128, mut denominator: u128) -> (u128, u128) {
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    numerator /= a;
    denominator /= a;
    (numerator, denominator)
}
fn decimal_fraction(value: &Value) -> Option<(u128, u128)> {
    if !decimal(value) {
        return None;
    }
    let mut parts = value.as_str()?.split('.');
    let whole = parts.next()?;
    let tail = parts.next().unwrap_or("");
    Some((
        format!("{whole}{tail}").parse().ok()?,
        10u128.pow(tail.len() as u32),
    ))
}
fn keys(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.len() == fields.len() && fields.iter().all(|key| v.contains_key(*key)))
}
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut names: Vec<_> = map.keys().collect();
            names.sort();
            format!(
                "{{{}}}",
                names
                    .into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical(&map[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => serde_json::to_string(value).unwrap(),
    }
}
pub(crate) fn validate(value: &Value, available: bool) -> Result<bool, ()> {
    match value.get("billing_mode").and_then(Value::as_str) {
        None if value.get("billing_mode").is_none()
            && FIELDS[1..].iter().all(|key| value.get(*key).is_none()) =>
        {
            return Ok(false);
        }
        Some("verified_input_v1") if FIELDS[1..].iter().all(|key| value.get(*key).is_none()) => {
            return Ok(false);
        }
        Some("provider_usage_capped_v1") => {}
        _ => return Err(()),
    }
    if !identity(&value["usage_contract_id"])
        || !identity(&value["provider_id"])
        || !value["upstream_model"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.chars().count() <= 256 && !v.contains('\0'))
        || value["context_preflight"] != "byte_bounds_only"
    {
        return Err(());
    }
    let p = &value["pricing_snapshot"];
    if !available && p.is_null() && value["pricing_digest"].is_null() {
        return Ok(true);
    }
    if !keys(
        p,
        &[
            "schema_version",
            "provider",
            "model",
            "kind",
            "version",
            "cost_status",
            "source",
            "verified_at",
            "credits_per_yuan",
            "multiplier",
            "unit",
            "denominator",
            "dimensions",
        ],
    ) || p["schema_version"] != 1
        || p["kind"] != "agent"
        || p["provider"] != value["provider_id"]
        || p["model"] != value["upstream_model"]
        || !identity(&p["version"])
        || p["version"] != value["pricing_version"]
        || !matches!(
            p["cost_status"].as_str(),
            Some("verified" | "verified_free")
        )
        || !p["source"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 2048 && !s.chars().any(char::is_control))
        || p["verified_at"]
            .as_str()
            .is_none_or(|s| chrono::DateTime::parse_from_rfc3339(s).is_err())
        || p["credits_per_yuan"] != 10
        || p["multiplier"] != 3
        || p["unit"] != "nanocredits"
        || !integer_text(&p["denominator"], 19, true)
        || p["denominator"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none_or(|n| n > 1_000_000_000_000_000_000)
        || !keys(&p["dimensions"], &["input", "cached_input", "output"])
    {
        return Err(());
    }
    for name in ["input", "cached_input", "output"] {
        let d = &p["dimensions"][name];
        if !keys(
            d,
            &[
                "unit",
                "cost_yuan",
                "selectors",
                "retail_source",
                "sale_override_credits",
                "rate_numerator",
            ],
        ) || d["unit"] != "per_million_tokens"
            || !decimal(&d["cost_yuan"])
            || !keys(&d["selectors"], &[])
            || !integer_text(&d["rate_numerator"], 45, false)
        {
            return Err(());
        }
        match d["retail_source"].as_str() {
            Some("cost_times_three") if d["sale_override_credits"].is_null() => {}
            Some("override") if decimal(&d["sale_override_credits"]) => {}
            _ => return Err(()),
        }
        let (cost, cost_denominator) = decimal_fraction(&d["cost_yuan"]).ok_or(())?;
        if p["cost_status"] == "verified_free" && cost != 0 {
            return Err(());
        }
        let (retail, denominator) = if d["retail_source"] == "override" {
            decimal_fraction(&d["sale_override_credits"]).ok_or(())?
        } else {
            (cost * 30, cost_denominator)
        };
        // Bounded decimal origins fit u128 after reducing each fraction;
        // cross products of the unreduced snapshot could overflow.
        let expected = fraction(retail * 1_000_000_000, denominator * 1_000_000);
        let reported = fraction(
            d["rate_numerator"]
                .as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .ok_or(())?,
            p["denominator"]
                .as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .ok_or(())?,
        );
        if expected != reported {
            return Err(());
        }
    }
    let expected = format!("{:x}", Sha256::digest(canonical(p).as_bytes()));
    if value["pricing_digest"].as_str() != Some(expected.as_str()) {
        return Err(());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_mode_and_opaque_price_fail_closed() {
        assert_eq!(validate(&serde_json::json!({}), true), Ok(false));
        assert!(validate(&serde_json::json!({"billing_mode":"unknown"}), true).is_err());
        assert!(validate(&serde_json::json!({"billing_mode":null}), true).is_err());
        assert!(validate(&serde_json::json!({"billing_mode":"provider_usage_capped_v1","usage_contract_id":"fixture","provider_id":"fixture","upstream_model":"fixture","context_preflight":"byte_bounds_only","pricing_snapshot":null,"pricing_digest":null}), true).is_err());
    }
    #[test]
    fn changed_markup_cannot_validate_by_rehashing_itself() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../../services/seecut-server/tests/fixtures/provider_usage_catalog.json"
        ))
        .unwrap();
        value["pricing_snapshot"]["dimensions"]["cached_input"]["rate_numerator"] =
            serde_json::json!("7000");
        value["pricing_digest"] = Value::String(format!(
            "{:x}",
            Sha256::digest(canonical(&value["pricing_snapshot"]).as_bytes())
        ));
        assert!(validate(&value, true).is_err());
    }
}
