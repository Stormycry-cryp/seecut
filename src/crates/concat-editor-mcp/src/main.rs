// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stdio MCP adapter. The running App owns all document and permission state.

use concat_editor_mcp::{
    AssetVersion, CanvasAction, CanvasEdit, CanvasParameters, LibraryAssetKind, LibraryAssetsQuery,
    LibraryQuery, Method, MoveParameters, OperationQuery, Request, Response, call, endpoint,
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
    #[serde(default)]
    object_ids: Option<Vec<String>>,
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
struct CanvasEditArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
    expected_edit_state_id: String,
    edit: CanvasEdit,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClipEditArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    operation_id: String,
    client_sequence: u64,
    selection_revision: u64,
    expected_edit_state_id: String,
    timeline_id: String,
    edit: concat_editor_mcp::ClipEdit,
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
struct LibraryAssetsArgs {
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    kind: LibraryAssetKind,
    #[serde(default)]
    asset_id: Option<String>,
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
            clip_parameters: None,
            object_ids: None,
            library_query: None,
            library_assets_query: None,
            operation_query: None,
            flow: None,
            flow_workspace: None,
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
        description = "Read the current embedded Flow node configuration or original generation task status. Supply nodeId in args; never launches generation."
    )]
    fn flow_get_node_config(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_get_node_config".into();
        let mut request = self.request(Method::FlowWorkspace);
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Read the current embedded Flow node configuration or original generation task status. Supply nodeId in args; never launches generation."
    )]
    fn flow_get_task(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_get_task".into();
        let mut request = self.request(Method::FlowWorkspace);
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Read current embedded Flow selection, canvas and edges. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_get_workspace(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_get_workspace".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Read node data, ports and outputs using nodeIds and fields. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_read_canvas(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_read_canvas".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Read available node types and canvases. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_list_node_types(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_list_node_types".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Read actual node function schemas. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_get_node_tools(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_get_node_tools".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Query original operationId. Never retry with a new identity. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_get_operation(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_get_operation".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Create node through current Flow UI and save. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_add_node(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_add_node".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Configure node:setConfig or node:setPrompt through node tools. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_update_node(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_update_node".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Connect validated ports and save. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_connect_nodes(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_connect_nodes".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Delete specified nodes through Flow editor. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_delete_nodes(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_delete_nodes".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Save current canvas and switch to known canvasId. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_switch_canvas(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_switch_canvas".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
    #[tool(
        description = "Execute node:generateImage or node:generateVideo using existing generation ledger. Unknown submissions only query original tasks. Freeze projectId, canvasId, sessionId, accountScope and revision from context. Writes require original operationId and App edit permission. The name field is supplied by this named tool."
    )]
    fn flow_execute_node(
        &self,
        Parameters(mut args): Parameters<concat_editor_mcp::FlowWorkspace>,
    ) -> String {
        args.name = "flow_execute_node".into();
        let mut request = self.request(Method::FlowWorkspace);
        if !args.is_read() {
            request.client_token = self.write_token.clone().unwrap_or_default();
        }
        request.flow_workspace = Some(args);
        self.dispatch(request)
    }
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
        request.object_ids = args.object_ids;
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
        description = "Search bounded metadata for fixed-version local personal-library image, video and audio assets. kind defaults all; optional exact assetId, query at most160 Unicode characters, offset, limit1..50/default20. Requires exact App media approval for this query. Returns path-free assetId/name/kind/version/availability with contentVerified:false; availability is metadata only. Use clip_edit importLibrary with the exact returned assetId/version to probe and import one item; then placeMedia. No cloud/team assets, paths or content hashing in pagination."
    )]
    fn library_assets(&self, Parameters(args): Parameters<LibraryAssetsArgs>) -> String {
        let mut request = self.request(Method::LibraryAssets);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.library_assets_query = Some(LibraryAssetsQuery {
            kind: args.kind,
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
        description = "Apply one explicit local canvas edit through the native editor and undo history. Closed edit.kind enum covers layers/groups, raster selections/masks, brush, undo/redo, adjustments and curves. Stable layer:n IDs only; folded descendants require a separately approved groupSetExpanded first. Coordinates are document pixels, finite within +/-32768; strokes 1..2048 points, curve channels red/green/blue. Pixel/adjustment/curve writes require the named active object and exact layerPixels/layerMask target. Requires exact native approval, E lease, current revision and selectionRevision. Freeze expectedEditStateId from project.canvasState before approval; any brush, pixel-selection or paint-mode drift returns staleContext with no edit. Receipt includes original edit, actual before/after document content hashes, changed/documentChanged, created/deleted IDs and readback; independently read project.canvasState. No paths/scripts, text, PSD or lookup-file input. Single new bitmap/selection at most16Mi pixels, referenced total64Mi pixels, 10000 nodes. Stroke geometry and scratch copies are preflighted through the native bitmap/mask mapping with a conservative16Mi work limit; simplify or shorten costly paths. Frozen edit JSON at most16KiB; one deletion at most512 nodes; receipt budget48KiB with complete IDs, otherwise resourceLimit before mutation. UNKNOWN only queries original canvas_operation, never a new sequence."
    )]
    fn canvas_edit(&self, Parameters(args): Parameters<CanvasEditArgs>) -> String {
        self.canvas_write(
            CanvasIdentityArgs {
                project_id: args.project_id,
                document_session_id: args.document_session_id,
                revision: args.revision,
                operation_id: args.operation_id,
                client_sequence: args.client_sequence,
                selection_revision: args.selection_revision,
            },
            CanvasAction::Edit {
                edit: args.edit,
                expected_edit_state_id: args.expected_edit_state_id,
            },
        )
    }
    #[tool(
        description = "Apply one explicit bounded ClipEdit to the current owned native Session and fixed timeline. Stable media/track/clip/timeline IDs only; no paths or scripts. importLibrary accepts only an existing local assetId and immutable version, probes one asset asynchronously with an 8GiB bound, then returns mediaId for placeMedia. Freeze project.clipState.editStateId before native approval; revision, selection and complete edit state must match. All direct and indirect locked-track changes are refused, including history. Actions cover existing-bin placement, tracks/timelines, move/trim/split/delete/merge, image freeze, rate/curve, transform/properties, audio detach/reattach, text and keyframes, undo/redo. Video still extraction is unavailable. Typed action at most16KiB; each native source/staged/history state at most16MiB (checked without a second allocation); audio detach at most128 generated clips and conservative16Mi lane-scan work, with copied clip/title bytes budgeted before preview; full untruncated receipt at most56KiB, otherwise refuse before mutation. UNKNOWN only queries the original clip_operation; never replay a new sequence."
    )]
    fn clip_edit(&self, Parameters(args): Parameters<ClipEditArgs>) -> String {
        let mut request = self.request(Method::ClipEdit);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.revision = Some(args.revision);
        request.clip_parameters = Some(concat_editor_mcp::ClipParameters {
            operation_id: args.operation_id,
            client_sequence: args.client_sequence,
            selection_revision: args.selection_revision,
            action: concat_editor_mcp::ClipAction {
                timeline_id: args.timeline_id,
                expected_edit_state_id: args.expected_edit_state_id,
                edit: args.edit,
            },
        });
        self.dispatch(request)
    }
    #[tool(
        description = "Read the exact original clip_edit receipt using its unchanged operationId and clientSequence. Read credential and exact native approval required. No current timeline/version binding and no replay."
    )]
    fn clip_operation(&self, Parameters(args): Parameters<OperationArgs>) -> String {
        let mut request = self.request(Method::ClipOperation);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.operation_query = Some(concat_editor_mcp::OperationQuery {
            operation_id: args.operation_id,
            client_sequence: args.client_sequence,
        });
        self.dispatch(request)
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
        description = "Retry only global-library registration of an original frozen PNG from export_canvas_png. Uses its exact original operationId/clientSequence and existing bytes/provenance. No compose, PNG rewrite, new export, new sequence or path argument. Requires exact trusted App approval and current App-issued E permission. Query canvas_operation with the original identity for its final PNG SHA/asset version."
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
        description = "Move the current single selected SeeCut image layer by deltaX and deltaY document pixels in one undo step. Requires E permission granted in trusted App UI for its declared lifetime and SEECUT_MCP_WRITE_TOKEN; this tool cannot grant or renew it. Each delta must be finite with magnitude <=32768. Match the explicit project/session/revision/selectionRevision and layer:<nonzero u64> objectId. Supply operationId scm2:1:<client UTF-8 byte length>:<client>:<session UTF-8 byte length>:<session>:<nonzero clientSequence>. Retry outcomeUnknown with the same identity and exact parameters, never a new sequence. App capabilities govern current permission."
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
    #[test]
    fn library_assets_schema_is_closed_and_uses_read_credential() {
        let tool = Bridge::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "library_assets")
            .unwrap();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        for key in [
            "projectId",
            "documentSessionId",
            "kind",
            "assetId",
            "query",
            "offset",
            "limit",
        ] {
            assert!(schema["properties"].get(key).is_some(), "{key}");
        }
        assert_eq!(schema["properties"].as_object().unwrap().len(), 7);
        assert_eq!(
            bridge(Some("write".into()))
                .request(Method::LibraryAssets)
                .client_token,
            "read-credential"
        );
        assert!(
            serde_json::from_value::<LibraryAssetsArgs>(
                serde_json::json!({"projectId":"p","documentSessionId":"s","path":"/unapproved"})
            )
            .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn library_assets_cli_defaults_reach_bounded_metadata_only_ipc() {
        use std::os::unix::net::UnixListener;
        let path = endpoint(&uuid::Uuid::new_v4().to_string()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Request = concat_editor_mcp::read_message(&mut stream).unwrap();
            assert_eq!(request.method, Method::LibraryAssets);
            assert_eq!(request.client_token, "read-credential");
            assert!(concat_editor_mcp::validate_request_shape(&request).is_ok());
            let query = request.library_assets_query.unwrap();
            assert_eq!(query.kind, LibraryAssetKind::All);
            assert!(query.asset_id.is_none());
            assert_eq!(query.limit, 20);
            assert_eq!(query.offset, 0);
            concat_editor_mcp::write_message(&mut stream,&Response::ok(serde_json::json!({"projectId":"p","documentSessionId":"s","offset":0,"total":0,"nextOffset":null,"assets":[]}))).unwrap();
        });
        let mut adapter = bridge(Some("write".into()));
        adapter.socket = path.clone();
        let args = serde_json::from_value(
            serde_json::json!({"projectId":"p","documentSessionId":"s","assetId":null}),
        )
        .unwrap();
        let response: Response =
            serde_json::from_str(&adapter.library_assets(Parameters(args))).unwrap();
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(response.status, "ok");
        assert_eq!(response.data.unwrap()["assets"], serde_json::json!([]));
    }

    #[test]
    fn canvas_edit_public_tool_has_closed_schema_and_independent_write_credential() {
        let tool = Bridge::tool_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == "canvas_edit")
            .unwrap();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        for field in [
            "projectId",
            "documentSessionId",
            "revision",
            "selectionRevision",
            "operationId",
            "clientSequence",
            "edit",
            "expectedEditStateId",
        ] {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(field))
            );
        }
        assert_eq!(schema["additionalProperties"], false);
        let args:CanvasEditArgs=serde_json::from_value(serde_json::json!({"projectId":"p","documentSessionId":"s","revision":2,"selectionRevision":3,"expectedEditStateId":format!("edit-v1-{}","a".repeat(64)),"operationId":concat_editor_mcp::operation_id("client","s",1).unwrap(),"clientSequence":1,"edit":{"kind":"layerAdd"}})).unwrap();
        let response: Response =
            serde_json::from_str(&bridge(None).canvas_edit(Parameters(args))).unwrap();
        assert_eq!(response.status, "notAuthorized");
        assert_eq!(
            bridge(Some("write".into()))
                .request(Method::CanvasEdit)
                .client_token,
            "write"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canvas_edit_public_call_reaches_typed_ipc_and_returns_original_receipt() {
        let path = endpoint(&uuid::Uuid::new_v4().to_string()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Request = concat_editor_mcp::read_message(&mut stream).unwrap();
            assert_eq!(request.method, Method::CanvasEdit);
            assert_eq!(request.client_token, "write");
            assert!(concat_editor_mcp::validate_request_shape(&request).is_ok());
            assert!(matches!(
                request.canvas_parameters.as_ref().unwrap().action,
                CanvasAction::Edit {
                    edit: CanvasEdit::LayerAdd,
                    ..
                }
            ));
            concat_editor_mcp::write_message(&mut stream,&Response::ok(serde_json::json!({"tool":"canvas_edit","operationId":request.canvas_parameters.unwrap().operation_id,"changed":true}))).unwrap();
        });
        let mut adapter = bridge(Some("write".into()));
        adapter.socket = path.clone();
        let operation = concat_editor_mcp::operation_id("client", "s", 1).unwrap();
        let response: Response =
            serde_json::from_str(&adapter.canvas_edit(Parameters(CanvasEditArgs {
                project_id: "p".into(),
                document_session_id: "s".into(),
                revision: 2,
                operation_id: operation.clone(),
                client_sequence: 1,
                selection_revision: 3,
                expected_edit_state_id: format!("edit-v1-{}", "a".repeat(64)),
                edit: CanvasEdit::LayerAdd,
            })))
            .unwrap();
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(response.status, "ok");
        assert_eq!(response.data.unwrap()["operationId"], operation);
    }

    #[test]
    fn clip_public_tools_use_closed_schema_and_independent_credentials() {
        let tools = Bridge::tool_router().list_all();
        let edit = tools.iter().find(|t| t.name == "clip_edit").unwrap();
        let schema = serde_json::to_value(&edit.input_schema).unwrap();
        for key in [
            "projectId",
            "documentSessionId",
            "timelineId",
            "revision",
            "selectionRevision",
            "expectedEditStateId",
            "operationId",
            "clientSequence",
            "edit",
        ] {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(key))
            );
        }
        assert_eq!(schema["additionalProperties"], false);
        let query = tools.iter().find(|t| t.name == "clip_operation").unwrap();
        let schema = serde_json::to_value(&query.input_schema).unwrap();
        assert!(schema["properties"].get("revision").is_none());
        assert_eq!(
            bridge(Some("w".into()))
                .request(Method::ClipEdit)
                .client_token,
            "w"
        );
        assert_eq!(
            bridge(Some("w".into()))
                .request(Method::ClipOperation)
                .client_token,
            "read-credential"
        );
    }
    #[cfg(unix)]
    #[test]
    fn clip_public_call_reaches_exact_typed_ipc_and_returns_original_receipt() {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("sc-clip-test-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let r: Request = concat_editor_mcp::read_message(&mut stream).unwrap();
            assert_eq!(r.method, Method::ClipEdit);
            assert_eq!(r.client_token, "w");
            assert!(concat_editor_mcp::validate_request_shape(&r).is_ok());
            let p = r.clip_parameters.unwrap();
            assert_eq!(p.action.timeline_id, "timeline:TL1");
            assert_eq!(p.action.edit, concat_editor_mcp::ClipEdit::AddTrack);
            concat_editor_mcp::write_message(&mut stream,&Response::ok(serde_json::json!({"tool":"clip_edit","operationId":p.operation_id,"changed":true}))).unwrap();
        });
        let mut adapter = bridge(Some("w".into()));
        adapter.socket = path.clone();
        let op = concat_editor_mcp::operation_id("client", "s", 1).unwrap();
        let result: Response = serde_json::from_str(&adapter.clip_edit(Parameters(ClipEditArgs {
            project_id: "p".into(),
            document_session_id: "s".into(),
            revision: 1,
            selection_revision: 2,
            timeline_id: "timeline:TL1".into(),
            expected_edit_state_id: format!("clip-edit-v1-{}", "a".repeat(64)),
            operation_id: op.clone(),
            client_sequence: 1,
            edit: concat_editor_mcp::ClipEdit::AddTrack,
        })))
        .unwrap();
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.data.unwrap()["operationId"], op);
    }

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
    fn retired_flow_tools_are_not_advertised() {
        let tools = Bridge::tool_router().list_all();
        assert!(tools.iter().any(|tool| tool.name == "save_canvas_project"));
        assert!(
            tools
                .iter()
                .all(|tool| !matches!(tool.name.as_ref(), "flow_read" | "flow_write"))
        );
    }

    #[test]
    fn embedded_flow_tools_are_explicit_and_have_frozen_identity_fields() {
        let tools = Bridge::tool_router().list_all();
        let names = [
            "flow_get_workspace",
            "flow_read_canvas",
            "flow_list_node_types",
            "flow_get_node_tools",
            "flow_get_node_config",
            "flow_get_task",
            "flow_get_operation",
            "flow_add_node",
            "flow_update_node",
            "flow_connect_nodes",
            "flow_delete_nodes",
            "flow_switch_canvas",
            "flow_execute_node",
        ];
        for name in names {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .expect("named Flow tool");
            let schema = serde_json::to_value(&tool.input_schema).unwrap();
            for field in [
                "projectId",
                "canvasId",
                "sessionId",
                "accountScope",
                "revision",
            ] {
                assert!(
                    schema["required"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!(field)),
                    "{name}: {field}"
                );
            }
            assert_eq!(schema["additionalProperties"], false);
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
