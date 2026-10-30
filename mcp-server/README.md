# Meetily MCP server

A read-only [MCP](https://modelcontextprotocol.io) server that lets Claude read the Meetily app's local database
(`~/Library/Application Support/com.meetily.ai/meeting_minutes.sqlite`). It never writes to it.

Tools: `list_meetings`, `get_meeting`, `get_summary`, `get_transcript`, `search_transcripts`.

## Setup

```bash
cd mcp-server
python3.11 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

Register it in Claude Desktop (`~/Library/Application Support/Claude/claude_desktop_config.json`), then restart Desktop:

```json
{
  "mcpServers": {
    "meetily": {
      "command": "/path/to/meetily/mcp-server/.venv/bin/python",
      "args": ["/path/to/meetily/mcp-server/meetily_mcp.py"]
    }
  }
}
```

Or in Claude Code:

```bash
claude mcp add --scope user meetily -- /path/to/meetily/mcp-server/.venv/bin/python /path/to/meetily/mcp-server/meetily_mcp.py
```

Set `MEETILY_DB` to point at a different database file.
