// SPDX-License-Identifier: AGPL-3.0-or-later
//! User-facing failure copy is allowlisted by code, never derived from provider text.

pub(super) fn copy(code: &str) -> (&'static str, &'static str) {
    match code {
        "REFERENCE_URL_UNAVAILABLE" => (
            "参考图不可用",
            "服务暂时无法提供参考素材。可编辑输入移除参考；如需保留参考，请联系管理员检查素材服务。",
        ),
        "GENERATION_ASSET_UNAVAILABLE" => (
            "参考素材已失效",
            "请编辑输入，重新选择或移除失效的参考素材。",
        ),
        "REFERENCE_TOO_LARGE" => ("参考素材过大", "请编辑输入，换用较小的参考素材。"),
        "INVALID_REFERENCE_MEDIA" | "UNSUPPORTED_REFERENCE_MEDIA_TYPE" => {
            ("参考格式不支持", "请编辑输入，换用当前模型支持的参考素材。")
        }
        "GRSAI_NOT_CONFIGURED" | "XIANGXIN_NOT_CONFIGURED" => (
            "服务尚未就绪",
            "请联系管理员检查生成服务配置。编辑输入不会自动提交任务。",
        ),
        "XIANGXIN_UNAVAILABLE" | "PROVIDER_UNAVAILABLE" | "PROVIDER_TIMEOUT" => (
            "服务暂不可用",
            "请稍后再试。可先编辑输入；重新生成前仍需确认当前报价。",
        ),
        "INVALID_PROVIDER_RESULT"
        | "INVALID_PROVIDER_MEDIA"
        | "PROVIDER_RESULT_HOST_DENIED"
        | "PROVIDER_RESULT_UNAVAILABLE" => (
            "结果暂不可用",
            "本次结果无法读取。请稍后查看原任务，或联系管理员。",
        ),
        // Do not turn unknown outcomes into an invitation to submit a duplicate.
        "SUBMISSION_RESULT_UNKNOWN"
        | "GRSAI_RESPONSE_UNKNOWN"
        | "UNKNOWN_PROVIDER_STATUS"
        | "GRSAI_TASK_MISMATCH" => (
            "任务结果待核实",
            "请查询原任务或联系管理员，确认结果前不要重复提交。",
        ),
        _ => (
            "生成未完成",
            "暂时无法确认失败原因。请查看原任务或联系管理员；编辑输入不会自动提交任务。",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::copy;

    #[test]
    fn distinct_recoverable_causes_and_unknown_outcomes() {
        assert_eq!(copy("REFERENCE_URL_UNAVAILABLE").0, "参考图不可用");
        assert_eq!(copy("XIANGXIN_UNAVAILABLE").0, "服务暂不可用");
        assert!(copy("GENERATION_ASSET_UNAVAILABLE").1.contains("重新选择"));
        assert!(copy("GRSAI_RESPONSE_UNKNOWN").1.contains("不要重复提交"));
        assert_ne!(
            copy("GRSAI_NOT_CONFIGURED").0,
            copy("XIANGXIN_UNAVAILABLE").0
        );
    }

    #[test]
    fn arbitrary_codes_are_not_echoed_or_interpreted_as_safe_copy() {
        for code in [
            "",
            "https://private.invalid/?token=secret",
            "REFERENCE_URL_UNAVAILABLE\nBearer secret",
            "XIANGXIN_HTTP_ERROR",
        ] {
            assert_eq!(copy(code), copy(""));
        }
    }
}
