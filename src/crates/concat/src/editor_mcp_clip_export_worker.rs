//! 冻结渲染、发布和登记均在原 Job 生命周期内完成。
use super::*;
use concat_host::export::{ExportSpec, VideoCodec};
use concat_host::export_publication::{
    PublishPermit, PublishRefusal, PublishRevocation, PublishState,
};
use concat_host::export_snapshot::{FreezeLimits, FrozenExport, SnapshotRenderError};
use concat_host::export_verification::{
    OutputExpectation, VerificationLimits, VerifiedOutput, verify_output,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(super) struct Work {
    pub source: ClipExportData,
    pub project: concat_project::Project,
    pub provenance: crate::personal_library::OutputProvenance,
    pub project_dir: Option<PathBuf>,
    pub titles: Arc<concat_host::Titles>,
    pub catalogue: concat_effects::Catalogue,
    pub staging_parent: PathBuf,
    pub output: PathBuf,
    pub library_root: PathBuf,
    pub job: concat_host::jobs::Job,
    pub gate: PublishPermit,
    pub revoker: concat_host::export_publication::PublishRevoker,
}

/// 只有完整核验与同步成功才能建立恢复材料。公开 JSON 无法构造此附件。
#[derive(Clone)]
pub(in crate::editor_mcp) struct VerifiedMaterial {
    pub proof: Arc<VerifiedOutput>,
    pub output: PathBuf,
    pub library_root: PathBuf,
    pub name: String,
    pub registration_id: String,
    pub manifest_sha256: String,
    pub expected: ClipExportExpected,
    pub asset: Option<ClipExportAsset>,
    pub provenance: crate::personal_library::OutputProvenance,
}

pub(super) struct Outcome {
    pub response: Response,
    pub material: Option<VerifiedMaterial>,
}

fn failed(source: ClipExportData, claimed: bool, reason: ClipExportReason) -> Outcome {
    Outcome {
        response: not_published(source, claimed, reason),
        material: None,
    }
}
fn unknown(
    source: ClipExportData,
    publication: ClipExportPublication,
    stage: ClipExportStage,
) -> Outcome {
    Outcome {
        response: response(
            source,
            ClipExportProof::OutcomeUnknown {
                version: 1,
                publish_claimed: true,
                publication,
                stage,
            },
        ),
        material: None,
    }
}

pub(super) fn revocation_reason(reason: PublishRevocation) -> ClipExportReason {
    match reason {
        PublishRevocation::Stopped => ClipExportReason::Stopped,
        PublishRevocation::Revoked | PublishRevocation::Abandoned => ClipExportReason::Revoked,
        PublishRevocation::AccountChanged => ClipExportReason::AccountChanged,
        PublishRevocation::SessionChanged => ClipExportReason::SessionChanged,
        PublishRevocation::DocumentChanged => ClipExportReason::DocumentChanged,
        PublishRevocation::LeaseExpired => ClipExportReason::LeaseExpired,
    }
}
fn before_claim_reason(work: &Work, fallback: ClipExportReason) -> ClipExportReason {
    match work.revoker.state() {
        PublishState::Revoked(reason) => revocation_reason(reason),
        _ if work.job.cancelled() => ClipExportReason::Stopped,
        _ => fallback,
    }
}

pub(super) fn codec(codec: ClipExportCodec) -> VideoCodec {
    match codec {
        ClipExportCodec::H264 => VideoCodec::H264,
        ClipExportCodec::Hevc => VideoCodec::Hevc,
        ClipExportCodec::Av1 => VideoCodec::Av1,
    }
}
fn public_codec(codec: VideoCodec) -> ClipExportCodec {
    match codec {
        VideoCodec::H264 => ClipExportCodec::H264,
        VideoCodec::Hevc => ClipExportCodec::Hevc,
        VideoCodec::Av1 => ClipExportCodec::Av1,
    }
}
fn expected(
    facts: concat_export::RenderFacts,
) -> Result<(ClipExportExpected, OutputExpectation), ()> {
    let public = ClipExportExpected {
        width: facts.width,
        height: facts.height,
        rate_num: u64::try_from(facts.rate_num).map_err(|_| ())?,
        rate_den: u64::try_from(facts.rate_den).map_err(|_| ())?,
        frames: facts.video_frames,
        codec: public_codec(facts.codec),
        ten_bit: facts.ten_bit,
        audio_expected: facts.audio_expected,
    };
    public.validate().map_err(|_| ())?;
    let native = OutputExpectation {
        video: concat_project::model::VideoSettings {
            width: facts.width,
            height: facts.height,
            rate_num: facts.rate_num,
            rate_den: facts.rate_den,
        },
        frames: facts.video_frames,
        codec: facts.codec,
        ten_bit: facts.ten_bit,
        audio_expected: facts.audio_expected,
    };
    Ok((public, native))
}

pub(super) fn summary(proof: &VerifiedOutput) -> Result<ClipExportSummary, ()> {
    let s = proof.summary();
    let codec = match s.video_codec.as_str() {
        "h264" => ClipExportCodec::H264,
        "hevc" => ClipExportCodec::Hevc,
        "av1" => ClipExportCodec::Av1,
        _ => return Err(()),
    };
    let public = ClipExportSummary {
        byte_length: s.bytes,
        sha256: s.sha256.clone(),
        codec,
        bit_depth: s.video_bit_depth,
        width: s.width,
        height: s.height,
        rate_num: u64::try_from(s.rate_num).map_err(|_| ())?,
        rate_den: u64::try_from(s.rate_den).map_err(|_| ())?,
        video_frames: s.video_frames,
        first_video_position_seconds: s.first_video_position_seconds,
        last_video_position_seconds: s.last_video_position_seconds,
        video_seconds: s.video_seconds,
        audio_streams: s
            .audio
            .iter()
            .map(|a| ClipExportAudioStream {
                index: a.stream,
                codec: a.codec.clone(),
                source_sample_rate: a.source_rate,
                source_channels: a.source_channels,
                sample_frames: a.sample_frames,
                seconds: a.seconds,
                peak: a.peak,
                rms: a.rms,
            })
            .collect(),
    };
    public.validate_shape().map_err(|_| ())?;
    Ok(public)
}

/// 不使用当前 UI、当前 Project 或模型传入的文件路径。
pub(super) fn run(work: Work, progress: impl FnMut(ClipExportPhase, u64, u64)) -> Outcome {
    let source = work.source.clone();
    let revoker = work.revoker.clone();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_inner(work, progress))) {
        Ok(outcome) => outcome,
        Err(_) => match revoker.state() {
            PublishState::Publishing => unknown(
                source,
                ClipExportPublication::Unconfirmed,
                ClipExportStage::Completion,
            ),
            _ => failed(source, false, ClipExportReason::EngineFailure),
        },
    }
}

fn run_inner(work: Work, mut progress: impl FnMut(ClipExportPhase, u64, u64)) -> Outcome {
    let ClipEdit::ExportVideo {
        name,
        codec: requested_codec,
        quality,
        ten_bit,
        ..
    } = &work.source.edit
    else {
        return failed(work.source, false, ClipExportReason::UnsupportedInput);
    };
    let name = name.clone();
    let spec = ExportSpec {
        output: work.output.to_string_lossy().into_owned(),
        crf: quality.crf(),
        preset: "veryfast".into(),
        codec: codec(*requested_codec),
        ten_bit: *ten_bit,
    };
    if fs::create_dir_all(&work.staging_parent).is_err()
        || work
            .output
            .parent()
            .is_none_or(|p| fs::create_dir_all(p).is_err())
    {
        return failed(work.source, false, ClipExportReason::IoFailure);
    }
    let frozen = match FrozenExport::prepare(
        &work.project,
        &work.project.active_timeline_id,
        work.project_dir.as_deref(),
        &spec,
        &work.titles,
        work.catalogue.clone(),
        &work.staging_parent,
        FreezeLimits::default(),
        work.job.cancel_flag(),
    ) {
        Ok(frozen) => frozen,
        Err(_) => {
            let reason = before_claim_reason(&work, ClipExportReason::UnsupportedInput);
            return failed(work.source, false, reason);
        }
    };
    // 引用中的固定版本必须等于实际冻结副本，不能只把旧版本标签写进来源。
    for item in &work.project.media {
        if !work
            .project
            .active()
            .clips
            .iter()
            .any(|clip| clip.media_id == item.id)
        {
            continue;
        }
        if let Some(raw) = item.extra.get("seecutLibraryReference") {
            let expected_sha = raw
                .get("version")
                .and_then(|v| v.get("sha256"))
                .and_then(Value::as_str);
            let frozen_sha = frozen
                .manifest()
                .entries
                .iter()
                .find(|entry| entry.source == item.path)
                .and_then(|entry| match &entry.shape {
                    concat_host::export_snapshot::InputShape::File { sha256, .. } => {
                        Some(sha256.as_str())
                    }
                    _ => None,
                });
            if expected_sha.is_none() || frozen_sha != expected_sha {
                return failed(work.source, false, ClipExportReason::SourceChanged);
            }
        }
    }
    let (public_expected, native_expected) =
        match concat_export::render_facts(frozen.request(), concat_export::RenderMode::StrictLocal)
            .map_err(|_| ())
            .and_then(expected)
        {
            Ok(expected) => expected,
            Err(()) => return failed(work.source, false, ClipExportReason::UnsupportedInput),
        };
    let frames = public_expected.frames;
    let mut proof = None;
    let mut hook_failure = None;
    let mut claim_failure = None;
    let mut claimed = false;
    // Separate reporting closures share one mutable reporter only through this RefCell;
    // callbacks are synchronous and never hold its borrow across the engine call.
    let progress = std::cell::RefCell::new(&mut progress);
    let result = frozen.render_before_publish(
        work.job.cancel_flag(),
        |p| {
            let done = u64::try_from(p.frame).unwrap_or(0).min(frames);
            progress.borrow_mut()(ClipExportPhase::Rendering, done, frames);
        },
        |temporary| {
            progress.borrow_mut()(ClipExportPhase::Verifying, frames, frames);
            let verified = verify_output(
                temporary,
                native_expected,
                VerificationLimits::default(),
                work.job.cancel_flag(),
            )
            .inspect_err(|_| {
                hook_failure = Some(ClipExportReason::OutputInvalid);
            })?;
            File::open(temporary)
                .and_then(|f| f.sync_all())
                .map_err(|error| {
                    hook_failure = Some(ClipExportReason::IoFailure);
                    error.to_string()
                })?;
            // All fallible content work is before this CAS. Hook success leads directly
            // into the existing exclusive publication implementation.
            let _claim = work.gate.try_claim().map_err(|reason| {
                claim_failure = Some(reason);
                "original publication claim refused".to_owned()
            })?;
            claimed = true;
            proof = Some(verified);
            Ok(())
        },
    );
    match result {
        Err(SnapshotRenderError::PublishedInputsUnverified { .. }) => {
            return unknown(
                work.source,
                ClipExportPublication::Confirmed,
                ClipExportStage::InputPostcheck,
            );
        }
        Err(_) if claimed => {
            return unknown(
                work.source,
                ClipExportPublication::Unconfirmed,
                ClipExportStage::Publish,
            );
        }
        Err(_) => {
            let (claimed, reason) = match claim_failure {
                Some(PublishRefusal::LeaseExpiredAfterClaim) => {
                    (true, ClipExportReason::LeaseExpired)
                }
                Some(PublishRefusal::Revoked(reason)) => (false, revocation_reason(reason)),
                Some(PublishRefusal::AlreadyPublishing) => {
                    return unknown(
                        work.source,
                        ClipExportPublication::Unconfirmed,
                        ClipExportStage::Publish,
                    );
                }
                None => (
                    false,
                    before_claim_reason(
                        &work,
                        hook_failure.unwrap_or(ClipExportReason::EngineFailure),
                    ),
                ),
            };
            return failed(work.source, claimed, reason);
        }
        Ok(rendered) if Path::new(&rendered.path) != work.output => {
            return unknown(
                work.source,
                ClipExportPublication::Confirmed,
                ClipExportStage::OutputReadback,
            );
        }
        Ok(_) => {}
    }
    progress.borrow_mut()(ClipExportPhase::Publishing, frames, frames);
    if work
        .output
        .parent()
        .is_none_or(|parent| File::open(parent).and_then(|f| f.sync_all()).is_err())
    {
        return unknown(
            work.source,
            ClipExportPublication::Confirmed,
            ClipExportStage::DirectorySync,
        );
    }
    let proof = match proof.and_then(|proof| {
        proof
            .recheck_same_content(&work.output, work.job.cancel_flag())
            .ok()
    }) {
        Some(proof) => proof,
        None => {
            return unknown(
                work.source,
                ClipExportPublication::Confirmed,
                ClipExportStage::OutputReadback,
            );
        }
    };
    if frozen.verify_inputs(work.job.cancel_flag()).is_err() {
        return unknown(
            work.source,
            ClipExportPublication::Confirmed,
            ClipExportStage::InputPostcheck,
        );
    }
    let mut material = VerifiedMaterial {
        proof: Arc::new(proof),
        output: work.output,
        library_root: work.library_root,
        name,
        registration_id: work.source.registration_id.clone(),
        manifest_sha256: frozen.manifest_sha256().to_owned(),
        expected: public_expected,
        asset: None,
        provenance: work.provenance,
    };
    progress.borrow_mut()(ClipExportPhase::Registering, frames, frames);
    register(&mut material, work.job.cancel_flag());
    let response = material_response(work.source, &material);
    // Job and FrozenExport stay alive until every postcheck and registration ends.
    Outcome {
        response,
        material: Some(material),
    }
}

pub(super) fn register(material: &mut VerifiedMaterial, cancel: &AtomicBool) {
    register_with_commit_check(material, cancel, None);
}

pub(super) fn register_with_gate(
    material: &mut VerifiedMaterial,
    cancel: &AtomicBool,
    gate: &concat_host::export_publication::PublishPermit,
) {
    register_with_commit_check(material, cancel, Some(gate));
}

fn register_with_commit_check(
    material: &mut VerifiedMaterial,
    cancel: &AtomicBool,
    gate: Option<&concat_host::export_publication::PublishPermit>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::personal_library::with_library_write_lock(|| {
            if let Some(gate) = gate {
                crate::registration_retry::register_verified_video_output_with_commit_check(
                    &material.library_root,
                    &material.output,
                    Some(&material.name),
                    &material.registration_id,
                    &material.proof,
                    Some(material.provenance.clone()),
                    cancel,
                    || {
                        gate.try_claim()
                            .map(|_| ())
                            .map_err(|reason| format!("登记授权已失效：{reason:?}"))
                    },
                )
            } else {
                crate::registration_retry::register_verified_video_output(
                    &material.library_root,
                    &material.output,
                    Some(&material.name),
                    &material.registration_id,
                    &material.proof,
                    Some(material.provenance.clone()),
                    cancel,
                )
            }
        })
    }))
    .unwrap_or_else(|_| Err("原登记核验中断，保留已确认成片和原登记身份".into()));
    material.asset = result.ok().and_then(|asset| {
        asset.version.map(|version| ClipExportAsset {
            asset_id: asset.asset_id,
            version: concat_editor_mcp::AssetVersion {
                sha256: version.sha256,
                registration_id: version.registration_id,
                previous_id: version.previous_id,
            },
        })
    });
}

pub(super) fn material_response(source: ClipExportData, material: &VerifiedMaterial) -> Response {
    let Ok(summary) = summary(&material.proof) else {
        return response(
            source,
            ClipExportProof::OutcomeUnknown {
                version: 1,
                publish_claimed: true,
                publication: ClipExportPublication::Confirmed,
                stage: ClipExportStage::Completion,
            },
        );
    };
    let proof = match &material.asset {
        Some(asset) => ClipExportProof::Verified {
            version: 1,
            input_manifest_sha256: material.manifest_sha256.clone(),
            expected: material.expected.clone(),
            summary,
            durability: ClipExportDurability::Confirmed,
            asset: asset.clone(),
        },
        None => ClipExportProof::RegistrationPending {
            version: 1,
            input_manifest_sha256: material.manifest_sha256.clone(),
            expected: material.expected.clone(),
            summary,
            durability: ClipExportDurability::Confirmed,
        },
    };
    response(source, proof)
}
