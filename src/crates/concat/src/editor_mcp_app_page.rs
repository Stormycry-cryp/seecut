// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, pathless snapshots of already-rendered App page models.
//! No filesystem, network, credentials, edits, or automatic scope handoff.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_ITEMS: usize = 2_048;
const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
const MAX_REPLY_BYTES: usize = 48 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PageId {
    Generation,
    PersonalLibrary,
    TeamLibrary,
    Templates,
    Account,
    Credits,
    Settings,
    Projects,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenPage {
    pub app_session_id: String,
    pub page_id: PageId,
    pub page_epoch: u64,
    pub page_state_id: String,
}

pub(crate) use crate::agent_identity::AppIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Image,
    Video,
    Audio,
    Template,
    Inspiration,
    Project,
    LedgerEntry,
    CreditPlan,
    Model,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    Available,
    Missing,
    Unknown,
}

/// Closed page-specific display fields; never accepts arbitrary source JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Detail {
    Media {
        favorite: bool,
        failed: bool,
        refillable: bool,
        date_label: String,
    },
    Template {
        mode_label: String,
        model_name: String,
        parameter_summary: String,
        reference_summary: String,
        updated_label: String,
        prompt_preview: String,
    },
    Inspiration {
        category: i32,
        purpose: String,
        parameter_summary: String,
        prompt_preview: String,
    },
    Project {
        project_kind: &'static str,
        updated_label: String,
    },
    Account {
        value: String,
        date_label: String,
    },
    Plan {
        price: String,
        credits: String,
    },
    Model {
        installed: bool,
        active: bool,
        downloading: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub item_id: String,
    pub kind: Kind,
    pub name: String,
    pub name_truncated: bool,
    pub status: String,
    pub selected: bool,
    pub availability: Availability,
    pub preview_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    // Public opaque version, not a source image pathname or content claim.
    pub preview_version: u64,
}

/// Host-only input: source_key may be a path, but is never serializable.
pub struct CachedItem<P> {
    pub source_key: String,
    pub kind: Kind,
    pub name: String,
    pub status: String,
    pub selected: bool,
    pub availability: Availability,
    pub project: bool,
    pub detail: Option<Detail>,
    /// Only an already-loaded UI image handle; never a path or download request.
    pub preview: Option<(PreviewSource, P)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewSource {
    Image,
    Thumbnail,
}

#[derive(Clone)]
struct Retained<P> {
    source_key: String,
    item: Item,
    preview: Option<(PreviewSource, P)>,
    full_name_sha256: String,
    full_name_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub page_id: PageId,
    pub page_epoch: u64,
    pub page_state_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub scope_kind: &'static str,
    pub app_instance_id: String,
    pub client_id: String,
    pub account_scope: String,
    pub app_session_id: String,
    pub page: Page,
    /// Ordinary readback identity; never a user-view authorization stamp.
    pub content_state_id: String,
    pub current_preview: Option<CurrentPreview>,
    pub list_scope: &'static str,
    pub viewport: Value,
    pub focus: Value,
    pub view: Value,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentPreview {
    pub item_id: String,
    pub preview_version: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Items {
    #[serde(flatten)]
    pub context: Context,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub items: Vec<Item>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    SignedOut,
    UnsupportedPage,
    StalePage,
    ResourceLimit,
    InvalidQuery,
    PreviewUnavailable,
}
impl Error {
    /// Closed native read refusal status.
    pub fn status(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::SignedOut => "notAuthorized",
            Self::UnsupportedPage => "unsupportedPage",
            Self::StalePage => "stalePage",
            Self::ResourceLimit => "resourceLimit",
            Self::InvalidQuery => "invalidInput",
            Self::PreviewUnavailable => "unsupportedPreview",
        }
    }
}

/// Reject unsupported overlays and sensitive native work before any page data.
pub fn read_gate(
    unsupported_overlay: bool,
    exit_pending: bool,
    native_menu: bool,
    payment_busy: bool,
    auth_busy: bool,
) -> Result<(), Error> {
    if unsupported_overlay || exit_pending || native_menu || payment_busy || auth_busy {
        Err(Error::Busy)
    } else {
        Ok(())
    }
}

/// One host window owns this lifetime, shared by all named client registries.
#[derive(Default)]
struct AccountLifetime {
    account: Option<(u64, String)>,
    app_session: String,
}
#[derive(Default)]
pub struct Navigation {
    account: RefCell<AccountLifetime>,
    page_epoch: Cell<u64>,
    exhausted: Cell<bool>,
}
impl Navigation {
    /// Lightweight account identity only; does not capture or hash page rows.
    pub fn identity(&self, instance_id: String) -> Result<(u64, AppIdentity), Error> {
        if self.exhausted.get() {
            return Err(Error::ResourceLimit);
        }
        let current = self.account.try_borrow().map_err(|_| Error::Busy)?;
        let (epoch, scope) = current.account.as_ref().ok_or(Error::SignedOut)?;
        Ok((
            *epoch,
            AppIdentity {
                instance_id,
                account_scope: scope.clone(),
                app_session_id: current.app_session.clone(),
            },
        ))
    }
    pub fn account(&self, trusted: Option<(u64, String)>) -> Result<(), Error> {
        let mut current = self.account.try_borrow_mut().map_err(|_| Error::Busy)?;
        if current.account != trusted {
            current.account = trusted;
            current.app_session = if current.account.is_some() {
                uuid::Uuid::new_v4().to_string()
            } else {
                String::new()
            };
            self.navigation()?;
        }
        current.account.as_ref().ok_or(Error::SignedOut).map(|_| ())
    }
    /// Actual event hook; no model serialization or hashing in this method.
    pub fn navigation(&self) -> Result<(), Error> {
        let next = self
            .page_epoch
            .get()
            .checked_add(1)
            .filter(|n| *n <= MAX_SAFE_INTEGER)
            .ok_or_else(|| {
                self.exhausted.set(true);
                Error::ResourceLimit
            })?;
        self.page_epoch.set(next);
        Ok(())
    }
}

/// One instance/client owns one registry. Navigation is an event, not polling.
pub struct AppPages<P> {
    instance: String,
    client: String,
    navigation: Rc<Navigation>,
    captured_lifetime: Option<(String, u64)>,
    preview_version: u64,
    current: Option<Context>,
    items: Vec<Retained<P>>,
    auxiliary_preview: Option<Retained<P>>,
    input_previews: Vec<(String, Option<P>, u64)>,
}

impl<P: Clone + PartialEq> AppPages<P> {
    #[cfg(test)]
    pub fn new(instance: String, client: String) -> Self {
        Self::with_navigation(instance, client, Rc::new(Navigation::default()))
    }
    pub fn with_navigation(instance: String, client: String, navigation: Rc<Navigation>) -> Self {
        Self {
            instance,
            client,
            navigation,
            captured_lifetime: None,
            preview_version: 0,
            current: None,
            items: Vec::new(),
            auxiliary_preview: None,
            input_previews: Vec::new(),
        }
    }

    /// Must run on actual account epoch changes, before serving old credentials.
    pub fn account(&mut self, trusted: Option<(u64, String)>) -> Result<(), Error> {
        self.navigation.account(trusted)
    }

    /// Called for every real page/gallery/settings navigation, including A→B→A.
    #[cfg(test)]
    pub fn navigation(&mut self) -> Result<(), Error> {
        self.current = None;
        self.items.clear();
        self.navigation.navigation()
    }

    /// Failure to extract a current page must invalidate a previous projection.
    pub fn invalidate_capture(&mut self) {
        self.current = None;
    }

    /// Retain at most one actually opened preview, separate from filtered rows.
    /// This allows a reference/full-size preview without claiming it is in the list.
    pub fn set_auxiliary_preview(
        &mut self,
        row: Option<CachedItem<P>>,
    ) -> Result<Option<String>, Error> {
        let (_, session, epoch) = self.lifetime()?;
        if self.captured_lifetime.as_ref() != Some(&(session, epoch)) {
            self.auxiliary_preview = None;
        }
        let Some(row) = row else {
            self.auxiliary_preview = None;
            return Ok(None);
        };
        if row.source_key.is_empty() || row.source_key.len() > 16_384 {
            return Err(Error::ResourceLimit);
        }
        let old = self
            .auxiliary_preview
            .as_ref()
            .filter(|old| old.source_key == row.source_key);
        let item_id = old
            .map(|old| old.item.item_id.clone())
            .unwrap_or_else(|| format!("app-item-{}", uuid::Uuid::new_v4()));
        let preview_version = if old.is_some_and(|old| old.preview == row.preview) {
            old.unwrap().item.preview_version
        } else {
            self.preview_version = self
                .preview_version
                .checked_add(1)
                .filter(|n| *n <= MAX_SAFE_INTEGER)
                .ok_or(Error::ResourceLimit)?;
            self.preview_version
        };
        let key = row.source_key.clone();
        let (name, name_truncated, full_name_sha256) = display_name(&row.name)?;
        self.auxiliary_preview = Some(Retained {
            source_key: row.source_key,
            item: Item {
                item_id,
                kind: row.kind,
                name,
                name_truncated,
                status: text(&row.status, 256)?,
                selected: true,
                availability: row.availability,
                preview_available: row.preview.is_some(),
                project_entry_id: None,
                detail: row.detail,
                preview_version,
            },
            preview: row.preview,
            full_name_sha256,
            full_name_bytes: row.name.len(),
        });
        Ok(Some(key))
    }

    fn lifetime(&self) -> Result<(String, String, u64), Error> {
        let nav = &self.navigation;
        let current = nav.account.try_borrow().map_err(|_| Error::Busy)?;
        if nav.exhausted.get() {
            return Err(Error::ResourceLimit);
        }
        let scope = current.account.as_ref().ok_or(Error::SignedOut)?.1.clone();
        Ok((scope, current.app_session.clone(), nav.page_epoch.get()))
    }

    /// Detect navigation/account changes during a request-driven UI extraction.
    pub fn navigation_stamp(&self) -> Result<(String, String, u64), Error> {
        self.lifetime()
    }

    /// Version existing reference-image handles without re-opening source media.
    /// These are cached-view versions, never file hashes or AssetVersion claims.
    pub fn reference_versions(
        &mut self,
        inputs: Vec<(String, Option<P>)>,
    ) -> Result<Vec<u64>, Error> {
        if inputs.len() > 50 {
            return Err(Error::ResourceLimit);
        }
        let (_, session, epoch) = self.lifetime()?;
        if self.captured_lifetime.as_ref() != Some(&(session, epoch)) {
            self.input_previews.clear();
        }
        let mut next = Vec::new();
        let mut seen = HashSet::new();
        let old_inputs = self
            .input_previews
            .iter()
            .map(|old| (old.0.as_str(), old))
            .collect::<HashMap<_, _>>();
        for (id, preview) in inputs {
            if id.is_empty() || id.len() > 128 || !seen.insert(id.clone()) {
                return Err(Error::ResourceLimit);
            }
            let old = old_inputs.get(id.as_str()).copied();
            let version = if old.is_some_and(|(_, image, _)| image == &preview) {
                old.unwrap().2
            } else {
                self.preview_version = self
                    .preview_version
                    .checked_add(1)
                    .filter(|n| *n <= MAX_SAFE_INTEGER)
                    .ok_or(Error::ResourceLimit)?;
                self.preview_version
            };
            next.push((id, preview, version));
        }
        let versions = next.iter().map(|(_, _, version)| *version).collect();
        self.input_previews = next;
        Ok(versions)
    }

    /// Replace from current filtered UI models. No disk access or raw JSON fields.
    #[cfg(test)]
    pub fn capture(
        &mut self,
        page_id: PageId,
        view: Value,
        rows: Vec<CachedItem<P>>,
    ) -> Result<Context, Error> {
        self.capture_with_target(page_id, view, rows, None)
    }

    pub fn capture_with_target(
        &mut self,
        page_id: PageId,
        mut view: Value,
        rows: Vec<CachedItem<P>>,
        current_preview_source_key: Option<&str>,
    ) -> Result<Context, Error> {
        let (account_scope, app_session, page_epoch) = self.lifetime()?;
        let captured_lifetime = (app_session.clone(), page_epoch);
        if self.captured_lifetime.as_ref() != Some(&captured_lifetime) {
            self.current = None;
            self.items.clear();
            self.captured_lifetime = Some(captured_lifetime);
        }
        // A failed recapture must fence the previous cached authorization state.
        let old_context = self.current.take();
        if rows.len() > MAX_ITEMS {
            return Err(Error::ResourceLimit);
        }
        // A missed navigation hook must never reuse another page's identities.
        if old_context
            .as_ref()
            .is_some_and(|c| c.page.page_id != page_id)
        {
            return Err(Error::StalePage);
        }
        let mut seen = HashSet::new();
        let mut next = Vec::with_capacity(rows.len());
        let mut source_name_bytes = self
            .auxiliary_preview
            .as_ref()
            .map_or(0, |row| row.full_name_bytes);
        let old_items = self
            .items
            .iter()
            .map(|old| (old.source_key.as_str(), old))
            .collect::<HashMap<_, _>>();
        for row in rows {
            source_name_bytes = source_name_bytes
                .checked_add(row.name.len())
                .filter(|n| *n <= MAX_SNAPSHOT_BYTES)
                .ok_or(Error::ResourceLimit)?;
            if row.source_key.is_empty()
                || row.source_key.len() > 16_384
                || !seen.insert(row.source_key.clone())
            {
                return Err(Error::ResourceLimit);
            }
            let previous = old_items.get(row.source_key.as_str()).copied();
            let item_id = previous
                .map(|old| old.item.item_id.clone())
                .unwrap_or_else(|| format!("app-item-{}", uuid::Uuid::new_v4()));
            let preview_version = if previous.is_some_and(|old| old.preview == row.preview) {
                previous.unwrap().item.preview_version
            } else {
                self.preview_version = self
                    .preview_version
                    .checked_add(1)
                    .filter(|n| *n <= MAX_SAFE_INTEGER)
                    .ok_or(Error::ResourceLimit)?;
                self.preview_version
            };
            let (name, name_truncated, full_name_sha256) = display_name(&row.name)?;
            let item = Item {
                item_id: item_id.clone(),
                kind: row.kind,
                name,
                name_truncated,
                status: text(&row.status, 256)?,
                selected: row.selected,
                availability: row.availability,
                preview_available: row.preview.is_some(),
                project_entry_id: row.project.then_some(item_id),
                detail: row.detail,
                preview_version,
            };
            next.push(Retained {
                source_key: row.source_key,
                item,
                preview: row.preview,
                full_name_sha256,
                full_name_bytes: row.name.len(),
            });
        }
        let current_target = current_preview_source_key
            .map(|key| {
                next.iter()
                    .find(|row| row.source_key == key)
                    .or_else(|| {
                        self.auxiliary_preview
                            .as_ref()
                            .filter(|row| row.source_key == key)
                    })
                    .ok_or(Error::UnsupportedPage)
            })
            .transpose()?;
        if view.is_null() {
            view = serde_json::json!({});
        }
        if !view.is_object() {
            return Err(Error::InvalidQuery);
        }
        view["previewTargetId"] = current_target
            .map(|row| Value::String(row.item.item_id.clone()))
            .unwrap_or(Value::Null);
        let current_preview = current_target
            .filter(|row| row.preview.is_some())
            .map(|row| CurrentPreview {
                item_id: row.item.item_id.clone(),
                preview_version: row.item.preview_version,
            });
        let public_items = next.iter().map(|row| &row.item).collect::<Vec<_>>();
        let content = serde_json::json!({"pageId":page_id,"view":view,"items":public_items,"currentPreview":current_preview});
        let bytes = serde_json::to_vec(&canonical(content)).map_err(|_| Error::ResourceLimit)?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(Error::ResourceLimit);
        }
        let content_state_id = format!("app-content-v1-{:x}", Sha256::digest(&bytes));
        let state_items = if matches!(page_id, PageId::Account | PageId::Credits) {
            vec![]
        } else {
            next.iter().map(|row|serde_json::json!({"itemId":row.item.item_id,"kind":row.item.kind,
                    "name":row.item.name,"nameSha256":row.full_name_sha256,"selected":row.item.selected,
                "selectedPreviewVersion":row.item.selected.then_some(row.item.preview_version),
                "templateContent":match &row.item.detail {
                    Some(detail @ Detail::Template{..})|Some(detail @ Detail::Inspiration{..})=>Some(detail),_=>None
                }})).collect::<Vec<_>>()
        };
        let state = canonical(
            serde_json::json!({"pageId":page_id,"view":user_view(page_id,&view),"items":state_items,"currentPreview":current_preview,"currentTargetNameSha256":current_target.map(|row|&row.full_name_sha256)}),
        );
        let state_bytes = serde_json::to_vec(&state).map_err(|_| Error::ResourceLimit)?;
        let page_state_id = format!("app-page-v1-{:x}", Sha256::digest(state_bytes));
        let context = Context {
            scope_kind: "app",
            app_instance_id: self.instance.clone(),
            client_id: self.client.clone(),
            account_scope,
            app_session_id: app_session,
            page: Page {
                page_id,
                page_epoch,
                page_state_id,
            },
            content_state_id,
            current_preview,
            list_scope: "filteredList",
            viewport: serde_json::json!({"status":"unknown"}),
            focus: serde_json::json!({"status":"unknown"}),
            view,
            total: next.len(),
        };
        if serde_json::to_vec(&context)
            .map_err(|_| Error::ResourceLimit)?
            .len()
            > MAX_REPLY_BYTES
        {
            return Err(Error::ResourceLimit);
        }
        self.items = next;
        self.current = Some(context.clone());
        Ok(context)
    }

    pub fn require_current(&self, frozen: &FrozenPage) -> Result<&Context, Error> {
        let current = self.current.as_ref().ok_or(Error::StalePage)?;
        let (scope, session, epoch) = self.lifetime()?;
        if current.account_scope != scope
            || current.app_session_id != session
            || current.page.page_epoch != epoch
        {
            return Err(Error::StalePage);
        }
        if frozen.app_session_id != current.app_session_id
            || frozen.page_id != current.page.page_id
            || frozen.page_epoch != current.page.page_epoch
            || frozen.page_state_id != current.page.page_state_id
        {
            return Err(Error::StalePage);
        }
        Ok(current)
    }

    pub fn page_items(
        &self,
        frozen: &FrozenPage,
        offset: usize,
        limit: usize,
    ) -> Result<Items, Error> {
        let current = self.require_current(frozen)?.clone();
        if !(1..=50).contains(&limit) || offset > self.items.len() {
            return Err(Error::InvalidQuery);
        }
        let items = self
            .items
            .iter()
            .skip(offset)
            .take(limit)
            .map(|row| row.item.clone())
            .collect::<Vec<_>>();
        let end = offset + items.len();
        let reply = Items {
            context: current,
            offset,
            next_offset: (end < self.items.len()).then_some(end),
            items,
        };
        if serde_json::to_vec(&reply)
            .map_err(|_| Error::ResourceLimit)?
            .len()
            > MAX_REPLY_BYTES
        {
            return Err(Error::ResourceLimit);
        }
        Ok(reply)
    }

    /// Caller encodes this existing image only, then recaptures and rechecks.
    pub fn page_preview(
        &self,
        frozen: &FrozenPage,
        item_id: &str,
    ) -> Result<(PreviewSource, P), Error> {
        self.require_current(frozen)?;
        self.items
            .iter()
            .chain(self.auxiliary_preview.iter())
            .find(|row| row.item.item_id == item_id)
            .and_then(|row| row.preview.clone())
            .ok_or(Error::PreviewUnavailable)
    }

    /// Stable target binding independent of unrelated progress/ledger refreshes.
    pub fn preview_version(&self, frozen: &FrozenPage, item_id: &str) -> Result<u64, Error> {
        self.require_current(frozen)?;
        self.items
            .iter()
            .chain(self.auxiliary_preview.iter())
            .find(|row| row.item.item_id == item_id && row.preview.is_some())
            .map(|row| row.item.preview_version)
            .ok_or(Error::PreviewUnavailable)
    }
}

/// Only user-selected view inputs participate in pageStateId. Dynamic response
/// values remain available in view/contentStateId as ordinary ToolResult data.
fn user_view(page: PageId, view: &Value) -> Value {
    let fields: &[&str] = match page {
        PageId::Generation => &[
            "mode",
            "generationStep",
            "browseTab",
            "inspirationCategory",
            "prompt",
            "modelId",
            "modelName",
            "parameters",
            "references",
        ],
        PageId::PersonalLibrary => &[
            "search",
            "kindFilter",
            "sort",
            "sourceFilter",
            "folderId",
            "folderFilter",
            "trash",
            "favoritesOnly",
        ],
        PageId::TeamLibrary => &["teamId", "search", "kindFilter", "trash", "listPurpose"],
        PageId::Templates => &[
            "editorOpen",
            "editorTarget",
            "editorName",
            "editorPrompt",
            "editorMode",
        ],
        PageId::Account => &["reducedMotion"],
        PageId::Credits => &[
            "historyTab",
            "ledgerFilter",
            "historyPage",
            "selectedEntryId",
        ],
        PageId::Settings => &[
            "tab",
            "language",
            "playheadStops",
            "customContextActions",
            "hardwareDecode",
        ],
        PageId::Projects => &["projectKind", "search", "sort", "layout"],
    };
    let mut output = serde_json::Map::new();
    for key in fields
        .iter()
        .copied()
        .chain(["overlays", "previewTargetId"])
    {
        if let Some(value) = view.get(key) {
            output.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(output)
}

pub fn text(source: &str, max_chars: usize) -> Result<String, Error> {
    if source.chars().count() > max_chars {
        return Err(Error::ResourceLimit);
    }
    Ok(source
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect())
}
fn display_name(source: &str) -> Result<(String, bool, String), Error> {
    if source.len() > 32_768 {
        return Err(Error::ResourceLimit);
    }
    let name = source
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(256)
        .collect();
    Ok((
        name,
        source.chars().count() > 256,
        format!("{:x}", Sha256::digest(source.as_bytes())),
    ))
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(k, v)| (k, canonical(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.into_iter().map(canonical).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> AppPages<u8> {
        let mut pages = AppPages::new("instance".into(), "client".into());
        pages.account(Some((1, "a".repeat(64)))).unwrap();
        pages
    }
    fn row(key: &str, preview: u8) -> CachedItem<u8> {
        CachedItem {
            source_key: key.into(),
            kind: Kind::Project,
            name: "project\nname".into(),
            status: String::new(),
            selected: false,
            availability: Availability::Unknown,
            project: true,
            detail: None,
            preview: Some((PreviewSource::Thumbnail, preview)),
        }
    }
    fn frozen(context: &Context) -> FrozenPage {
        FrozenPage {
            app_session_id: context.app_session_id.clone(),
            page_id: context.page.page_id,
            page_epoch: context.page.page_epoch,
            page_state_id: context.page.page_state_id.clone(),
        }
    }
    #[test]
    fn opaque_projects_and_stable_memory_preview_state() {
        let mut p = registry();
        let a = p
            .capture(
                PageId::Projects,
                serde_json::json!({"query":""}),
                vec![row("/private/project", 1)],
            )
            .unwrap();
        let b = p
            .capture(
                PageId::Projects,
                serde_json::json!({"query":""}),
                vec![row("/private/project", 1)],
            )
            .unwrap();
        assert_eq!(a.page.page_state_id, b.page.page_state_id);
        let reply = serde_json::to_string(&p.page_items(&frozen(&a), 0, 20).unwrap()).unwrap();
        assert!(!reply.contains("/private"));
        assert!(reply.contains("project name"));
        let c = p
            .capture(
                PageId::Projects,
                serde_json::json!({"query":""}),
                vec![row("/private/project", 2)],
            )
            .unwrap();
        assert_eq!(a.page.page_state_id, c.page.page_state_id);
        assert_ne!(a.content_state_id, c.content_state_id);
        assert_eq!(
            p.page_preview(&frozen(&a), "x"),
            Err(Error::PreviewUnavailable)
        );
    }
    #[test]
    fn navigation_a_b_a_and_same_account_relogin_invalidate_old_stamps() {
        let mut p = registry();
        let a = p
            .capture(PageId::Projects, Value::Null, vec![row("path", 1)])
            .unwrap();
        p.navigation().unwrap();
        p.navigation().unwrap();
        let b = p
            .capture(PageId::Projects, Value::Null, vec![row("path", 1)])
            .unwrap();
        assert_eq!(b.page.page_epoch, a.page.page_epoch + 2);
        assert!(p.require_current(&frozen(&a)).is_err());
        p.account(Some((2, "a".repeat(64)))).unwrap();
        let c = p.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        assert_ne!(b.app_session_id, c.app_session_id);
    }
    #[test]
    fn page_change_without_real_navigation_notification_fails_closed() {
        let mut p = registry();
        p.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        assert!(matches!(
            p.capture(PageId::Generation, Value::Null, vec![]),
            Err(Error::StalePage)
        ));
        assert!(matches!(
            p.capture(
                PageId::Projects,
                Value::Null,
                (0..2049).map(|n| row(&n.to_string(), 0)).collect()
            ),
            Err(Error::ResourceLimit)
        ));
    }
    #[test]
    fn selection_and_filter_changes_are_content_changes_not_navigation() {
        let mut p = registry();
        let a = p
            .capture(
                PageId::PersonalLibrary,
                serde_json::json!({"search":"a"}),
                vec![row("x", 1)],
            )
            .unwrap();
        let mut selected = row("x", 1);
        selected.selected = true;
        let b = p
            .capture(
                PageId::PersonalLibrary,
                serde_json::json!({"search":"a"}),
                vec![selected],
            )
            .unwrap();
        assert_eq!(a.page.page_epoch, b.page.page_epoch);
        assert_ne!(a.page.page_state_id, b.page.page_state_id);
        let c = p
            .capture(
                PageId::PersonalLibrary,
                serde_json::json!({"search":"b"}),
                vec![],
            )
            .unwrap();
        assert_ne!(b.page.page_state_id, c.page.page_state_id);
    }
    #[test]
    fn managed_charge_and_progress_do_not_invalidate_user_view() {
        let mut p = registry();
        let a=p.capture(PageId::Credits,serde_json::json!({"historyTab":0,"ledgerFilter":0,"historyPage":1,"availableCreditsLabel":"100"}),vec![]).unwrap();
        let b=p.capture(PageId::Credits,serde_json::json!({"historyTab":0,"ledgerFilter":0,"historyPage":1,"availableCreditsLabel":"90"}),vec![row("new-charge",1)]).unwrap();
        assert_eq!(a.page.page_state_id, b.page.page_state_id);
        assert_ne!(a.content_state_id, b.content_state_id);
        p.navigation().unwrap();
        let a = p
            .capture(
                PageId::Generation,
                serde_json::json!({"browseTab":"results"}),
                vec![row("job", 1)],
            )
            .unwrap();
        let mut progress = row("job", 1);
        progress.status = "ready".into();
        progress.availability = Availability::Available;
        let b = p
            .capture(
                PageId::Generation,
                serde_json::json!({"browseTab":"results"}),
                vec![progress],
            )
            .unwrap();
        assert_eq!(a.page.page_state_id, b.page.page_state_id);
        assert_ne!(a.content_state_id, b.content_state_id);
    }
    #[test]
    fn opened_preview_version_is_bound_but_unrelated_preview_is_not() {
        let mut p = registry();
        let a = p
            .capture_with_target(
                PageId::Projects,
                Value::Null,
                vec![row("target", 1), row("other", 1)],
                Some("target"),
            )
            .unwrap();
        let b = p
            .capture_with_target(
                PageId::Projects,
                Value::Null,
                vec![row("target", 1), row("other", 2)],
                Some("target"),
            )
            .unwrap();
        assert_eq!(a.page.page_state_id, b.page.page_state_id);
        let c = p
            .capture_with_target(
                PageId::Projects,
                Value::Null,
                vec![row("target", 2), row("other", 2)],
                Some("target"),
            )
            .unwrap();
        assert_ne!(b.page.page_state_id, c.page.page_state_id);
    }
    #[test]
    fn shared_host_navigation_keeps_one_account_session_for_named_clients() {
        let nav = Rc::new(Navigation::default());
        let mut a = AppPages::<u8>::with_navigation("i".into(), "a".into(), nav.clone());
        let mut b = AppPages::<u8>::with_navigation("i".into(), "b".into(), nav);
        a.account(Some((1, "a".repeat(64)))).unwrap();
        let ca = a.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        let cb = b.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        assert_eq!(ca.app_session_id, cb.app_session_id);
        a.navigation().unwrap();
        assert!(b.require_current(&frozen(&cb)).is_err());
    }
    #[test]
    fn navigation_counter_does_not_wait_for_account_borrow() {
        let mut p = registry();
        let a = p.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        let nav = p.navigation.clone();
        let held = nav.account.borrow_mut();
        assert_eq!(p.account(Some((2, "b".repeat(64)))), Err(Error::Busy));
        p.navigation().unwrap();
        drop(held);
        assert!(p.require_current(&frozen(&a)).is_err());
    }
    #[test]
    fn sensitive_and_unsupported_overlays_are_blocked() {
        assert_eq!(read_gate(false, false, false, false, false), Ok(()));
        for i in 0..5 {
            let mut flags = [false; 5];
            flags[i] = true;
            assert_eq!(
                read_gate(flags[0], flags[1], flags[2], flags[3], flags[4]),
                Err(Error::Busy)
            );
        }
    }
    #[test]
    fn long_names_are_explicitly_truncated_but_full_tail_binds_view() {
        let mut p = registry();
        let mut first = row("stable", 1);
        first.name = format!("{}a\n", "中".repeat(256));
        let a = p
            .capture(PageId::Projects, Value::Null, vec![first])
            .unwrap();
        let displayed = p.page_items(&frozen(&a), 0, 20).unwrap().items.remove(0);
        assert_eq!(displayed.name.chars().count(), 256);
        assert!(displayed.name_truncated);
        assert!(!displayed.name.chars().any(char::is_control));
        let mut first = row("stable", 1);
        first.name = format!("{}b\n", "中".repeat(256));
        let b = p
            .capture(PageId::Projects, Value::Null, vec![first])
            .unwrap();
        assert_ne!(a.page.page_state_id, b.page.page_state_id);
        let mut first = row("stable", 1);
        first.name = "a".repeat(32769);
        assert!(matches!(
            p.capture(PageId::Projects, Value::Null, vec![first]),
            Err(Error::ResourceLimit)
        ));
    }
    #[test]
    fn cache_failure_invalidates_previous_projection() {
        let mut p = registry();
        let a = p.capture(PageId::Projects, Value::Null, vec![]).unwrap();
        assert!(
            p.capture(
                PageId::Projects,
                Value::Null,
                (0..2049).map(|n| row(&n.to_string(), 0)).collect()
            )
            .is_err()
        );
        assert!(p.require_current(&frozen(&a)).is_err());
    }
    #[test]
    fn actual_open_preview_is_separate_from_filtered_list_and_stable() {
        let mut p = registry();
        let key = p
            .set_auxiliary_preview(Some(row("reference-current", 1)))
            .unwrap()
            .unwrap();
        let a = p
            .capture_with_target(PageId::Generation, Value::Null, vec![], Some(&key))
            .unwrap();
        assert_eq!(a.total, 0);
        let target = a.current_preview.as_ref().unwrap();
        assert_eq!(
            p.page_preview(&frozen(&a), &target.item_id),
            Ok((PreviewSource::Thumbnail, 1))
        );
        let key = p
            .set_auxiliary_preview(Some(row("reference-current", 1)))
            .unwrap()
            .unwrap();
        let b = p
            .capture_with_target(PageId::Generation, Value::Null, vec![], Some(&key))
            .unwrap();
        assert_eq!(a.page.page_state_id, b.page.page_state_id);
        assert_eq!(
            target.preview_version,
            b.current_preview.unwrap().preview_version
        );
        let key = p
            .set_auxiliary_preview(Some(row("reference-current", 2)))
            .unwrap()
            .unwrap();
        let c = p
            .capture_with_target(PageId::Generation, Value::Null, vec![], Some(&key))
            .unwrap();
        assert_ne!(a.page.page_state_id, c.page.page_state_id);
    }
}
