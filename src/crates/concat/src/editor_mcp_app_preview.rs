// SPDX-License-Identifier: AGPL-3.0-or-later
//! Capture pixels from the existing UI image; encode off the UI thread.
//! There is no filesystem/path/URL input and no video decode or screenshot.

use crate::editor_mcp_app_page::{AppPages, Error, FrozenPage, PreviewSource};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// UI-thread preparation. The authenticated caller has already recaptured the
/// current filtered page and checked its AppReadGrant/account/client binding.
pub struct PreviewPixels {
    pub frozen: FrozenPage,
    pub item_id: String,
    pub source: PreviewSource,
    pub preview_version: u64,
    pixels: slint::SharedPixelBuffer<slint::Rgba8Pixel>,
}

pub struct EncodedPreview {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    png_sha256: String,
    source_pixel_sha256: String,
}

pub fn capture_pixels(
    registry: &AppPages<slint::Image>,
    frozen: FrozenPage,
    item_id: String,
    preview_version: u64,
) -> Result<PreviewPixels, Error> {
    if registry.preview_version(&frozen, &item_id)? != preview_version {
        return Err(Error::StalePage);
    }
    let (source, image) = registry.page_preview(&frozen, &item_id)?;
    let size = image.size();
    // Same source-pixel ceiling as the existing canvas preview; bound before copy.
    if size.width == 0
        || size.height == 0
        || u64::from(size.width) * u64::from(size.height) > 4_194_304
    {
        return Err(Error::ResourceLimit);
    }
    // Current Slint 1.17.1 Image::load_from_path decodes on load; to_rgba8 only
    // renders/copies its existing ImageInner buffer. It does not reopen its path.
    let pixels = image.to_rgba8().ok_or(Error::PreviewUnavailable)?;
    if pixels.width() != size.width || pixels.height() != size.height {
        return Err(Error::PreviewUnavailable);
    }
    Ok(PreviewPixels {
        frozen,
        item_id,
        source,
        preview_version,
        pixels,
    })
}

/// Worker preparation: at most 256x256 PNG from the retained image/thumbnail.
pub fn encode(job: &PreviewPixels) -> Result<EncodedPreview, Error> {
    let rgba = image::RgbaImage::from_raw(
        job.pixels.width(),
        job.pixels.height(),
        job.pixels.as_bytes().to_vec(),
    )
    .ok_or(Error::PreviewUnavailable)?;
    let edge = job.pixels.width().max(job.pixels.height()).min(256);
    let thumbnail = image::DynamicImage::ImageRgba8(rgba).thumbnail(edge, edge);
    let mut buffer = std::io::Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|_| Error::PreviewUnavailable)?;
    let bytes = buffer.into_inner();
    if bytes.len() > 1_048_576 {
        return Err(Error::ResourceLimit);
    }
    let png_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut pixel_hash = Sha256::new();
    pixel_hash.update(job.pixels.width().to_le_bytes());
    pixel_hash.update(job.pixels.height().to_le_bytes());
    pixel_hash.update(job.pixels.as_bytes());
    let source_pixel_sha256 = format!("{:x}", pixel_hash.finalize());
    Ok(EncodedPreview {
        width: thumbnail.width(),
        height: thumbnail.height(),
        bytes,
        png_sha256,
        source_pixel_sha256,
    })
}

/// UI-thread final step AFTER recapture and final AppReadGrant/account checks.
/// Returning a cached Context without recapturing is explicitly insufficient.
pub fn finish(
    registry: &AppPages<slint::Image>,
    job: &PreviewPixels,
    encoded: EncodedPreview,
) -> Result<Value, Error> {
    let context = registry.require_current(&job.frozen)?;
    // Also ensure this item still names the same retained preview in this state.
    registry.page_preview(&job.frozen, &job.item_id)?;
    if registry.preview_version(&job.frozen, &job.item_id)? != job.preview_version {
        return Err(Error::StalePage);
    }
    Ok(
        json!({"scopeKind":"app","appInstanceId":context.app_instance_id,"clientId":context.client_id,
        "accountScope":context.account_scope,"appSessionId":context.app_session_id,"page":context.page,
        "itemId":job.item_id,"previewVersion":job.preview_version,"mimeType":"image/png","width":encoded.width,"height":encoded.height,
        "metadata":{"scope":"item","source":job.source,"viewportCapture":false,"mediaTime":null,
            "pageStateId":context.page.page_state_id,"pngSha256":encoded.png_sha256,"sourcePixelSha256":encoded.source_pixel_sha256},
        "base64":base64::engine::general_purpose::STANDARD.encode(encoded.bytes)}),
    )
}

// CLI bridge must reuse main.rs::preview's existing pattern: remove base64 from
// Response.data, emit ContentBlock::image(encoded,"image/png"), and put ONLY the
// remaining bounded metadata in structured_content. Never stringify PNG bytes
// into COTO text or claim the model viewed an image from metadata alone.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_mcp_app_page::*;
    use crate::editor_mcp_app_preview as preview;
    use base64::Engine;
    fn row(pixel: u8) -> CachedItem<slint::Image> {
        let rgba = vec![pixel; 32 * 16 * 4];
        let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, 32, 16);
        CachedItem {
            source_key: "cached".into(),
            kind: Kind::Image,
            name: "image".into(),
            status: String::new(),
            selected: false,
            availability: Availability::Available,
            project: false,
            detail: None,
            preview: Some((PreviewSource::Image, slint::Image::from_rgba8(pixels))),
        }
    }
    fn frozen(c: &Context) -> FrozenPage {
        FrozenPage {
            app_session_id: c.app_session_id.clone(),
            page_id: c.page.page_id,
            page_epoch: c.page.page_epoch,
            page_state_id: c.page.page_state_id.clone(),
        }
    }
    #[test]
    fn existing_buffer_png_encodes_and_strips_no_source_identity() {
        fn send<T: Send>() {}
        send::<preview::PreviewPixels>();
        let mut p = AppPages::new("i".into(), "c".into());
        p.account(Some((1, "a".repeat(64)))).unwrap();
        let c = p
            .capture(
                PageId::Generation,
                serde_json::json!({"browseTab":"results"}),
                vec![row(255)],
            )
            .unwrap();
        let items = p.page_items(&frozen(&c), 0, 20).unwrap();
        let item = &items.items[0];
        let job =
            preview::capture_pixels(&p, frozen(&c), item.item_id.clone(), item.preview_version)
                .unwrap();
        let png = preview::encode(&job).unwrap();
        let result = preview::finish(&p, &job, png).unwrap();
        assert_eq!(result["mimeType"], "image/png");
        assert_eq!(result["width"], 32);
        assert_eq!(result["height"], 16);
        assert_eq!(result["previewVersion"], item.preview_version);
        assert_eq!(result["metadata"]["source"], "image");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result["base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 16));
        assert_eq!(
            result["metadata"]["pngSha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        let mut source_hash = Sha256::new();
        source_hash.update(32u32.to_le_bytes());
        source_hash.update(16u32.to_le_bytes());
        source_hash.update(vec![255; 32 * 16 * 4]);
        assert_eq!(
            result["metadata"]["sourcePixelSha256"],
            format!("{:x}", source_hash.finalize())
        );
        assert_eq!(result["metadata"]["scope"], "item");
        assert_eq!(result["metadata"]["viewportCapture"], false);
        assert!(result["metadata"]["mediaTime"].is_null());
        assert_eq!(result["metadata"]["pageStateId"], c.page.page_state_id);
    }
    #[test]
    fn worker_completion_rejects_changed_target_version_and_navigation() {
        let mut p = AppPages::new("i".into(), "c".into());
        p.account(Some((1, "a".repeat(64)))).unwrap();
        let c = p
            .capture(PageId::Generation, serde_json::json!({}), vec![row(1)])
            .unwrap();
        let items = p.page_items(&frozen(&c), 0, 20).unwrap();
        let item = &items.items[0];
        let job =
            preview::capture_pixels(&p, frozen(&c), item.item_id.clone(), item.preview_version)
                .unwrap();
        let png = preview::encode(&job).unwrap();
        let updated = p
            .capture(PageId::Generation, serde_json::json!({}), vec![row(2)])
            .unwrap();
        assert_eq!(c.page.page_state_id, updated.page.page_state_id);
        assert!(matches!(
            preview::finish(&p, &job, png),
            Err(Error::StalePage)
        ));
        let item = p
            .page_items(&frozen(&updated), 0, 20)
            .unwrap()
            .items
            .remove(0);
        let job = preview::capture_pixels(&p, frozen(&updated), item.item_id, item.preview_version)
            .unwrap();
        let png = preview::encode(&job).unwrap();
        p.navigation().unwrap();
        assert!(matches!(
            preview::finish(&p, &job, png),
            Err(Error::StalePage)
        ));
    }
}
