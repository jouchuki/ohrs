#!/usr/bin/env python3
"""
Minimal JSON-RPC 2.0 MCP echo server (stdio transport).

Implements exactly the MCP messages that the oh-mcp rmcp client sends:
  - initialize  → InitializeResult with capabilities
  - initialized (notification) → ignored
  - tools/list  → ListToolsResult with one "echo" tool
  - tools/call  → CallToolResult echoing the "msg" argument
  - ping        → empty result {}
"""
import json
import sys
import os

def send(msg: dict) -> None:
    line = json.dumps(msg)
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def handle(msg: dict) -> None:
    method = msg.get("method", "")
    id_ = msg.get("id")

    # Notifications have no id and expect no response.
    if id_ is None and method in ("notifications/initialized", "initialized"):
        return

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": id_,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {"listChanged": False},
                    "resources": {"listChanged": False},
                },
                "serverInfo": {"name": "echo-fixture", "version": "0.1.0"},
            },
        })

    elif method == "tools/list":
        send({
            "jsonrpc": "2.0",
            "id": id_,
            "result": {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echoes the msg argument back to the caller.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "msg": {"type": "string", "description": "Message to echo"}
                            },
                            "required": ["msg"],
                        },
                    }
                ]
            },
        })

    elif method == "resources/list":
        send({
            "jsonrpc": "2.0",
            "id": id_,
            "result": {"resources": []},
        })

    elif method == "tools/call":
        params = msg.get("params", {})
        tool = params.get("name", "")
        args = params.get("arguments", {})
        if tool == "echo":
            msg_val = args.get("msg", "")
            send({
                "jsonrpc": "2.0",
                "id": id_,
                "result": {
                    "content": [{"type": "text", "text": msg_val}],
                    "isError": False,
                },
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": id_,
                "error": {"code": -32601, "message": f"tool not found: {tool}"},
            })

    elif method == "ping":
        send({"jsonrpc": "2.0", "id": id_, "result": {}})

    else:
        # Unknown method — respond with method-not-found so rmcp doesn't hang.
        if id_ is not None:
            send({
                "jsonrpc": "2.0",
                "id": id_,
                "error": {"code": -32601, "message": f"method not found: {method}"},
            })


def main() -> None:
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            continue
        handle(msg)


if __name__ == "__main__":
    main()
