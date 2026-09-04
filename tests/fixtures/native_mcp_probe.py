"""Local native-MCP/HTTP protocol probe; no external service or model inference."""
import http.client
import json
import os
import queue
import subprocess
import sys
import threading
import urllib.parse


def emit(message):
    print(json.dumps(message), flush=True)


class HttpMcp:
    def __init__(self, url):
        self.url = urllib.parse.urlparse(url)
        assert self.url.hostname == "127.0.0.1"
        assert len(self.url.path) == 33, "unguessable UUID token must protect the endpoint"
        self.session = None
        self.notified = threading.Event()
        self.errors = queue.Queue()

    def post(self, frame):
        connection = http.client.HTTPConnection(self.url.hostname, self.url.port, timeout=8)
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        connection.request("POST", self.url.path, json.dumps(frame), headers)
        response = connection.getresponse()
        self.session = response.getheader("Mcp-Session-Id") or self.session
        body = response.read()
        assert response.status in (200, 202), (response.status, body)
        connection.close()
        return json.loads(body) if body else None

    def stream(self):
        try:
            connection = http.client.HTTPConnection(self.url.hostname, self.url.port, timeout=12)
            connection.request("GET", self.url.path, headers={"Accept": "text/event-stream", "Mcp-Session-Id": self.session})
            response = connection.getresponse()
            assert response.status == 200, response.status
            while line := response.readline():
                if not line.startswith(b"data:"):
                    continue
                frame = json.loads(line[5:].strip())
                if frame.get("method") == "sampling/createMessage":
                    assert frame["params"] == {"maxTokens": 1, "messages": []}
                    self.post({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": -32055, "message": "sampling denied", "data": {"scope": "test"}}})
                elif frame.get("method") == "notifications/tools/list_changed":
                    self.notified.set()
                else:
                    raise AssertionError(frame)
            connection.close()
        except Exception as error:
            self.errors.put(error)
            self.notified.set()

    def exercise(self):
        initialized = self.post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {"sampling": {}}, "clientInfo": {"name": "probe", "version": "1"}}})
        assert initialized["result"]["serverInfo"]["name"] == "native-provider"
        assert self.session, "MCP session header is required for reverse common SSE streams"
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})
        threading.Thread(target=self.stream, daemon=True).start()
        tools = self.post({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        assert tools["result"]["tools"][0]["name"] == "echo"
        result = self.post({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "echo", "arguments": {"value": "native-roundtrip"}}})
        assert result == {"jsonrpc": "2.0", "id": 3, "result": {"content": [{"type": "text", "text": "native-roundtrip"}]}}
        assert self.notified.wait(8), "reverse notification did not reach the HTTP MCP client"
        if not self.errors.empty():
            raise self.errors.get()


def backend():
    threads = {}
    index = 0
    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method, params = request["method"], request.get("params", {})
        result = {}
        if method == "model/list":
            result = {"data": [{"model": "test", "displayName": "Test", "supportedReasoningEfforts": []}], "nextCursor": None}
        elif method == "thread/start":
            url = params["config"]["mcp_servers"]["native"]["url"]
            http = HttpMcp(url)
            http.exercise()
            if params.get("model") == "fail":
                emit({"id": request["id"], "error": {"code": -32042, "message": "intentional setup failure", "data": {"url": url}}})
                continue
            index += 1
            thread = {"id": f"thread-{index}", "cwd": params["cwd"], "status": {"type": "idle"}, "parentThreadId": None, "turns": [], "bridgeUrl": url}
            threads[thread["id"]] = thread
            result = {"thread": thread, "model": "test", "modelProvider": "test", "cwd": params["cwd"], "sandbox": {"type": "readOnly", "networkAccess": False}, "approvalPolicy": "on-request", "reasoningEffort": None}
        elif method == "thread/read":
            result = {"thread": threads[params["threadId"]]}
        elif method == "thread/loaded/list":
            result = {"data": list(threads), "nextCursor": None}
        elif method == "thread/unsubscribe":
            result = {"status": "unsubscribed"}
        elif method in ("initialize", "thread/backgroundTerminals/clean"):
            pass
        else:
            raise AssertionError((method, params))
        emit({"id": request["id"], "result": result})


def probe(binary):
    process = subprocess.Popen([binary, "--codex-path", sys.executable,
        "--codex-arg", os.path.abspath(__file__), "--codex-arg", "backend",
        "--request-timeout-seconds", "8", "--interaction-timeout-seconds", "8"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    received = queue.Queue()
    def read():
        for line in process.stdout:
            received.put(json.loads(line))
        received.put(None)
    threading.Thread(target=read, daemon=True).start()
    next_id = 0
    connections = []
    disconnected = []
    reverse = {}
    def send(frame):
        process.stdin.write(json.dumps({"jsonrpc": "2.0", **frame}) + "\n")
        process.stdin.flush()
    def frame():
        message = received.get(timeout=12)
        assert message is not None, "adapter exited early"
        return message
    def handle(message):
        method, params = message.get("method"), message.get("params", {})
        if method == "mcp/connect":
            assert params["serverId"] == "client-native"
            connection_id = f"native-{len(connections)}"
            connections.append(connection_id)
            send({"id": message["id"], "result": {"connectionId": connection_id}})
        elif method == "mcp/disconnect":
            disconnected.append(params["connectionId"])
            send({"id": message["id"], "result": {}})
        elif method == "mcp/message":
            assert params["connectionId"] in connections
            if "id" not in message:
                assert params["method"] == "notifications/initialized"
            elif params["method"] == "initialize":
                send({"id": message["id"], "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {"listChanged": True}}, "serverInfo": {"name": "native-provider", "version": "1"}}})
            elif params["method"] == "tools/list":
                send({"id": message["id"], "result": {"tools": [{"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}]}})
            elif params["method"] == "tools/call":
                assert params["params"] == {"name": "echo", "arguments": {"value": "native-roundtrip"}}
                key = f"reverse-{params['connectionId']}"
                reverse[key] = message
                send({"id": key, "method": "mcp/message", "params": {"connectionId": params["connectionId"], "method": "sampling/createMessage", "params": {"maxTokens": 1, "messages": []}}})
            else:
                raise AssertionError(message)
        elif message.get("id") in reverse:
            assert message["error"] == {"code": -32055, "message": "sampling denied", "data": {"scope": "test"}}
            original = reverse.pop(message["id"])
            send({"method": "mcp/message", "params": {"connectionId": original["params"]["connectionId"], "method": "notifications/tools/list_changed"}})
            send({"id": original["id"], "result": {"content": [{"type": "text", "text": "native-roundtrip"}]}})
        elif method not in ("session/update", "_codex/event"):
            raise AssertionError(message)
    def rpc(method, params):
        nonlocal next_id
        next_id += 1
        send({"id": next_id, "method": method, "params": params})
        while True:
            message = frame()
            if message.get("id") == next_id and "method" not in message:
                return message
            handle(message)
    def await_disconnected(count):
        while len(disconnected) < count:
            handle(frame())
    def assert_closed(url):
        parsed = urllib.parse.urlparse(url)
        connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=2)
        try:
            connection.request("GET", parsed.path)
            response = connection.getresponse()
            assert response.status == 410, "closed endpoint remained usable"
        except (ConnectionRefusedError, ConnectionResetError, http.client.RemoteDisconnected):
            pass
        finally:
            connection.close()
    try:
        initialized = rpc("initialize", {"protocolVersion": 2, "info": {"name": "native-probe", "version": "1"}, "capabilities": {"_meta": {"codex": {"version": 1}}}})
        assert "acp" in initialized["result"]["capabilities"]["session"]["mcp"]
        declaration = {"type": "acp", "name": "native", "serverId": "client-native"}
        failed = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": [declaration], "_meta": {"codex": {"thread": {"model": "fail"}}}})
        assert failed["error"]["code"] == -32042
        await_disconnected(1)
        assert_closed(failed["error"]["data"]["url"])
        created = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": [declaration]})
        session = created["result"]["sessionId"]
        inspected = rpc("_codex/request", {"version": 1, "sessionId": session, "method": "thread/read", "params": {"threadId": session}})
        url = inspected["result"]["thread"]["bridgeUrl"]
        parsed = urllib.parse.urlparse(url)
        unauthorized = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=2)
        unauthorized.request("GET", "/not-the-secret")
        assert unauthorized.getresponse().status == 404
        unauthorized.close()
        closed = rpc("session/close", {"sessionId": session})
        assert "result" in closed, closed
        await_disconnected(2)
        assert_closed(url)
        assert len(set(connections)) == 2 and sorted(connections) == sorted(disconnected)
        process.stdin.close()
        assert process.wait(timeout=8) == 0, process.stderr.read()
        print("native MCP full duplex, consent error fidelity, failed setup/reconnect and close verified")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


if __name__ == "__main__":
    if sys.argv[1] == "backend":
        backend()
    else:
        probe(sys.argv[1])
