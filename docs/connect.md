# Connect

[Enfour Memory](../README.md)

Use an existing server's URL and bearer token. Add this to `~/.codex/config.toml` on the agent's computer:

```toml
[mcp_servers.enfour-memory]
url = "http://memory.example.com:7463/mcp"
http_headers = { Authorization = "Bearer YOUR_TOKEN" }
startup_timeout_sec = 30
tool_timeout_sec = 180
```

Replace the hostname and token. Keep the configuration private, then restart the client.
Both `/mcp` and `/mcp/rag` use hybrid retrieval and reranking.
One token gives access to all scopes. Use a trusted LAN, TLS proxy, or SSH tunnel.

Install the skills on the agent's computer with Node.js and `npx`:

```sh
DISABLE_TELEMETRY=1 npx skills add http://memory.example.com:7463 --agent codex --skill enfour-recall enfour-maintain -g
```

The skills select a stable project scope, check saved claims, and help with memory writes. Try:

> Save this project decision: use SQLite for local storage. Use this session as the source.

In a new session:

> Recall the storage decision for this project and show its source.

The server runs the local models. No model files or memory checkout are necessary on clients.
Enfour uses no cloud inference or telemetry. Your agent can send retrieved text to its model provider.
