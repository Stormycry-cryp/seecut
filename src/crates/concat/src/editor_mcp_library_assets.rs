// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path-free personal-library metadata queries and final original-authority checks.
use super::*;
use crate::personal_library::{Asset, AssetKind, Library, ManagedVersionMetadataError};
use canvas_policy::{OutputOriginCapture, OwnedOutput};
use concat_editor_mcp::{AssetVersion, LibraryAssetKind, LibraryAssetsQuery};
use std::sync::Mutex;

pub(super) const MAX_LIBRARY_MANIFEST: usize = 16 * 1024 * 1024;
pub(super) const MAX_LIBRARY_ASSETS: usize = 10_000;
const MAX_LIBRARY_RESPONSE: usize = 48 * 1024;

/// Original named approval or original successful export; never model authority.
pub(super) enum LibraryAuthority {
    Exact(Box<Permit>),
    OwnedOutput(OwnedOutput),
}

/// Capture account/conversation provenance on the UI thread, without exposing it.
pub(super) fn output_origin(bridge: &BridgeUi) -> OutputOriginCapture {
    OutputOriginCapture {
        account_scope: crate::cloud::canvas_output_scope(),
        assistant_session: bridge.assistant_session.clone(),
    }
}

/// Use only after typed request validation and current scope/media authorization.
/// The caller checks this same original proof again after metadata/content IO.
pub(super) fn claim_library_authority(
    bridge: &mut BridgeUi,
    request: &Request,
    scope: &Scope,
    intent: &Intent,
) -> Option<LibraryAuthority> {
    if let Some(permit) =
        bridge
            .canvas_permits
            .claim(scope, &request.client_id, intent, Instant::now())
    {
        return Some(LibraryAuthority::Exact(Box::new(permit)));
    }
    if request.method != Method::LibraryImages {
        return None;
    }
    let query = request.library_query.as_ref()?;
    if !canvas_policy::owned_output_query(query) {
        return None;
    }
    let proof = bridge.canvas_receipts.owned_canvas_output(
        scope,
        &request.client_id,
        query.asset_id.as_deref()?,
        &output_origin(bridge),
        Instant::now(),
    )?;
    Some(LibraryAuthority::OwnedOutput(proof))
}

/// Recheck current account, document, media grant and the original authority.
/// Called on the final serialized UI callback, including for read-only queries.
pub(super) fn library_job_current(job: &LibraryJob, studio: &Studio, app: &App) -> bool {
    let Ok(mut bridge) = studio.editor_mcp.try_borrow_mut() else {
        return false;
    };
    let Some((kind, ids)) = bridge.active_ids(studio, app) else {
        return false;
    };
    if request_scope(&job.request).as_ref() != Some(&bridge.scope(&ids))
        || !bridge.authorized(&job.request, &ids.project, true)
        || output_origin(&bridge) != job.origin
        || (job.request.method == Method::LibraryImages && kind != "canvas")
        || !matches!(kind.as_str(), "canvas" | "clip" | "flow")
    {
        return false;
    }
    match &job.authority {
        LibraryAuthority::Exact(permit) => bridge.canvas_permits.valid(permit, Instant::now()),
        LibraryAuthority::OwnedOutput(proof) => {
            job.request.method == Method::LibraryImages
                && bridge.canvas_receipts.owned_output_valid(
                    &bridge.scope(&ids),
                    &job.request.client_id,
                    proof,
                    &job.origin,
                    Instant::now(),
                )
        }
    }
}

/// Project only display text; stable asset identities and source names stay intact.
pub(super) fn display_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(256).collect()
}

pub(super) fn fixed_version(asset: &Asset) -> Option<AssetVersion> {
    let version: AssetVersion =
        serde_json::from_value(serde_json::to_value(asset.version.as_ref()?).ok()?).ok()?;
    concat_editor_mcp::validate_library_asset(&asset.id, &version).ok()?;
    Some(version)
}

fn matches_kind(filter: LibraryAssetKind, kind: AssetKind) -> bool {
    matches!(filter, LibraryAssetKind::All)
        || matches!(
            (filter, kind),
            (LibraryAssetKind::Image, AssetKind::Image)
                | (LibraryAssetKind::Video, AssetKind::Video)
                | (LibraryAssetKind::Audio, AssetKind::Audio)
        )
}

/// Metadata only: at most one bounded manifest and metadata for returned rows.
/// No digest, migration, probe, content read or library write happens here.
fn metadata_page(root: &std::path::Path, query: &LibraryAssetsQuery) -> Result<Value, String> {
    query.validate()?;
    let library = Library::load_for_relink_bounded(root, MAX_LIBRARY_MANIFEST, MAX_LIBRARY_ASSETS)?;
    let search = query.query.to_lowercase();
    let mut total = 0usize;
    let mut rows = Vec::new();
    let mut row_bytes = 0usize;
    let mut page_full = false;
    for asset in library.items() {
        if asset.trashed
            || !matches_kind(query.kind, asset.kind)
            || query.asset_id.as_ref().is_some_and(|id| id != &asset.id)
            || !asset.name.to_lowercase().contains(&search)
        {
            continue;
        }
        let Some(version) = fixed_version(asset) else {
            continue;
        };
        let index = total;
        total = total
            .checked_add(1)
            .filter(|n| *n <= MAX_LIBRARY_ASSETS)
            .ok_or("resourceLimit")?;
        if index < query.offset || page_full || rows.len() >= query.limit {
            continue;
        }
        let availability =
            match crate::personal_library::checked_managed_version_metadata(root, asset) {
                Ok(_) => "available",
                Err(ManagedVersionMetadataError::Missing) => "missing",
                Err(ManagedVersionMetadataError::Unavailable) => "unavailable",
            };
        let row = json!({"assetId":asset.id,"name":display_name(&asset.name),"kind":asset.kind,
            "version":version,"availability":availability,"contentVerified":false});
        let size = serde_json::to_vec(&row).map_err(|_| "ioFailure")?.len();
        if row_bytes.saturating_add(size) > MAX_LIBRARY_RESPONSE {
            if rows.is_empty() {
                return Err("resourceLimit".into());
            }
            page_full = true;
            continue;
        }
        row_bytes += size;
        rows.push(row);
    }
    Ok(json!({"offset":query.offset,"total":total,
        "nextOffset":(query.offset.saturating_add(rows.len()) < total).then_some(query.offset + rows.len()),"assets":rows}))
}

/// Run off the UI thread, then recheck the original authority before delivery.
pub(super) fn dispatch_library_assets(job: LibraryJob) -> Response {
    static QUERY_SLOT: Mutex<()> = Mutex::new(());
    let Ok(_slot) = QUERY_SLOT.try_lock() else {
        return Response::error("resourceLimit");
    };
    if !matches!(&job.authority, LibraryAuthority::Exact(_)) {
        return Response::error("notAuthorized");
    }
    let Some(query) = job.request.library_assets_query.as_ref() else {
        return Response::error("invalidInput");
    };
    let result = concat_host::AppDirs::locate()
        .and_then(|dirs| metadata_page(&dirs.data.join("personal-library"), query));
    let project = job.request.project_id.clone();
    let document = job.request.document_session_id.clone();
    let stable = on_ui(move |shell, app| {
        let Ok(studio) = shell.studio.try_borrow() else {
            return false;
        };
        library_job_current(&job, &studio, &app)
    });
    if !matches!(stable, Ok(true)) {
        return Response::error("notAuthorized");
    }
    match result {
        Ok(mut value) => {
            value["projectId"] = json!(project);
            value["documentSessionId"] = json!(document);
            Response::ok(value)
        }
        Err(error) => {
            log::warn!("personal library metadata query: {error}");
            Response::error(if error == "resourceLimit" {
                "resourceLimit"
            } else {
                "libraryUnavailable"
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_projection_removes_controls_before_unicode_character_limit() {
        assert_eq!(display_name("a\nb\rc\td\0e\u{0085}f"), "abcdef");
        let source = format!("\n{}\tlast", "图".repeat(256));
        let value = display_name(&source);
        assert_eq!(value.chars().count(), 256);
        assert!(!value.chars().any(char::is_control));
        assert_eq!(value, "图".repeat(256));
    }
    #[test]
    fn categories_map_without_guessing_the_filename_or_mime() {
        for kind in [AssetKind::Image, AssetKind::Video, AssetKind::Audio] {
            assert!(matches_kind(LibraryAssetKind::All, kind));
        }
        assert!(matches_kind(LibraryAssetKind::Image, AssetKind::Image));
        assert!(!matches_kind(LibraryAssetKind::Image, AssetKind::Video));
        assert!(!matches_kind(LibraryAssetKind::Audio, AssetKind::Image));
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use crate::personal_library::{AssetSource, AssetVersion as NativeVersion};
    use std::{fs, path::PathBuf};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("seecut-library-metadata-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            let path = path.canonicalize().unwrap();
            fs::create_dir(path.join("imported")).unwrap();
            Self(path)
        }
        fn asset(&self, index: usize, kind: AssetKind, name: &str) -> Asset {
            let path = self.0.join("imported").join(format!("asset-{index}.bin"));
            fs::write(&path, b"deliberately not the claimed content hash").unwrap();
            Asset {
                id: format!("asset-{index}"),
                name: name.into(),
                path,
                kind,
                source: AssetSource::Imported,
                version: Some(NativeVersion {
                    sha256: "a".repeat(64),
                    registration_id: format!("registration-{index}"),
                    previous_id: None,
                }),
                original_path: None,
                recovery: None,
                provenance: None,
                created_at: 0,
                trashed: false,
                favorite: false,
                folder_id: None,
            }
        }
        fn save(&self, assets: &[Asset]) {
            fs::write(
                self.0.join(crate::personal_library::MANIFEST_NAME),
                serde_json::to_vec(&json!({"items":assets,"folders":[]})).unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn query() -> LibraryAssetsQuery {
        serde_json::from_value(json!({})).unwrap()
    }

    #[test]
    fn combined_categories_and_kind_filters_use_one_fixed_version_metadata_page() {
        let root = Scratch::new();
        root.save(&[
            root.asset(1, AssetKind::Image, "image"),
            root.asset(2, AssetKind::Video, "video"),
            root.asset(3, AssetKind::Audio, "audio"),
        ]);
        let original = fs::read(root.0.join(crate::personal_library::MANIFEST_NAME)).unwrap();
        let all = metadata_page(&root.0, &query()).unwrap();
        assert_eq!(all["total"], 3);
        assert_eq!(all["assets"].as_array().unwrap().len(), 3);
        assert!(all["nextOffset"].is_null());
        for (filter, kind) in [
            (LibraryAssetKind::Image, "image"),
            (LibraryAssetKind::Video, "video"),
            (LibraryAssetKind::Audio, "audio"),
        ] {
            let filtered = metadata_page(
                &root.0,
                &LibraryAssetsQuery {
                    kind: filter,
                    ..query()
                },
            )
            .unwrap();
            assert_eq!(filtered["total"], 1);
            assert_eq!(filtered["assets"][0]["kind"], kind);
        }
        assert_eq!(
            fs::read(root.0.join(crate::personal_library::MANIFEST_NAME)).unwrap(),
            original
        );
    }
    #[test]
    fn fifty_video_rows_do_not_digest_or_probe_asset_contents() {
        let root = Scratch::new();
        let assets = (0..50)
            .map(|i| root.asset(i, AssetKind::Video, "video"))
            .collect::<Vec<_>>();
        // A content verifier rejects every file, while metadata-only availability
        // remains available and explicitly unverified for the entire bounded page.
        assert!(
            crate::personal_library::checked_managed_version_path(&root.0, &assets[0]).is_err()
        );
        root.save(&assets);
        let value = metadata_page(
            &root.0,
            &LibraryAssetsQuery {
                limit: 50,
                ..query()
            },
        )
        .unwrap();
        let rows = value["assets"].as_array().unwrap();
        assert_eq!(rows.len(), 50);
        for row in rows {
            assert_eq!(row["availability"], "available");
            assert_eq!(row["contentVerified"], false);
            assert!(row.get("path").is_none());
            assert!(row.get("extra").is_none());
        }
    }
    #[test]
    fn source_name_search_and_controlled_display_name_have_distinct_roles() {
        let root = Scratch::new();
        root.save(&[root.asset(1, AssetKind::Video, "raw\nname\t")]);
        let value = metadata_page(
            &root.0,
            &LibraryAssetsQuery {
                query: "raw".into(),
                ..query()
            },
        )
        .unwrap();
        assert_eq!(value["assets"][0]["name"], "rawname");
        assert_eq!(
            metadata_page(
                &root.0,
                &LibraryAssetsQuery {
                    query: "rawname".into(),
                    ..query()
                }
            )
            .unwrap()["total"],
            0
        );
    }
    #[test]
    fn response_budget_preserves_contiguous_offset_without_skipping_rows() {
        let root = Scratch::new();
        let mut assets = (0..50)
            .map(|i| root.asset(i, AssetKind::Video, &"图".repeat(256)))
            .collect::<Vec<_>>();
        for asset in &mut assets {
            asset.version.as_mut().unwrap().registration_id = "r".repeat(512);
        }
        root.save(&assets);
        let first = metadata_page(
            &root.0,
            &LibraryAssetsQuery {
                limit: 50,
                ..query()
            },
        )
        .unwrap();
        let count = first["assets"].as_array().unwrap().len();
        assert!(count > 0 && count < 50);
        assert_eq!(first["nextOffset"], count);
        let second = metadata_page(
            &root.0,
            &LibraryAssetsQuery {
                offset: count,
                limit: 50,
                ..query()
            },
        )
        .unwrap();
        assert_eq!(second["assets"][0]["assetId"], format!("asset-{count}"));
        assert_eq!(count + second["assets"].as_array().unwrap().len(), 50);
    }
    #[test]
    fn missing_files_and_unsafe_paths_never_claim_available_or_verified_content() {
        let root = Scratch::new();
        let one = root.asset(1, AssetKind::Image, "missing");
        let mut two = root.asset(2, AssetKind::Video, "outside");
        fs::remove_file(&one.path).unwrap();
        two.path = root.0.join("not-managed.bin");
        fs::write(&two.path, b"outside managed folders").unwrap();
        root.save(&[one, two]);
        let value = metadata_page(&root.0, &query()).unwrap();
        assert_eq!(value["assets"][0]["availability"], "missing");
        assert_eq!(value["assets"][1]["availability"], "unavailable");
    }
}
