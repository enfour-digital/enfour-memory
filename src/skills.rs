//! Static, public skill distribution. No engine, filesystem writes, or execution.
use crate::output::OutputFormat;
use axum::{Router, routing::get};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

struct Skill {
    name: &'static str,
    text: &'static str,
}
const SKILLS: &[Skill] = &[
    Skill {
        name: "enfour-recall",
        text: include_str!("../skills/enfour-recall/SKILL.md"),
    },
    Skill {
        name: "enfour-maintain",
        text: include_str!("../skills/enfour-maintain/SKILL.md"),
    },
];
const ROOT: &str = "/.well-known/agent-skills";
static INDEX: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
        "skills": SKILLS.iter().map(|skill| json!({
            "name": skill.name,
            "type": "skill-md",
            "description": skill.text.lines().find_map(|line| line.strip_prefix("description: ")).expect("embedded skill description"),
            "url": format!("{}/SKILL.md", skill.name),
            "digest": format!("sha256:{}", Sha256::digest(skill.text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()),
        })).collect::<Vec<_>>()
    })
});

pub fn is_public_path(path: &str) -> bool {
    matches!(path, "/mcp/skills" | "/mcp/skills/")
        || path == format!("{ROOT}/index.json")
        || SKILLS
            .iter()
            .any(|s| path == format!("{ROOT}/{}/SKILL.md", s.name))
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    let mut router = Router::new().route(
        &format!("{ROOT}/index.json"),
        get(|| async { axum::Json(INDEX.clone()) }),
    );
    for skill in SKILLS {
        router = router.route(
            &format!("{ROOT}/{}/SKILL.md", skill.name),
            get(move || async move {
                (
                    [("content-type", "text/markdown; charset=utf-8")],
                    skill.text,
                )
            }),
        );
    }
    router
}

const INSTALL_INSTRUCTIONS: [&str; 4] = [
    "Run on the agent's computer. Install Node.js and npm first. Review the two skills before installation.",
    "The -g option installs for all projects. Remove -g to install in the current project. Change --agent for a different supported agent.",
    "Connect the memory MCP endpoint with its bearer token. Skill installation does not give database access.",
    "Restart the agent after installation. Then remove the installation endpoint from the client.",
];
const MANUAL_INSTALL_INSTRUCTIONS: &str = "Download each SKILL.md from the index into ~/.agents/skills/<skill-name>/SKILL.md for Codex. Keep the folder names. Review a previous file before replacement.";

#[derive(Clone)]
pub struct SkillServer {
    tool_router: ToolRouter<Self>,
    output_format: OutputFormat,
}
#[derive(Deserialize, JsonSchema)]
pub struct Install {
    /// Reachable HTTP(S) origin of this server, e.g. https://memory.example.com.
    /// Use the URL of this MCP endpoint without /mcp/skills.
    pub source_url: String,
}
impl SkillServer {
    pub fn new(output_format: OutputFormat) -> Self {
        Self {
            tool_router: Self::tool_router(),
            output_format,
        }
    }
}
fn source_origin(source: &str) -> Result<String, ErrorData> {
    let url = reqwest::Url::parse(source)
        .map_err(|_| ErrorData::invalid_params("source_url must be an HTTP(S) origin", None))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ErrorData::invalid_params(
            "source_url must contain only scheme, host and optional port",
            None,
        ));
    }
    Ok(url.origin().ascii_serialization())
}
#[tool_router]
impl SkillServer {
    #[tool(
        annotations(read_only_hint = true, open_world_hint = false),
        description = "Return skills.sh installation instructions and included skill metadata. Does not install files or access memory. Run the command on the client computer only when the user requests installation."
    )]
    async fn install_skills(
        &self,
        Parameters(args): Parameters<Install>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let origin = source_origin(&args.source_url)?;
        // Quote even validated origins: never turn a URL into shell syntax.
        let quoted = format!("'{}'", origin.replace('\'', "'\\''"));
        let format = match ctx.extensions.get::<axum::http::request::Parts>() {
            Some(parts) => OutputFormat::from_headers(&parts.headers, self.output_format)
                .map_err(|e| ErrorData::invalid_params(e, None))?,
            None => self.output_format,
        };
        let value = json!({
            "command": format!("DISABLE_TELEMETRY=1 npx skills add {quoted} --agent codex --skill enfour-recall enfour-maintain -g"),
            "instructions": INSTALL_INSTRUCTIONS,
            "index_url": format!("{origin}{ROOT}/index.json"),
            "skills": INDEX["skills"],
            "manual_install": MANUAL_INSTALL_INSTRUCTIONS,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format
                .encode(&value)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        )]))
    }
}
#[tool_handler(router = self.tool_router)]
impl ServerHandler for SkillServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Public Enfour Memory skill installation instructions. Call install_skills with this server's HTTP(S) origin. This endpoint has no memory tools and cannot execute commands.")
            .with_server_info(Implementation::new("Enfour Memory Skills", env!("CARGO_PKG_VERSION")))
    }
}
