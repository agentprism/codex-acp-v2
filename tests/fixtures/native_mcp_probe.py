"""Local native-MCP/HTTP protocol probe; no external service or model inference."""
import http.client
import json
import os
import queue
import subprocess
import sys
import threading
import urllib.parse

# Protocol streams are UTF-8 even when Windows uses a legacy console code page.
sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")

output_lock = threading.Lock()


def emit(message):
    with output_lock:
        print(json.dumps(message), flush=True)


class HttpMcp:
    def __init__(self, url):
        self.url = urllib.parse.urlparse(url)
        assert self.url.hostname == "127.0.0.1"
        assert len(self.url.path) == 33, "unguessable UUID token must protect the endpoint"
        self.session = None
        self.notified = threading.Event()
        self.errors = queue.Queue()
        self.thread_id = None
        self.held_requests = set()

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
                    if frame["params"].get("hold"):
                        self.held_requests.add(frame["id"])
                        emit({"method": "fixture/reverseHeld", "params": {"threadId": self.thread_id, "requestId": frame["id"]}})
                        continue
                    assert frame["params"] == {"maxTokens": 1, "messages": []}
                    self.post({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": -32055, "message": "sampling denied", "data": {"scope": "test"}}})
                elif frame.get("method") == "notifications/tools/list_changed":
                    self.notified.set()
                elif frame.get("method") == "notifications/cancelled":
                    assert frame["params"]["requestId"] in self.held_requests
                    self.held_requests.remove(frame["params"]["requestId"])
                    emit({"method": "fixture/reverseCancelled", "params": {"threadId": self.thread_id, "requestId": frame["params"]["requestId"]}})
                else:
                    raise AssertionError(frame)
            connection.close()
        except Exception as error:
            self.errors.put(error)
            self.notified.set()

    def disconnect(self):
        connection = http.client.HTTPConnection(self.url.hostname, self.url.port, timeout=8)
        connection.request("DELETE", self.url.path, headers={"Mcp-Session-Id": self.session})
        response = connection.getresponse()
        assert response.status == 204, response.status
        response.read()
        connection.close()

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
            if params.get("model") == "fail":
                rejected = HttpMcp(url)
                response = rejected.post({"jsonrpc": "2.0", "id": "rejected", "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "reject-initialization", "version": "1"}}})
                assert response == {"jsonrpc": "2.0", "id": "rejected", "error": {"code": -32054, "message": "provider initialization denied", "data": {"reason": "probe"}}}
                assert rejected.session is None, "failed initialize must not retain an HTTP session"
            http = HttpMcp(url)
            http.exercise()
            if params.get("model") == "fail":
                emit({"id": request["id"], "error": {"code": -32042, "message": "intentional setup failure", "data": {"url": url}}})
                continue
            index += 1
            http.thread_id = f"thread-{index}"
            child = HttpMcp(url)
            child.exercise()
            assert child.session != http.session
            child.disconnect()
            still_live = http.post({"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {}})
            assert still_live["result"]["tools"][0]["name"] == "echo", "deleting child session disrupted parent"
            reconnect = HttpMcp(url)
            reconnect.thread_id = http.thread_id
            reconnect.exercise()
            assert reconnect.session not in (http.session, child.session)
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
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8")
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
    held = False
    pending_cancelled = False
    initializing = False
    pending_initialize_id = None
    initialize_cancelled = False
    explicit_cancelled = False
    cancelled_http_request = None
    held_http_request = None
    def send(frame):
        process.stdin.write(json.dumps({"jsonrpc": "2.0", **frame}) + "\n")
        process.stdin.flush()
    def frame():
        message = received.get(timeout=12)
        assert message is not None, "adapter exited early"
        return message
    def handle(message):
        nonlocal held, pending_cancelled, initializing, pending_initialize_id, initialize_cancelled, explicit_cancelled, cancelled_http_request, held_http_request
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
                name = params.get("params", {}).get("clientInfo", {}).get("name")
                if name == "hold-initialization":
                    initializing = True
                    pending_initialize_id = message["id"]
                elif name == "reject-initialization":
                    send({"id": message["id"], "error": {"code": -32054, "message": "provider initialization denied", "data": {"reason": "probe"}}})
                else:
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
        elif message.get("id") == "pending-on-close":
            assert message["error"]["code"] == -32800, message
            pending_cancelled = True
        elif message.get("id") == "explicit-cancel":
            assert message["error"]["code"] == -32800, message
            explicit_cancelled = True
        elif method == "$/cancel_request":
            assert params["requestId"] == pending_initialize_id, message
            initialize_cancelled = True
        elif method == "_codex/event" and params["method"] == "fixture/reverseHeld":
            held = True
            held_http_request = params["params"]["requestId"]
        elif method == "_codex/event" and params["method"] == "fixture/reverseCancelled":
            cancelled_http_request = params["params"]["requestId"]
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
        # Windows retries refused loopback connections for about two seconds.
        # Wait for the actual refusal; a timeout must not count as clean shutdown.
        connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=8)
        try:
            connection.request("GET", parsed.path)
            response = connection.getresponse()
            assert response.status == 410, "closed endpoint remained usable"
        except (ConnectionRefusedError, ConnectionResetError, http.client.RemoteDisconnected):
            pass
        finally:
            connection.close()
    try:
        initialized = rpc("initialize", {"protocolVersion": 2, "info": {"name": "native-probe", "version": "1"}, "capabilities": {"_meta": {"codex": {"version": 1, "events": ["fixture/reverseHeld", "fixture/reverseCancelled"]}}}})
        assert "acp" in initialized["result"]["capabilities"]["session"]["mcp"]
        declaration = {"type": "acp", "name": "native", "serverId": "client-native"}
        failed = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": [declaration], "_meta": {"codex": {"thread": {"model": "fail"}}}})
        assert failed["error"]["code"] == -32042
        await_disconnected(2)
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
        active_connection = connections[-1]
        # Closed sessions must return capacity, not exhaust the listener after
        # enough provider failures. These connections intentionally have no SSE
        # consumer; one oversized notification forces explicit retirement.
        for _ in range(33):
            def initialize_for_overload():
                try:
                    http = HttpMcp(url)
                    result = http.post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "overload", "version": "1"}}})
                    received.put({"fixture_initialize": result})
                except Exception as error:
                    received.put({"fixture_initialize": str(error)})
            worker = threading.Thread(target=initialize_for_overload, daemon=True)
            worker.start()
            while True:
                message = frame()
                if "fixture_initialize" in message:
                    assert "result" in message["fixture_initialize"], message
                    break
                handle(message)
            worker.join(timeout=1)
            previous = len(disconnected)
            send({"method": "mcp/message", "params": {"connectionId": connections[-1], "method": "notifications/message", "params": {"data": "x" * (1024 * 1024)}}})
            await_disconnected(previous + 1)
        send({"id": "explicit-cancel", "method": "mcp/message", "params": {"connectionId": active_connection, "method": "sampling/createMessage", "params": {"hold": True}}})
        while not held:
            handle(frame())
        send({"method": "$/cancel_request", "params": {"requestId": "explicit-cancel"}})
        while not explicit_cancelled or cancelled_http_request is None:
            handle(frame())
        assert cancelled_http_request == held_http_request, "cancellation changed the reverse MCP request identity"
        held = False
        send({"id": "pending-on-close", "method": "mcp/message", "params": {"connectionId": active_connection, "method": "sampling/createMessage", "params": {"hold": True}}})
        while not held:
            handle(frame())
        setup_closed = queue.Queue()
        def unfinished_setup():
            connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=8)
            try:
                connection.request("POST", parsed.path, json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "hold-initialization", "version": "1"}}}), {"Content-Type": "application/json"})
                response = connection.getresponse()
                response.read()
                setup_closed.put(response.status)
            except (ConnectionResetError, http.client.RemoteDisconnected):
                setup_closed.put(410)
            finally:
                connection.close()
        setup_thread = threading.Thread(target=unfinished_setup, daemon=True)
        setup_thread.start()
        while not initializing:
            handle(frame())
        closed = rpc("session/close", {"sessionId": session})
        assert "result" in closed, closed
        await_disconnected(39)
        assert setup_closed.get(timeout=8) == 410, "session close must cancel unfinished MCP initialization"
        setup_thread.join(timeout=1)
        while not pending_cancelled:
            handle(frame())
        while not initialize_cancelled:
            handle(frame())
        assert_closed(url)
        missing = rpc("mcp/message", {"connectionId": connections[-1], "method": "tools/list", "params": {}})
        assert missing["error"]["code"] == -32602, missing
        assert len(set(connections)) == 39 and sorted(connections) == sorted(disconnected)
        reopened = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": [declaration]})
        reopened_id = reopened["result"]["sessionId"]
        live = rpc("_codex/request", {"version": 1, "sessionId": reopened_id, "method": "thread/read", "params": {"threadId": reopened_id}})
        live_url = live["result"]["thread"]["bridgeUrl"]
        process.stdin.close()
        assert process.wait(timeout=8) == 0, process.stderr.read()
        assert_closed(live_url)
        print("native MCP full duplex, independent child/reconnect sessions, reverse-call cancellation and cleanup verified")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


if __name__ == "__main__":
    if sys.argv[1] == "backend":
        backend()
    else:
        probe(sys.argv[1])
