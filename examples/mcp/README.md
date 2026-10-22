# MCP Client Example

`text-tools/server.mjs` is a dependency-free MCP stdio server.

```bash
node examples/mcp/text-tools/server.mjs
```

Configure Yukii with:

```bash
YUKII_MCP_CONFIG=examples/mcp/mcp-servers.example.json
```

The server tools are exposed as:

```text
mcp__text_tools__text_stats
```

The client starts the server as a child process, performs `initialize`, `tools/list`, then routes tool calls through `tools/call`.

`desktop-host` also reads `YUKII_MCP_CONFIG` for both CUA and browser modes; its MCP tools are forced to `local_computer` target.

HTTP example:

```bash
node examples/mcp/text-tools/http-server.mjs
```

```bash
YUKII_MCP_CONFIG=examples/mcp/mcp-http-servers.example.json \
  ./tooling/cargo.sh run -p yukii-api-server
```

SSE example uses the same server at `/mcp-sse`:

```bash
YUKII_MCP_CONFIG=examples/mcp/mcp-sse-servers.example.json \
  ./tooling/cargo.sh run -p yukii-api-server
```

Tool allowlist:

```json
{
  "name": "text_tools",
  "command": "node",
  "args": ["examples/mcp/text-tools/server.mjs"],
  "enabled_tools": ["text_stats"]
}
```

Limitations:

- stdio and basic Streamable HTTP (JSON/SSE response) are supported;
- one request at a time;
- no OAuth/remote HTTP transport yet;
- MCP content images have no size metadata (`width/height = 0`);
- server command comes only from local config, never from model arguments.
