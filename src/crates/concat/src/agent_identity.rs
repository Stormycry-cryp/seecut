// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document scope shared by editor permissions and the optional local runtime.
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentIdentity {
    pub instance_id: String,
    pub project_id: String,
    pub document_session_id: String,
}

/// A real native App read scope. Its client is checked separately by the bridge.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppIdentity {
    /// The current native process identity.
    pub instance_id: String,
    /// The trusted signed-in account scope.
    pub account_scope: String,
    /// The current native App read session; never a history key.
    pub app_session_id: String,
}

/// Exactly one trusted native scope; App reads cannot carry document identity.
#[derive(Clone, PartialEq, Eq)]
pub enum AgentScope {
    /// An active native document with its original write/history boundary.
    Document(DocumentIdentity),
    /// Read-only signed-in App pages, independent of document authority.
    App(AppIdentity),
}
impl AgentScope {
    /// The actual native process attached to this scope.
    pub fn instance_id(&self) -> &str {
        match self {
            Self::Document(document) => &document.instance_id,
            Self::App(app) => &app.instance_id,
        }
    }
    /// The active document, or none for an App read scope.
    pub fn document(&self) -> Option<&DocumentIdentity> {
        match self {
            Self::Document(document) => Some(document),
            Self::App(_) => None,
        }
    }
    /// The current App read identity, or none for a document.
    pub fn app(&self) -> Option<&AppIdentity> {
        match self {
            Self::Document(_) => None,
            Self::App(app) => Some(app),
        }
    }
    /// A fresh explicit App grant may rebind the original account's read history.
    /// Document recovery retains its existing exact runtime identity requirement.
    pub fn same_restore_scope(&self, next: &Self) -> bool {
        match (self, next) {
            (Self::Document(before), Self::Document(after)) => before == after,
            (Self::App(before), Self::App(after)) => before.account_scope == after.account_scope,
            _ => false,
        }
    }
}
impl From<DocumentIdentity> for AgentScope {
    fn from(value: DocumentIdentity) -> Self {
        Self::Document(value)
    }
}
