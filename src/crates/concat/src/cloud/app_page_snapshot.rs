// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read whitelisted Cloud UI caches while keeping private account state private.
//! Capture is request-driven, never called by the 40 ms assistant status timer.

use super::{AccountAccess, App, CloudItem, Model, SeeCut};
use crate::editor_mcp_app_page::{
    AppPages, Availability, CachedItem, Context, Detail, Error, Kind, PageId, PreviewSource, text,
};
use crate::studio::{Models, Studio};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slint::ComponentHandle;

/// Shared with private managed-provider URL normalization; no UI identity input.
fn canonical_account_scope(base: &str, uid: &str) -> Result<String, Error> {
    if uid.is_empty() || uid.len() > 256 || uid.chars().any(char::is_control) {
        return Err(Error::SignedOut);
    }
    let url = url::Url::parse(base).map_err(|_| Error::SignedOut)?;
    let loopback = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if url.scheme() != "https" && !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::SignedOut);
    }
    let base = url.as_str().trim_end_matches('/');
    let mut hash = Sha256::new();
    hash.update(base);
    hash.update([0]);
    hash.update(uid);
    Ok(format!("{:x}", hash.finalize()))
}

/// Lightweight identity only. Does not serialize/hash models or hold a token copy.
pub(super) fn identity(access: &AccountAccess) -> Result<(u64, String), Error> {
    let state = access.state.upgrade().ok_or(Error::SignedOut)?;
    let cloud = state.try_borrow().map_err(|_| Error::Busy)?;
    if cloud.token.is_empty() {
        return Err(Error::SignedOut);
    }
    let uid = cloud
        .history_name
        .strip_suffix("-history.json")
        .ok_or(Error::SignedOut)?;
    Ok((cloud.epoch, canonical_account_scope(&cloud.base, uid)?))
}

/// Extract current cached models. The caller MUST refresh this immediately before
/// fulfilling page_items, and after preview encoding before returning its bytes.
pub(super) fn capture(
    access: &AccountAccess,
    app: &App,
    studio: &Studio,
    models: &Models,
    registry: &mut AppPages<slint::Image>,
) -> Result<Context, Error> {
    let result = super::app_read_gate(studio, app)
        .and_then(|()| capture_current(access, app, studio, models, registry));
    if result.is_err() {
        registry.invalidate_capture();
    }
    result
}

fn capture_current(
    access: &AccountAccess,
    app: &App,
    studio: &Studio,
    models: &Models,
    registry: &mut AppPages<slint::Image>,
) -> Result<Context, Error> {
    let trusted = match identity(access) {
        Ok(identity) => identity,
        Err(Error::SignedOut) => {
            registry.account(None)?;
            return Err(Error::SignedOut);
        }
        Err(error) => return Err(error),
    };
    registry.account(Some(trusted))?;
    let navigation_start = registry.navigation_stamp()?;
    let ui = app.global::<SeeCut>();
    let mut items = ItemBuilder::default();
    let (page, mut view) = if studio.settings.open {
        let settings = &studio.settings;
        let view = json!({"tab":settings.tab,"language":studio.languages.get(settings.language).map(|l|l.code.as_str()),
            "playheadStops":settings.playhead_stops,"customContextActions":settings.custom_context_actions,
            "hardwareDecode":settings.hardware_decode});
        // Only the model list on the current settings tab. Never SettingsData,
        // download-base/server-listen/server-token/MCP tokens/path or system facts.
        let model_rows = match settings.tab {
            2 => settings.transcribers.as_slice(),
            3 => settings.voices.as_slice(),
            _ => &[],
        };
        for model in model_rows {
            push(
                &mut items,
                CachedItem {
                    source_key: format!("model:{}:{}", settings.tab, model.id),
                    kind: Kind::Model,
                    name: model.name.clone(),
                    status: String::new(),
                    selected: model.active,
                    availability: if model.installed {
                        Availability::Available
                    } else {
                        Availability::Unknown
                    },
                    project: false,
                    detail: Some(Detail::Model {
                        installed: model.installed,
                        active: model.active,
                        downloading: model.fetched.is_some(),
                    }),
                    preview: None,
                },
            )?;
        }
        (PageId::Settings, view)
    } else {
        match ui.get_page() {
            1 => {
                let inspiration = ui.get_generation_browse_tab() == 1;
                let names = ui.get_model_names();
                let index = usize::try_from(ui.get_model_index()).ok();
                let model_name = index
                    .and_then(|i| names.row_data(i))
                    .map(|s| text(s.as_str(), 256))
                    .transpose()?;
                let reference_rows = ui.get_references().iter().collect::<Vec<_>>();
                bounded_count(reference_rows.len())?;
                let versions = registry.reference_versions(
                    reference_rows
                        .iter()
                        .map(|row| {
                            (
                                row.id.to_string(),
                                preview(&row.preview, PreviewSource::Thumbnail)
                                    .map(|(_, image)| image),
                            )
                        })
                        .collect(),
                )?;
                let references=reference_rows.iter().zip(versions).map(|(row,version)|Ok(json!({"referenceId":text(row.id.as_str(),128)?,
                    "kind":text(row.kind.as_str(),32)?,"name":text(row.name.as_str(),256)?,"cachedViewVersion":version,
                    "fileContentVerified":false}))).collect::<Result<Vec<_>,Error>>()?;
                let state = access.state.upgrade().ok_or(Error::SignedOut)?;
                let cloud = state.try_borrow().map_err(|_| Error::Busy)?;
                let model_id =
                    super::model_id_at_index(&cloud.models, ui.get_mode(), ui.get_model_index());
                let view = json!({"mode":if ui.get_mode()==0{"image"}else{"video"},"generationStep":ui.get_generation_step(),
                    "browseTab":if inspiration{"inspiration"}else{"results"},"inspirationCategory":ui.get_inspiration_category(),
                    "prompt":form_text(ui.get_prompt().as_str(),32_768)?,"modelName":model_name,"modelId":model_id,
                    "parameters":{"indices":{"ratio":ui.get_ratio_index(),"resolution":ui.get_resolution_index(),"quality":ui.get_quality_index(),"duration":ui.get_duration_index(),"quantity":ui.get_quantity_index(),"effort":ui.get_generation_effort_index()},"ratio":choice(ui.get_ratios(),ui.get_ratio_index())?,"resolution":choice(ui.get_resolutions(),ui.get_resolution_index())?,
                        "quality":choice(ui.get_qualities(),ui.get_quality_index())?,"duration":choice(ui.get_durations(),ui.get_duration_index())?,
                        "quantity":choice(ui.get_quantities(),ui.get_quantity_index())?,"effort":choice(ui.get_generation_effort_names(),ui.get_generation_effort_index())?,
                        "generateAudio":ui.get_generate_audio()},"references":references});
                if inspiration {
                    let rows = ui.get_inspirations();
                    bounded_count(rows.row_count())?;
                    for row in rows.iter() {
                        let preview = preview(&row.preview, PreviewSource::Thumbnail);
                        push(
                            &mut items,
                            CachedItem {
                                source_key: format!("inspiration:{}", row.id),
                                kind: Kind::Inspiration,
                                name: row.name.to_string(),
                                status: String::new(),
                                selected: false,
                                availability: Availability::Unknown,
                                project: false,
                                detail: Some(Detail::Inspiration {
                                    category: row.category,
                                    purpose: text(row.purpose.as_str(), 256)?,
                                    parameter_summary: text(row.parameters.as_str(), 1024)?,
                                    prompt_preview: form_text(row.prompt.as_str(), 4096)?,
                                }),
                                preview,
                            },
                        )?;
                    }
                } else {
                    let batches = ui.get_batches();
                    bounded_count(batches.row_count())?;
                    for batch in batches.iter() {
                        bounded_count(batch.items.row_count())?;
                        for row in batch.items.iter() {
                            push(&mut items, media(row, format!("result:{}", batch.id))?)?;
                        }
                    }
                }
                (PageId::Generation, view)
            }
            5 => {
                let state = access.state.upgrade().ok_or(Error::SignedOut)?;
                let cloud = state.try_borrow().map_err(|_| Error::Busy)?;
                let folder_id = usize::try_from(ui.get_personal_folder_filter() - 2)
                    .ok()
                    .and_then(|index| cloud.personal_folders.get(index))
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .map(|id| text(id, 128))
                    .transpose()?;
                let view = json!({"search":text(ui.get_personal_search().as_str(),160)?,"kindFilter":ui.get_personal_filter(),
                    "sort":ui.get_personal_sort(),"sourceFilter":ui.get_personal_source(),"folderFilter":ui.get_personal_folder_filter(),
                    "folderId":folder_id,"trash":ui.get_personal_trash(),"favoritesOnly":ui.get_personal_favorites()});
                let rows = ui.get_personal_assets();
                bounded_count(rows.row_count())?;
                for row in rows.iter() {
                    push(&mut items, media(row, "personal".into())?)?;
                }
                (PageId::PersonalLibrary, view)
            }
            2 => {
                let state = access.state.upgrade().ok_or(Error::SignedOut)?;
                let cloud = state.try_borrow().map_err(|_| Error::Busy)?;
                let team_id = super::team_id(&ui, &cloud);
                let picker = ui.get_asset_picker_open() && ui.get_asset_picker_source() == 1;
                let search = if picker {
                    ui.get_asset_picker_team_search()
                } else {
                    ui.get_asset_search()
                };
                let filter = if picker {
                    ui.get_asset_picker_team_filter()
                } else {
                    ui.get_asset_filter()
                };
                let view = json!({"search":text(search.as_str(),160)?,"kindFilter":filter,"listPurpose":if picker{"picker"}else{"page"},
                    "teamName":text(ui.get_team_name().as_str(),256)?,"teamIndex":ui.get_team_index(),"teamId":text(&team_id,128)?,"trash":ui.get_trash_open()});
                let rows = ui.get_assets();
                bounded_count(rows.row_count())?;
                for row in rows.iter() {
                    push(&mut items, media(row, "team".into())?)?;
                }
                // No uploader_email, members, invite-input/url or raw server JSON.
                (PageId::TeamLibrary, view)
            }
            7 => {
                let rows = ui.get_templates();
                bounded_count(rows.row_count())?;
                for row in rows.iter() {
                    push(
                        &mut items,
                        CachedItem {
                            source_key: format!("template:{}", row.id),
                            kind: Kind::Template,
                            name: row.name.to_string(),
                            status: String::new(),
                            selected: ui.get_template_menu_id() == row.id,
                            availability: Availability::Unknown,
                            project: false,
                            preview: None,
                            detail: Some(Detail::Template {
                                mode_label: text(row.mode_label.as_str(), 256)?,
                                model_name: text(row.model_name.as_str(), 256)?,
                                parameter_summary: text(row.parameter_summary.as_str(), 1024)?,
                                reference_summary: text(row.reference_summary.as_str(), 1024)?,
                                updated_label: text(row.updated_label.as_str(), 256)?,
                                prompt_preview: form_text(row.prompt_preview.as_str(), 4096)?,
                            }),
                        },
                    )?;
                }
                // Template list has reference images, not a canonical result preview.
                (
                    PageId::Templates,
                    json!({"editorOpen":ui.get_template_editor_open(),"editorTarget":text(ui.get_template_id().as_str(),128)?,
                        "editorName":text(ui.get_template_name().as_str(),256)?,"editorPrompt":form_text(ui.get_template_prompt().as_str(),32_768)?,"editorMode":ui.get_template_mode()}),
                )
            }
            4 => (
                PageId::Account,
                json!({"signedIn":true,"reducedMotion":ui.get_reduced_motion()}),
            ),
            3 => {
                let view = json!({"availableCreditsLabel":text(ui.get_balance().as_str(),128)?,"heldCreditsLabel":text(ui.get_frozen().as_str(),128)?,
                    "paymentEnabled":ui.get_payment_enabled(),"historyTab":ui.get_account_history_tab(),"ledgerFilter":ui.get_ledger_filter_index(),
                    "historyPage":ui.get_history_page(),"historyLoading":ui.get_history_loading(),"hasPrevious":ui.get_history_previous(),"hasNext":ui.get_history_next(),"selectedEntryId":text(ui.get_account_expanded_entry().as_str(),128)?});
                if ui.get_payment_enabled() {
                    let rows = ui.get_plans();
                    bounded_count(rows.row_count())?;
                    for row in rows.iter() {
                        push(
                            &mut items,
                            CachedItem {
                                source_key: format!("plan:{}", row.id),
                                kind: Kind::CreditPlan,
                                name: row.name.to_string(),
                                status: String::new(),
                                selected: false,
                                availability: Availability::Unknown,
                                project: false,
                                preview: None,
                                detail: Some(Detail::Plan {
                                    price: text(row.price.as_str(), 128)?,
                                    credits: text(row.credits.as_str(), 128)?,
                                }),
                            },
                        )?;
                    }
                }
                let rows = if ui.get_account_history_tab() == 1 {
                    ui.get_orders()
                } else {
                    ui.get_ledger()
                };
                bounded_count(rows.row_count())?;
                for row in rows.iter() {
                    push(
                        &mut items,
                        CachedItem {
                            source_key: format!(
                                "history:{}:{}",
                                ui.get_account_history_tab(),
                                row.id
                            ),
                            kind: Kind::LedgerEntry,
                            name: row.title.to_string(),
                            status: String::new(),
                            selected: false,
                            availability: Availability::Unknown,
                            project: false,
                            preview: None,
                            detail: Some(Detail::Account {
                                value: text(row.value.as_str(), 128)?,
                                date_label: text(row.detail.as_str(), 256)?,
                            }),
                        },
                    )?;
                }
                (PageId::Credits, view)
            }
            0 if studio.on_start => {
                bounded_count(models.recents.row_count())?;
                for row in models.recents.iter() {
                    push(
                        &mut items,
                        CachedItem {
                            source_key: format!("clip-project:{}", row.path),
                            kind: Kind::Project,
                            name: row.name.to_string(),
                            status: String::new(),
                            selected: false,
                            availability: Availability::Unknown,
                            project: true,
                            detail: Some(Detail::Project {
                                project_kind: "clip",
                                updated_label: text(row.when.as_str(), 256)?,
                            }),
                            preview: preview(&row.poster, PreviewSource::Thumbnail),
                        },
                    )?;
                }
                (
                    PageId::Projects,
                    json!({"projectKind":"clip","search":text(ui.get_clip_project_search().as_str(),160)?,
                    "sort":ui.get_clip_project_sort(),"layout":ui.get_clip_project_view()}),
                )
            }
            6 if ui.get_canvas_gallery_open() => {
                let rows = ui.get_canvas_projects();
                bounded_count(rows.row_count())?;
                for row in rows.iter() {
                    let mut item = media(row, "canvas-project".into())?;
                    item.kind = Kind::Project;
                    item.project = true;
                    item.preview = item
                        .preview
                        .map(|(_, image)| (PreviewSource::Thumbnail, image));
                    item.detail = Some(Detail::Project {
                        project_kind: "canvas",
                        updated_label: String::new(),
                    });
                    push(&mut items, item)?;
                }
                (
                    PageId::Projects,
                    json!({"projectKind":"canvas","search":text(ui.get_canvas_project_search().as_str(),160)?,
                    "sort":ui.get_canvas_project_sort(),"layout":ui.get_canvas_project_view()}),
                )
            }
            _ => return Err(Error::UnsupportedPage),
        }
    };
    view["overlays"] = json!({"authentication":ui.get_auth_open(),"assetPicker":ui.get_asset_picker_open(),"mediaPreview":ui.get_media_preview_open(),
        "templateEditor":ui.get_template_editor_open(),"handoff":ui.get_handoff_open()});
    view["visibility"] = json!({"status":"unknown"});
    let current_preview_source_key = if ui.get_media_preview_open() {
        let source = ui.get_media_preview_source();
        if !matches!(source.as_str(), "personal" | "asset" | "task" | "reference") {
            return Err(Error::UnsupportedPage);
        }
        let row = ui.get_media_preview_item();
        let mut cached = media(row, format!("open-preview:{}", source))?;
        let source = if cached.kind == Kind::Image {
            PreviewSource::Image
        } else {
            PreviewSource::Thumbnail
        };
        cached.preview = cached.preview.map(|(_, image)| (source, image));
        registry.set_auxiliary_preview(Some(cached))?
    } else if ui.get_inspiration_preview_open() {
        let row = ui.get_inspiration_selected();
        registry.set_auxiliary_preview(Some(CachedItem {
            source_key: format!("open-inspiration:{}", row.id),
            kind: Kind::Inspiration,
            name: row.name.to_string(),
            status: String::new(),
            selected: true,
            availability: Availability::Unknown,
            project: false,
            detail: None,
            preview: preview(&row.preview, PreviewSource::Thumbnail),
        }))?
    } else {
        registry.set_auxiliary_preview(None)?;
        if ui.get_result_preview_open() {
            usize::try_from(ui.get_selected_task())
                .ok()
                .and_then(|index| ui.get_tasks().row_data(index))
                .and_then(|selected| {
                    items
                        .iter()
                        .find(|item| {
                            item.source_key.starts_with("result:")
                                && item.source_key.ends_with(&format!(":{}", selected.id))
                        })
                        .map(|item| item.source_key.clone())
                })
        } else {
            None
        }
    };
    if registry.navigation_stamp()? != navigation_start {
        return Err(Error::StalePage);
    }
    registry.capture_with_target(
        page,
        view,
        items.rows,
        current_preview_source_key.as_deref(),
    )
}

fn choice(
    values: slint::ModelRc<slint::SharedString>,
    index: i32,
) -> Result<Option<String>, Error> {
    usize::try_from(index)
        .ok()
        .and_then(|index| values.row_data(index))
        .map(|value| text(value.as_str(), 256))
        .transpose()
}

fn bounded_count(count: usize) -> Result<(), Error> {
    if count > 2048 {
        Err(Error::ResourceLimit)
    } else {
        Ok(())
    }
}
#[derive(Default)]
struct ItemBuilder {
    rows: Vec<CachedItem<slint::Image>>,
    source_name_bytes: usize,
}
impl ItemBuilder {
    fn iter(&self) -> impl Iterator<Item = &CachedItem<slint::Image>> {
        self.rows.iter()
    }
}
fn push(items: &mut ItemBuilder, item: CachedItem<slint::Image>) -> Result<(), Error> {
    if items.rows.len() >= 2048 || item.name.len() > 32_768 {
        return Err(Error::ResourceLimit);
    }
    items.source_name_bytes = items
        .source_name_bytes
        .checked_add(item.name.len())
        .filter(|n| *n <= 512 * 1024)
        .ok_or(Error::ResourceLimit)?;
    items.rows.push(item);
    Ok(())
}
fn preview(image: &slint::Image, source: PreviewSource) -> Option<(PreviewSource, slint::Image)> {
    let size = image.size();
    (size.width > 0 && size.height > 0).then(|| (source, image.clone()))
}
fn media(row: CloudItem, namespace: String) -> Result<CachedItem<slint::Image>, Error> {
    let kind = match row.kind.as_str() {
        "image" => Kind::Image,
        "video" => Kind::Video,
        "audio" => Kind::Audio,
        _ => Kind::Unknown,
    };
    // CloudItem.detail may hold uploader email, source path, or arbitrary error.
    // Only the dedicated bounded status is displayed; no source file is opened.
    let source = if kind == Kind::Image && (namespace.starts_with("result:") || namespace == "team")
    {
        PreviewSource::Image
    } else {
        PreviewSource::Thumbnail
    };
    Ok(CachedItem {
        source_key: format!("{namespace}:{}", row.id),
        kind,
        name: row.name.to_string(),
        status: if row.failed {
            "failed"
        } else if row.missing {
            "missing"
        } else if row.ready {
            "ready"
        } else {
            "pending"
        }
        .into(),
        selected: row.selected,
        availability: if row.missing {
            Availability::Missing
        } else if row.ready {
            Availability::Available
        } else {
            Availability::Unknown
        },
        project: false,
        detail: Some(Detail::Media {
            favorite: row.favorite,
            failed: row.failed,
            refillable: row.refillable,
            date_label: text(row.date_label.as_str(), 256)?,
        }),
        preview: preview(&row.preview, source),
    })
}

// Preserve actual form newlines/tabs in ToolResult and its view hash. Labels use
// text() control normalization; form identity must not collapse newline to space.
fn form_text(source: &str, max_chars: usize) -> Result<String, Error> {
    if source.chars().count() > max_chars {
        return Err(Error::ResourceLimit);
    }
    Ok(source.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trusted_account_url_matches_private_normalization_vector() {
        assert_eq!(
            canonical_account_scope("HTTPS://EXAMPLE.COM:443/", "uid").unwrap(),
            canonical_account_scope("https://example.com", "uid").unwrap()
        );
        assert!(canonical_account_scope("https://user:secret@example.com", "uid").is_err());
        assert!(canonical_account_scope("https://example.com", "").is_err());
    }
}
