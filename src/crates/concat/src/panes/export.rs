// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The export sheet: the form, the running render, and the result.
//!
//! The first pane cut out of the window's controller, and the shape every
//! later one follows. The pane owns its state; every way it can change -
//! a field edited, a button pressed, a worker reporting - is one
//! [`ExportMsg`]; [`ExportPane::update`] is the only code that changes the
//! state; and [`ExportPane::data`] is the one place its Slint rows are
//! built. The controller it needs for context - the session, the output
//! size, the job slots - is handed to `update` as the studio, which the
//! pane reads and asks things of but never reaches into for its own
//! fields.

use concat_host::export::{self, ExportSpec};
use slint::ComponentHandle;

use crate::format::{bytes, eta};
use crate::host::{on_ui, spawn};
use crate::i18n::{self, t, tf};
use crate::panes::Msg;
use crate::platform;
use crate::studio::{
    AUDIO_BPS, EXPORT_CRF, EXPORT_RATES, EXPORT_SHORT_SIDES, EXPORT_TIERS, Studio, home_folder,
};
use crate::ui::{ExportData, ExportPhase};

/// Everything that can happen to the export sheet.
#[derive(Clone, Debug)]
pub enum ExportMsg {
    ChooseDestination(bool),
    /// The sheet is dismissed.
    Close,
    NameEdited(String),
    ResolutionChanged(i32),
    RateChanged(i32),
    QualityChanged(i32),
    CodecChanged(i32),
    TenBitChanged(bool),
    /// Back to the form after a finished or failed render.
    Again,
    /// Pick the destination folder.
    Browse,
    /// Show the finished file in the file manager.
    Reveal,
    Start,
    Cancel,
    /// The render's worker reporting where it is.
    Progress {
        job_id: u64,
        /// Of the whole, `0..=1`.
        fraction: f32,
        /// What it is doing, in the person's language.
        stage: String,
    },
    /// The render's worker is done: the file written, or why not.
    Finished {
        job_id: u64,
        result: Result<String, String>,
    },
}

struct ActiveExport {
    id: u64,
    to_library: bool,
    name: String,
    output: String,
}

/// The export sheet's state.
pub struct ExportPane {
    pub open: bool,
    pub name: String,
    pub folder: String,
    pub resolution: usize,
    pub rate: usize,
    pub quality: usize,
    /// Index into `VideoCodec::ALL`.
    pub codec: usize,
    pub ten_bit: bool,
    pub phase: ExportPhase,
    pub progress: f32,
    pub stage: String,
    pub message: String,
    /// Where the finished file is, for Reveal.
    pub written: String,
    to_library: bool,
    library_output: Option<String>,
    /// When the render started, for a real ETA.
    started_at: Option<std::time::Instant>,
    next_job_id: u64,
    active_job: Option<ActiveExport>,
    cancelled_jobs: std::collections::HashSet<u64>,
}

impl Default for ExportPane {
    fn default() -> Self {
        Self {
            open: false,
            name: "Untitled".into(),
            folder: home_folder("Movies"),
            resolution: 2,
            rate: 1,
            quality: 1,
            codec: 0,
            ten_bit: false,
            phase: ExportPhase::Idle,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            written: String::new(),
            to_library: false,
            library_output: None,
            started_at: None,
            next_job_id: 0,
            active_job: None,
            cancelled_jobs: std::collections::HashSet::new(),
        }
    }
}

impl ExportPane {
    /// Applies one message. The studio is the rest of the window, for
    /// what the sheet needs to know and to start; the pane's own state is
    /// `self`, and while this runs the studio's copy of it is a blank the
    /// pane must not read.
    pub fn update(&mut self, msg: ExportMsg, studio: &mut Studio) {
        match msg {
            ExportMsg::ChooseDestination(to_library) => {
                if self.active_job.is_some() {
                    self.open = true;
                    return;
                }
                let folder = if to_library {
                    match concat_host::AppDirs::locate() {
                        Ok(dirs) => dirs.data.join("clip-exports"),
                        Err(error) => {
                            studio.notify(&format!("无法打开资产库：{error}"), true);
                            return;
                        }
                    }
                } else {
                    let Some(folder) = platform::pick_folder(&i18n::t("Export to"), &self.folder)
                    else {
                        return;
                    };
                    folder
                };
                if let Err(error) = std::fs::create_dir_all(&folder) {
                    studio.notify(&format!("无法创建导出目录：{error}"), true);
                    return;
                }
                self.folder = folder.to_string_lossy().into_owned();
                self.to_library = to_library;
                self.library_output = to_library.then(|| {
                    folder
                        .join(format!("{}.mp4", uuid::Uuid::new_v4()))
                        .to_string_lossy()
                        .into_owned()
                });
                self.open = true;
                self.phase = ExportPhase::Idle;
                self.message.clear();
            }
            ExportMsg::Close => self.open = false,
            ExportMsg::NameEdited(name) => self.name = name,
            ExportMsg::ResolutionChanged(index) => {
                self.resolution = (index.max(0) as usize).min(3);
            }
            ExportMsg::RateChanged(index) => self.rate = (index.max(0) as usize).min(2),
            ExportMsg::QualityChanged(index) => self.quality = (index.max(0) as usize).min(2),
            ExportMsg::CodecChanged(index) => self.codec = (index.max(0) as usize).min(2),
            ExportMsg::TenBitChanged(on) => self.ten_bit = on,
            ExportMsg::Again => {
                if self.active_job.is_some() {
                    return;
                }
                self.phase = ExportPhase::Idle;
                self.progress = 0.0;
                if self.to_library {
                    self.library_output = Some(format!(
                        "{}/{}.mp4",
                        self.folder.trim_end_matches('/'),
                        uuid::Uuid::new_v4()
                    ));
                }
            }
            ExportMsg::Browse => {
                if let Some(folder) = platform::pick_folder(&i18n::t("Export to"), &self.folder) {
                    self.folder = folder.to_string_lossy().into_owned();
                    self.to_library = false;
                    self.library_output = None;
                }
            }
            ExportMsg::Reveal => {
                if !self.written.is_empty()
                    && let Err(error) = platform::reveal(&self.written)
                {
                    studio.notify(&i18n::tf("Could not show the file: {0}", &[&error]), true);
                }
            }
            ExportMsg::Start => self.start(studio),
            ExportMsg::Cancel => {
                if let Some(job) = self.active_job.take() {
                    self.cancelled_jobs.insert(job.id);
                    studio.host.exporter.cancel();
                }
                self.phase = ExportPhase::Idle;
                self.progress = 0.0;
                if self.to_library {
                    self.library_output = Some(format!(
                        "{}/{}.mp4",
                        self.folder.trim_end_matches('/'),
                        uuid::Uuid::new_v4()
                    ));
                }
            }
            ExportMsg::Progress {
                job_id,
                fraction,
                stage,
            } => {
                if self.phase == ExportPhase::Running
                    && self.active_job.as_ref().is_some_and(|job| job.id == job_id)
                {
                    self.progress = fraction.clamp(0.0, 1.0);
                    self.stage = stage;
                }
            }
            ExportMsg::Finished { job_id, result } => {
                let Some(job) = self.take_finished_job(job_id) else {
                    if self.cancelled_jobs.remove(&job_id)
                        && let Ok(written) = result
                    {
                        studio.notify(&format!("上一次导出已完成：{written}"), false);
                    }
                    return;
                };
                match result {
                    Ok(written) => self.finish_success(studio, written, job),
                    Err(error) => {
                        self.phase = ExportPhase::Failed;
                        self.message = error.clone();
                        studio.notify(&tf("Export failed: {0}", &[&error]), true);
                    }
                }
            }
        }
    }

    fn take_finished_job(&mut self, job_id: u64) -> Option<ActiveExport> {
        if self.active_job.as_ref().is_some_and(|job| job.id == job_id) {
            self.active_job.take()
        } else {
            None
        }
    }

    fn finish_success(&mut self, studio: &mut Studio, written: String, job: ActiveExport) {
        self.phase = ExportPhase::Done;
        self.progress = 1.0;
        self.written = written;
        if job.to_library {
            let registration = crate::personal_library::with_library_write_lock(|| {
                let dirs = concat_host::AppDirs::locate()?;
                let mut library =
                    crate::personal_library::Library::load(dirs.data.join("personal-library"))?;
                library.register_generated_with_name(&self.written, Some(&job.name))?;
                Ok::<(), String>(())
            });
            match registration {
                Ok(()) => {
                    studio.notify("视频已导出并加入资产库", false);
                    crate::host::Shell::with(|_, app| {
                        app.global::<crate::ui::SeeCut>()
                            .invoke_action("personal-refresh".into(), "".into())
                    });
                }
                Err(error) => studio.notify(&format!("视频已导出，入库失败：{error}"), true),
            }
        } else {
            studio.notify(&t("Export finished"), false);
        }
    }

    /// The frame the export renders at: the sheet's short side, scaled
    /// along the project's aspect and rounded to even dimensions, which is
    /// what the encoder's chroma subsampling needs.
    pub fn size(&self, studio: &Studio) -> (u32, u32) {
        let short = EXPORT_SHORT_SIDES[self.resolution.min(EXPORT_SHORT_SIDES.len() - 1)];
        let (project_w, project_h) = studio.output_size();
        let (project_w, project_h) = (project_w.max(1) as f64, project_h.max(1) as f64);
        let even = |side: f64| ((side / 2.0).round() as u32 * 2).max(2);
        if project_w >= project_h {
            (even(short as f64 * project_w / project_h), short)
        } else {
            (short, even(short as f64 * project_h / project_w))
        }
    }

    /// A rough size of the file at one quality tier, in bytes.
    pub fn size_bytes(&self, studio: &Studio, tier: usize) -> f32 {
        let (width, height) = self.size(studio);
        let (num, den) = EXPORT_RATES[self.rate.min(2)];
        let rate = num as f32 / den as f32;
        let pixels = (width as f32 * height as f32) / (1920.0 * 1080.0);
        let video = EXPORT_TIERS[tier.min(2)]
            * 1_000_000.0
            * pixels
            * (rate / 30.0)
            * self.codec().size_factor()
            * if self.ten_bit { 1.05 } else { 1.0 };
        (video + AUDIO_BPS) * studio.duration().max(1.0) / 8.0
    }

    /// The codec the sheet has chosen.
    pub fn codec(&self) -> concat_media::VideoCodec {
        concat_media::VideoCodec::ALL[self.codec.min(concat_media::VideoCodec::ALL.len() - 1)]
    }

    /// Starts the render on a worker. Its reports come back as messages.
    fn start(&mut self, studio: &mut Studio) {
        if self.active_job.is_some() {
            return;
        }
        let Some(session) = studio.session.as_ref() else {
            return;
        };
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.contains('/')
            || self.name.contains('\\')
            || self.name.chars().any(char::is_control)
        {
            self.phase = ExportPhase::Failed;
            self.message = "文件名不能包含路径或首尾空格".into();
            return;
        }
        if studio.timeline().clips.is_empty() {
            self.phase = ExportPhase::Failed;
            self.message = t("There is nothing on the timeline to export");
            return;
        }
        let requested = self.library_output.clone().unwrap_or_else(|| {
            std::path::Path::new(&self.folder)
                .join(format!("{}.mp4", self.name.trim()))
                .to_string_lossy()
                .into_owned()
        });
        let (output, renamed) = match unique_export_path(std::path::Path::new(&requested)) {
            Ok(value) => value,
            Err(error) => {
                self.phase = ExportPhase::Failed;
                self.message = error;
                return;
            }
        };
        let job = match studio.host.exporter.begin() {
            Ok(job) => job,
            Err(error) => {
                self.phase = ExportPhase::Failed;
                self.message = error;
                return;
            }
        };
        let output = output.to_string_lossy().into_owned();
        let spec = ExportSpec {
            output: output.clone(),
            crf: EXPORT_CRF[self.quality.min(2)],
            preset: "veryfast".into(),
            codec: self.codec(),
            ten_bit: self.ten_bit,
        };
        let (frame_w, frame_h) = studio.output_size();
        let titles = studio
            .host
            .titles
            .clips(session.project(), frame_w, frame_h)
            .into_iter()
            .map(|title| title.clip)
            .collect();
        let mut request = export::request(session, &spec, titles);
        if renamed {
            studio.notify(&format!("同名文件已存在，改为导出到 {output}"), false);
        }
        let (width, height) = self.size(studio);
        let (num, den) = EXPORT_RATES[self.rate.min(2)];
        request.width = width;
        request.height = height;
        request.rate_num = num;
        request.rate_den = den;

        studio.pause();
        self.phase = ExportPhase::Running;
        self.progress = 0.0;
        self.stage = t("Rendering video");
        self.message.clear();
        self.written.clear();
        self.started_at = Some(std::time::Instant::now());
        self.next_job_id = self.next_job_id.wrapping_add(1);
        let job_id = self.next_job_id;
        self.active_job = Some(ActiveExport {
            id: job_id,
            to_library: self.to_library,
            name: self.name.clone(),
            output: output.clone(),
        });

        spawn(
            move || {
                let job = job;
                export::run(&request, job.cancel_flag(), |progress| {
                    let fraction = if progress.total > 0 {
                        progress.frame as f32 / progress.total as f32
                    } else {
                        0.0
                    };
                    let stage = match progress.stage {
                        "rendering" => t("Rendering video"),
                        "mixing audio" => t("Mixing audio"),
                        "muxing" => t("Finalising file"),
                        other => other.to_owned(),
                    };
                    on_ui(move |studio, _, _| {
                        studio.handle(Msg::Export(ExportMsg::Progress {
                            job_id,
                            fraction,
                            stage,
                        }));
                    });
                })
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Export(ExportMsg::Finished { job_id, result }))
            },
        );
    }

    /// The sheet as Slint shows it.
    pub fn data(&self, studio: &Studio) -> ExportData {
        let (width, height) = self.size(studio);
        let (num, den) = EXPORT_RATES[self.rate.min(2)];
        let rate = num as f32 / den as f32;
        let clips = studio.timeline().clips.len();
        let titles = studio
            .timeline()
            .clips
            .iter()
            .filter(|clip| clip.kind == concat_project::model::ClipKind::Text)
            .count();
        let clip_count = if clips == 1 {
            tf("{0} clip", &[&clips])
        } else {
            tf("{0} clips", &[&clips])
        };
        let title_count = if titles == 1 {
            tf("{0} title", &[&titles])
        } else {
            tf("{0} titles", &[&titles])
        };
        ExportData {
            open: self.open,
            name: self.name.as_str().into(),
            path: if let Some(job) = &self.active_job {
                job.output.clone()
            } else if self.phase == ExportPhase::Done && !self.written.is_empty() {
                self.written.clone()
            } else {
                self.library_output.clone().unwrap_or_else(|| {
                    format!("{}/{}.mp4", self.folder.trim_end_matches('/'), self.name)
                })
            }
            .into(),
            format: format!("{width} × {height} · {rate:.2} fps").into(),
            duration: {
                let whole = studio.duration().max(0.0) as i32;
                format!("{}:{:02}", whole / 60, whole % 60).into()
            },
            contents: if titles > 0 {
                format!("{clip_count} · {title_count}")
            } else {
                clip_count
            }
            .into(),
            resolution: self.resolution as i32,
            rate: self.rate as i32,
            quality: self.quality as i32,
            codec: self.codec as i32,
            ten_bit: self.ten_bit,
            encoding: {
                // "HEVC 10-bit · hardware": the standard, the depth when it
                // is the deeper one, and whether the platform's own encoder
                // will be doing it.
                let codec = self.codec();
                let mut words = vec![codec.label().to_owned()];
                if self.ten_bit {
                    words.push("10-bit".to_owned());
                }
                if codec
                    .encoders(true)
                    .first()
                    .is_some_and(|name| name.ends_with("_videotoolbox"))
                {
                    words.push(format!("· {}", t("hardware")));
                }
                words.join(" ")
            }
            .into(),
            size_high: bytes(self.size_bytes(studio, 0)).into(),
            size_balanced: bytes(self.size_bytes(studio, 1)).into(),
            size_small: bytes(self.size_bytes(studio, 2)).into(),
            phase: self.phase,
            progress: self.progress,
            stage: self.stage.as_str().into(),
            eta: if self.phase == ExportPhase::Running && self.progress > 0.02 {
                self.started_at
                    .map(|started| {
                        let elapsed = started.elapsed().as_secs_f32();
                        eta(elapsed / self.progress * (1.0 - self.progress)).into()
                    })
                    .unwrap_or_default()
            } else {
                slint::SharedString::new()
            },
            message: self.message.as_str().into(),
            done_size: bytes(self.size_bytes(studio, self.quality)).into(),
            empty: clips == 0,
        }
    }
}

/// Preserve every existing file when the folder picker supplies no system
/// overwrite confirmation. The engine checks again with an exclusive final
/// placement, closing the race after this user-facing choice.
fn unique_export_path(requested: &std::path::Path) -> Result<(std::path::PathBuf, bool), String> {
    let stem = requested
        .file_stem()
        .ok_or_else(|| "导出文件名无效".to_owned())?
        .to_string_lossy();
    let parent = requested.parent().unwrap_or(std::path::Path::new("."));
    for number in 1..=10_000 {
        let candidate = if number == 1 {
            requested.to_path_buf()
        } else {
            parent.join(format!("{stem} ({number}).mp4"))
        };
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((candidate, number != 1));
            }
            Err(error) => return Err(format!("无法检查导出文件：{error}")),
        }
    }
    Err("此目录中同名导出文件过多".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_completion_cannot_claim_a_new_job() {
        let mut pane = ExportPane::default();
        pane.active_job = Some(ActiveExport {
            id: 2,
            to_library: true,
            name: "new".into(),
            output: "new.mp4".into(),
        });
        assert!(pane.take_finished_job(1).is_none());
        assert_eq!(pane.take_finished_job(2).unwrap().name, "new");
        assert!(pane.take_finished_job(2).is_none());
    }

    #[test]
    fn existing_export_names_are_preserved() {
        let directory =
            std::env::temp_dir().join(format!("concat-export-name-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let requested = directory.join("film.mp4");
        std::fs::write(&requested, b"first").unwrap();
        std::fs::write(directory.join("film (2).mp4"), b"second").unwrap();
        let (chosen, renamed) = unique_export_path(&requested).unwrap();
        assert!(renamed);
        assert_eq!(chosen, directory.join("film (3).mp4"));
        assert_eq!(std::fs::read(&requested).unwrap(), b"first");
        assert_eq!(
            std::fs::read(directory.join("film (2).mp4")).unwrap(),
            b"second"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
