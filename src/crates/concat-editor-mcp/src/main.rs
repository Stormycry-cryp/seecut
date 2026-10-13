// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stdio MCP adapter. The running App owns all document and permission state.

use concat_editor_mcp::{
    AssetVersion, CanvasAction, CanvasParameters, LibraryQuery, Method, MoveParameters,
    OperationQuery, Request, Response, call, endpoint,
};
use rmcp::{
    ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
    transport::stdio,
};
use std::path::PathBuf;

#[derive(Clone)]
struct Bridge {
    instance: String,
    client: String,
    token: String,
    write_token: Option<String>,
    socket: PathBuf,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectArgs {
    #[serde(default)]
    object_id: Option<String>,
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    context_revision: u64,
    #[serde(default)]
    max_edge: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
    object_id: String,
    delta_x: f64,
    delta_y: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanvasIdentityArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InsertArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
    asset_id: String,
    version: AssetVersion,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransformArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
    object_id: String,
    x: f64,
    y: f64,
    scale: f64,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryArgs {
    #[serde(default)]
    asset_id: Option<String>,
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationArgs {
    project_id: String,
    document_session_id: String,
    operation_id: String,
    client_sequence: u64,
}

impl Bridge {
    fn request(&self, method: Method) -> Request {
        Request {
            object_id: None,
            canvas_parameters: None,
            library_query: None,
            operation_query: None,
            instance_id: self.instance.clone(),
            client_id: self.client.clone(),
            client_token: if method.is_write() {
                self.write_token.clone().unwrap_or_default()
            } else {
                self.token.clone()
            },
            method,
            project_id: None,
            document_session_id: None,
            revision: None,
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
            move_parameters: None,
        }
    }
    fn dispatch(&self, request: Request) -> String {
        serde_json::to_string(&call(&self.socket, &request))
            .unwrap_or_else(|_| "{\"status\":\"ioFailure\"}".into())
    }
    fn canvas_write(&self, identity: CanvasIdentityArgs, action: CanvasAction) -> String {
        if self.write_token.is_none() {
            return serde_json::to_string(&Response::error("notAuthorized")).unwrap_or_default();
        }
        let mut request = self.request(action.method());
        request.project_id = Some(identity.project_id);
        request.document_session_id = Some(identity.document_session_id);
        request.revision = Some(identity.revision);
        request.canvas_parameters = Some(CanvasParameters {
            operation_id: identity.operation_id,
            client_sequence: identity.client_sequence,
            selection_revision: identity.selection_revision,
            action,
        });
        self.dispatch(request)
    }
}

#[tool_router(server_handler)]
impl Bridge {
    #[tool(
        description = "Read the running SeeCut instance's named capabilities, current permission state and limits; tool presence alone does not authorize an edit"
    )]
    fn capabilities(&self) -> String {
        self.dispatch(self.request(Method::Capabilities))
    }

    #[tool(
        description = "Read current SeeCut workspace and document session, revisions, selection, and busy state"
    )]
    fn context(&self) -> String {
        self.dispatch(self.request(Method::Context))
    }

    #[tool(
        description = "Read one exact canvas layer by optional canonical objectId, or a bounded project page. Do not combine objectId with pagination. Match project and document session ID."
    )]
    fn project(&self, Parameters(args): Parameters<ProjectArgs>) -> String {
        let mut request = self.request(Method::Project);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.object_id = args.object_id;
        request.offset = args.offset;
        request.limit = args.limit;
        self.dispatch(request)
    }

    #[tool(
        description = "Search a bounded page of versioned global library images by name or optional exact assetId, including retrieval of the exported output for the current canvas task. Requires explicit App media authorization. Returns assetId, immutable version, name, kind and availability; never local paths. Limit 1..50, default20. Use returned identity/version for insertion."
    )]
    fn library_images(&self, Parameters(args): Parameters<LibraryArgs>) -> String {
        let mut request = self.request(Method::LibraryImages);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.library_query = Some(LibraryQuery {
            asset_id: args.asset_id,
            query: args.query,
            offset: args.offset,
            limit: args.limit.unwrap_or(20),
        });
        self.dispatch(request)
    }
    #[tool(
        description = "Insert a specified immutable library image into the current canvas in one undo step. Requires specific trusted App authorization and current E lease. Returns createdLayerId and committed revision/selectionRevision. No paths. UNKNOWN must query canvas_operation using the original operation identity; never submit a new sequence."
    )]
    fn insert_library_image(&self, Parameters(args): Parameters<InsertArgs>) -> String {
        self.canvas_write(
            CanvasIdentityArgs {
                project_id: args.project_id,
                document_session_id: args.document_session_id,
                revision: args.revision,
                operation_id: args.operation_id,
                client_sequence: args.client_sequence,
                selection_revision: args.selection_revision,
            },
            CanvasAction::Insert {
                asset_id: args.asset_id,
                version: args.version,
            },
        )
    }
    #[tool(
        description = "Set position x/y (document pixels, finite within +/-32768) and uniform scale (0.01..64) of the specified current selected canvas image layer in one undo step. Requires specific trusted App authorization and E lease; checks document/selection revisions. UNKNOWN only queries original canvas_operation."
    )]
    fn transform_canvas_layer(&self, Parameters(args): Parameters<TransformArgs>) -> String {
        self.canvas_write(
            CanvasIdentityArgs {
                project_id: args.project_id,
                document_session_id: args.document_session_id,
                revision: args.revision,
                operation_id: args.operation_id,
                client_sequence: args.client_sequence,
                selection_revision: args.selection_revision,
            },
            CanvasAction::Transform {
                object_id: args.object_id,
                x: args.x,
                y: args.y,
                scale: args.scale,
            },
        )
    }
    #[tool(
        description = "Save the current canvas to its App-recorded project asynchronously. No path parameter. Unnamed projects stop at awaitingSelection for the existing trusted App save flow. Query the original canvas_operation for actual persisted revision; acceptance alone is not saved success."
    )]
    fn save_canvas_project(&self, Parameters(args): Parameters<CanvasIdentityArgs>) -> String {
        self.canvas_write(args, CanvasAction::Save)
    }
    #[tool(
        description = "Formally export the current canvas PNG asynchronously into the App managed directory and default-register it in the existing global library. No path parameter. Query original canvas_operation for file SHA and assetId/version. Written-but-unregistered is partial; retry registration through original operation, never recompose or create a second export."
    )]
    fn export_canvas_png(&self, Parameters(args): Parameters<CanvasIdentityArgs>) -> String {
        self.canvas_write(args, CanvasAction::Export)
    }
    #[tool(
        description = "Query an original canvas operation bound to this instance/client/project/document and original operationId/clientSequence. Requires explicit App authorization to read this client's operation receipt. Returns progress or exact layer/transform/persisted-revision/PNG-hash/library-version evidence; no arbitrary tool replay."
    )]
    fn canvas_operation(&self, Parameters(args): Parameters<OperationArgs>) -> String {
        let mut request = self.request(Method::CanvasOperation);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.operation_query = Some(OperationQuery {
            operation_id: args.operation_id,
            client_sequence: args.client_sequence,
        });
        self.dispatch(request)
    }

    #[tool(
        description = "Retry only global-library registration of an original frozen PNG from export_canvas_png. Uses its exact original operationId/clientSequence and existing bytes/provenance. No compose, PNG rewrite, new export, new sequence or path argument. Requires exact trusted App approval and current five-minute E lease. Query canvas_operation with the original identity for its final PNG SHA/asset version."
    )]
    fn retry_canvas_registration(&self, Parameters(args): Parameters<OperationArgs>) -> String {
        let mut request = self.request(Method::RetryCanvasRegistration);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.operation_query = Some(OperationQuery {
            operation_id: args.operation_id,
            client_sequence: args.client_sequence,
        });
        self.dispatch(request)
    }

    #[tool(
        description = "Move the current single selected SeeCut image layer by deltaX and deltaY document pixels in one undo step. Requires independent E permission granted in trusted App UI for five minutes and SEECUT_MCP_WRITE_TOKEN; this tool cannot grant or renew it. Each delta must be finite with magnitude <=32768. Match the explicit project/session/revision/selectionRevision and layer:<nonzero u64> objectId. Supply operationId scm2:1:<client UTF-8 byte length>:<client>:<session UTF-8 byte length>:<session>:<nonzero clientSequence>. Retry outcomeUnknown with the same identity and exact parameters, never a new sequence. App capabilities govern current permission."
    )]
    fn move_selected_image(&self, Parameters(args): Parameters<MoveArgs>) -> String {
        if self.write_token.is_none() {
            return serde_json::to_string(&Response::error("notAuthorized")).unwrap_or_default();
        }
        let mut request = self.request(Method::MoveSelectedImage);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.revision = Some(args.revision);
        request.move_parameters = Some(MoveParameters {
            operation_id: args.operation_id,
            client_sequence: args.client_sequence,
            selection_revision: args.selection_revision,
            object_id: args.object_id,
            delta_x: args.delta_x,
            delta_y: args.delta_y,
        });
        self.dispatch(request)
    }

    #[tool(
        description = "Read a stable version-matching canvas PNG preview (max 256 px edge, 4194304 source pixels); busy or stalePreview on a changed frame, unsupportedPreview outside these limits"
    )]
    fn preview(&self, Parameters(args): Parameters<PreviewArgs>) -> CallToolResult {
        let mut request = self.request(Method::Preview);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.revision = Some(args.revision);
        request.context_revision = Some(args.context_revision);
        request.max_edge = args.max_edge;
        let response = call(&self.socket, &request);
        if response.status != "ok" {
            return CallToolResult::error(vec![ContentBlock::text(
                serde_json::to_string(&response).unwrap_or_default(),
            )]);
        }
        let mut metadata = response.data.unwrap_or_default();
        let Some(encoded) = metadata
            .as_object_mut()
            .and_then(|object| object.remove("base64"))
            .and_then(|value| value.as_str().map(str::to_owned))
        else {
            return CallToolResult::error(vec![ContentBlock::text("ioFailure")]);
        };
        let mut result = CallToolResult::success(vec![
            ContentBlock::text(metadata.to_string()),
            ContentBlock::image(encoded, "image/png"),
        ]);
        result.structured_content = Some(metadata);
        result
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 || args[1] != "--instance" || args[3] != "--client" {
        return Err("usage: seecut-editor-mcp --instance UUID --client CLIENT_ID (set SEECUT_MCP_CLIENT_TOKEN; optionally set independent SEECUT_MCP_WRITE_TOKEN)".into());
    }
    let token = std::env::var("SEECUT_MCP_CLIENT_TOKEN")?;
    let write_token = match std::env::var("SEECUT_MCP_WRITE_TOKEN") {
        Ok(token) => Some(token),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error.into()),
    };
    let instance = args[2].clone();
    let client = args[4].clone();
    if client.is_empty()
        || client.len() > 64
        || token.len() != 36
        || write_token.as_ref().is_some_and(|token| token.len() != 36)
    {
        return Err("invalid client ID or token".into());
    }
    let socket = endpoint(&instance)?;
    let server = Bridge {
        instance,
        client,
        token,
        write_token,
        socket,
    }
    .serve(stdio())
    .await?;
    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(write_token: Option<String>) -> Bridge {
        Bridge {
            instance: "instance".into(),
            client: "client".into(),
            token: "read-credential".into(),
            write_token,
            socket: PathBuf::from("unused-test-endpoint"),
        }
    }

    #[test]
    fn reads_and_move_use_independent_credentials() {
        let adapter = bridge(Some("write-credential".into()));
        for method in [
            Method::Capabilities,
            Method::Context,
            Method::Project,
            Method::Preview,
        ] {
            assert_eq!(adapter.request(method).client_token, "read-credential");
        }
        assert_eq!(
            adapter.request(Method::MoveSelectedImage).client_token,
            "write-credential"
        );
        assert!(
            bridge(None)
                .request(Method::MoveSelectedImage)
                .client_token
                .is_empty()
        );
    }

    #[test]
    fn move_without_write_credential_returns_not_authorized_before_ipc() {
        let args = MoveArgs {
            project_id: "project".into(),
            document_session_id: "session".into(),
            revision: 7,
            operation_id: concat_editor_mcp::operation_id("client", "session", 1).unwrap(),
            client_sequence: 1,
            selection_revision: 11,
            object_id: "layer:42".into(),
            delta_x: 4.0,
            delta_y: -3.0,
        };
        let response: Response =
            serde_json::from_str(&bridge(None).move_selected_image(Parameters(args))).unwrap();
        assert_eq!(response.status, "notAuthorized");
    }

    #[test]
    fn public_move_arguments_are_flat_and_reject_unrelated_fields() {
        let value = serde_json::json!({
            "projectId": "project", "documentSessionId": "session", "revision": 7,
            "operationId": "scm2:1:6:client:7:session:1", "clientSequence": 1,
            "selectionRevision": 11, "objectId": "layer:42", "deltaX": 4.0, "deltaY": -3.0
        });
        assert!(serde_json::from_value::<MoveArgs>(value.clone()).is_ok());
        for key in [
            "contextRevision",
            "maxEdge",
            "offset",
            "limit",
            "path",
            "clientToken",
            "grant",
        ] {
            let mut extra = value.clone();
            extra[key] = serde_json::json!(1);
            assert!(serde_json::from_value::<MoveArgs>(extra).is_err(), "{key}");
        }
        let mut nested = value;
        nested["moveParameters"] = serde_json::json!({});
        assert!(serde_json::from_value::<MoveArgs>(nested).is_err());
    }
}
