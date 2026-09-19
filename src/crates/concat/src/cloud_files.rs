// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded, redirect-free file transfer helpers for the SeeCut service.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Requests an upload slot and streams `path` to its same-origin signed URL.
pub fn upload(
    base: &str,
    token: &str,
    path: &Path,
    purpose: &str,
    team: Option<&str>,
) -> Result<Value, String> {
    let base = checked_base(base)?;
    let metadata = path
        .metadata()
        .map_err(|error| format!("无法读取上传文件：{error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("上传文件为空或不是普通文件".into());
    }
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "上传文件名无效".to_owned())?;
    let content_type = media_type(path)?;
    let body = json!({
        "purpose": purpose,
        "team_id": team,
        "filename": filename,
        "content_type": content_type,
        "size_bytes": metadata.len(),
    });

    let agent = agent();
    let response = agent
        .post(
            base.join("api/uploads")
                .map_err(|_| "上传接口地址无效")?
                .as_str(),
        )
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(body)
        .map_err(api_error)?;
    let upload_info: Value = response
        .into_json()
        .map_err(|_| "服务器返回了无效的上传信息".to_owned())?;
    if upload_info.get("method").and_then(Value::as_str) != Some("PUT") {
        return Err("服务器返回了不支持的上传方式".into());
    }
    let upload_url = upload_info
        .get("upload_url")
        .and_then(Value::as_str)
        .ok_or_else(|| "服务器未返回上传地址".to_owned())?;
    let upload_url = checked_same_origin(&base, upload_url)?;
    let mut request = agent
        .put(upload_url.as_str())
        .set("Content-Length", &metadata.len().to_string());
    if let Some(headers) = upload_info
        .get("required_headers")
        .and_then(Value::as_object)
    {
        for (name, value) in headers {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "host" | "content-length" | "cookie" | "proxy-authorization"
            ) {
                return Err("服务器返回了不允许的上传请求头".into());
            }
            let value = value
                .as_str()
                .ok_or_else(|| "服务器返回了无效的上传请求头".to_owned())?;
            request = request.set(name, value);
        }
    } else {
        request = request.set("Content-Type", content_type);
    }
    let file = File::open(path).map_err(|error| format!("无法打开上传文件：{error}"))?;
    request.send(file).map_err(api_error)?;
    Ok(upload_info)
}

/// Streams a same-origin URL into a temporary file and atomically installs it.
pub fn download(base: &str, token: &str, url: &str, target: &Path) -> Result<PathBuf, String> {
    let base = checked_base(base)?;
    let url = checked_same_origin(&base, url)?;
    let parent = target
        .parent()
        .ok_or_else(|| "下载目标缺少父目录".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建下载目录：{error}"))?;
    let temp = parent.join(format!(
        ".{}.{}.part",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("download"),
        uuid::Uuid::new_v4()
    ));

    let result = (|| {
        let response = agent()
            .get(url.as_str())
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(api_error)?;
        let expected_length = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok());
        if expected_length.is_some_and(|length| length > MAX_DOWNLOAD_BYTES) {
            return Err("下载文件超过 512 MB 上限".into());
        }
        let mut reader = response.into_reader().take(MAX_DOWNLOAD_BYTES + 1);
        let mut output =
            File::create(&temp).map_err(|error| format!("无法创建临时文件：{error}"))?;
        let written = std::io::copy(&mut reader, &mut output)
            .map_err(|error| format!("下载写入失败：{error}"))?;
        if written > MAX_DOWNLOAD_BYTES {
            return Err("下载文件超过 512 MB 上限".into());
        }
        if written == 0 || expected_length.is_some_and(|length| length != written) {
            return Err("下载文件不完整，请重试".into());
        }
        output
            .flush()
            .map_err(|error| format!("下载写入失败：{error}"))?;
        output
            .sync_all()
            .map_err(|error| format!("下载写入失败：{error}"))?;
        fs::rename(&temp, target).map_err(|error| format!("无法保存下载文件：{error}"))?;
        Ok(target.to_path_buf())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .redirects(0)
        .build()
}

fn checked_base(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "服务地址格式无效".to_owned())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || !secure_or_loopback(&url)
    {
        return Err("服务地址必须是 HTTPS 或本机回环 HTTP 地址".into());
    }
    Ok(url)
}

fn checked_same_origin(base: &Url, raw: &str) -> Result<Url, String> {
    let url = base.join(raw).map_err(|_| "文件地址无效".to_owned())?;
    if !secure_or_loopback(&url)
        || url.username() != base.username()
        || url.password() != base.password()
        || url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
    {
        return Err("拒绝访问跨源或不安全的文件地址".into());
    }
    Ok(url)
}

fn secure_or_loopback(url: &Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }))
}

fn media_type(path: &Path) -> Result<&'static str, String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        "mp4" | "m4v" => Ok("video/mp4"),
        "mov" => Ok("video/quicktime"),
        "webm" => Ok("video/webm"),
        "mp3" => Ok("audio/mpeg"),
        "wav" => Ok("audio/wav"),
        "m4a" => Ok("audio/mp4"),
        _ => Err("不支持该文件类型".into()),
    }
}

fn api_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let value: Value = response.into_json().unwrap_or(Value::Null);
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("文件传输失败 ({status})"))
        }
        ureq::Error::Transport(error) => format!("文件传输中断：{error}"),
    }
}
