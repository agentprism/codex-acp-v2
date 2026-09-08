"""MCP Apps ACP workflow with a deterministic app-server peer; no real MCP or model."""

import json
import os
import queue
import subprocess
import sys
import threading

sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")

UI_URI = "ui://weather/dashboard.html"
UI_CAPABILITY = {"mimeTypes": ["text/html;profile=mcp-app"]}


def emit(frame):
    print(json.dumps(frame), flush=True)


def backend():
    thread = None
    item = None
    reads = 0
    calls = 0
    for line in sys.stdin:
        request = json.loads(line)
        method, params = request.get("method"), request.get("params", {})
        if method == "initialized":
            continue
        if method == "initialize":
            assert params["capabilities"]["extensions"] == {
                "io.modelcontextprotocol/ui": UI_CAPABILITY,
            }
            result = {"userAgent": "mcp-apps-fixture"}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method in ("thread/start", "thread/resume"):
            if thread is None:
                thread = {"id": "apps-root", "cwd": params["cwd"], "status": {"type": "idle"}, "parentThreadId": None, "turns": []}
            else:
                assert method == "thread/resume" and reads == 1 and calls == 1
            result = {"thread": thread, "model": "fixture", "sandbox": {"type": "readOnly"}, "approvalPolicy": "on-request"}
        elif method == "turn/start":
            assert item is None, "history replay must not create another model turn"
            item = {
                "id": "widget-call", "type": "mcpToolCall", "server": "codex_apps", "tool": "show_dashboard",
                "status": "inProgress", "arguments": {"city": "Paris"}, "result": None, "error": None,
                "appContext": {"connectorId": "weather-connector", "linkId": "weather-link", "resourceUri": UI_URI,
                               "appName": "Weather", "actionName": "dashboard", "futureField": {"retained": True}},
                "pluginId": "weather@example", "readOnlyHint": True,
            }
            emit({"method": "turn/started", "params": {"threadId": thread["id"], "turn": {"id": "apps-turn", "status": "inProgress", "items": []}}})
            emit({"method": "item/started", "params": {"threadId": thread["id"], "turnId": "apps-turn", "item": item}})
            emit({"id": request["id"], "result": {"turn": {"id": "apps-turn", "status": "inProgress", "items": []}}})
            item["status"] = "completed"
            item["result"] = {"content": [{"type": "text", "text": "20 C"}], "structuredContent": {"temperature": 20}, "_meta": {"viewState": {"units": "C"}}}
            emit({"method": "item/completed", "params": {"threadId": thread["id"], "turnId": "apps-turn", "item": item}})
            emit({"method": "turn/completed", "params": {"threadId": thread["id"], "turn": {"id": "apps-turn", "status": "completed", "items": [], "error": None}}})
            continue
        elif method == "mcpServer/resource/read":
            assert params == {"threadId": "apps-root", "server": "codex_apps", "originCallId": "widget-call", "uri": UI_URI, "connectorId": "weather-connector"}
            reads += 1
            result = {"originCallId": "widget-call", "contents": [{
                "uri": UI_URI, "mimeType": "text/html;profile=mcp-app", "text": "<html><body>Weather</body></html>",
                "_meta": {"ui": {"csp": {"connectDomains": ["https://weather.example"]}, "prefersBorder": True}},
            }]}
        elif method == "mcpServer/tool/call":
            assert params == {"threadId": "apps-root", "server": "codex_apps", "tool": "refresh_dashboard", "arguments": {"city": "Paris"}, "_meta": {"widgetSession": "opaque"}}
            calls += 1
            result = {"content": [{"type": "text", "text": "21 C"}], "structuredContent": {"temperature": 21}, "isError": False, "_meta": {"viewState": {"units": "C", "refreshed": True}}}
        elif method == "thread/read":
            result = {"thread": thread}
        elif method == "thread/items/list":
            result = {"data": [{"turnId": "apps-turn", "item": item}], "nextCursor": None}
        elif method == "thread/turns/list":
            result = {"data": [{"id": "apps-turn", "status": "completed", "items": [], "error": None}], "nextCursor": None}
        elif method in ("thread/list", "thread/loaded/list"):
            result = {"data": [], "nextCursor": None}
        elif method in ("thread/backgroundTerminals/clean", "thread/unsubscribe"):
            result = {"status": "unsubscribed"} if method == "thread/unsubscribe" else {}
        else:
            raise AssertionError((method, params))
        emit({"id": request["id"], "result": result})
    assert reads == 2 and calls == 1, (reads, calls)


def exercise(binary):
    capabilities = {"extensions": {"io.modelcontextprotocol/ui": UI_CAPABILITY}}
    process = subprocess.Popen([
        binary, "--codex-path", sys.executable, "--codex-arg", os.path.abspath(__file__), "--codex-arg", "backend",
        "--request-timeout-seconds", "5", "--interaction-timeout-seconds", "5",
        "--backend-capabilities", json.dumps(capabilities),
    ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, encoding="utf-8")
    received = queue.Queue()
    updates = []
    next_id = 0

    def reader():
        for line in process.stdout:
            received.put(json.loads(line))
        received.put(None)

    threading.Thread(target=reader, daemon=True).start()

    def read():
        frame = received.get(timeout=8)
        assert frame is not None, "adapter exited early"
        return frame

    def notification(frame):
        assert frame["method"] == "session/update", frame
        updates.append(frame["params"]["update"])

    def rpc(method, params):
        nonlocal next_id
        next_id += 1
        process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}) + "\n")
        process.stdin.flush()
        while True:
            frame = read()
            if frame.get("id") == next_id and "method" not in frame:
                assert "result" in frame, frame
                return frame["result"]
            notification(frame)

    def read_widget(session, tool):
        binding = tool["_meta"]["codex"]["mcpToolCall"]
        context = binding["appContext"]
        response = rpc("_codex/request", {
            "version": 1, "sessionId": session, "method": "mcpServer/resource/read",
            "params": {"threadId": session, "server": binding["server"], "originCallId": tool["toolCallId"],
                       "uri": context["resourceUri"], "connectorId": context["connectorId"]},
        })
        assert response == {"originCallId": "widget-call", "contents": [{
            "uri": UI_URI, "mimeType": "text/html;profile=mcp-app", "text": "<html><body>Weather</body></html>",
            "_meta": {"ui": {"csp": {"connectDomains": ["https://weather.example"]}, "prefersBorder": True}},
        }]}, response

    try:
        rpc("initialize", {"protocolVersion": 2, "info": {"name": "mcp-apps-probe", "version": "1"},
                           "capabilities": {"_meta": {"codex": {"version": 1, "serverRequests": True}}}})
        session = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": []})["sessionId"]
        updates.clear()
        rpc("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "Show the weather"}]})
        while not any(update.get("state") == "idle" for update in updates):
            notification(read())
        tools = [update for update in updates if update["sessionUpdate"] == "tool_call_update"]
        expected_binding = {"server": "codex_apps", "tool": "show_dashboard", "pluginId": "weather@example", "readOnlyHint": True,
                            "appContext": {"connectorId": "weather-connector", "linkId": "weather-link", "resourceUri": UI_URI,
                                           "appName": "Weather", "actionName": "dashboard", "futureField": {"retained": True}}}
        assert [tool["status"] for tool in tools] == ["in_progress", "completed"], tools
        assert all(tool["_meta"] == {"codex": {"mcpToolCall": expected_binding}} for tool in tools), tools
        completed = tools[-1]
        assert completed["rawInput"] == {"city": "Paris"}
        assert completed["rawOutput"] == {"content": [{"type": "text", "text": "20 C"}], "structuredContent": {"temperature": 20}, "_meta": {"viewState": {"units": "C"}}}
        read_widget(session, completed)
        refreshed = rpc("_codex/request", {"version": 1, "sessionId": session, "method": "mcpServer/tool/call",
                        "params": {"threadId": session, "server": expected_binding["server"], "tool": "refresh_dashboard",
                                   "arguments": completed["rawInput"], "_meta": {"widgetSession": "opaque"}}})
        assert refreshed == {"content": [{"type": "text", "text": "21 C"}], "structuredContent": {"temperature": 21}, "isError": False, "_meta": {"viewState": {"units": "C", "refreshed": True}}}
        rpc("session/close", {"sessionId": session})
        updates.clear()
        rpc("session/resume", {"sessionId": session, "cwd": os.getcwd(), "mcpServers": [], "replayFrom": {"type": "start"}})
        replayed = [update for update in updates if update["sessionUpdate"] == "tool_call_update"]
        assert replayed == [completed], replayed
        read_widget(session, replayed[0])
        rpc("session/close", {"sessionId": session})
        process.stdin.close()
        assert process.wait(timeout=8) == 0
        print("MCP Apps binding, resource metadata, UI tool result and replay preserved through ACP")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


if __name__ == "__main__":
    if sys.argv[1] == "backend":
        backend()
    else:
        exercise(sys.argv[1])
