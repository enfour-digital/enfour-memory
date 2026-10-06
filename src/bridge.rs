//! A thin official-SDK client/server bridge: one shared inference process,
//! stdio compatibility for agents, and no token in the agent configuration.
use anyhow::Result;
use rmcp::transport::{
    stdio,
    streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig},
};
use rmcp::{
    ErrorData, Peer, RoleClient, RoleServer, ServerHandler, ServiceExt, model::*,
    service::RequestContext,
};
use std::path::Path;
struct Bridge {
    peer: Peer<RoleClient>,
}
impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(crate::server::INSTRUCTIONS)
            .with_server_info(Implementation::new(
                "Enfour Memory connector",
                env!("CARGO_PKG_VERSION"),
            ))
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.peer
            .list_tools(request)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.peer
            .call_tool_once(request)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}
pub async fn connect(url: String, token_file: &Path) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let token = std::fs::read_to_string(token_file)?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()?;
    let transport = StreamableHttpClientTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.trim()),
    );
    let remote = ().serve(transport).await?;
    let bridge = Bridge {
        peer: remote.peer().clone(),
    };
    bridge.serve(stdio()).await?.waiting().await?;
    remote.cancel().await?;
    Ok(())
}
