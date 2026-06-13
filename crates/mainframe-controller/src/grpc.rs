//! gRPC service backing the mainframe-controller's tool + persona surface.
//!
//! Three call sites: the transponder fetches the LLM-tool list via
//! `WatchTools` and dispatches `Skill`/`Skills` via `CallTool`; the
//! transponder also calls `GetAgent("")` per turn for the primary
//! persona; the transponder's `Agent`/`Agents` runtime primitives call
//! `GetAgent(name)` and `ListAgents()`.
//!
//! The trust property the gRPC layer enforces: workspace identity comes
//! from the SA token, never from the request body. Every handler routes
//! through `verify_workspace` to derive it.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use mainframe_proto::mainframe_controller_server::MainframeController;
use mainframe_proto::{
    AgentInfo, CallToolRequest, CallToolResponse, GetAgentRequest, GetAgentResponse,
    ListAgentsRequest, ListAgentsResponse, ToolInfo, ToolListUpdate, WatchToolsRequest,
};

use crate::kernel::{first_paragraph, Kernel, KernelError};
use shared::auth::{extract_bearer_token, TokenVerifier};

pub const SKILL_TOOL_NAME: &str = "Skill";
pub const SKILLS_TOOL_NAME: &str = "Skills";

pub struct ControllerService {
    kernel: Arc<Kernel>,
    verifier: Option<Arc<dyn TokenVerifier>>,
}

impl ControllerService {
    pub fn new(kernel: Arc<Kernel>, verifier: Option<Arc<dyn TokenVerifier>>) -> Self {
        Self { kernel, verifier }
    }

    async fn verify_workspace<T>(&self, request: &Request<T>) -> Result<String, Status> {
        match &self.verifier {
            Some(verifier) => {
                let token = extract_bearer_token(request)?;
                verifier.verify_token(token).await
            }
            // Dev / test path: skip auth, attribute the call to the
            // dev-fixed workspace name. Real deployments always have a
            // verifier; the chart wires one in.
            None => Ok("dev".to_string()),
        }
    }
}

/// Static LLM-facing tool list. Today the registry never changes — the
/// `Skill` / `Skills` shape is fixed and skill content discovery happens
/// per call. If we later add tools or dynamic refresh, the `WatchTools`
/// stream can re-emit; for v1 it sends one snapshot then idles.
fn static_tool_list() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: SKILL_TOOL_NAME.into(),
            description: "Read a skill file from the mainframe and return its markdown contents. \
                          Skills are operator-authored procedures the agent can follow."
                .into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name (basename of skills/<name>.md, without the .md extension)."
                    }
                },
                "required": ["name"]
            })
            .to_string(),
        },
        ToolInfo {
            name: SKILLS_TOOL_NAME.into(),
            description: "List the names of skills available in the current workspace.".into(),
            parameters_json: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
            .to_string(),
        },
    ]
}

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

#[tonic::async_trait]
impl MainframeController for ControllerService {
    type WatchToolsStream =
        Pin<Box<dyn Stream<Item = Result<ToolListUpdate, Status>> + Send + 'static>>;

    async fn watch_tools(
        &self,
        request: Request<WatchToolsRequest>,
    ) -> Result<Response<Self::WatchToolsStream>, Status> {
        // Authenticate but discard the workspace — the LLM-facing tool
        // shape is identical across workspaces.
        let _ = self.verify_workspace(&request).await?;

        let (tx, rx) = mpsc::channel::<Result<ToolListUpdate, Status>>(1);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(ToolListUpdate {
                    tools: static_tool_list(),
                }))
                .await;
            // Static for v1: park the sender, never re-emit. When the
            // client disconnects, tx drops and the loop exits naturally.
            tx.closed().await;
        });

        let stream: Self::WatchToolsStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }

    async fn call_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();

        match req.name.as_str() {
            SKILL_TOOL_NAME => {
                let args: SkillArgs = serde_json::from_str(&req.input_json).map_err(|e| {
                    Status::invalid_argument(format!("invalid Skill arguments: {e}"))
                })?;
                match self.kernel.read_skill(&workspace, &args.name) {
                    Ok(content) => Ok(Response::new(CallToolResponse {
                        output: content,
                        is_error: false,
                    })),
                    Err(KernelError::NotFound) => Ok(Response::new(CallToolResponse {
                        output: format!("skill not found: {}", args.name),
                        is_error: true,
                    })),
                    Err(KernelError::InvalidName(n)) => Ok(Response::new(CallToolResponse {
                        output: format!("invalid skill name: {n}"),
                        is_error: true,
                    })),
                    Err(KernelError::PathEscape) => Ok(Response::new(CallToolResponse {
                        output: "skill path escapes workspace root".into(),
                        is_error: true,
                    })),
                    Err(KernelError::Io(e)) => Err(Status::internal(format!("io error: {e}"))),
                }
            }
            SKILLS_TOOL_NAME => {
                let names = self
                    .kernel
                    .list_skills(&workspace)
                    .map_err(|e| Status::internal(format!("list skills failed: {e}")))?;
                let json = serde_json::to_string(&names)
                    .map_err(|e| Status::internal(format!("serialize: {e}")))?;
                Ok(Response::new(CallToolResponse {
                    output: json,
                    is_error: false,
                }))
            }
            other => Err(Status::not_found(format!("unknown tool: {other}"))),
        }
    }

    async fn get_agent(
        &self,
        request: Request<GetAgentRequest>,
    ) -> Result<Response<GetAgentResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let req = request.into_inner();

        let content = if req.name.is_empty() {
            self.kernel.read_primary_agent(&workspace)
        } else {
            self.kernel.read_agent(&workspace, &req.name)
        };

        match content {
            Ok(content) => Ok(Response::new(GetAgentResponse { content })),
            Err(KernelError::NotFound) => Err(Status::not_found("persona not found")),
            Err(KernelError::InvalidName(n)) => {
                Err(Status::invalid_argument(format!("invalid name: {n}")))
            }
            Err(KernelError::PathEscape) => Err(Status::invalid_argument("path escapes root")),
            Err(KernelError::Io(e)) => Err(Status::internal(format!("io error: {e}"))),
        }
    }

    async fn list_agents(
        &self,
        request: Request<ListAgentsRequest>,
    ) -> Result<Response<ListAgentsResponse>, Status> {
        let workspace = self.verify_workspace(&request).await?;
        let names = self
            .kernel
            .list_agents(&workspace)
            .map_err(|e| Status::internal(format!("list agents failed: {e}")))?;
        let mut agents = Vec::with_capacity(names.len());
        for name in names {
            // Best-effort description: read the file and extract its
            // first paragraph. A missing file mid-enumeration shouldn't
            // tank the listing (race between list and read), so we just
            // skip it.
            if let Ok(body) = self.kernel.read_agent(&workspace, &name) {
                agents.push(AgentInfo {
                    name,
                    description: first_paragraph(&body),
                });
            }
        }
        Ok(Response::new(ListAgentsResponse { agents }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tonic::Request;

    fn write_md(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    fn svc(root: &Path) -> ControllerService {
        ControllerService::new(Arc::new(Kernel::new(root)), None)
    }

    #[tokio::test]
    async fn call_tool_skill_returns_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "dev/skills/classify.md", "classify body");
        let svc = svc(tmp.path());
        let resp = svc
            .call_tool(Request::new(CallToolRequest {
                name: "Skill".into(),
                input_json: r#"{"name":"classify"}"#.into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.is_error);
        assert_eq!(resp.output, "classify body");
    }

    #[tokio::test]
    async fn call_tool_skill_missing_returns_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("dev")).unwrap();
        let svc = svc(tmp.path());
        let resp = svc
            .call_tool(Request::new(CallToolRequest {
                name: "Skill".into(),
                input_json: r#"{"name":"missing"}"#.into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.is_error);
        assert!(resp.output.contains("not found"));
    }

    #[tokio::test]
    async fn call_tool_skills_returns_sorted_names_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "dev/skills/beta.md", "b");
        write_md(tmp.path(), "dev/skills/alpha.md", "a");
        let svc = svc(tmp.path());
        let resp = svc
            .call_tool(Request::new(CallToolRequest {
                name: "Skills".into(),
                input_json: "{}".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.is_error);
        let names: Vec<String> = serde_json::from_str(&resp.output).unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn call_tool_unknown_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = svc(tmp.path());
        let status = svc
            .call_tool(Request::new(CallToolRequest {
                name: "Nope".into(),
                input_json: "{}".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn get_agent_empty_name_returns_primary() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "dev/AGENTS.md", "primary persona");
        let svc = svc(tmp.path());
        let resp = svc
            .get_agent(Request::new(GetAgentRequest {
                name: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.content, "primary persona");
    }

    #[tokio::test]
    async fn get_agent_named_returns_sub_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "dev/agents/alice.md", "alice persona");
        let svc = svc(tmp.path());
        let resp = svc
            .get_agent(Request::new(GetAgentRequest {
                name: "alice".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.content, "alice persona");
    }

    #[tokio::test]
    async fn get_agent_missing_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("dev")).unwrap();
        let svc = svc(tmp.path());
        let status = svc
            .get_agent(Request::new(GetAgentRequest {
                name: "ghost".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn list_agents_returns_name_and_description_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(
            tmp.path(),
            "dev/agents/alice.md",
            "# Alice\n\nThe legal specialist.\n",
        );
        write_md(
            tmp.path(),
            "dev/agents/bob.md",
            "# Bob\n\nThe ops specialist.\n",
        );
        let svc = svc(tmp.path());
        let resp = svc
            .list_agents(Request::new(ListAgentsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.agents.len(), 2);
        assert_eq!(resp.agents[0].name, "alice");
        assert_eq!(resp.agents[0].description, "The legal specialist.");
        assert_eq!(resp.agents[1].name, "bob");
        assert_eq!(resp.agents[1].description, "The ops specialist.");
    }

    #[tokio::test]
    async fn list_agents_empty_when_no_agents_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("dev")).unwrap();
        let svc = svc(tmp.path());
        let resp = svc
            .list_agents(Request::new(ListAgentsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.agents.is_empty());
    }

    #[tokio::test]
    async fn static_tool_list_includes_skill_and_skills() {
        let tools = static_tool_list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"Skills"));
    }
}
