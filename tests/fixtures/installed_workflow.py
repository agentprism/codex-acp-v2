"""Opt-in actual Codex workflow. All state and files are isolated; no paid inference."""

import base64
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Protocol streams are UTF-8 even when Windows uses a legacy console code page.
sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")


def mcp_peer():
    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method = request["method"]
        if method == "initialize":
            result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "workflow-mcp", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "echo", "description": "Return a deterministic integration marker", "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}}]}
        elif method == "tools/call":
            assert request["params"]["name"] == "echo"
            assert request["params"]["arguments"] == {"value": "from-codex"}
            result = {"content": [{"type": "text", "text": "mcp-ok:from-codex"}], "structuredContent": {"received": "from-codex"}, "isError": False}
        elif method in ("resources/list", "resources/templates/list"):
            result = {"resources" if method == "resources/list" else "resourceTemplates": []}
        elif method == "ping":
            result = {}
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": method}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)


class Model(BaseHTTPRequestHandler):
    phase = 0
    child_phase = 0
    child_model = None
    requests = []
    parent_requests = []
    cancellation_started = threading.Event()
    cancellation_done = threading.Event()

    def log_message(self, *_args):
        pass

    def do_POST(self):
        size = int(self.headers.get("Content-Length", 0))
        assert 0 < size <= 4 * 1024 * 1024
        request = json.loads(self.rfile.read(size))
        Model.requests.append(request)
        user_text = "\n".join(block.get("text", "") for item in request.get("input", []) if item.get("role") == "user" for block in item.get("content", []) if isinstance(block, dict))
        mode = max((user_text.rfind(f"[workflow:{name}]"), name) for name in ("tools", "child", "error", "cancel"))[1]
        if mode == "error":
            body = json.dumps({"error": {"message": "Intentional isolated provider failure", "type": "invalid_request_error", "code": "model_not_found"}}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if mode == "cancel":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(b'event: response.created\ndata: {"type":"response.created","response":{"id":"cancel-response"}}\n\n')
            self.wfile.flush()
            Model.cancellation_started.set()
            assert Model.cancellation_done.wait(30), "client never cancelled pending inference"
            return
        if mode == "child":
            stage = Model.child_phase
            Model.child_phase += 1
            if stage == 0:
                item = function("child-exec", "exec_command", {"cmd": "echo child-command-ok", "sandbox_permissions": "require_escalated", "justification": "Harmless child integration echo", "yield_time_ms": 1000})
            elif stage == 1:
                item = function("child-native-mcp", "echo", {"value": "native-from-child"})
                item["namespace"] = "mcp__native"
            else:
                assert stage == 2
                output = next(item["output"] for item in request["input"] if item.get("type") == "function_call_output" and item.get("call_id") == "child-native-mcp")
                assert '"received":"native-from-child"' in (output if isinstance(output, str) else json.dumps(output)), output
                item = message("child-answer", "child-done")
            self.send_items(item, f"child-{stage}", stage)
            return
        Model.parent_requests.append(request)
        stage = Model.phase
        Model.phase += 1
        if stage == 0:
            item = function("exec-fixture", "exec_command", {"cmd": "echo command-ok", "sandbox_permissions": "require_escalated", "justification": "Execute the harmless isolated integration echo", "yield_time_ms": 1000})
        elif stage == 1:
            item = function("patch-fixture", "exec_command", {"cmd": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: workflow.txt\n+patch-ok\n*** End Patch\nPATCH", "yield_time_ms": 1000})
        elif stage == 2:
            item = function("dynamic-ok", "audit_client", {"mode": "success"})
        elif stage == 3:
            item = function("dynamic-error", "audit_client", {"mode": "error"})
        elif stage == 4:
            item = function("mcp-fixture", "echo", {"value": "from-codex"})
            item["namespace"] = "mcp__workflow"
        elif stage == 5:
            item = function("native-mcp-fixture", "echo", {"value": "native-from-codex"})
            item["namespace"] = "mcp__native"
        elif stage == 6:
            item = function("spawn-fixture", "spawn_agent", {"message": "[workflow:child]", "model": Model.child_model})
            item["namespace"] = "multi_agent_v1"
        elif stage == 7:
            spawn = next(item["output"] for item in request["input"] if item.get("type") == "function_call_output" and item.get("call_id") == "spawn-fixture")
            child = json.loads(spawn)["agent_id"]
            item = function("wait-fixture", "wait_agent", {"targets": [child], "timeout_ms": 10000})
            item["namespace"] = "multi_agent_v1"
        else:
            assert stage == 8, "unexpected extra inference request"
            item = message("workflow-answer", "tools-ok")
        self.send_items(item, f"workflow-{stage}", stage)

    def send_items(self, item, response_id, stage):
        events = [
            {"type": "response.created", "response": {"id": response_id}},
            {"type": "response.output_item.done", "item": item},
            {"type": "response.completed", "response": {"id": response_id, "usage": {"input_tokens": 100 + stage, "output_tokens": 10, "total_tokens": 110 + stage}}},
        ]
        body = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def function(call_id, name, arguments):
    return {"type": "function_call", "call_id": call_id, "name": name, "arguments": json.dumps(arguments)}


def message(item_id, text):
    return {"type": "message", "role": "assistant", "id": item_id, "phase": "final_answer", "content": [{"type": "output_text", "text": text}]}


class Client:
    def __init__(self, binary, directory, url):
        self.events = []
        self.approvals = []
        self.callbacks = []
        self.mcp_connections = []
        self.mcp_disconnected = []
        self.mcp_calls = []
        self.next_id = 1
        self.frames = queue.Queue()
        environment = os.environ.copy()
        environment["CODEX_HOME"] = str(directory / "profile")
        environment.pop("OPENAI_API_KEY", None)
        environment.pop("CODEX_API_KEY", None)
        args = [binary, "--request-timeout-seconds", "20", "--interaction-timeout-seconds", "20"]
        config = {"name": "Workflow local mock", "base_url": url, "wire_api": "responses", "requires_openai_auth": False, "supports_websockets": False, "request_max_retries": 0, "stream_max_retries": 0}
        for key, value in config.items():
            args += ["--codex-arg=-c", f"--codex-arg=model_providers.workflow.{key}={json.dumps(value)}"]
        args += ["--codex-arg=-c", "--codex-arg=features.code_mode=false", "--codex-arg=-c", "--codex-arg=features.unified_exec=true"]
        self.process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, encoding="utf-8", env=environment, start_new_session=os.name != "nt")
        threading.Thread(target=self.reader, daemon=True).start()
        self.rpc("initialize", {"protocolVersion": 2, "info": {"name": "workflow-client", "version": "1"}, "capabilities": {"_meta": {"codex": {"version": 1, "events": [], "serverRequests": True}}}})

    def reader(self):
        for line in self.process.stdout:
            self.frames.put(json.loads(line))
        self.frames.put(None)

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def receive(self):
        try:
            frame = self.frames.get(timeout=30)
        except queue.Empty:
            raise AssertionError(f"timed out; recent ACP activity: {self.events[-12:]}") from None
        assert frame is not None, f"adapter exited: {self.process.poll()}"
        if frame.get("method", "").startswith("mcp/"):
            self.mcp_message(frame)
            return frame
        if "method" in frame and "id" in frame:
            if frame["method"] == "session/request_permission":
                self.approvals.append(frame)
                choice = next(option for option in frame["params"]["options"] if option["kind"] == "allow_once")
                self.send({"jsonrpc": "2.0", "id": frame["id"], "result": {"outcome": {"outcome": "selected", "optionId": choice["optionId"]}}})
            elif frame["method"] == "_codex/serverRequest":
                self.callbacks.append(frame)
                params = frame["params"]["params"]
                assert frame["params"]["method"] == "item/tool/call", frame
                assert params["tool"] == "audit_client", frame
                if params["arguments"]["mode"] == "success":
                    self.send({"jsonrpc": "2.0", "id": frame["id"], "result": {"contentItems": [{"type": "inputText", "text": "dynamic-ok"}], "success": True}})
                else:
                    self.send({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": -32042, "message": "intentional-dynamic-error", "data": {"retryable": False}}})
            else:
                raise AssertionError(f"unexpected backend callback: {frame}")
        if frame.get("method") == "session/update":
            self.events.append(frame["params"]["update"])
        return frame

    def mcp_message(self, frame):
        params = frame["params"]
        if frame["method"] == "mcp/connect":
            assert params["serverId"] == "workflow-native"
            connection = f"workflow-native-{len(self.mcp_connections)}"
            self.mcp_connections.append(connection)
            result = {"connectionId": connection}
        elif frame["method"] == "mcp/disconnect":
            assert params["connectionId"] in self.mcp_connections
            assert params["connectionId"] not in self.mcp_disconnected
            self.mcp_disconnected.append(params["connectionId"])
            result = {}
        else:
            assert frame["method"] == "mcp/message", frame
            assert params["connectionId"] in self.mcp_connections
            assert params["connectionId"] not in self.mcp_disconnected
            if "id" not in frame:
                assert params["method"] == "notifications/initialized", frame
                return
            method = params["method"]
            if method == "initialize":
                result = {"protocolVersion": params["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "workflow-native", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": "echo", "description": "Return a native bridge marker", "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}}]}
            elif method == "tools/call":
                assert params["params"]["name"] == "echo"
                value = params["params"]["arguments"]["value"]
                assert value in ("native-from-codex", "native-from-child")
                self.mcp_calls.append(params)
                result = {"content": [{"type": "text", "text": f"native-mcp-ok:{value}"}], "structuredContent": {"received": value}, "isError": False}
            elif method in ("resources/list", "resources/templates/list"):
                result = {"resources" if method == "resources/list" else "resourceTemplates": []}
            elif method == "ping":
                result = {}
            else:
                raise AssertionError(f"unexpected native MCP request: {frame}")
        self.send({"jsonrpc": "2.0", "id": frame["id"], "result": result})

    def rpc(self, method, params):
        request_id = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        while True:
            frame = self.receive()
            if frame.get("id") == request_id and "method" not in frame:
                assert "error" not in frame, (method, frame)
                return frame["result"]

    def idle_since(self, start):
        while not any(event.get("sessionUpdate") == "state_update" and event.get("state") == "idle" for event in self.events[start:]):
            self.receive()
        return self.events[start:]

    def shutdown(self):
        self.process.stdin.close()
        assert self.process.wait(timeout=15) == 0


def workflow(binary):
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="codex-acp-workflow-") as temporary:
        directory = Path(temporary)
        (directory / "profile").mkdir()
        workspace = directory / "workspace"
        workspace.mkdir()
        client = Client(binary, directory, f"http://127.0.0.1:{server.server_port}/v1")
        try:
            options = {"model": "workflow-model", "modelProvider": "workflow", "sandbox": "read-only", "approvalPolicy": "on-request", "dynamicTools": [{"type": "function", "name": "audit_client", "description": "Integration client callback", "inputSchema": {"type": "object", "properties": {"mode": {"type": "string"}}, "required": ["mode"]}}]}
            mcp = [
                {"type": "stdio", "name": "workflow", "command": sys.executable, "args": [str(Path(__file__).resolve()), "--mcp"], "env": []},
                {"type": "acp", "name": "native", "serverId": "workflow-native"},
            ]
            result = client.rpc("session/new", {"cwd": str(workspace), "mcpServers": mcp, "_meta": {"codex": {"thread": options}}})
            model_option = next(option for option in result["configOptions"] if option["configId"] == "model")
            Model.child_model = next(option["value"] for option in model_option["options"] if option["value"] != "workflow-model")
            session = result["sessionId"]
            client.idle_since(0)
            start = len(client.events)
            client.rpc("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "[workflow:tools]"}]})
            events = client.idle_since(start)
            assert (workspace / "workflow.txt").read_text() == "patch-ok\n"
            assert len(client.approvals) >= 2, client.approvals
            assert len(client.callbacks) == 2, client.callbacks
            rendered = json.dumps(events)
            assert "tools-ok" in rendered and "mcp-ok:from-codex" in rendered and "dynamic-ok" in rendered and "native-mcp-ok" in rendered, events
            assert {call["params"]["arguments"]["value"] for call in client.mcp_calls} == {"native-from-codex", "native-from-child"}, client.mcp_calls
            terminal = [event for event in events if event.get("sessionUpdate") == "terminal_update"]
            assert any("command-ok" in base64.b64decode(event.get("output", {}).get("data", "")).decode() and event.get("exitStatus", {}).get("exitCode") == 0 for event in terminal), terminal
            patches = [content for event in events for content in event.get("content", []) if isinstance(content, dict) and content.get("type") == "diff"]
            assert any(change["operation"] == "add" and change["path"] == str(workspace / "workflow.txt") for patch in patches for change in patch["changes"]), patches
            outputs = {item["call_id"]: item["output"] for item in Model.parent_requests[-1]["input"] if item.get("type") == "function_call_output"}
            assert "command-ok" in outputs["exec-fixture"] and "A workflow.txt" in outputs["patch-fixture"], outputs
            assert outputs["dynamic-ok"] == "dynamic-ok" and outputs["dynamic-error"] == "dynamic tool request failed", outputs
            assert '"received":"from-codex"' in outputs["mcp-fixture"], outputs
            assert '"received":"native-from-codex"' in outputs["native-mcp-fixture"], outputs
            live_ids = {event["toolCallId"] for event in events if event.get("sessionUpdate") == "tool_call_update"}
            assert any(item_id.startswith("codex-child:") and item_id.endswith(":child-exec") for item_id in live_ids), live_ids
            assert any("child-command-ok" in base64.b64decode(event.get("output", {}).get("data", "")).decode() for event in terminal), terminal
            client.rpc("session/close", {"sessionId": session})
            assert set(client.mcp_connections) == set(client.mcp_disconnected), (client.mcp_connections, client.mcp_disconnected)
            start = len(client.events)
            client.rpc("session/resume", {"sessionId": session, "cwd": str(workspace), "mcpServers": mcp, "replayFrom": {"type": "start"}})
            replay_ids = {event["toolCallId"] for event in client.events[start:] if event.get("sessionUpdate") == "tool_call_update"}
            assert live_ids <= replay_ids, (live_ids, replay_ids)
            start = len(client.events)
            client.rpc("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "[workflow:error]"}]})
            failed = client.idle_since(start)
            assert any("Intentional isolated provider failure" in str(event.get("content")) for event in failed), failed
            start = len(client.events)
            client.rpc("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "[workflow:cancel]"}]})
            assert Model.cancellation_started.wait(10)
            client.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}})
            cancelled = client.idle_since(start)
            assert any(event.get("stopReason") == "cancelled" for event in cancelled), cancelled
            Model.cancellation_done.set()
            client.rpc("session/close", {"sessionId": session})
            assert len(client.mcp_connections) >= 3 and set(client.mcp_connections) == set(client.mcp_disconnected), (client.mcp_connections, client.mcp_disconnected)
            client.shutdown()
            print("Actual Codex: command/file approvals, execution, patch projection, dynamic success/error, stdio and native ACP MCP, child tools, durable root/child replay, provider diagnosis, cancellation: passed.")
        finally:
            Model.cancellation_done.set()
            if client.process.poll() is None:
                if os.name != "nt":
                    os.killpg(client.process.pid, signal.SIGTERM)
                else:
                    client.process.terminate()
                client.process.wait(timeout=10)
            server.shutdown()


if __name__ == "__main__":
    if sys.argv[1] == "--mcp":
        mcp_peer()
    else:
        workflow(sys.argv[1])
