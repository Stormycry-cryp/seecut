use super::*;
use crate::personal_library::{
    Asset, AssetKind, AssetVersion, Library, checked_managed_version_path, with_library_write_lock,
};

const MAX_MEDIA: u64 = 100 * 1024 * 1024;

#[derive(PartialEq, Eq)]
struct Session {
    key: String,
    epoch: Option<u64>,
}
fn session() -> Result<Session> {
    let (sender, receiver) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        ACCOUNT.with(|slot| {
            let account = slot.borrow();
            let account = account.as_ref();
            let key = account
                .and_then(AccountAccess::image_client)
                .map(|client| digest(&client.scope))
                .unwrap_or_else(|| "local".into());
            let _ = sender.send(Session {
                key,
                epoch: account.and_then(AccountAccess::epoch),
            });
        });
    })
    .map_err(|_| RpcError::new("APP_UNAVAILABLE", "原生窗口暂不可用。"))?;
    receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| RpcError::new("APP_BUSY", "原生窗口正在处理操作，请稍后重试。"))
}

pub(super) fn dispatch(state: &State, method: &str, params: &Value) -> Result<Value> {
    dispatch_with_session(state, method, params, session)
}
fn dispatch_with_session(
    state: &State,
    method: &str,
    params: &Value,
    mut current: impl FnMut() -> Result<Session>,
) -> Result<Value> {
    let initial = current()?;
    check_expected_scope(method, params, &initial.key)?;
    let result = perform(state, method, params, &initial.key);
    if current()? != initial {
        return Err(RpcError::new(
            "FLOW_ACCOUNT_CHANGED",
            "账户会话已变化，请刷新后重新操作。",
        ));
    }
    let mut result = result?;
    result["scopeKey"] = json!(initial.key);
    Ok(result)
}

fn perform(state: &State, method: &str, params: &Value, scope: &str) -> Result<Value> {
    match method {
        "assets.list" => list(state, params),
        "assets.resolve" | "assets.thumbnail" => {
            let asset = resolve(state, argument(params, "assetId")?, &version(params)?)?;
            let mut value = entry(&asset, &state.library);
            if method == "assets.thumbnail" {
                let path = crate::personal_library::thumbnail(&asset, &state.library)
                    .map(|path| checked_thumbnail(state, &path))
                    .transpose()?;
                value["available"] = json!(path.is_some());
                value["path"] = json!(path);
                value["contentType"] = json!("image/jpeg");
            } else {
                validate_media(&asset.path, asset.kind)?;
                value["path"] = json!(asset.path);
            }
            Ok(value)
        }
        "assets.copyToProject" => {
            let project = argument(params, "projectId")?;
            let _guard = state.project_guard(scope, project)?;
            copy_to_project(state, project, params)
        }
        "assets.registerProjectMedia" => {
            let project = argument(params, "projectId")?;
            let _guard = state.project_guard(scope, project)?;
            register_project_media(state, project, params)
        }
        _ => Err(RpcError::new(
            "METHOD_NOT_FOUND",
            "原生桥不支持此资产操作。",
        )),
    }
}
fn checked_thumbnail(state: &State, path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| RpcError::new("INVALID_PATH", "缩略图路径无效。"))?;
    let directory = fs::symlink_metadata(parent).map_err(RpcError::io)?;
    let metadata = fs::symlink_metadata(path).map_err(RpcError::io)?;
    let canonical = path.canonicalize().map_err(RpcError::io)?;
    let expected = state
        .library
        .canonicalize()
        .map_err(RpcError::io)?
        .join("thumbnails");
    if !metadata.is_file()
        || !directory.is_dir()
        || directory.file_type().is_symlink()
        || canonical.parent() != Some(expected.as_path())
    {
        return Err(RpcError::new(
            "INVALID_PATH",
            "缩略图必须是个人库缓存目录内的普通文件。",
        ));
    }
    Ok(canonical)
}
fn version(params: &Value) -> Result<AssetVersion> {
    let raw = params
        .get("version")
        .ok_or_else(|| RpcError::new("INVALID_VERSION", "请选择素材的固定版本。"))?;
    let value = if let Some(encoded) = raw.as_str() {
        serde_json::from_str(encoded)
    } else {
        serde_json::from_value(raw.clone())
    };
    value.map_err(|_| RpcError::new("INVALID_VERSION", "素材版本无效，请刷新后重试。"))
}
fn resolve(state: &State, id: &str, version: &AssetVersion) -> Result<Asset> {
    with_library_write_lock(|| {
        let library = Library::load(&state.library)?;
        let asset = library
            .get(id)
            .filter(|asset| !asset.trashed)
            .ok_or_else(|| RpcError::new("ASSET_NOT_FOUND", "库内素材不可用，请刷新后重试。"))?;
        if asset.version.as_ref() != Some(version) {
            return Err(RpcError::new(
                "ASSET_VERSION_CONFLICT",
                "库内素材版本已变化，请重新选择。",
            ));
        }
        checked_managed_version_path(&state.library, asset)?;
        Ok(asset.clone())
    })
}
fn type_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Image => "image",
        AssetKind::Video => "video",
        AssetKind::Audio => "audio",
    }
}
fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mpeg" | "mpg" => "video/mpeg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "aiff" | "aif" => "audio/aiff",
        _ => "application/octet-stream",
    }
}
fn entry(asset: &Asset, _root: &Path) -> Value {
    let metadata = fs::symlink_metadata(&asset.path)
        .ok()
        .filter(|m| m.is_file());
    json!({
        "assetId":asset.id,"name":asset.name,"type":type_name(asset.kind),
        "version":asset.version,"createdAt":asset.created_at,"source":asset.source,
        "available":metadata.is_some(),"size":metadata.as_ref().map(|m|m.len()).unwrap_or(0),
        "mimeType":mime(&asset.path),
        "thumbnailAvailable":metadata.is_some() && asset.kind != AssetKind::Audio
    })
}
fn number(params: &Value, name: &str, default: u64) -> Result<u64> {
    match params.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| RpcError::new("INVALID_ARGUMENT", format!("{name} 必须是非负整数。"))),
    }
}
fn list(state: &State, params: &Value) -> Result<Value> {
    let offset = number(params, "offset", 0)?;
    let limit = number(params, "limit", 24)?;
    if !(1..=50).contains(&limit) || offset > usize::MAX as u64 {
        return Err(RpcError::new("INVALID_ARGUMENT", "分页数量须在1到50之间。"));
    }
    let query = match params.get("q") {
        None => "",
        Some(value) => value
            .as_str()
            .filter(|q| q.len() <= 512)
            .ok_or_else(|| RpcError::new("INVALID_ARGUMENT", "搜索文字过长或格式无效。"))?,
    }
    .trim()
    .to_lowercase();
    let kind = match params.get("type") {
        None => None,
        Some(Value::String(value)) if value.is_empty() || value == "all" => None,
        Some(Value::String(value)) if matches!(value.as_str(), "image" | "video" | "audio") => {
            Some(value.as_str())
        }
        _ => return Err(RpcError::new("INVALID_ARGUMENT", "素材类型无效。")),
    };
    with_library_write_lock(|| {
        let library = Library::load(&state.library)?;
        let mut assets = library
            .items()
            .iter()
            .filter(|asset| {
                !asset.trashed
                    && kind.is_none_or(|kind| type_name(asset.kind) == kind)
                    && (query.is_empty() || asset.name.to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();
        assets.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        let total = assets.len();
        let items = assets
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|asset| entry(asset, &state.library))
            .collect::<Vec<_>>();
        let next = offset.saturating_add(items.len() as u64);
        Ok(
            json!({"items":items,"total":total,"offset":offset,"limit":limit,"nextOffset":(next < total as u64).then_some(next)}),
        )
    })
}
fn validate_media(path: &Path, kind: AssetKind) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(RpcError::io)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_MEDIA {
        return Err(RpcError::new(
            "INVALID_MEDIA",
            "素材必须是100MB以内的普通媒体文件。",
        ));
    }
    let info = concat_media::probe(path)
        .map_err(|_| RpcError::new("INVALID_MEDIA", "无法读取素材内容，请检查文件格式。"))?;
    let valid = match kind {
        AssetKind::Audio => info.audio.is_some() && info.video.is_none(),
        AssetKind::Video => info.video.as_ref().is_some_and(|video| {
            !matches!(
                video.codec.as_str(),
                "png" | "webp" | "gif" | "bmp" | "tiff"
            )
        }),
        AssetKind::Image => info.video.as_ref().is_some_and(|video| {
            matches!(
                (mime(path), video.codec.as_str()),
                ("image/png", "png")
                    | ("image/jpeg", "mjpeg")
                    | ("image/webp", "webp")
                    | ("image/gif", "gif")
                    | ("image/bmp", "bmp")
                    | ("image/tiff", "tiff")
                    | ("image/heic", "hevc")
                    | ("image/avif", "av1")
            )
        }),
    };
    if !valid {
        return Err(RpcError::new("INVALID_MEDIA", "素材内容与声明类型不一致。"));
    }
    Ok(())
}
fn directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else {
            return Err(RpcError::new("INVALID_PATH", "素材目录无效。"));
        };
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(RpcError::new(
                    "INVALID_PATH",
                    "素材目录不能使用链接或普通文件。",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(RpcError::io)?
            }
            Err(error) => return Err(RpcError::io(error)),
        }
        if path.canonicalize().map_err(RpcError::io)? != path {
            return Err(RpcError::new("INVALID_PATH", "素材目录已变化。"));
        }
    }
    Ok(path)
}
fn project_media(state: &State, project: &str, raw: &str) -> Result<PathBuf> {
    let relative = Path::new(raw);
    if relative.is_absolute()
        || !matches!(relative.components().next(),Some(Component::Normal(name)) if name == "assets")
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(RpcError::new(
            "INVALID_PATH",
            "请选择项目assets目录内的素材。",
        ));
    }
    let root = state.project(project)?;
    let mut current = root.clone();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(RpcError::io)?;
        if metadata.file_type().is_symlink() {
            return Err(RpcError::new("INVALID_PATH", "项目素材不能使用符号链接。"));
        }
    }
    let path = state.project_file(project, raw, false)?;
    if path != root.join(relative) {
        return Err(RpcError::new("INVALID_PATH", "项目素材路径已变化。"));
    }
    Ok(path)
}
#[derive(Serialize, Deserialize)]
struct Source {
    path: String,
    #[serde(rename = "assetId")]
    asset_id: String,
    version: AssetVersion,
    sha256: String,
    #[serde(rename = "type")]
    kind: AssetKind,
}
fn source_path(root: &Path, relative: &str) -> PathBuf {
    root.join(".seecut/assets")
        .join(format!("{:x}.json", Sha256::digest(relative.as_bytes())))
}
fn save_source(state: &State, project: &str, relative: &str, asset: &Asset) -> Result<()> {
    let root = state.project(project)?;
    directory(&root, ".seecut/assets")?;
    let version = asset.version.clone().ok_or("素材没有固定版本")?;
    atomic_json(
        &source_path(&root, relative),
        &Source {
            path: relative.into(),
            asset_id: asset.id.clone(),
            sha256: version.sha256.clone(),
            version,
            kind: asset.kind,
        },
    )
}
fn copy_to_project(state: &State, project: &str, params: &Value) -> Result<Value> {
    let version = version(params)?;
    let asset = resolve(state, argument(params, "assetId")?, &version)?;
    validate_media(&asset.path, asset.kind)?;
    let root = state.project(project)?;
    let target = directory(&root, "assets/imported")?;
    let extension = asset
        .path
        .extension()
        .and_then(|v| v.to_str())
        .ok_or("素材扩展名无效")?
        .to_ascii_lowercase();
    if !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(RpcError::new("INVALID_MEDIA", "素材扩展名无效。"));
    }
    let filename = format!("{}.{}", digest(&(&asset.id, &version)), extension);
    let relative = format!("assets/imported/{filename}");
    let destination = target.join(filename);
    let deduplicated = destination.exists();
    if deduplicated {
        let path = project_media(state, project, &relative)?;
        if file_sha(&path)? != version.sha256 {
            return Err(RpcError::new(
                "ASSET_VERSION_CONFLICT",
                "项目已有素材内容已变化，未覆盖文件。",
            ));
        }
    } else {
        let mut temporary = tempfile::NamedTempFile::new_in(&target).map_err(RpcError::io)?;
        let mut input = File::open(&asset.path)
            .map_err(RpcError::io)?
            .take(MAX_MEDIA + 1);
        let copied = std::io::copy(&mut input, temporary.as_file_mut()).map_err(RpcError::io)?;
        if copied > MAX_MEDIA || file_sha(temporary.path())? != version.sha256 {
            return Err(RpcError::new(
                "ASSET_VERSION_CONFLICT",
                "素材复制期间内容已变化，请重试。",
            ));
        }
        temporary.as_file().sync_all().map_err(RpcError::io)?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|e| RpcError::io(e.error))?;
    }
    save_source(state, project, &relative, &asset)?;
    Ok(
        json!({"path":relative,"mimeType":mime(&asset.path),"type":type_name(asset.kind),"name":asset.name,
        "assetId":asset.id,"version":version,"sha256":version.sha256,"deduplicated":deduplicated}),
    )
}
fn referenced_asset(state: &State, project: &str, file: &Path) -> Result<Option<Asset>> {
    let root = state.project(project)?;
    let relative = file
        .strip_prefix(&root)
        .map_err(|_| RpcError::new("INVALID_PATH", "素材不属于当前项目。"))?
        .components()
        .map(|component| component.as_os_str().to_str().ok_or("素材路径无效"))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("/");
    let path = source_path(&root, &relative);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RpcError::io(error)),
        Ok(metadata) if !metadata.is_file() || metadata.len() > 16 * 1024 => {
            return Err(RpcError::new("INVALID_SOURCE", "素材来源记录无效。"));
        }
        Ok(_) => {}
    }
    let checked = bounded_path(&root, path.to_str().ok_or("素材来源路径无效")?, false)?;
    if checked != path {
        return Err(RpcError::new(
            "INVALID_SOURCE",
            "素材来源不能使用符号链接。",
        ));
    }
    let source: Source = serde_json::from_reader(File::open(&path).map_err(RpcError::io)?)
        .map_err(|_| RpcError::new("INVALID_SOURCE", "素材来源记录已损坏。"))?;
    if source.path != relative
        || source.sha256 != source.version.sha256
        || file_sha(file)? != source.sha256
    {
        return Err(RpcError::new(
            "ASSET_VERSION_CONFLICT",
            "项目素材与原固定版本不一致，请重新选择。",
        ));
    }
    validate_media(file, source.kind)?;
    with_library_write_lock(|| {
        let library = Library::load(&state.library)?;
        let Some(asset) = library.get(&source.asset_id) else {
            return Ok(None);
        };
        if asset.trashed
            || asset.version.as_ref() != Some(&source.version)
            || asset.kind != source.kind
        {
            return Ok(None);
        }
        match fs::symlink_metadata(&asset.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(RpcError::io(error)),
            Ok(_) => {}
        }
        checked_managed_version_path(&state.library, asset)?;
        Ok(Some(asset.clone()))
    })
}
pub(super) fn reference_asset(state: &State, project: &str, file: &Path) -> Result<Asset> {
    if let Some(asset) = referenced_asset(state, project, file)? {
        return Ok(asset);
    }
    with_library_write_lock(|| {
        let mut library = Library::load(&state.library)?;
        library.import(file).cloned().map_err(Into::into)
    })
}
fn register_project_media(state: &State, project: &str, params: &Value) -> Result<Value> {
    if let Some(name) = params.get("name") {
        let name = name
            .as_str()
            .ok_or_else(|| RpcError::new("INVALID_ARGUMENT", "素材名称格式无效。"))?;
        if name.trim().is_empty()
            || name.trim() != name
            || name.len() > 1024
            || name.contains('/')
            || name.contains('\\')
            || name.chars().any(char::is_control)
        {
            return Err(RpcError::new(
                "INVALID_ARGUMENT",
                "素材名称不能包含路径、首尾空格或控制字符。",
            ));
        }
    }
    let raw = argument(params, "path")?;
    let file = project_media(state, project, raw)?;
    if let Some(asset) = referenced_asset(state, project, &file)? {
        return Ok(
            json!({"assetId":asset.id,"version":asset.version,"name":asset.name,"type":type_name(asset.kind),"deduplicated":true}),
        );
    }
    let content_type = mime(&file);
    let kind = match content_type.split('/').next() {
        Some("image") => AssetKind::Image,
        Some("video") => AssetKind::Video,
        Some("audio") => AssetKind::Audio,
        _ => return Err(RpcError::new("INVALID_MEDIA", "不支持此素材格式。")),
    };
    validate_media(&file, kind)?;
    let sha = file_sha(&file)?;
    let (asset, deduplicated) = with_library_write_lock(|| {
        let mut library = Library::load(&state.library)?;
        for asset in library.items().iter().filter(|asset| {
            !asset.trashed
                && asset.kind == kind
                && asset.version.as_ref().is_some_and(|v| v.sha256 == sha)
        }) {
            if checked_managed_version_path(&state.library, asset).is_ok() {
                return Ok::<_, RpcError>((asset.clone(), true));
            }
        }
        let asset = library.import(&file)?.clone();
        if asset.version.as_ref().is_none_or(|v| v.sha256 != sha) {
            return Err(RpcError::new(
                "ASSET_VERSION_CONFLICT",
                "项目素材在入库期间发生变化。",
            ));
        }
        if let Some(name) = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            library.rename(&asset.id, name)?;
        }
        Ok((
            library.get(&asset.id).ok_or("入库结果不可用")?.clone(),
            false,
        ))
    })?;
    save_source(state, project, raw, &asset)?;
    Ok(
        json!({"assetId":asset.id,"version":asset.version,"name":asset.name,"type":type_name(asset.kind),"deduplicated":deduplicated}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, State, String, Asset) {
        let temporary = tempfile::tempdir().unwrap();
        let state = State {
            root: temporary.path().canonicalize().unwrap(),
            library: temporary.path().join("library"),
            token: "private-test-token".into(),
            stop: Arc::new(AtomicBool::new(false)),
            connections: AtomicUsize::new(0),
            projects: Mutex::new(HashSet::new()),
            _directory_lock: None,
        };
        let project = uuid::Uuid::new_v4().to_string();
        fs::create_dir_all(state.root.join("workspaces").join(&project)).unwrap();
        let source = state.root.join("sound.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&52u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[0; 16]);
        fs::write(&source, wav).unwrap();
        let asset = with_library_write_lock(|| {
            Library::load(&state.library)
                .unwrap()
                .import(&source)
                .unwrap()
                .clone()
        });
        (temporary, state, project, asset)
    }
    fn offline() -> Result<Session> {
        Ok(Session {
            key: "local".into(),
            epoch: Some(1),
        })
    }

    #[test]
    fn assets_offline_list_requires_scope_and_rechecks_session() {
        let (_temporary, state, _project, _asset) = fixture();
        let params = json!({"expectedScopeKey":"local"});
        let result = dispatch_with_session(&state, "assets.list", &params, offline).unwrap();
        assert_eq!(result["scopeKey"], "local");
        assert_eq!(result["total"], 1);
        assert!(result["items"][0].get("path").is_none());
        assert_eq!(
            dispatch_with_session(&state, "assets.list", &json!({}), offline)
                .unwrap_err()
                .code,
            "FLOW_ACCOUNT_CHANGED"
        );
        for next in [
            Session {
                key: "signed-in".into(),
                epoch: Some(1),
            },
            Session {
                key: "local".into(),
                epoch: Some(2),
            },
        ] {
            let mut read = 0;
            let result = dispatch_with_session(&state, "assets.list", &params, || {
                read += 1;
                if read == 1 {
                    offline()
                } else {
                    Ok(Session {
                        key: next.key.clone(),
                        epoch: next.epoch,
                    })
                }
            });
            assert_eq!(result.unwrap_err().code, "FLOW_ACCOUNT_CHANGED");
        }
    }
    #[test]
    fn assets_list_filters_before_stable_paging() {
        let (_temporary, state, _project, asset) = fixture();
        with_library_write_lock(|| {
            let mut library = Library::load(&state.library).unwrap();
            library.rename(&asset.id, "Alpha sound").unwrap();
            let file = state.root.join("image.png");
            fs::write(&file, b"fake image, list does not decode").unwrap();
            library.import(&file).unwrap();
        });
        let page = list(&state, &json!({"q":"ALPHA","type":"audio","limit":1})).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["assetId"], asset.id);
        assert!(page["nextOffset"].is_null());
        let all = list(&state, &json!({"limit":1})).unwrap();
        let next = list(&state, &json!({"limit":1,"offset":1})).unwrap();
        assert_eq!(all["total"], 2);
        assert_ne!(all["items"][0]["assetId"], next["items"][0]["assetId"]);
        assert!(list(&state, &json!({"limit":51})).is_err());
        assert!(list(&state, &json!({"offset":-1})).is_err());
    }
    #[test]
    fn assets_project_copy_and_save_reuse_original_identity() {
        let (_temporary, state, project, asset) = fixture();
        let params = json!({"assetId":asset.id,"version":asset.version});
        let first = copy_to_project(&state, &project, &params).unwrap();
        assert_eq!(first["deduplicated"], false);
        let second = copy_to_project(&state, &project, &params).unwrap();
        assert_eq!(second["deduplicated"], true);
        assert_eq!(first["path"], second["path"]);
        let file = project_media(&state, &project, first["path"].as_str().unwrap()).unwrap();
        assert_eq!(
            reference_asset(&state, &project, &file).unwrap().id,
            asset.id
        );
        let saved =
            register_project_media(&state, &project, &json!({"path":first["path"]})).unwrap();
        assert_eq!(saved["assetId"], asset.id);
        assert_eq!(saved["deduplicated"], true);
        assert_eq!(Library::load(&state.library).unwrap().items().len(), 1);
        fs::write(&file, b"changed").unwrap();
        assert_eq!(
            reference_asset(&state, &project, &file).unwrap_err().code,
            "ASSET_VERSION_CONFLICT"
        );
    }
    #[test]
    fn assets_missing_original_can_import_verified_project_copy() {
        let (_temporary, state, project, asset) = fixture();
        let copied = copy_to_project(
            &state,
            &project,
            &json!({"assetId":asset.id,"version":asset.version}),
        )
        .unwrap();
        fs::remove_file(&asset.path).unwrap();
        let file = project_media(&state, &project, copied["path"].as_str().unwrap()).unwrap();
        let restored = reference_asset(&state, &project, &file).unwrap();
        assert_ne!(restored.id, asset.id);
        assert_eq!(
            restored.version.unwrap().sha256,
            asset.version.unwrap().sha256
        );
    }
    #[test]
    fn assets_project_copy_survives_original_trash_or_version_change() {
        for change_version in [false, true] {
            let (_temporary, state, project, asset) = fixture();
            let params = json!({"assetId":asset.id,"version":asset.version});
            let copied = copy_to_project(&state, &project, &params).unwrap();
            with_library_write_lock(|| {
                if change_version {
                    let path = state.library.join(crate::personal_library::MANIFEST_NAME);
                    let mut manifest: Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    manifest["items"][0]["version"]["registration_id"] =
                        json!("new-library-version");
                    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                } else {
                    Library::load(&state.library)
                        .unwrap()
                        .trash(&asset.id)
                        .unwrap();
                }
            });
            let file = project_media(&state, &project, copied["path"].as_str().unwrap()).unwrap();
            let restored = reference_asset(&state, &project, &file).unwrap();
            assert_ne!(restored.id, asset.id);
            assert!(!restored.trashed);
            assert_eq!(
                restored.version.as_ref().unwrap().sha256,
                asset.version.as_ref().unwrap().sha256
            );
            assert_eq!(
                file_sha(&file).unwrap(),
                restored.version.as_ref().unwrap().sha256
            );
            checked_managed_version_path(&state.library, &restored).unwrap();
            let rejected = copy_to_project(&state, &project, &params).unwrap_err();
            assert_eq!(
                rejected.code,
                if change_version {
                    "ASSET_VERSION_CONFLICT"
                } else {
                    "ASSET_NOT_FOUND"
                }
            );
        }
    }
    #[test]
    fn assets_uploaded_media_can_be_reused_after_its_library_entry_is_trashed() {
        for use_reference in [false, true] {
            let (_temporary, state, project, asset) = fixture();
            let root = state.project(&project).unwrap();
            directory(&root, "assets/uploads").unwrap();
            let relative = "assets/uploads/uploaded.wav";
            let file = root.join(relative);
            let mut bytes = fs::read(&asset.path).unwrap();
            bytes[44] = 1;
            fs::write(&file, bytes).unwrap();
            let params = json!({"path":relative});
            let first = register_project_media(&state, &project, &params).unwrap();
            let original_id = first["assetId"].as_str().unwrap();
            with_library_write_lock(|| {
                Library::load(&state.library)
                    .unwrap()
                    .trash(original_id)
                    .unwrap();
            });
            let replacement = if use_reference {
                reference_asset(&state, &project, &file).unwrap()
            } else {
                let saved = register_project_media(&state, &project, &params).unwrap();
                assert_eq!(saved["deduplicated"], false);
                Library::load(&state.library)
                    .unwrap()
                    .get(saved["assetId"].as_str().unwrap())
                    .unwrap()
                    .clone()
            };
            assert_ne!(replacement.id, original_id);
            assert!(!replacement.trashed);
            let version = replacement.version.as_ref().unwrap();
            assert_eq!(version.sha256, first["version"]["sha256"]);
            assert_eq!(version.previous_id.as_deref(), Some(original_id));
            checked_managed_version_path(&state.library, &replacement).unwrap();
            let repeated = reference_asset(&state, &project, &file).unwrap();
            assert_eq!(repeated.id, replacement.id);
            let library = Library::load(&state.library).unwrap();
            assert!(library.get(original_id).unwrap().trashed);
        }
    }
    #[test]
    fn assets_manual_save_deduplicates_verified_bytes_and_rejects_invalid_input() {
        let (_temporary, state, project, asset) = fixture();
        let root = state.project(&project).unwrap();
        directory(&root, "assets/uploads").unwrap();
        fs::copy(&asset.path, root.join("assets/uploads/manual.wav")).unwrap();
        let saved = register_project_media(
            &state,
            &project,
            &json!({"path":"assets/uploads/manual.wav"}),
        )
        .unwrap();
        assert_eq!(saved["assetId"], asset.id);
        assert_eq!(saved["deduplicated"], true);
        fs::write(root.join("assets/uploads/bad.png"), b"not image").unwrap();
        assert_eq!(
            register_project_media(&state, &project, &json!({"path":"assets/uploads/bad.png"}))
                .unwrap_err()
                .code,
            "INVALID_MEDIA"
        );
        for path in ["../sound.wav", "/tmp/sound.wav", "snapshot.json"] {
            assert_eq!(
                project_media(&state, &project, path).unwrap_err().code,
                "INVALID_PATH"
            );
        }
        let mut stale = asset.version.clone().unwrap();
        stale.sha256 = "0".repeat(64);
        assert_eq!(
            resolve(&state, &asset.id, &stale).unwrap_err().code,
            "ASSET_VERSION_CONFLICT"
        );
    }
    #[cfg(unix)]
    #[test]
    fn assets_project_rejects_file_and_directory_symlinks() {
        let (_temporary, state, project, asset) = fixture();
        let root = state.project(&project).unwrap();
        directory(&root, "assets/uploads").unwrap();
        std::os::unix::fs::symlink(&asset.path, root.join("assets/uploads/link.wav")).unwrap();
        assert_eq!(
            project_media(&state, &project, "assets/uploads/link.wav")
                .unwrap_err()
                .code,
            "INVALID_PATH"
        );
        std::os::unix::fs::symlink(asset.path.parent().unwrap(), root.join("assets/escape"))
            .unwrap();
        assert!(directory(&root, "assets/escape/nested").is_err());
        let thumbnails = state.library.join("thumbnails");
        fs::create_dir(&thumbnails).unwrap();
        let cached = thumbnails.join("cached.jpg");
        fs::write(&cached, b"cached thumbnail").unwrap();
        assert_eq!(
            checked_thumbnail(&state, &cached).unwrap(),
            cached.canonicalize().unwrap()
        );
        let linked = thumbnails.join("linked.jpg");
        std::os::unix::fs::symlink(&asset.path, &linked).unwrap();
        assert_eq!(
            checked_thumbnail(&state, &linked).unwrap_err().code,
            "INVALID_PATH"
        );
    }
}
