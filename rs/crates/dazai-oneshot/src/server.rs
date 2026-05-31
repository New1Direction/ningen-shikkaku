//! The MCP server: dynamic tool dispatch (manual `ServerHandler`, since tools
//! are configured at runtime), wired to the call counter and the wipe+exit path.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ListToolsResult, PaginatedRequestParam,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};

use crate::counter::CallCounter;
use crate::tools::{self, Action};
use crate::{wipe_and_exit, ExitCtx};

/// The self-immolating MCP server.
pub struct OneshotServer {
    exit: Arc<ExitCtx>,
    counter: Arc<CallCounter>,
}

impl OneshotServer {
    /// Build a server from the shared exit context and call counter.
    pub fn new(exit: Arc<ExitCtx>, counter: Arc<CallCounter>) -> Self {
        OneshotServer { exit, counter }
    }

    /// Spawn the wipe+exit on a dedicated OS thread (outside the async runtime),
    /// after the current response has been returned to rmcp for sending.
    fn trigger_exit(&self) {
        let ctx = Arc::clone(&self.exit);
        std::thread::spawn(move || wipe_and_exit(&ctx));
    }
}

/// An MCP input schema for a tool that takes no arguments.
fn empty_object_schema() -> Arc<serde_json::Map<String, serde_json::Value>> {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    schema.insert(
        "properties".to_string(),
        serde_json::Value::Object(serde_json::Map::new()),
    );
    Arc::new(schema)
}

impl ServerHandler for OneshotServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "dazai-oneshot: serves a fixed number of tool calls, then wipes its \
                 secret state and exits."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let descriptors = {
            let tools = self.exit.tools.lock().unwrap_or_else(|p| p.into_inner());
            tools.descriptors()
        };
        let tools = descriptors
            .into_iter()
            .map(|(name, description)| Tool::new(name, description, empty_object_schema()))
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Once an exit condition has fired, serve nothing more — otherwise the
        // client could keep re-reading the un-wiped secret during the
        // flush/grace window before the wipe runs.
        if self.exit.closed.load(Ordering::SeqCst) {
            return Err(McpError::internal_error(
                "dazai-oneshot has fired its exit condition; no longer serving",
                None,
            ));
        }

        let name = request.name.as_ref();
        // Resolve the action without holding the registry lock across I/O.
        let action = {
            let tools = self.exit.tools.lock().unwrap_or_else(|p| p.into_inner());
            tools.lookup(name)
        };
        let action = action
            .ok_or_else(|| McpError::invalid_params(format!("unknown tool {name:?}"), None))?;

        let output = match action {
            Action::Static(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Action::Exec(cmd) => tools::run_command(&cmd)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?,
        };

        // Count this completed call. If it satisfies the exit condition, close
        // serving SYNCHRONOUSLY (so the very next call is refused — stdio
        // dispatch is sequential) before spawning the deferred wipe+exit. This
        // call's response is still returned to rmcp and sent.
        if self.counter.decrement() {
            self.exit.closed.store(true, Ordering::SeqCst);
            self.trigger_exit();
        }
        Ok(CallToolResult::success(vec![Content::text(output)]))
    }
}

/// Serve the MCP server over stdio until the client disconnects.
pub async fn serve_stdio(server: OneshotServer) -> anyhow::Result<()> {
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
