use airlock_proto::airlock_controller_server::AirlockController;
use airlock_proto::{
    CallToolRequest, CallToolResponse, GetToolCallRequest, ListToolsRequest, ListToolsResponse,
    SendToolResultAck, SendToolResultRequest, ToolCallAssignment,
};
use tonic::{Request, Response, Status};

use crate::tools;

pub(crate) struct WorkspaceToolsService {
    max_output_chars: usize,
}

impl WorkspaceToolsService {
    pub(crate) fn new(max_output_chars: usize) -> Self {
        Self { max_output_chars }
    }
}

#[tonic::async_trait]
impl AirlockController for WorkspaceToolsService {
    async fn list_tools(
        &self,
        _request: Request<ListToolsRequest>,
    ) -> Result<Response<ListToolsResponse>, Status> {
        let tools = tools::tool_definitions();
        Ok(Response::new(ListToolsResponse { tools }))
    }

    async fn call_tool(
        &self,
        request: Request<CallToolRequest>,
    ) -> Result<Response<CallToolResponse>, Status> {
        let req = request.into_inner();

        let input: serde_json::Value = serde_json::from_str(&req.input_json)
            .map_err(|e| Status::invalid_argument(format!("invalid input_json: {e}")))?;

        let (output, is_error) =
            tools::execute_tool(&req.name, &input, self.max_output_chars).await;

        Ok(Response::new(CallToolResponse { output, is_error }))
    }

    async fn get_tool_call(
        &self,
        _request: Request<GetToolCallRequest>,
    ) -> Result<Response<ToolCallAssignment>, Status> {
        Err(Status::unimplemented(
            "get_tool_call is not supported by workspace-tools",
        ))
    }

    async fn send_tool_result(
        &self,
        _request: Request<SendToolResultRequest>,
    ) -> Result<Response<SendToolResultAck>, Status> {
        Err(Status::unimplemented(
            "send_tool_result is not supported by workspace-tools",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_tools_returns_four() {
        let service = WorkspaceToolsService::new(30000);
        let response = service
            .list_tools(Request::new(ListToolsRequest {}))
            .await
            .unwrap();
        let tools = &response.get_ref().tools;
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_directory"));
    }

    #[tokio::test]
    async fn call_tool_bash_echo() {
        let service = WorkspaceToolsService::new(30000);
        let response = service
            .call_tool(Request::new(CallToolRequest {
                name: "bash".into(),
                input_json: r#"{"command": "echo hello"}"#.into(),
            }))
            .await
            .unwrap();
        let resp = response.get_ref();
        assert!(!resp.is_error);
        assert!(resp.output.contains("hello"));
    }

    #[tokio::test]
    async fn call_tool_invalid_json() {
        let service = WorkspaceToolsService::new(30000);
        let result = service
            .call_tool(Request::new(CallToolRequest {
                name: "bash".into(),
                input_json: "not json".into(),
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn call_tool_unknown_tool() {
        let service = WorkspaceToolsService::new(30000);
        let response = service
            .call_tool(Request::new(CallToolRequest {
                name: "nonexistent".into(),
                input_json: "{}".into(),
            }))
            .await
            .unwrap();
        let resp = response.get_ref();
        assert!(resp.is_error);
        assert!(resp.output.contains("unknown tool"));
    }

    #[tokio::test]
    async fn get_tool_call_unimplemented() {
        let service = WorkspaceToolsService::new(30000);
        let result = service
            .get_tool_call(Request::new(GetToolCallRequest {
                job_id: String::new(),
                tool_name: String::new(),
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unimplemented);
    }

    #[tokio::test]
    async fn send_tool_result_unimplemented() {
        let service = WorkspaceToolsService::new(30000);
        let result = service
            .send_tool_result(Request::new(SendToolResultRequest {
                call_id: String::new(),
                output: String::new(),
                is_error: false,
                exit_code: 0,
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unimplemented);
    }
}
