//! Private historical adopted-composite reads. No model serialization or grants.
use crate::{HostError, NativeHost, PhotoIdentity, Result, Uuid};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const MAX_RESULT: usize = (1 << 20) - 1024;
const MAX_PNG: usize = 512 * 1024;

/// Closed layer kinds supported by the native snapshot source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoSnapshotLayerKind {
    /// Ordinary raster layer.
    Raster,
    /// Group, including its separately enumerated children.
    Group,
}
/// Frozen native layer properties. Names are untrusted and omitted from Debug.
#[derive(Clone, PartialEq)]
pub struct PhotoSnapshotLayer {
    /// Exact native layer ID; do not convert through JavaScript Number.
    pub id: u64,
    /// Bounded untrusted document content.
    pub name: String,
    /// Supported native content kind.
    pub kind: PhotoSnapshotLayerKind,
    /// Depth in the frozen document tree.
    pub depth: u32,
    /// Own visibility, without a screen-visibility claim.
    pub visible: bool,
    /// Own opacity.
    pub opacity: f64,
    /// Own fill opacity.
    pub fill_opacity: f64,
}
impl std::fmt::Debug for PhotoSnapshotLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoSnapshotLayer")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
/// Frozen document metadata from the same adopted material as the composite.
#[derive(Clone, PartialEq)]
pub struct PhotoSnapshotDocument {
    /// Bounded untrusted name; never a backing path or history identity.
    pub name: String,
    /// Original document dimensions.
    pub size: [u32; 2],
    /// Document order, bottom to top, including groups and children.
    pub layers: Vec<PhotoSnapshotLayer>,
    /// Native active layer.
    pub active_layer: Option<u64>,
    /// Native selected layers in document order.
    pub selected_layers: Vec<u64>,
    /// Dirty fact frozen at adoption.
    pub dirty: bool,
    /// Undo availability frozen at adoption.
    pub can_undo: bool,
    /// Redo availability frozen at adoption.
    pub can_redo: bool,
}
impl std::fmt::Debug for PhotoSnapshotDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoSnapshotDocument")
            .field("size", &self.size)
            .field("active_layer", &self.active_layer)
            .finish_non_exhaustive()
    }
}
/// Frozen selection metadata; the host does not resample selection pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSnapshotSelection {
    /// Whether a native selection object existed, including an empty selection.
    pub present: bool,
    /// Document-pixel domain, xyxy exclusive.
    pub domain: [u32; 4],
    /// Nonzero coverage bounds, xyxy exclusive, when present.
    pub bounds: Option<[i32; 4]>,
    /// Native full-coverage digest, not recomputed from PNG pixels.
    pub coverage_hash: String,
    /// Native selection identity, checked against active/selected IDs and coverage hash.
    pub selection_state_id: String,
}
/// Actual adopted transform, with the source's f32/f64 precision preserved.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSnapshotGeometry {
    /// x, y, width, height in egui points inside rulers.
    pub rect_points: [f32; 4],
    /// Actual egui pixels per point.
    pub pixels_per_point: f32,
    /// Actual points per document pixel.
    pub zoom: f32,
    /// Actual document center.
    pub center: [f32; 2],
    /// Actual horizontal flip.
    pub flip: bool,
    /// Actual rotation in degrees.
    pub rotation: f32,
    /// Actual pixel aspect.
    pub aspect: f32,
    /// Actual device-pixel display camera.
    pub display_camera: [f32; 3],
    /// Frozen target camera, required to match the actual display camera.
    pub target_camera: [f32; 3],
    /// Four actual transformed document corners, preserving f64.
    pub visible_document_polygon: [[f64; 2]; 4],
}
/// Frozen view metadata, not parent-window geometry or a screen capture.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSnapshotView {
    /// Actual adopted transform.
    pub geometry: PhotoSnapshotGeometry,
    /// Bounding box of the four transformed corners, xyxy exclusive.
    pub visible_document_aabb: [f64; 4],
    /// Bounding box clipped to document dimensions, xyxy exclusive.
    pub clipped_visible_document_aabb: [f64; 4],
    /// Native digest bound to identity, viewport, canvas and geometry.
    pub view_state_id: String,
}
/// Native source renderer, not a claim about physical screen presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoSnapshotRenderer {
    /// CPU canvas source.
    Cpu,
    /// GPU canvas source.
    Gpu,
}
/// Checked source declarations; the host does not decode the embedded ICC profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoSnapshotPresentation {
    /// Source renderer.
    pub renderer: PhotoSnapshotRenderer,
    /// Exact native display key, including values above 2^53.
    pub display_key: u64,
    /// Whether the source used an identity display transform.
    pub display_identity: bool,
    /// Native declared embedded sRGB-profile digest.
    pub profile_hash: String,
}
/// Checked PNG metadata. The host verifies its container and PNG digest only.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSnapshotPixels {
    /// PNG width and height, at most the requested edge and 256.
    pub size: [u32; 2],
    /// Full source document rectangle in document pixels.
    pub document_rect: [u32; 4],
    /// Source document pixels per output pixel on each axis.
    pub document_pixels_per_output_pixel: [f64; 2],
    /// Native declared decoded-pixel digest; not independently decoded by this host.
    pub pixel_hash: String,
    /// Independently recomputed SHA-256 of the complete decoded PNG bytes.
    pub png_hash: String,
}
/// Timing observations measured by the child, not a network-arrival clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoSnapshotTiming {
    /// Delay from UI enqueue to actual adopted capture.
    pub captured_after_ui_enqueue_ms: u64,
    /// Age from adopted capture to the native historical reply.
    pub capture_age_at_reply_ms: u64,
}
/// Historical `uiPaintSubmitted` composite. Never implicitly fresh or presented.
/// No Serialize, no raw Value, and no PNG/document strings in Debug.
pub struct PhotoSnapshot {
    request_id: String,
    snapshot_id: String,
    identity: PhotoIdentity,
    captured_frame: u64,
    viewport_id: u64,
    canvas_id: u64,
    timing: PhotoSnapshotTiming,
    document: PhotoSnapshotDocument,
    selection: PhotoSnapshotSelection,
    view: PhotoSnapshotView,
    presentation: PhotoSnapshotPresentation,
    pixels: PhotoSnapshotPixels,
    png: Vec<u8>,
    request_started: Instant,
    received_at: Instant,
}
impl std::fmt::Debug for PhotoSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoSnapshot")
            .field("identity", &self.identity)
            .field("snapshot_id", &self.snapshot_id)
            .field("captured_frame", &self.captured_frame)
            .finish_non_exhaustive()
    }
}
impl PhotoSnapshot {
    /// Parent monotonic start of this original request, including authentication.
    pub fn request_started_at(&self) -> Instant {
        self.request_started
    }
    /// Parent monotonic receipt of the original native result, before parsing.
    pub fn received_at(&self) -> Instant {
        self.received_at
    }
    /// Original snapshot request ID, independent of validation request IDs.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// Original native historical snapshot identity.
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
    /// Full native identity, preserved without floating-point conversion.
    pub fn identity(&self) -> &PhotoIdentity {
        &self.identity
    }
    /// Frame at actual adopted capture; not a screen-present fence.
    pub fn captured_frame(&self) -> u64 {
        self.captured_frame
    }
    /// Exact native viewport hash/ID; use a separate opaque parent mapping.
    pub fn viewport_id(&self) -> u64 {
        self.viewport_id
    }
    /// Exact native canvas hash/ID; use a separate opaque parent mapping.
    pub fn canvas_id(&self) -> u64 {
        self.canvas_id
    }
    /// Child timing with explicit UI-enqueue and adopted-capture bases.
    pub fn timing(&self) -> &PhotoSnapshotTiming {
        &self.timing
    }
    /// Frozen untrusted document content, not permission or file identity.
    pub fn document(&self) -> &PhotoSnapshotDocument {
        &self.document
    }
    /// Frozen selection facts, without independently sampling the mask.
    pub fn selection(&self) -> &PhotoSnapshotSelection {
        &self.selection
    }
    /// Frozen adopted geometry and view digest.
    pub fn view(&self) -> &PhotoSnapshotView {
        &self.view
    }
    /// Source color and renderer declarations, excluding monitor-pixel claims.
    pub fn presentation(&self) -> &PhotoSnapshotPresentation {
        &self.presentation
    }
    /// PNG container metadata and declared decoded-pixel digest.
    pub fn pixels(&self) -> &PhotoSnapshotPixels {
        &self.pixels
    }
    /// Verified-hash historical PNG bytes. Before model delivery the caller must
    /// validate the original snapshot and recheck original read/account/Task gates.
    /// Container verification is not complete decompression or decoded-pixel proof.
    pub fn png_bytes(&self) -> &[u8] {
        &self.png
    }
}

/// A successful native post-input-frame observation of this original snapshot.
/// It is historical boundary evidence, never a lease or permanent freshness bit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoSnapshotValidation {
    request_id: String,
    snapshot_id: String,
    identity: PhotoIdentity,
    view_state_id: String,
    selection_state_id: String,
    validated_frame: u64,
    capture_age_ms: u64,
    validation_started: Instant,
    received_at: Instant,
    capture_age_upper_bound_ms: u64,
}
impl PhotoSnapshotValidation {
    /// Parent monotonic start before dispatch of this validation request.
    pub fn request_started_at(&self) -> Instant {
        self.validation_started
    }
    /// Parent monotonic receipt of this native validation result.
    pub fn received_at(&self) -> Instant {
        self.received_at
    }
    /// Conservative age ceiling from original parent request start to this
    /// result's receipt, rounded upwards. This is not a child wall-clock reading.
    pub fn capture_age_upper_bound_ms(&self) -> u64 {
        self.capture_age_upper_bound_ms
    }
    /// This validation request ID, independent of the original capture request.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// The original snapshot that was validated, without substitution.
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
    /// Exact original native document binding.
    pub fn identity(&self) -> &PhotoIdentity {
        &self.identity
    }
    /// Original view identity observed at the post-input-frame boundary.
    pub fn view_state_id(&self) -> &str {
        &self.view_state_id
    }
    /// Original selection identity observed at that boundary.
    pub fn selection_state_id(&self) -> &str {
        &self.selection_state_id
    }
    /// Native paint frame whose relevant input processing had finished.
    pub fn validated_frame(&self) -> u64 {
        self.validated_frame
    }
    /// Age measured from original adopted capture by the child.
    pub fn capture_age_ms(&self) -> u64 {
        self.capture_age_ms
    }
}

impl NativeHost {
    /// Closed read-only snapshot call, without retries or screenshot fallback.
    /// Host-worker only. Original read/account/Task gates belong to the caller
    /// before and after this operation and again before model image delivery.
    pub fn photo_snapshot_after_gate(
        &mut self,
        id: Uuid,
        request_id: &str,
        expected: PhotoIdentity,
        max_edge: u32,
    ) -> Result<PhotoSnapshot> {
        check_request(id, request_id, &expected, max_edge)?;
        let r = self.instance(id)?;
        if !r.info.ready {
            return Err(HostError::NotReady);
        }
        let params = json!({"requestId":request_id,"expected":{
            "hostInstanceId":expected.host_instance_id.to_string(),"docId":expected.doc_id,
            "documentGeneration":expected.document_generation,"revision":expected.revision},"maxEdge":max_edge});
        let request_started = Instant::now();
        let value = super::call(r, "seecut.snapshot", params, false)?;
        let received_at = Instant::now();
        parse_snapshot(
            &value,
            request_id,
            &expected,
            max_edge,
            request_started,
            received_at,
        )
    }
    /// Read-only validation of this exact historical capture at a new native
    /// postInputFrame boundary. No writes, retries or alternate-snapshot fallback.
    /// A successful result grants nothing and can become stale immediately;
    /// original outer read/account/Task gates must still hold before publication.
    pub fn validate_photo_snapshot_after_gate(
        &mut self,
        id: Uuid,
        request_id: &str,
        snapshot: &PhotoSnapshot,
    ) -> Result<PhotoSnapshotValidation> {
        check_request(id, request_id, snapshot.identity(), 1)?;
        let validation_started = Instant::now();
        if validation_started
            .checked_duration_since(snapshot.received_at)
            .ok_or_else(bad)?
            >= Duration::from_secs(5)
        {
            return Err(HostError::ReadFailed(
                "Photo snapshot expired locally before validation",
            ));
        }
        let r = self.instance(id)?;
        if !r.info.ready {
            return Err(HostError::NotReady);
        }
        let value = super::call(
            r,
            "seecut.snapshot.validate",
            json!({"requestId":request_id,"snapshotId":snapshot.snapshot_id()}),
            false,
        )?;
        let received_at = Instant::now();
        parse_validation(
            &value,
            request_id,
            snapshot,
            validation_started,
            received_at,
        )
    }
}

fn bad() -> HostError {
    HostError::ReadFailed("invalid bounded Photo snapshot or validation")
}
fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn check_request(
    id: Uuid,
    request_id: &str,
    expected: &PhotoIdentity,
    max_edge: u32,
) -> Result<()> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(HostError::Unsupported);
    }
    if !valid_request_id(request_id)
        || expected.host_instance_id != id
        || expected.doc_id == 0
        || expected.document_generation == 0
        || expected.revision == 0
        || !(1..=256).contains(&max_edge)
    {
        return Err(HostError::NotSent("invalid Photo snapshot request"));
    }
    Ok(())
}
fn fields<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let m = v.as_object().ok_or_else(bad)?;
    if m.len() != keys.len() || keys.iter().any(|k| !m.contains_key(*k)) {
        return Err(bad());
    }
    Ok(m)
}
fn bounded(v: &Value) -> Result<()> {
    struct Limit(usize);
    impl std::io::Write for Limit {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(b.len());
            if self.0 > MAX_RESULT {
                return Err(std::io::Error::other("snapshot budget"));
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limit(0), v).map_err(|_| bad())
}
fn text(v: &Value, max: usize) -> Result<String> {
    let s = v
        .as_str()
        .filter(|s| s.len() <= max && !s.contains('\0'))
        .ok_or_else(bad)?;
    Ok(s.into())
}
fn integer(v: &Value) -> Result<u64> {
    v.as_u64().ok_or_else(bad)
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().ok_or_else(bad)
}
fn number(v: &Value) -> Result<f64> {
    v.as_f64().filter(|v| v.is_finite()).ok_or_else(bad)
}
fn f32_number(v: &Value) -> Result<f32> {
    let n = number(v)?;
    let f = n as f32;
    if !f.is_finite() || f64::from(f) != n {
        return Err(bad());
    }
    Ok(f)
}
fn array<T, const N: usize>(
    v: &Value,
    mut parse: impl FnMut(&Value) -> Result<T>,
) -> Result<[T; N]> {
    let a = v.as_array().filter(|a| a.len() == N).ok_or_else(bad)?;
    a.iter()
        .map(&mut parse)
        .collect::<Result<Vec<T>>>()?
        .try_into()
        .map_err(|_| bad())
}
fn u32_number(v: &Value) -> Result<u32> {
    u32::try_from(integer(v)?).map_err(|_| bad())
}
fn digest(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update(part);
    }
    format!("{:x}", h.finalize())
}
fn hash(v: &Value) -> Result<String> {
    let s = text(v, 64)?;
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad());
    }
    Ok(s)
}
fn identifier(v: &Value, prefix: &str) -> Result<String> {
    let s = text(v, prefix.len() + 64)?;
    let suffix = s.strip_prefix(prefix).ok_or_else(bad)?;
    hash(&Value::String(suffix.into()))?;
    Ok(s)
}
fn parse_identity(v: &Value, expected: &PhotoIdentity) -> Result<PhotoIdentity> {
    let m = fields(
        v,
        &["hostInstanceId", "docId", "documentGeneration", "revision"],
    )?;
    if m["hostInstanceId"].as_str() != Some(expected.host_instance_id.to_string().as_str())
        || integer(&m["docId"])? != expected.doc_id
        || integer(&m["documentGeneration"])? != expected.document_generation
        || integer(&m["revision"])? != expected.revision
    {
        return Err(bad());
    }
    Ok(expected.clone())
}
fn identity_bytes(i: &PhotoIdentity) -> Vec<u8> {
    format!(
        "{{\"hostInstanceId\":\"{}\",\"docId\":{},\"documentGeneration\":{},\"revision\":{}}}",
        i.host_instance_id, i.doc_id, i.document_generation, i.revision
    )
    .into_bytes()
}
fn parse_document(v: &Value) -> Result<PhotoSnapshotDocument> {
    let m = fields(
        v,
        &[
            "name",
            "size",
            "layers",
            "layersOrder",
            "activeLayer",
            "selectedLayers",
            "selectedLayersOrder",
            "dirty",
            "canUndo",
            "canRedo",
        ],
    )?;
    if m["layersOrder"] != "documentBottomToTop"
        || m["selectedLayersOrder"] != "documentBottomToTop"
    {
        return Err(bad());
    }
    let size = array(&m["size"], u32_number)?;
    if size.iter().any(|n| *n == 0 || *n > 4096)
        || u64::from(size[0]) * u64::from(size[1]) > 4_194_304
    {
        return Err(bad());
    }
    let mut layers: Vec<PhotoSnapshotLayer> = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for v in m["layers"]
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or_else(bad)?
    {
        let l = fields(
            v,
            &[
                "id",
                "name",
                "kind",
                "depth",
                "visible",
                "opacity",
                "fillOpacity",
                "mask",
            ],
        )?;
        let id = integer(&l["id"])?;
        let kind = match l["kind"].as_str() {
            Some("raster") => PhotoSnapshotLayerKind::Raster,
            Some("group") => PhotoSnapshotLayerKind::Group,
            _ => return Err(bad()),
        };
        let depth = u32_number(&l["depth"])?;
        let opacity = number(&l["opacity"])?;
        let fill_opacity = number(&l["fillOpacity"])?;
        if id == 0
            || !ids.insert(id)
            || depth > 8
            || l["mask"] != false
            || !(0.0..=1.0).contains(&opacity)
            || !(0.0..=1.0).contains(&fill_opacity)
            || layers.last().map_or(depth != 0, |prev| {
                depth > prev.depth + 1
                    || depth == prev.depth + 1 && prev.kind != PhotoSnapshotLayerKind::Group
            })
        {
            return Err(bad());
        }
        layers.push(PhotoSnapshotLayer {
            id,
            name: text(&l["name"], 256)?,
            kind,
            depth,
            visible: boolean(&l["visible"])?,
            opacity,
            fill_opacity,
        });
    }
    let active_layer = if m["activeLayer"].is_null() {
        None
    } else {
        Some(integer(&m["activeLayer"])?)
    };
    let selected_layers = m["selectedLayers"]
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or_else(bad)?
        .iter()
        .map(integer)
        .collect::<Result<Vec<_>>>()?;
    let selected: std::collections::HashSet<u64> = selected_layers.iter().copied().collect();
    if selected.len() != selected_layers.len()
        || selected.iter().any(|id| !ids.contains(id))
        || active_layer.is_none() && !selected.is_empty()
        || active_layer.is_some_and(|id| !ids.contains(&id) || !selected.contains(&id))
        || layers
            .iter()
            .filter(|l| selected.contains(&l.id))
            .map(|l| l.id)
            .collect::<Vec<_>>()
            != selected_layers
    {
        return Err(bad());
    }
    Ok(PhotoSnapshotDocument {
        name: text(&m["name"], 256)?,
        size,
        layers,
        active_layer,
        selected_layers,
        dirty: boolean(&m["dirty"])?,
        can_undo: boolean(&m["canUndo"])?,
        can_redo: boolean(&m["canRedo"])?,
    })
}
fn parse_selection(v: &Value, d: &PhotoSnapshotDocument) -> Result<PhotoSnapshotSelection> {
    let m = fields(
        v,
        &[
            "present",
            "domain",
            "bounds",
            "boundsFormat",
            "coverageHash",
            "selectionStateId",
        ],
    )?;
    let present = boolean(&m["present"])?;
    let domain = array(&m["domain"], u32_number)?;
    let bounds = if m["bounds"].is_null() {
        None
    } else {
        Some(array(&m["bounds"], |v| {
            i32::try_from(v.as_i64().ok_or_else(bad)?).map_err(|_| bad())
        })?)
    };
    if domain != [0, 0, d.size[0], d.size[1]]
        || m["boundsFormat"] != "xyxyExclusive"
        || bounds.is_some_and(|[x0, y0, x1, y1]| {
            !present
                || x0 < 0
                || y0 < 0
                || x1 <= x0
                || y1 <= y0
                || x1 as u32 > d.size[0]
                || y1 as u32 > d.size[1]
        })
    {
        return Err(bad());
    }
    let coverage_hash = hash(&m["coverageHash"])?;
    if !present
        && coverage_hash
            != digest(&[
                b"seecut-coverage-v1\0",
                &d.size[0].to_le_bytes(),
                &d.size[1].to_le_bytes(),
                &[0],
            ])
    {
        return Err(bad());
    }
    let selection_state_id = identifier(&m["selectionStateId"], "selection-v1-")?;
    let mut h = Sha256::new();
    h.update(b"seecut-selection-state-v1\0");
    h.update(coverage_hash.as_bytes());
    h.update([u8::from(d.active_layer.is_some())]);
    h.update(d.active_layer.unwrap_or(0).to_le_bytes());
    h.update((d.selected_layers.len() as u32).to_le_bytes());
    for id in &d.selected_layers {
        h.update(id.to_le_bytes());
    }
    if selection_state_id != format!("selection-v1-{:x}", h.finalize()) {
        return Err(bad());
    }
    Ok(PhotoSnapshotSelection {
        present,
        domain,
        bounds,
        coverage_hash,
        selection_state_id,
    })
}
fn parse_view(
    v: &Value,
    i: &PhotoIdentity,
    viewport: u64,
    canvas: u64,
    size: [u32; 2],
) -> Result<PhotoSnapshotView> {
    let m = fields(
        v,
        &[
            "geometry",
            "visibleDocumentAabb",
            "clippedVisibleDocumentAabb",
            "aabbFormat",
            "viewStateId",
        ],
    )?;
    let g = fields(
        &m["geometry"],
        &[
            "rectPoints",
            "pixelsPerPoint",
            "zoom",
            "center",
            "flip",
            "rotation",
            "aspect",
            "displayCamera",
            "targetCamera",
            "visibleDocumentPolygon",
        ],
    )?;
    let geometry = PhotoSnapshotGeometry {
        rect_points: array(&g["rectPoints"], f32_number)?,
        pixels_per_point: f32_number(&g["pixelsPerPoint"])?,
        zoom: f32_number(&g["zoom"])?,
        center: array(&g["center"], f32_number)?,
        flip: boolean(&g["flip"])?,
        rotation: f32_number(&g["rotation"])?,
        aspect: f32_number(&g["aspect"])?,
        display_camera: array(&g["displayCamera"], f32_number)?,
        target_camera: array(&g["targetCamera"], f32_number)?,
        visible_document_polygon: array(&g["visibleDocumentPolygon"], |v| array(v, number))?,
    };
    if geometry.rect_points[2] <= 0.0
        || geometry.rect_points[3] <= 0.0
        || geometry.pixels_per_point <= 0.0
        || geometry.zoom <= 0.0
        || geometry.aspect <= 0.0
        || geometry.display_camera != geometry.target_camera
        || geometry.display_camera[0] <= 0.0
        || m["aabbFormat"] != "xyxyExclusive"
    {
        return Err(bad());
    }
    let aabb: [f64; 4] = array(&m["visibleDocumentAabb"], number)?;
    let clipped: [f64; 4] = array(&m["clippedVisibleDocumentAabb"], number)?;
    let p = &geometry.visible_document_polygon;
    let actual = [
        p.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
        p.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
        p.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
        p.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
    ];
    if aabb != actual
        || clipped
            != [
                aabb[0].clamp(0.0, f64::from(size[0])),
                aabb[1].clamp(0.0, f64::from(size[1])),
                aabb[2].clamp(0.0, f64::from(size[0])),
                aabb[3].clamp(0.0, f64::from(size[1])),
            ]
    {
        return Err(bad());
    }
    // Reproduce the actual FrozenGeometry struct field order and f32 serializer,
    // rather than hashing sorted Value keys or converting native IDs to f64.
    macro_rules! enc {
        ($v:expr) => {
            serde_json::to_string(&$v).map_err(|_| bad())?
        };
    }
    let bytes = format!(
        "{{\"rectPoints\":{},\"pixelsPerPoint\":{},\"zoom\":{},\"center\":{},\"flip\":{},\"rotation\":{},\"aspect\":{},\"displayCamera\":{},\"targetCamera\":{},\"visibleDocumentPolygon\":{}}}",
        enc!(geometry.rect_points),
        enc!(geometry.pixels_per_point),
        enc!(geometry.zoom),
        enc!(geometry.center),
        enc!(geometry.flip),
        enc!(geometry.rotation),
        enc!(geometry.aspect),
        enc!(geometry.display_camera),
        enc!(geometry.target_camera),
        enc!(geometry.visible_document_polygon)
    );
    let expected = digest(&[
        b"seecut-view-v1\0",
        &identity_bytes(i),
        &viewport.to_le_bytes(),
        &canvas.to_le_bytes(),
        bytes.as_bytes(),
    ]);
    let view_state_id = identifier(&m["viewStateId"], "view-v1-")?;
    if view_state_id != format!("view-v1-{expected}") {
        return Err(bad());
    }
    Ok(PhotoSnapshotView {
        geometry,
        visible_document_aabb: aabb,
        clipped_visible_document_aabb: clipped,
        view_state_id,
    })
}
fn parse_presentation(v: &Value) -> Result<PhotoSnapshotPresentation> {
    let m = fields(
        v,
        &["source", "colorSpace", "profileHash", "pixelSemantics"],
    )?;
    if m["colorSpace"] != "sRGB IEC61966-2.1"
        || m["pixelSemantics"] != "documentCompositeNotMonitorPixels"
    {
        return Err(bad());
    }
    let s = fields(
        &m["source"],
        &[
            "renderer",
            "displayKey",
            "rawPreviewKey",
            "complete",
            "displayIdentity",
            "sourceIsSrgb",
            "proof",
            "hdr",
            "channelComposite",
        ],
    )?;
    if integer(&s["rawPreviewKey"])? != 0
        || s["complete"] != true
        || s["sourceIsSrgb"] != true
        || s["proof"] != false
        || s["hdr"] != false
        || s["channelComposite"] != true
    {
        return Err(bad());
    }
    let renderer = match s["renderer"].as_str() {
        Some("cpu") => PhotoSnapshotRenderer::Cpu,
        Some("gpu") => PhotoSnapshotRenderer::Gpu,
        _ => return Err(bad()),
    };
    Ok(PhotoSnapshotPresentation {
        renderer,
        display_key: integer(&s["displayKey"])?,
        display_identity: boolean(&s["displayIdentity"])?,
        profile_hash: hash(&m["profileHash"])?,
    })
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    !c
}
fn check_png(png: &[u8], size: [u32; 2]) -> Result<()> {
    if png.len() > MAX_PNG || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(bad());
    }
    let mut at = 8;
    let mut header = false;
    let mut data = false;
    let mut ended = false;
    while at < png.len() {
        if png.len() - at < 12 {
            return Err(bad());
        }
        let len = u32::from_be_bytes(png[at..at + 4].try_into().map_err(|_| bad())?) as usize;
        let end = at
            .checked_add(12)
            .and_then(|v| v.checked_add(len))
            .filter(|v| *v <= png.len())
            .ok_or_else(bad)?;
        let kind = &png[at + 4..at + 8];
        let body = &png[at + 8..end - 4];
        if !kind.iter().all(u8::is_ascii_alphabetic)
            || crc32(&png[at + 4..end - 4])
                != u32::from_be_bytes(png[end - 4..end].try_into().map_err(|_| bad())?)
        {
            return Err(bad());
        }
        if !header && kind != b"IHDR" {
            return Err(bad());
        }
        match kind {
            b"IHDR" => {
                if header
                    || len != 13
                    || body[0..4] != size[0].to_be_bytes()
                    || body[4..8] != size[1].to_be_bytes()
                    || body[8..] != [8, 6, 0, 0, 0]
                {
                    return Err(bad());
                }
                header = true;
            }
            b"IDAT" => {
                if len == 0 {
                    return Err(bad());
                }
                data = true;
            }
            b"IEND" => {
                if len != 0 || !data || end != png.len() {
                    return Err(bad());
                }
                ended = true;
            }
            _ if kind[0].is_ascii_uppercase() && kind != b"PLTE" => return Err(bad()),
            _ => {}
        }
        at = end;
    }
    if !ended {
        return Err(bad());
    }
    Ok(())
}
fn parse_pixels(
    v: &Value,
    size: [u32; 2],
    max_edge: u32,
) -> Result<(PhotoSnapshotPixels, Vec<u8>)> {
    let m = fields(
        v,
        &[
            "kind",
            "transparent",
            "mime",
            "width",
            "height",
            "documentRect",
            "documentPixelsPerOutputPixel",
            "pixelHash",
            "pngHash",
            "base64",
        ],
    )?;
    let output = [u32_number(&m["width"])?, u32_number(&m["height"])?];
    let rect = array(&m["documentRect"], u32_number)?;
    let scale = array(&m["documentPixelsPerOutputPixel"], number)?;
    if m["kind"] != "documentComposite"
        || m["transparent"] != true
        || m["mime"] != "image/png"
        || output.iter().any(|n| *n == 0 || *n > max_edge || *n > 256)
        || rect != [0, 0, size[0], size[1]]
        || scale
            != [
                f64::from(size[0]) / f64::from(output[0]),
                f64::from(size[1]) / f64::from(output[1]),
            ]
    {
        return Err(bad());
    }
    let encoded = m["base64"]
        .as_str()
        .filter(|s| s.len() <= MAX_PNG.div_ceil(3) * 4)
        .ok_or_else(bad)?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| bad())?;
    check_png(&png, output)?;
    let png_hash = hash(&m["pngHash"])?;
    if digest(&[&png]) != png_hash {
        return Err(bad());
    }
    Ok((
        PhotoSnapshotPixels {
            size: output,
            document_rect: rect,
            document_pixels_per_output_pixel: scale,
            pixel_hash: hash(&m["pixelHash"])?,
            png_hash,
        },
        png,
    ))
}
fn parse_snapshot(
    v: &Value,
    request: &str,
    expected: &PhotoIdentity,
    max_edge: u32,
    request_started: Instant,
    received_at: Instant,
) -> Result<PhotoSnapshot> {
    if received_at < request_started {
        return Err(bad());
    }
    bounded(v)?;
    let m = fields(
        v,
        &[
            "kind",
            "adoptionStage",
            "snapshotId",
            "requestId",
            "capturedFrame",
            "capturedAfterUiEnqueueMs",
            "timeBasis",
            "returnedHistoricalFrame",
            "captureAgeAtReplyMs",
            "viewportId",
            "canvasId",
            "identity",
            "document",
            "selection",
            "view",
            "presentation",
            "pixels",
        ],
    )?;
    if m["kind"] != "adoptedDocumentComposite"
        || m["adoptionStage"] != "uiPaintSubmitted"
        || m["requestId"].as_str() != Some(request)
        || m["returnedHistoricalFrame"] != true
    {
        return Err(bad());
    }
    let time = fields(&m["timeBasis"], &["captureDelay", "captureAge"])?;
    if time["captureDelay"] != "uiEnqueue" || time["captureAge"] != "adoptedFrameCapture" {
        return Err(bad());
    }
    let identity = parse_identity(&m["identity"], expected)?;
    let captured_frame = integer(&m["capturedFrame"])?;
    let viewport_id = integer(&m["viewportId"])?;
    let canvas_id = integer(&m["canvasId"])?;
    let timing = PhotoSnapshotTiming {
        captured_after_ui_enqueue_ms: integer(&m["capturedAfterUiEnqueueMs"])?,
        capture_age_at_reply_ms: integer(&m["captureAgeAtReplyMs"])?,
    };
    if timing.captured_after_ui_enqueue_ms >= 2000 || timing.capture_age_at_reply_ms >= 5000 {
        return Err(bad());
    }
    let document = parse_document(&m["document"])?;
    let selection = parse_selection(&m["selection"], &document)?;
    let view = parse_view(&m["view"], &identity, viewport_id, canvas_id, document.size)?;
    let presentation = parse_presentation(&m["presentation"])?;
    let (pixels, png) = parse_pixels(&m["pixels"], document.size, max_edge)?;
    let snapshot_id = identifier(&m["snapshotId"], "snapshot-v1-")?;
    let expected_id = digest(&[
        b"seecut-snapshot-v1\0",
        request.as_bytes(),
        &identity_bytes(&identity),
        &captured_frame.to_le_bytes(),
        pixels.pixel_hash.as_bytes(),
        selection.selection_state_id.as_bytes(),
        view.view_state_id
            .strip_prefix("view-v1-")
            .ok_or_else(bad)?
            .as_bytes(),
    ]);
    if snapshot_id != format!("snapshot-v1-{expected_id}") {
        return Err(bad());
    }
    Ok(PhotoSnapshot {
        request_id: request.into(),
        snapshot_id,
        identity,
        captured_frame,
        viewport_id,
        canvas_id,
        timing,
        document,
        selection,
        view,
        presentation,
        pixels,
        png,
        request_started,
        received_at,
    })
}
fn parse_validation(
    v: &Value,
    request: &str,
    snapshot: &PhotoSnapshot,
    validation_started: Instant,
    received_at: Instant,
) -> Result<PhotoSnapshotValidation> {
    if validation_started < snapshot.received_at || received_at < validation_started {
        return Err(bad());
    }
    let capture_age_upper_bound_ms = u64::try_from(
        received_at
            .checked_duration_since(snapshot.request_started)
            .ok_or_else(bad)?
            .as_nanos()
            .div_ceil(1_000_000),
    )
    .map_err(|_| bad())?;
    bounded(v)?;
    let m = fields(
        v,
        &[
            "kind",
            "adoptionStage",
            "validationStage",
            "requestId",
            "snapshotId",
            "identity",
            "viewStateId",
            "selectionStateId",
            "validatedFrame",
            "captureAgeMs",
        ],
    )?;
    if m["kind"] != "adoptedSnapshotValidation"
        || m["adoptionStage"] != "uiPaintSubmitted"
        || m["validationStage"] != "postInputFrame"
        || m["requestId"].as_str() != Some(request)
        || m["snapshotId"].as_str() != Some(snapshot.snapshot_id())
        || m["viewStateId"].as_str() != Some(snapshot.view.view_state_id.as_str())
        || m["selectionStateId"].as_str() != Some(snapshot.selection.selection_state_id.as_str())
    {
        return Err(bad());
    }
    let identity = parse_identity(&m["identity"], snapshot.identity())?;
    let validated_frame = integer(&m["validatedFrame"])?;
    let capture_age_ms = integer(&m["captureAgeMs"])?;
    if validated_frame <= snapshot.captured_frame
        || capture_age_ms >= 5000
        || capture_age_ms < snapshot.timing.capture_age_at_reply_ms
        || capture_age_ms > capture_age_upper_bound_ms
    {
        return Err(bad());
    }
    Ok(PhotoSnapshotValidation {
        request_id: request.into(),
        snapshot_id: snapshot.snapshot_id.clone(),
        identity,
        view_state_id: snapshot.view.view_state_id.clone(),
        selection_state_id: snapshot.selection.selection_state_id.clone(),
        validated_frame,
        capture_age_ms,
        validation_started,
        received_at,
        capture_age_upper_bound_ms,
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "snapshot_tests.rs"]
mod tests;
