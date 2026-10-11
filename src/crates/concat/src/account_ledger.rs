// SPDX-License-Identifier: AGPL-3.0-or-later

//! Presentation of the existing wallet ledger; never changes a balance.

use serde_json::Value;

pub(crate) fn title(entry: &Value) -> &'static str {
    match (entry["reference_type"].as_str(), entry["kind"].as_str()) {
        (Some("assistant_operation"), Some("hold")) => "Agent 预扣",
        (Some("assistant_operation"), Some("capture")) => "Agent 消费",
        (Some("assistant_operation"), Some("release")) => "Agent 退回积分",
        (_, Some("hold")) => "生成预扣",
        (_, Some("capture")) => "生成消费",
        (_, Some("release")) => "退回积分",
        (_, Some("purchase")) => "充值到账",
        _ => "积分变动",
    }
}

pub(crate) fn order_status(order: &Value, now: i64) -> &'static str {
    match order["status"].as_str() {
        Some("paid") => "已到账",
        Some("closed") => "已关闭",
        Some("pending")
            if order["expires_at"]
                .as_i64()
                .is_some_and(|expires_at| expires_at <= now) =>
        {
            "支付已过期"
        }
        Some("pending") => "等待支付",
        _ => "状态待确认",
    }
}

pub(crate) fn amount(entry: &Value) -> String {
    // Capture charges an existing hold: available stays unchanged, while
    // held decreases by the actual cost. It is not a zero-cost operation.
    let available = entry["delta_available"].as_i64().unwrap_or(0);
    let value = if entry["kind"] == "capture" {
        entry["delta_held"]
            .as_i64()
            .filter(|value| *value <= 0)
            .unwrap_or(available)
    } else {
        available
    };
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{amount, order_status, title};
    use serde_json::json;

    #[test]
    fn partial_agent_settlement_displays_actual_charge_and_refund() {
        let hold = json!({"reference_type":"assistant_operation", "kind":"hold",
            "delta_available":-100, "delta_held":100});
        let capture = json!({"reference_type":"assistant_operation", "kind":"capture",
            "delta_available":0, "delta_held":-30});
        let release = json!({"reference_type":"assistant_operation", "kind":"release",
            "delta_available":70, "delta_held":-70});
        assert_eq!((title(&hold), amount(&hold)), ("Agent 预扣", "-100".into()));
        assert_eq!(
            (title(&capture), amount(&capture)),
            ("Agent 消费", "-30".into())
        );
        assert_eq!(
            (title(&release), amount(&release)),
            ("Agent 退回积分", "70".into())
        );
    }

    #[test]
    fn generation_capture_uses_the_same_wallet_contract() {
        let capture = json!({"reference_type":"generation_task", "kind":"capture",
            "delta_available":0, "delta_held":-50});
        assert_eq!(title(&capture), "生成消费");
        assert_eq!(amount(&capture), "-50");
    }

    #[test]
    fn purchase_and_cancel_keep_available_credit_changes() {
        let purchase = json!({"reference_type":"order", "reference_id":"ord_test",
            "kind":"purchase", "delta_available":500, "delta_held":0});
        let cancelled = json!({"reference_type":"assistant_operation", "kind":"release",
            "delta_available":100, "delta_held":-100});
        assert_eq!(
            (title(&purchase), amount(&purchase)),
            ("充值到账", "500".into())
        );
        assert_eq!(amount(&cancelled), "100");
    }

    #[test]
    fn order_expiry_does_not_override_verified_paid_or_closed_status() {
        assert_eq!(
            order_status(&json!({"status":"pending", "expires_at":1800}), 1799),
            "等待支付"
        );
        assert_eq!(
            order_status(&json!({"status":"pending", "expires_at":1800}), 1800),
            "支付已过期"
        );
        assert_eq!(
            order_status(&json!({"status":"paid", "expires_at":1800}), 1900),
            "已到账"
        );
        assert_eq!(
            order_status(&json!({"status":"closed", "expires_at":1800}), 1900),
            "已关闭"
        );
        assert_eq!(order_status(&json!({"status":"pending"}), 1900), "等待支付");
        assert_eq!(
            order_status(&json!({"status":"unexpected"}), 1900),
            "状态待确认"
        );
    }

    #[test]
    fn legacy_capture_without_held_field_keeps_its_available_delta() {
        let legacy = json!({"kind":"capture", "delta_available":-12});
        assert_eq!(amount(&legacy), "-12");
    }
}
