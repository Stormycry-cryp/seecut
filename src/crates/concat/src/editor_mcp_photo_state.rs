// SPDX-License-Identifier: AGPL-3.0-or-later
//! One bounded original metadata mapping. No grant, I/O, snapshot or tool registration.
//! Only trusted parent scope/owned-child facts and fresh typed native reads enter here.

use super::write_registry::Scope;
use concat_editor_mcp::{PhotoDocumentId, PhotoExpected, PhotoLayerId, PhotoStateId};
use seecut_photo_host::{
    PhotoDocument, PhotoIdentity, PhotoLayer, PhotoPathStatus, PhotoState, Uuid,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const MAX_LAYERS: usize = 256;
const MAX_NAME: usize = 1024;
const MAX_TOTAL_NAMES: usize = 64 * 1024;
const MAX_SCOPE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StateError {
    Exhausted,
    InvalidScope,
    InvalidRead,
    ResourceLimit,
    WrongOwnedChild,
    #[cfg(test)]
    NoEditTarget,
    #[cfg(test)]
    StaleContext,
    #[cfg(test)]
    UnknownExpected,
    #[cfg(test)]
    UnknownLayer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Unavailable {
    NoDocument,
    Busy,
    TruncatedLists,
    PixelSelectionCoverageUnknown,
    IncompleteLayerReferences,
    SavedBackingNotReady,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateReply {
    kind: &'static str,
    evidence_level: &'static str,
    atomic_pixels: bool,
    view: &'static str,
    pixel_selection_coverage: &'static str,
    layer_masks: &'static str,
    busy: bool,
    document: Option<DocumentReply>,
    edit_available: bool,
    // This is an original metadata binding hint. No edit tool is enabled.
    edit_target: Option<PhotoExpected>,
    edit_target_unavailable: Option<Unavailable>,
}
#[cfg(test)]
impl StateReply {
    pub(crate) fn edit_target(&self) -> Option<&PhotoExpected> {
        self.edit_target.as_ref()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentReply {
    document_id: PhotoDocumentId,
    state_id: PhotoStateId,
    name: String,
    size: [u32; 2],
    active_layer: Option<PhotoLayerId>,
    selected_layers: Vec<PhotoLayerId>,
    selection_list_truncated: bool,
    has_pixel_selection: bool,
    layers: Vec<LayerReply>,
    layer_list_may_be_truncated: bool,
    dirty: bool,
    can_undo: bool,
    can_redo: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LayerReply {
    layer_id: PhotoLayerId,
    name: String,
    opacity: f64,
    visible: bool,
}

// This bounded projection is shared by the actual PhotoDocument adapter and
// pure mapping tests. Private backing paths, frame/view and mask pixels are not
// projected. Known counters remain exact Rust u64, never response fields.
#[derive(Clone, PartialEq)]
struct Facts {
    identity: PhotoIdentity,
    name: String,
    size: [u32; 2],
    active: Option<u64>,
    selected: Vec<u64>,
    selection_truncated: bool,
    pixel_selection: bool,
    layers: Vec<PhotoLayer>,
    layers_truncated: bool,
    dirty: bool,
    can_undo: bool,
    can_redo: bool,
    #[cfg(test)]
    history_entries: u64,
    path_status: PhotoPathStatus,
    source_read_only: bool,
    saved_revision: u64,
}
impl Facts {
    fn from_native(d: &PhotoDocument) -> Result<Self, StateError> {
        // Check bounds before cloning user-controlled vectors or names.
        validate_lists(&d.name, &d.layers, &d.selected_layers)?;
        Ok(Self {
            identity: d.identity.clone(),
            name: d.name.clone(),
            size: d.size,
            active: d.active_layer,
            selected: d.selected_layers.clone(),
            selection_truncated: d.selection_list_truncated,
            pixel_selection: d.has_pixel_selection,
            layers: d.layers.clone(),
            layers_truncated: d.layer_list_may_be_truncated,
            dirty: d.dirty,
            can_undo: d.can_undo,
            can_redo: d.can_redo,
            #[cfg(test)]
            history_entries: d.history_entries,
            path_status: d.backing.path_status,
            source_read_only: d.backing.source_read_only,
            saved_revision: d.backing.saved_revision,
        })
    }
    fn validate(&self) -> Result<(), StateError> {
        validate_lists(&self.name, &self.layers, &self.selected)?;
        if self.identity.host_instance_id.is_nil()
            || self.identity.doc_id == 0
            || self.identity.document_generation == 0
            || self.identity.revision == 0
            || self.size.contains(&0)
            || self.active == Some(0)
            || self.selected.contains(&0)
            || self
                .layers
                .iter()
                .any(|l| l.id == 0 || !l.opacity.is_finite() || !(0.0..=1.0).contains(&l.opacity))
        {
            return Err(StateError::InvalidRead);
        }
        if self
            .selected
            .iter()
            .enumerate()
            .any(|(i, id)| self.selected[..i].contains(id))
            || self
                .layers
                .iter()
                .enumerate()
                .any(|(i, l)| self.layers[..i].iter().any(|old| old.id == l.id))
        {
            return Err(StateError::InvalidRead);
        }
        Ok(())
    }
    fn unavailable(&self, busy: bool) -> Option<Unavailable> {
        if busy {
            Some(Unavailable::Busy)
        } else if self.selection_truncated || self.layers_truncated {
            Some(Unavailable::TruncatedLists)
        } else if self.pixel_selection {
            Some(Unavailable::PixelSelectionCoverageUnknown)
        } else if self.path_status != PhotoPathStatus::Recorded
            || self.source_read_only
            || self.saved_revision == 0
        {
            Some(Unavailable::SavedBackingNotReady)
        } else if self.layers.is_empty()
            || self
                .active
                .is_some_and(|id| !self.layers.iter().any(|l| l.id == id))
            || self
                .selected
                .iter()
                .any(|id| !self.layers.iter().any(|l| l.id == *id))
        {
            Some(Unavailable::IncompleteLayerReferences)
        } else {
            None
        }
    }
}
fn validate_lists(name: &str, layers: &[PhotoLayer], selected: &[u64]) -> Result<(), StateError> {
    if layers.len() > MAX_LAYERS || selected.len() > MAX_LAYERS {
        return Err(StateError::ResourceLimit);
    }
    let mut total = 0usize;
    for text in std::iter::once(name).chain(layers.iter().map(|l| l.name.as_str())) {
        if text.len() > MAX_NAME {
            return Err(StateError::ResourceLimit);
        }
        if text.contains('\0') {
            return Err(StateError::InvalidRead);
        }
        total = total
            .checked_add(text.len())
            .ok_or(StateError::ResourceLimit)?;
        if total > MAX_TOTAL_NAMES {
            return Err(StateError::ResourceLimit);
        }
    }
    Ok(())
}
struct Projection {
    host: Uuid,
    _sampled_frame: u64,
    document: Option<Facts>,
    busy: bool,
}
impl Projection {
    fn from_native(state: &PhotoState) -> Result<Self, StateError> {
        Ok(Self {
            host: state.host_instance_id,
            _sampled_frame: state.frame,
            document: state
                .document
                .as_ref()
                .map(Facts::from_native)
                .transpose()?,
            busy: state.busy.is_some(),
        })
    }
}
// Future write-binding regression tests remain isolated from the production
// read adapter. They do not install a write registry or enable an edit tool.
#[cfg(test)]
struct LayerMapping {
    handle: PhotoLayerId,
    native: u64,
}
#[cfg(test)]
struct Mapping {
    scope: Scope,
    child: Uuid,
    facts: Facts,
    expected: PhotoExpected,
    layers: Vec<LayerMapping>,
}
/// Bounded opaque references for one original metadata observation. Each new
/// admitted read may invalidate its namespace; references never retarget.
pub(crate) struct StateMap {
    namespace: Uuid,
    counter: u64,
    exhausted: bool,
    #[cfg(test)]
    edit: Option<Mapping>,
}
impl StateMap {
    pub(crate) fn new() -> Self {
        Self {
            namespace: Uuid::new_v4(),
            counter: 0,
            exhausted: false,
            #[cfg(test)]
            edit: None,
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.namespace = Uuid::new_v4();
        #[cfg(test)]
        {
            self.edit = None;
        }
    }
    fn begin_observe(&mut self) -> Result<(), StateError> {
        // Clear before projection/validation, including invalid/missing reads.
        #[cfg(test)]
        {
            self.edit = None;
        }
        if self.exhausted {
            return Err(StateError::Exhausted);
        }
        self.counter = self.counter.checked_add(1).ok_or_else(|| {
            self.exhausted = true;
            StateError::Exhausted
        })?;
        Ok(())
    }
    /// Called only after the original read gate. It does not check that gate.
    /// Every accepted observation gets a new state handle. References describe
    /// only that sampled metadata and provide no current edit authority.
    pub(crate) fn observe(
        &mut self,
        scope: &Scope,
        child: Uuid,
        state: &PhotoState,
    ) -> Result<StateReply, StateError> {
        self.begin_observe()?;
        let projection = Projection::from_native(state)?;
        self.install(scope, child, projection)
    }
    fn install(
        &mut self,
        scope: &Scope,
        child: Uuid,
        projection: Projection,
    ) -> Result<StateReply, StateError> {
        validate_scope(scope)?;
        if child.is_nil() || projection.host != child {
            return Err(StateError::WrongOwnedChild);
        }
        let mut reply = StateReply {
            kind: "photoMetadataState",
            evidence_level: "metadataOnly",
            atomic_pixels: false,
            view: "unknown",
            pixel_selection_coverage: "unknown",
            layer_masks: "unknown",
            busy: projection.busy,
            document: None,
            edit_available: false,
            edit_target: None,
            edit_target_unavailable: Some(Unavailable::NoDocument),
        };
        let Some(facts) = projection.document else {
            return Ok(reply);
        };
        facts.validate()?;
        if facts.identity.host_instance_id != child {
            return Err(StateError::WrongOwnedChild);
        }
        let document_id = self.document_handle(scope, child, &facts.identity)?;
        let mut hash = Sha256::new();
        hash.update(b"seecut-photo-state-mapping-v1\0");
        hash.update(self.namespace.as_bytes());
        lp(&mut hash, document_id.as_str());
        hash.update(self.counter.to_le_bytes());
        hash.update(facts.identity.revision.to_le_bytes());
        // This ID distinguishes the original sampled observation; it is not a
        // native revision or permission to apply a later edit.
        let state_id = PhotoStateId::parse(&format!("photo-state-v1-{:x}", hash.finalize()))
            .map_err(|_| StateError::InvalidRead)?;
        let expected = PhotoExpected {
            document_id: document_id.clone(),
            state_id: state_id.clone(),
        };
        let mut layers = Vec::with_capacity(facts.layers.len());
        #[cfg(test)]
        let mut mapped = Vec::with_capacity(facts.layers.len());
        for layer in &facts.layers {
            let handle = layer_handle(&document_id, layer.id)?;
            layers.push(LayerReply {
                layer_id: handle.clone(),
                name: layer.name.clone(),
                opacity: layer.opacity,
                visible: layer.visible,
            });
            #[cfg(test)]
            mapped.push(LayerMapping {
                handle,
                native: layer.id,
            });
        }
        let unavailable = facts.unavailable(projection.busy);
        reply.document = Some(DocumentReply {
            document_id: document_id.clone(),
            state_id,
            name: facts.name.clone(),
            size: facts.size,
            active_layer: facts
                .active
                .map(|id| layer_handle(&document_id, id))
                .transpose()?,
            selected_layers: facts
                .selected
                .iter()
                .map(|id| layer_handle(&document_id, *id))
                .collect::<Result<_, _>>()?,
            selection_list_truncated: facts.selection_truncated,
            has_pixel_selection: facts.pixel_selection,
            layers,
            layer_list_may_be_truncated: facts.layers_truncated,
            dirty: facts.dirty,
            can_undo: facts.can_undo,
            can_redo: facts.can_redo,
        });
        reply.edit_target_unavailable = unavailable;
        if unavailable.is_none() {
            reply.edit_target = Some(expected.clone());
            #[cfg(test)]
            {
                self.edit = Some(Mapping {
                    scope: scope.clone(),
                    child,
                    facts,
                    expected,
                    layers: mapped,
                });
            }
        }
        Ok(reply)
    }
    fn document_handle(
        &self,
        scope: &Scope,
        child: Uuid,
        identity: &PhotoIdentity,
    ) -> Result<PhotoDocumentId, StateError> {
        let mut hash = Sha256::new();
        hash.update(b"seecut-photo-document-mapping-v1\0");
        hash.update(self.namespace.as_bytes());
        for text in [&scope.instance, &scope.project, &scope.session] {
            lp(&mut hash, text);
        }
        hash.update(child.as_bytes());
        hash.update(identity.doc_id.to_le_bytes());
        hash.update(identity.document_generation.to_le_bytes());
        PhotoDocumentId::parse(&format!("photo-document-v1-{:x}", hash.finalize()))
            .map_err(|_| StateError::InvalidRead)
    }
}

#[cfg(test)]
impl StateMap {
    /// Resolve an original edit binding only after a new trusted native read.
    /// The return is the original native expected, never adopted from fresh.
    pub(crate) fn resolve_edit_expected(
        &mut self,
        scope: &Scope,
        child: Uuid,
        expected: &PhotoExpected,
        fresh: &PhotoState,
    ) -> Result<PhotoIdentity, StateError> {
        let fresh = self.fresh_projection(fresh)?;
        self.resolve_expected_projected(scope, child, expected, &fresh)
    }
    pub(crate) fn resolve_edit_layer(
        &mut self,
        scope: &Scope,
        child: Uuid,
        expected: &PhotoExpected,
        layer: &PhotoLayerId,
        fresh: &PhotoState,
    ) -> Result<u64, StateError> {
        let fresh = self.fresh_projection(fresh)?;
        self.resolve_layer_projected(scope, child, expected, layer, &fresh)
    }
    fn fresh_projection(&mut self, fresh: &PhotoState) -> Result<Projection, StateError> {
        match Projection::from_native(fresh) {
            Ok(projected) => Ok(projected),
            Err(error) => {
                self.invalidate();
                Err(error)
            }
        }
    }
    fn resolve_expected_projected(
        &mut self,
        scope: &Scope,
        child: Uuid,
        expected: &PhotoExpected,
        fresh: &Projection,
    ) -> Result<PhotoIdentity, StateError> {
        self.validate_current(scope, child, expected, fresh)?;
        Ok(self
            .edit
            .as_ref()
            .ok_or(StateError::NoEditTarget)?
            .facts
            .identity
            .clone())
    }
    fn resolve_layer_projected(
        &mut self,
        scope: &Scope,
        child: Uuid,
        expected: &PhotoExpected,
        layer: &PhotoLayerId,
        fresh: &Projection,
    ) -> Result<u64, StateError> {
        self.validate_current(scope, child, expected, fresh)?;
        self.edit
            .as_ref()
            .ok_or(StateError::NoEditTarget)?
            .layers
            .iter()
            .find(|mapped| &mapped.handle == layer)
            .map(|mapped| mapped.native)
            .ok_or(StateError::UnknownLayer)
    }
    fn validate_current(
        &mut self,
        scope: &Scope,
        child: Uuid,
        expected: &PhotoExpected,
        fresh: &Projection,
    ) -> Result<(), StateError> {
        let original = self.edit.as_ref().ok_or(StateError::NoEditTarget)?;
        if validate_scope(scope).is_err()
            || original.scope != *scope
            || original.child != child
            || fresh.host != child
            || fresh.busy
            || fresh.document.as_ref() != Some(&original.facts)
        {
            self.invalidate();
            return Err(StateError::StaleContext);
        }
        if &original.expected != expected {
            return Err(StateError::UnknownExpected);
        }
        Ok(())
    }
}
fn validate_scope(scope: &Scope) -> Result<(), StateError> {
    if [&scope.instance, &scope.project, &scope.session]
        .iter()
        .any(|s| s.is_empty() || s.len() > MAX_SCOPE || s.contains('\0'))
    {
        Err(StateError::InvalidScope)
    } else {
        Ok(())
    }
}
fn lp(hash: &mut Sha256, text: &str) {
    // Only validated <=1024-byte Scope strings or fixed-size handles enter here.
    hash.update((text.len() as u32).to_le_bytes());
    hash.update(text.as_bytes());
}
fn layer_handle(document: &PhotoDocumentId, native: u64) -> Result<PhotoLayerId, StateError> {
    let mut hash = Sha256::new();
    hash.update(b"seecut-photo-layer-mapping-v1\0");
    lp(&mut hash, document.as_str());
    hash.update(native.to_le_bytes());
    PhotoLayerId::parse(&format!("photo-layer-v1-{:x}", hash.finalize()))
        .map_err(|_| StateError::InvalidRead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn scope() -> Scope {
        Scope {
            instance: "parent".into(),
            project: "original-project".into(),
            session: "original-session".into(),
        }
    }
    fn facts(child: Uuid) -> Facts {
        Facts {
            identity: PhotoIdentity {
                host_instance_id: child,
                doc_id: u64::MAX,
                document_generation: u64::MAX - 1,
                revision: u64::MAX - 2,
            },
            name: "untrusted document".into(),
            size: [256, 128],
            active: Some(u64::MAX - 3),
            selected: vec![u64::MAX - 3],
            selection_truncated: false,
            pixel_selection: false,
            layers: vec![PhotoLayer {
                id: u64::MAX - 3,
                name: "untrusted layer".into(),
                opacity: 0.4,
                visible: true,
            }],
            layers_truncated: false,
            dirty: true,
            can_undo: true,
            can_redo: false,
            history_entries: u64::MAX,
            path_status: PhotoPathStatus::Recorded,
            source_read_only: false,
            saved_revision: u64::MAX - 4,
        }
    }
    fn projection(child: Uuid, facts: Option<Facts>, frame: u64, busy: bool) -> Projection {
        Projection {
            host: child,
            _sampled_frame: frame,
            document: facts,
            busy,
        }
    }
    fn observe(
        map: &mut StateMap,
        scope: &Scope,
        child: Uuid,
        facts: Option<Facts>,
        busy: bool,
    ) -> Result<StateReply, StateError> {
        map.begin_observe()?;
        map.install(scope, child, projection(child, facts, 10, busy))
    }
    fn prepared() -> (StateMap, Scope, Uuid, Facts, PhotoExpected, PhotoLayerId) {
        let mut map = StateMap::new();
        let scope = scope();
        let child = Uuid::new_v4();
        let facts = facts(child);
        let reply = observe(&mut map, &scope, child, Some(facts.clone()), false).unwrap();
        let expected = reply.edit_target().unwrap().clone();
        let layer = reply.document.unwrap().layers.remove(0).layer_id;
        (map, scope, child, facts, expected, layer)
    }

    #[test]
    fn seecut_photo_state_preserves_native_u64_and_response_has_no_native_counters() {
        let (mut map, scope, child, facts, expected, layer) = prepared();
        let fresh = projection(child, Some(facts.clone()), 11, false);
        assert_eq!(
            map.resolve_expected_projected(&scope, child, &expected, &fresh)
                .unwrap(),
            facts.identity
        );
        assert_eq!(
            map.resolve_layer_projected(&scope, child, &expected, &layer, &fresh)
                .unwrap(),
            u64::MAX - 3
        );
        let reply = observe(&mut map, &scope, child, Some(facts), false).unwrap();
        let wire = serde_json::to_value(reply).unwrap();
        assert_eq!(wire["evidenceLevel"], "metadataOnly");
        assert_eq!(wire["atomicPixels"], false);
        assert_eq!(wire["editAvailable"], false);
        for field in ["view", "pixelSelectionCoverage", "layerMasks"] {
            assert_eq!(wire[field], "unknown");
        }
        assert_eq!(wire["document"]["size"], json!([256, 128]));
        assert!(wire["document"]["documentId"].is_string());
        assert!(wire["document"]["selectedLayers"][0].is_string());
        assert!(wire["document"]["layers"][0]["layerId"].is_string());
        let text = serde_json::to_string(&wire).unwrap();
        for native in [
            u64::MAX,
            u64::MAX - 1,
            u64::MAX - 2,
            u64::MAX - 3,
            u64::MAX - 4,
        ] {
            assert!(!text.contains(&native.to_string()));
        }
        fn numbers(value: &Value) {
            match value {
                Value::Number(n) if n.is_u64() => {
                    assert!(n.as_u64().unwrap() <= u64::from(u32::MAX))
                }
                Value::Array(a) => a.iter().for_each(numbers),
                Value::Object(m) => m.values().for_each(numbers),
                _ => {}
            }
        }
        numbers(&wire);
    }

    #[test]
    fn seecut_photo_state_idle_frame_growth_keeps_original_expected_and_layer() {
        let (mut map, scope, child, facts, expected, layer) = prepared();
        let original = projection(child, Some(facts.clone()), 10, false);
        let fresh = projection(child, Some(facts.clone()), u64::MAX, false);
        assert!(fresh._sampled_frame > original._sampled_frame);
        assert_eq!(
            map.resolve_expected_projected(&scope, child, &expected, &fresh)
                .unwrap(),
            facts.identity
        );
        assert_eq!(
            map.resolve_layer_projected(&scope, child, &expected, &layer, &fresh)
                .unwrap(),
            u64::MAX - 3
        );
    }

    #[test]
    fn seecut_photo_state_metadata_only_and_actual_empty_typed_reads_clear_old_target() {
        for case in 0..8 {
            let (mut map, scope, child, mut facts, expected, _) = prepared();
            let mut busy = false;
            match case {
                0 => busy = true,
                1 => facts.selection_truncated = true,
                2 => facts.layers_truncated = true,
                3 => facts.pixel_selection = true,
                4 => facts.saved_revision = 0,
                5 => facts.source_read_only = true,
                6 => facts.path_status = PhotoPathStatus::Missing,
                _ => facts.selected = vec![9],
            }
            let reply = observe(&mut map, &scope, child, Some(facts.clone()), busy).unwrap();
            assert!(reply.edit_target().is_none());
            assert!(map.edit.is_none());
            assert!(reply.edit_target_unavailable.is_some());
            assert_eq!(
                map.resolve_expected_projected(
                    &scope,
                    child,
                    &expected,
                    &projection(child, Some(facts), 11, false)
                ),
                Err(StateError::NoEditTarget)
            );
        }
        let (mut map, scope, child, _, expected, layer) = prepared();
        let empty = PhotoState {
            host_instance_id: child,
            frame: u64::MAX,
            document: None,
            busy: None,
        };
        assert!(
            map.observe(&scope, child, &empty)
                .unwrap()
                .document
                .is_none()
        );
        assert_eq!(
            map.resolve_edit_expected(&scope, child, &expected, &empty),
            Err(StateError::NoEditTarget)
        );
        assert_eq!(
            map.resolve_edit_layer(&scope, child, &expected, &layer, &empty),
            Err(StateError::NoEditTarget)
        );
    }

    #[test]
    fn seecut_photo_state_fresh_context_or_semantic_changes_invalidate_without_retargeting() {
        for change in 0..12 {
            let (mut map, scope, child, facts, expected, _) = prepared();
            let mut current_scope = scope.clone();
            let mut current_child = child;
            let mut changed = facts.clone();
            let mut busy = false;
            match change {
                0 => current_scope.instance.push('x'),
                1 => current_scope.project.push('x'),
                2 => current_scope.session.push('x'),
                3 => current_child = Uuid::new_v4(),
                4 => changed.identity.doc_id -= 1,
                5 => changed.identity.document_generation -= 1,
                6 => changed.identity.revision -= 1,
                7 => changed.selected.clear(),
                8 => changed.active = None,
                9 => changed.layers[0].opacity = 0.6,
                10 => changed.saved_revision -= 1,
                _ => busy = true,
            }
            assert_eq!(
                map.resolve_expected_projected(
                    &current_scope,
                    current_child,
                    &expected,
                    &projection(child, Some(changed), 11, busy)
                ),
                Err(StateError::StaleContext)
            );
            assert!(map.edit.is_none());
            // Returning to the old terminal facts cannot revive a mapping that
            // this store actually saw become stale. No fresh read is installed.
            assert_eq!(
                map.resolve_expected_projected(
                    &scope,
                    child,
                    &expected,
                    &projection(child, Some(facts), 12, false)
                ),
                Err(StateError::NoEditTarget)
            );
        }
    }

    #[test]
    fn seecut_photo_state_observed_a_b_a_uses_new_state_and_unknown_handles_never_guess() {
        let (mut map, scope, child, facts, old, layer) = prepared();
        let mut b = facts.clone();
        b.selected.clear();
        let b_reply = observe(&mut map, &scope, child, Some(b), false).unwrap();
        let b_id = b_reply.edit_target().unwrap().clone();
        let a_reply = observe(&mut map, &scope, child, Some(facts.clone()), false).unwrap();
        let current = a_reply.edit_target().unwrap().clone();
        assert_eq!(old.document_id, current.document_id);
        assert_ne!(old.state_id, current.state_id);
        assert_ne!(b_id.state_id, current.state_id);
        let fresh = projection(child, Some(facts), 999, false);
        assert_eq!(
            map.resolve_expected_projected(&scope, child, &old, &fresh),
            Err(StateError::UnknownExpected)
        );
        assert_eq!(
            map.resolve_expected_projected(&scope, child, &b_id, &fresh),
            Err(StateError::UnknownExpected)
        );
        let foreign = PhotoLayerId::parse(&format!("photo-layer-v1-{}", "b".repeat(64))).unwrap();
        assert_eq!(
            map.resolve_layer_projected(&scope, child, &current, &foreign, &fresh),
            Err(StateError::UnknownLayer)
        );
        assert_eq!(
            map.resolve_layer_projected(&scope, child, &current, &layer, &fresh)
                .unwrap(),
            u64::MAX - 3
        );
    }

    #[test]
    fn seecut_photo_state_handles_cannot_cross_parent_project_or_session_scope() {
        let (mut map, original_scope, child, facts, old_expected, old_layer) = prepared();
        let original_document = old_expected.document_id.clone();
        for change in 0..3 {
            let mut scope = original_scope.clone();
            match change {
                0 => scope.instance.push_str("-replacement"),
                1 => scope.project.push_str("-replacement"),
                _ => scope.session.push_str("-replacement"),
            }
            let reply = observe(&mut map, &scope, child, Some(facts.clone()), false).unwrap();
            let expected = reply.edit_target().unwrap().clone();
            assert_ne!(expected.document_id, original_document);
            let fresh = projection(child, Some(facts.clone()), 20, false);
            assert_eq!(
                map.resolve_expected_projected(&scope, child, &old_expected, &fresh),
                Err(StateError::UnknownExpected)
            );
            assert_eq!(
                map.resolve_layer_projected(&scope, child, &expected, &old_layer, &fresh),
                Err(StateError::UnknownLayer)
            );
        }
    }

    #[test]
    fn seecut_photo_state_explicit_invalidation_never_reissues_old_handles() {
        let (mut map, scope, child, facts, original, original_layer) = prepared();
        map.invalidate();
        let reply = observe(&mut map, &scope, child, Some(facts), false).unwrap();
        let fresh = reply.edit_target().unwrap();
        assert_ne!(fresh.document_id, original.document_id);
        assert_ne!(fresh.state_id, original.state_id);
        assert_ne!(
            reply.document.as_ref().unwrap().layers[0].layer_id,
            original_layer
        );
        assert!(!reply.edit_available);
    }

    #[test]
    fn seecut_photo_state_invalid_new_reads_and_capacity_clear_before_validation() {
        for case in 0..10 {
            let (mut map, mut scope, child, mut facts, _, _) = prepared();
            let mut owned = child;
            match case {
                0 => scope.session.clear(),
                1 => scope.project = "x".repeat(MAX_SCOPE + 1),
                2 => owned = Uuid::new_v4(),
                3 => facts.layers = vec![facts.layers[0].clone(); MAX_LAYERS + 1],
                4 => facts.selected = vec![1; MAX_LAYERS + 1],
                5 => facts.name = "x".repeat(MAX_NAME + 1),
                6 => facts.layers[0].opacity = f64::NAN,
                7 => facts.selected.push(facts.selected[0]),
                8 => facts.layers.push(facts.layers[0].clone()),
                _ => {
                    facts.layers = (1..=65)
                        .map(|id| PhotoLayer {
                            id,
                            name: "x".repeat(MAX_NAME),
                            opacity: 0.5,
                            visible: true,
                        })
                        .collect();
                }
            }
            assert!(observe(&mut map, &scope, owned, Some(facts), false).is_err());
            assert!(map.edit.is_none());
        }
        let (mut map, scope, child, facts, _, _) = prepared();
        let mut projection = projection(child, Some(facts), 10, false);
        projection.host = Uuid::new_v4();
        map.begin_observe().unwrap();
        assert!(matches!(
            map.install(&scope, child, projection),
            Err(StateError::WrongOwnedChild)
        ));
        assert!(map.edit.is_none());
        // The maximum accepted lists and worst JSON name escaping still fit
        // below the bounded native response budget; no native counters appear.
        let mut maximum = self::facts(child);
        maximum.name = "\u{1}".repeat(MAX_NAME);
        maximum.layers = (1..=MAX_LAYERS as u64)
            .map(|id| PhotoLayer {
                id,
                name: if id <= 63 {
                    "\u{1}".repeat(MAX_NAME)
                } else {
                    String::new()
                },
                opacity: 1.0,
                visible: true,
            })
            .collect();
        maximum.active = Some(1);
        maximum.selected = (1..=MAX_LAYERS as u64).collect();
        let reply = observe(&mut map, &scope, child, Some(maximum), false).unwrap();
        assert!(reply.edit_target().is_some());
        assert!(serde_json::to_vec(&reply).unwrap().len() <= 512 * 1024);
    }

    #[test]
    fn seecut_photo_state_checked_counter_exhaustion_is_permanent() {
        let (mut map, scope, child, facts, _, _) = prepared();
        map.counter = u64::MAX;
        assert!(matches!(
            observe(&mut map, &scope, child, Some(facts.clone()), false),
            Err(StateError::Exhausted)
        ));
        assert!(map.edit.is_none());
        assert!(map.exhausted);
        // Even an accidental later counter reset cannot reopen this store.
        map.counter = 0;
        assert!(matches!(
            observe(&mut map, &scope, child, Some(facts), false),
            Err(StateError::Exhausted)
        ));
        assert!(map.edit.is_none());
    }
}
