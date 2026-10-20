//! Clip original-location save v1. These types describe evidence, never paths or
//! authority to replay a save. Native adapters alone construct real evidence.
use crate::{Method, Request};
use serde::{Deserialize, Serialize};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_MANIFEST_BYTES: u64 = 16_777_216;

/// The sole alternate project read source. Missing means the existing memory read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ProjectReadSource {
    /// Fresh bounded read of the current real Clip Session's original manifest.
    PersistedClip,
}

/// Missing optional readSource retains the old mode; an explicit null is not a
/// second alternate source. Shared by IPC Request and the stdio ProjectArgs.
pub fn deserialize_read_source<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ProjectReadSource>, D::Error> {
    ProjectReadSource::deserialize(deserializer).map(Some)
}

/// The controlled manifest basename; never an arbitrary filename or path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "stdio", derive(schemars::JsonSchema))]
pub enum ClipManifestName {
    /// 原工程使用当前文件名 concat.json。
    #[serde(rename = "concat.json")]
    Concat,
    /// 原工程保留旧文件名 wolfcut.json，不迁移到新名称。
    #[serde(rename = "wolfcut.json")]
    Wolfcut,
}

/// 可证明未尝试重命名时，保存未提交的封闭原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipSaveReason {
    /// 保存被取消，且原生能够证明尚未尝试重命名。
    Cancelled,
    /// 原截止时间已到；是否已取得提交权仍由 commitClaimed 记录。
    Expired,
    /// 更新的保存票据使尚未提交的快照失效。
    Superseded,
    /// 原请求冻结的工程或编辑来源已改变。
    SourceChanged,
    /// 原 Session 的写入所有权已失效或改变。
    OwnershipChanged,
    /// 原工程选定的 manifest 目标已改变。
    TargetChanged,
    /// 保存快照或处理所需资源超过既定限制。
    ResourceLimit,
    /// 发生 I/O 错误，且原生能够证明尚未尝试重命名。
    IoFailure,
}
/// 已取得提交权后，原保存结果无法确认的封闭阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipSaveStage {
    /// 已取得提交权，但后续提交结果尚不能确认。
    AfterClaim,
    /// 尝试重命名原 manifest 时结果无法确认。
    Rename,
    /// 重命名后同步原父目录时无法确认完成。
    DirectorySync,
    /// 新开原 manifest 读取字节或核对摘要时无法确认结果。
    Readback,
    /// 实际文件的工程解析或与原冻结 Project 的等价核对失败。
    EngineParse,
}
/// 原生文件及父目录同步的持久确认状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipSaveDurability {
    /// 实际文件与父目录同步已获得原生确认。
    Confirmed,
}

/// A closed proof union. `validate_for_status` checks cross-field wire invariants;
/// passing validation is not proof that native filesystem work actually happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ClipSaveProof {
    /// 原操作已受理，尚不能判断是否已提交或完成。
    Pending {
        /// 保存证明协议版本，当前必须为 1。
        version: u8,
    },
    /// 原生能够证明未尝试重命名；取得提交权本身不表示已写入。
    NotCommitted {
        /// 保存证明协议版本，当前必须为 1。
        version: u8,
        /// 是否已通过原提交 CAS 取得提交权；明确到期仍可在取得后拒绝写入。
        commit_claimed: bool,
        /// 必须为 false，表示原操作未尝试重命名。
        rename_attempted: bool,
        /// 原生确认未提交的原因，不可用于重命名后的失败。
        reason: ClipSaveReason,
    },
    /// 原重命名、同步、新开文件读回及真实工程等价核对均已完成。
    Verified {
        /// 保存证明协议版本，当前必须为 1。
        version: u8,
        /// 原 SaveLane 票据的规范十进制 u64 字符串，不新增业务操作身份。
        save_ticket: String,
        /// 实际保存的原编辑版本，须与原 sourceRevision 相等。
        persisted_revision: u64,
        /// 原工程实际使用的受控 manifest 文件名。
        manifest_name: ClipManifestName,
        /// 实际 manifest 字节数，范围为 1 到 16777216。
        byte_length: u64,
        /// 原冻结编码字节的 SHA256，使用 64 字符小写十六进制。
        snapshot_sha256: String,
        /// 新开实际文件读回所得 SHA256，必须与 snapshotSha256 相同。
        manifest_sha256: String,
        /// 实际文件经 engine 解析所得 clip-v1 摘要，须与原来源摘要相同。
        persisted_document_state_id: String,
        /// 实际文件可读的工程文档版本，须为非负安全整数。
        document_version: u64,
        /// 原生持久确认，当前只能为 confirmed。
        durability: ClipSaveDurability,
    },
    /// 已取得提交权但原结果尚待核对，禁止换身份重做。
    OutcomeUnknown {
        /// 保存证明协议版本，当前必须为 1。
        version: u8,
        /// 必须为 true，保留原操作已经取得提交权的事实。
        commit_claimed: bool,
        /// 原操作是否实际尝试过重命名，不能根据查询结果推断。
        rename_attempted: bool,
        /// 原重命名是否成功；为 true 时 renameAttempted 也必须为 true。
        rename_succeeded: bool,
        /// 原操作最后无法确认的提交或核对阶段。
        stage: ClipSaveStage,
    },
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn document_hash(value: &str) -> bool {
    value.strip_prefix("clip-v1-").is_some_and(sha256)
}
fn edit_hash(value: &str) -> bool {
    value.strip_prefix("clip-edit-v1-").is_some_and(sha256)
}
fn safe(value: u64) -> bool {
    value <= MAX_SAFE_INTEGER
}
fn ticket(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}
impl ClipSaveProof {
    /// 校验证明形态与 envelope 状态是否一致；不证明原生文件操作真实发生。
    pub fn validate_for_status(&self, status: &str) -> Result<(), &'static str> {
        let valid = match self {
            Self::Pending { version } => *version == 1 && status == "pending",
            Self::NotCommitted {
                version,
                rename_attempted,
                ..
            } => *version == 1 && status == "saveNotCommitted" && !rename_attempted,
            Self::Verified {
                version,
                save_ticket,
                persisted_revision,
                byte_length,
                snapshot_sha256,
                manifest_sha256,
                persisted_document_state_id,
                document_version,
                ..
            } => {
                *version == 1
                    && status == "ok"
                    && ticket(save_ticket)
                    && safe(*persisted_revision)
                    && (1..=MAX_MANIFEST_BYTES).contains(byte_length)
                    && sha256(snapshot_sha256)
                    && manifest_sha256 == snapshot_sha256
                    && document_hash(persisted_document_state_id)
                    && safe(*document_version)
            }
            Self::OutcomeUnknown {
                version,
                commit_claimed,
                rename_attempted,
                rename_succeeded,
                ..
            } => {
                *version == 1
                    && status == "outcomeUnknown"
                    && *commit_claimed
                    && (!rename_succeeded || *rename_attempted)
            }
        };
        if valid {
            Ok(())
        } else {
            Err("invalidClipSaveProof")
        }
    }
}

/// Save receipt data in the existing original clip_edit response envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipSaveData {
    /// 原回执所属的原生 App 实例身份。
    pub app_instance_id: String,
    /// 原操作所属的具名客户端身份。
    pub client_id: String,
    /// 原操作绑定的工程身份。
    pub project_id: String,
    /// 原操作绑定的文档 Session 身份。
    pub document_session_id: String,
    /// 原规范操作身份，必须与客户端、文档 Session 及原序号一致。
    pub operation_id: String,
    /// 原操作的非零安全整数序号，查询或恢复不得递增。
    pub client_sequence: u64,
    /// 原具名工具，必须为 clip_edit。
    pub tool: String,
    /// 原冻结的无参 Save 动作，不接受其他编辑动作。
    pub edit: crate::ClipEdit,
    /// 原保存来源冻结时的文档 generation，须为非负安全整数。
    pub generation: u64,
    /// 原请求冻结的编辑版本，不用完成时的新版本替代。
    pub source_revision: u64,
    /// 原请求冻结的选择版本，不用完成时的新选择替代。
    pub source_selection_revision: u64,
    /// 原请求绑定的 timeline 身份，保存范围仍为整个工程。
    pub source_timeline_id: String,
    /// 原冻结编辑状态的 clip-edit-v1 摘要。
    pub source_edit_state_id: String,
    /// 原冻结 Project 的 clip-v1 摘要，用于核对真实文件解析结果。
    pub source_document_state_id: String,
    /// 属于同一原操作及冻结来源的保存证明。
    pub save_proof: ClipSaveProof,
}
impl ClipSaveData {
    /// 核对原身份、冻结来源与证明关系；不授予保存或重放权限。
    pub fn validate_for_status(&self, status: &str) -> Result<(), &'static str> {
        self.save_proof.validate_for_status(status)?;
        if self.tool != "clip_edit"
            || !matches!(self.edit, crate::ClipEdit::Save {})
            || ![
                self.generation,
                self.source_revision,
                self.source_selection_revision,
                self.client_sequence,
            ]
            .into_iter()
            .all(safe)
            || self.operation_id
                != crate::operation_id(
                    &self.client_id,
                    &self.document_session_id,
                    self.client_sequence,
                )
                .map_err(|_| "invalidClipSaveIdentity")?
            || crate::clip_edit::clip_native_id(&self.source_timeline_id, "timeline").is_err()
            || !edit_hash(&self.source_edit_state_id)
            || !document_hash(&self.source_document_state_id)
        {
            return Err("invalidClipSaveSource");
        }
        if let ClipSaveProof::Verified {
            persisted_revision,
            persisted_document_state_id,
            ..
        } = &self.save_proof
            && (*persisted_revision != self.source_revision
                || persisted_document_state_id != &self.source_document_state_id)
        {
            return Err("invalidClipSaveSnapshot");
        }
        Ok(())
    }
}

/// Path-free result of a distinct fresh-file project read. observedRevision is
/// the frozen memory observation, never a revision inferred from manifest JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipPersistedState {
    /// 独立落盘读取协议版本，当前必须为 1。
    pub clip_persistence_protocol_version: u8,
    /// 实际新开原 manifest 的来源标记，必须为 freshManifest。
    pub source: String,
    /// 本次读取所属的当前原生 App 实例身份。
    pub app_instance_id: String,
    /// 本次读取获准使用的具名客户端身份。
    pub client_id: String,
    /// 本次读取绑定的当前工程身份。
    pub project_id: String,
    /// 本次读取绑定的当前真实文档 Session 身份。
    pub document_session_id: String,
    /// 读取 job 冻结时的文档 generation，须为非负安全整数。
    pub generation: u64,
    /// 冻结时观察到的内存编辑版本，不是从文件推断的持久版本。
    pub observed_revision: u64,
    /// 实际新开读取的原受控 manifest 文件名。
    pub manifest_name: ClipManifestName,
    /// 实际新开文件的字节数，范围为 1 到 16777216。
    pub byte_length: u64,
    /// 实际新开文件字节的 64 字符小写十六进制 SHA256。
    pub manifest_sha256: String,
    /// 实际文件经 engine 解析所得 Project 的 clip-v1 摘要。
    pub document_state_id: String,
    /// 实际文件可读的工程文档版本，须为非负安全整数。
    pub document_version: u64,
}
impl ClipPersistedState {
    /// 校验新开文件读回对象的版本、固定来源、边界及摘要格式。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.clip_persistence_protocol_version == 1
            && self.source == "freshManifest"
            && [
                self.generation,
                self.observed_revision,
                self.document_version,
            ]
            .into_iter()
            .all(safe)
            && (1..=MAX_MANIFEST_BYTES).contains(&self.byte_length)
            && sha256(&self.manifest_sha256)
            && document_hash(&self.document_state_id)
        {
            Ok(())
        } else {
            Err("invalidClipPersistence")
        }
    }
}

pub(crate) fn validate_persisted_request(r: &Request) -> Result<(), String> {
    if r.method == Method::Project
        && r.project_id.as_deref().is_some_and(|s| !s.is_empty())
        && r.document_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        && r.app_scope.is_none()
        && r.app_page.is_none()
        && r.app_control_token.is_none()
        && r.app_navigate.is_none()
        && r.app_operation_query.is_none()
        && r.revision.is_none()
        && r.context_revision.is_none()
        && r.object_id.is_none()
        && r.object_ids.is_none()
        && r.offset.is_none()
        && r.limit.is_none()
        && r.max_edge.is_none()
        && r.move_parameters.is_none()
        && r.canvas_parameters.is_none()
        && r.clip_parameters.is_none()
        && r.library_query.is_none()
        && r.library_assets_query.is_none()
        && r.operation_query.is_none()
        && r.flow.is_none()
        && r.flow_workspace.is_none()
    {
        Ok(())
    } else {
        Err("invalidInput: persisted Clip read fields".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn verified() -> Value {
        json!({"version":1,"state":"verified","saveTicket":"18446744073709551615","persistedRevision":7,"manifestName":"wolfcut.json","byteLength":123,"snapshotSha256":"a".repeat(64),"manifestSha256":"a".repeat(64),"persistedDocumentStateId":format!("clip-v1-{}","b".repeat(64)),"documentVersion":1,"durability":"confirmed"})
    }
    #[test]
    fn save_is_closed_without_parameters_and_legacy_actions_still_decode() {
        let save: crate::ClipEdit = serde_json::from_value(json!({"kind":"save"})).unwrap();
        assert_eq!(save, crate::ClipEdit::Save {});
        assert!(save.validate().is_ok());
        assert_eq!(serde_json::to_value(&save).unwrap(), json!({"kind":"save"}));
        for field in ["path", "document", "manifestName", "expectedHash"] {
            for extra in [json!("forged"), json!({"fake":true}), Value::Null] {
                let mut value = json!({"kind":"save"});
                value[field] = extra;
                assert!(serde_json::from_value::<crate::ClipEdit>(value).is_err());
            }
        }
        assert_eq!(
            serde_json::from_value::<crate::ClipEdit>(json!({"kind":"addTrack"})).unwrap(),
            crate::ClipEdit::AddTrack
        );
    }
    #[test]
    fn persisted_read_is_fixed_exclusive_and_not_a_query_alias() {
        let original = json!({"instanceId":"i","clientId":"c","clientToken":"r","method":"project","projectId":"p","documentSessionId":"s","readSource":"persistedClip"});
        let r: Request = serde_json::from_value(original.clone()).unwrap();
        assert!(crate::validate_request_shape(&r).is_ok());
        for key in [
            "objectId",
            "objectIds",
            "offset",
            "limit",
            "revision",
            "contextRevision",
            "maxEdge",
            "operationQuery",
            "clipParameters",
            "canvasParameters",
            "appScope",
            "flow",
            "flowWorkspace",
        ] {
            let mut value = original.clone();
            value[key] = match key {
                "objectIds" => json!(["clip:c"]),
                "objectId" => json!("clip:c"),
                "offset" | "limit" | "revision" | "contextRevision" | "maxEdge" => json!(0),
                _ => json!({}),
            };
            assert!(
                match serde_json::from_value::<Request>(value) {
                    Err(_) => true,
                    Ok(r) => crate::validate_request_shape(&r).is_err(),
                },
                "{key}"
            );
        }
        for method in ["context", "clipEdit", "clipOperation", "appContext"] {
            let mut value = original.clone();
            value["method"] = json!(method);
            let r: Request = serde_json::from_value(value).unwrap();
            assert!(crate::validate_request_shape(&r).is_err());
        }
        for source in [
            json!("memory"),
            json!("../concat.json"),
            json!("wolfcut.json"),
            Value::Null,
        ] {
            let mut value = original.clone();
            value["readSource"] = source;
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
        let mut legacy = original;
        legacy.as_object_mut().unwrap().remove("readSource");
        assert!(
            crate::validate_request_shape(&serde_json::from_value::<Request>(legacy).unwrap())
                .is_ok()
        );
    }
    #[test]
    fn original_save_identity_and_parsed_file_digest_are_both_required() {
        let client = "c";
        let session = "s";
        let data = json!({"appInstanceId":"i","clientId":client,"projectId":"p","documentSessionId":session,"operationId":crate::operation_id(client,session,1).unwrap(),"clientSequence":1,"tool":"clip_edit","edit":{"kind":"save"},"generation":3,"sourceRevision":7,"sourceSelectionRevision":2,"sourceTimelineId":"timeline:t","sourceEditStateId":format!("clip-edit-v1-{}","c".repeat(64)),"sourceDocumentStateId":format!("clip-v1-{}","b".repeat(64)),"saveProof":verified()});
        let parsed: ClipSaveData = serde_json::from_value(data.clone()).unwrap();
        assert!(parsed.validate_for_status("ok").is_ok());
        for (key, value) in [
            ("sourceRevision", json!(8)),
            ("operationId", json!("forged")),
            ("sourceSelectionRevision", json!(9007199254740992u64)),
        ] {
            let mut changed = data.clone();
            changed[key] = value;
            let parsed: ClipSaveData = serde_json::from_value(changed).unwrap();
            assert!(parsed.validate_for_status("ok").is_err());
        }
        let persisted = json!({"clipPersistenceProtocolVersion":1,"source":"freshManifest","appInstanceId":"i","clientId":"c","projectId":"p","documentSessionId":"s","generation":3,"observedRevision":7,"manifestName":"wolfcut.json","byteLength":123,"manifestSha256":"a".repeat(64),"documentStateId":format!("clip-v1-{}","b".repeat(64)),"documentVersion":1});
        assert!(
            serde_json::from_value::<ClipPersistedState>(persisted.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        let mut cached = persisted.clone();
        cached["source"] = json!("cachedReceipt");
        assert!(
            serde_json::from_value::<ClipPersistedState>(cached)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut path = persisted;
        path["path"] = json!("/private");
        assert!(serde_json::from_value::<ClipPersistedState>(path).is_err());
    }

    #[test]
    fn proof_status_bounds_and_commit_outcomes_cannot_be_forged() {
        let p: ClipSaveProof = serde_json::from_value(verified()).unwrap();
        assert!(p.validate_for_status("ok").is_ok());
        assert!(p.validate_for_status("pending").is_err());
        for (key, value) in [
            ("saveTicket", json!("01")),
            ("saveTicket", json!("18446744073709551616")),
            ("byteLength", json!(0)),
            ("byteLength", json!(16777217)),
            ("manifestSha256", json!("b".repeat(64))),
            ("persistedRevision", json!(9007199254740992u64)),
        ] {
            let mut v = verified();
            v[key] = value;
            let p: ClipSaveProof = serde_json::from_value(v).unwrap();
            assert!(p.validate_for_status("ok").is_err());
        }
        let mut v = verified();
        v["path"] = json!("/private");
        assert!(serde_json::from_value::<ClipSaveProof>(v).is_err());
        let p:ClipSaveProof=serde_json::from_value(json!({"version":1,"state":"notCommitted","commitClaimed":true,"renameAttempted":false,"reason":"expired"})).unwrap();
        assert!(p.validate_for_status("saveNotCommitted").is_ok());
        let p:ClipSaveProof=serde_json::from_value(json!({"version":1,"state":"outcomeUnknown","commitClaimed":true,"renameAttempted":false,"renameSucceeded":true,"stage":"readback"})).unwrap();
        assert!(p.validate_for_status("outcomeUnknown").is_err());
    }
}
