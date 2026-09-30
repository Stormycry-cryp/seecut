// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stdio MCP adapter. The running App owns all document and permission state.

use concat_editor_mcp::{Method, Request, call, endpoint};
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
    socket: PathBuf,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ProjectArgs {
    project_id: String,
    document_session_id: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PreviewArgs {
    project_id: String,
    document_session_id: String,
    revision: u64,
    context_revision: u64,
    #[serde(default)]
    max_edge: Option<u32>,
}

impl Bridge {
    fn request(&self, method: Method) -> Request {
        Request {
            instance_id: self.instance.clone(),
            client_id: self.client.clone(),
            client_token: self.token.clone(),
            method,
            project_id: None,
            document_session_id: None,
            revision: None,
            context_revision: None,
            offset: None,
            limit: None,
            max_edge: None,
        }
    }
    fn dispatch(&self, request: Request) -> String {
        serde_json::to_string(&call(&self.socket, &request))
            .unwrap_or_else(|_| "{\"status\":\"ioFailure\"}".into())
    }
}

#[tool_router(server_handler)]
impl Bridge {
    #[tool(
        description = "Read the running SeeCut instance's named M1 read-only capabilities and limits"
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
        description = "Read a bounded page of the current SeeCut project structure by project and document session ID"
    )]
    fn project(&self, Parameters(args): Parameters<ProjectArgs>) -> String {
        let mut request = self.request(Method::Project);
        request.project_id = Some(args.project_id);
        request.document_session_id = Some(args.document_session_id);
        request.offset = args.offset;
        request.limit = args.limit;
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
        return Err("usage: seecut-editor-mcp --instance UUID --client CLIENT_ID (set SEECUT_MCP_CLIENT_TOKEN)".into());
    }
    let token = std::env::var("SEECUT_MCP_CLIENT_TOKEN")?;
    let instance = args[2].clone();
    let client = args[4].clone();
    if client.is_empty() || client.len() > 64 || token.len() != 36 {
        return Err("invalid client ID or token".into());
    }
    let socket = endpoint(&instance)?;
    let server = Bridge {
        instance,
        client,
        token,
        socket,
    }
    .serve(stdio())
    .await?;
    server.waiting().await?;
    Ok(())
}
