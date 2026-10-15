// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document scope shared by editor permissions and the optional local runtime.
#[derive(Clone, PartialEq, Eq)]
pub struct DocumentIdentity {
    pub instance_id: String,
    pub project_id: String,
    pub document_session_id: String,
}
