//! The MCP server: dynamic tool dispatch (manual `ServerHandler`, since tools
//! are configured at runtime), wired to the call counter and the wipe+exit path.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kiroku::Lifecycle;
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
        std::thread::spawn(move || wipe_and_exit(&ctx, kiroku::Reason::CallsExhausted));
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
                "motokano: serves a fixed number of tool calls, then wipes its \
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
        let name = request.name.as_ref();
        let sink = self.exit.lifecycle.as_deref();
        // Lifecycle records identify a tool by position + kind only; a call
        // naming an unknown tool is not recorded (its name is caller input).
        let record = |(tool, kind): (u32, kiroku::ReadKind), served: bool| {
            kiroku::emit(sink, Lifecycle::Read { tool, kind, served });
        };

        // Once an exit condition has fired, serve nothing more — otherwise the
        // client could keep re-reading the un-wiped secret during the
        // flush/grace window before the wipe runs.
        if self.exit.closed.load(Ordering::SeqCst) {
            let described = {
                let tools = self.exit.tools.lock().unwrap_or_else(|p| p.into_inner());
                tools.describe(name)
            };
            if let Some(d) = described {
                record(d, false);
            }
            return Err(McpError::internal_error(
                "motokano has fired its exit condition; no longer serving",
                None,
            ));
        }

        // Resolve the action without holding the registry lock across I/O.
        let (described, action) = {
            let tools = self.exit.tools.lock().unwrap_or_else(|p| p.into_inner());
            (tools.describe(name), tools.lookup(name))
        };
        let (Some(described), Some(action)) = (described, action) else {
            return Err(McpError::invalid_params(
                format!("unknown tool {name:?}"),
                None,
            ));
        };

        let output = match action {
            Action::Static(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Action::Exec(cmd) => match tools::run_command(&cmd).await {
                Ok(out) => out,
                Err(e) => {
                    record(described, false);
                    return Err(McpError::internal_error(e.to_string(), None));
                }
            },
        };
        // Recorded before the counter can schedule the wipe, so the read
        // always precedes the wipe in the sink.
        record(described, true);

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
