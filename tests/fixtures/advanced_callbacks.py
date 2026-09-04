"""Drive the real adapter with exact callback shapes from Codex's v2 DTOs."""

import json
import os
import queue
import subprocess
import sys
import threading

# Protocol streams are UTF-8 even when Windows uses a legacy console code page.
sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")


def callbacks(cwd):
    return {
        "permission": ("item/permissions/requestApproval", {
            "threadId": "callback-root", "turnId": "turn-1", "itemId": "permissions-1",
            "environmentId": None, "startedAtMs": 1234, "cwd": cwd,
            "reason": "Allow a restricted subset", "permissions": {
                "network": {"enabled": True},
                "fileSystem": {"read": [cwd], "write": [cwd]},
            },
        }),
        "form": ("mcpServer/elicitation/request", {
            "threadId": "callback-root", "turnId": None, "serverName": "consent",
            "mode": "openaiForm", "message": "Choose explicit consent",
            "requestedSchema": {"type": "object", "properties": {"choice": {"type": "object", "properties": {"persist": {"type": "boolean"}}}}},
            "_meta": {"openai/custom": {"consentChoices": ["once", "always"], "state": "opaque"}},
        }),
        "tool": ("item/tool/call", {
            "threadId": "callback-root", "turnId": "turn-1", "callId": "dynamic-1",
            "tool": "client_tool", "arguments": {"nonce": [1, None, {"nested": True}]},
        }),
        "auth": ("account/chatgptAuthTokens/refresh", {
            "reason": "unauthorized", "previousAccountId": "fixture-account",
        }),
        "attestation": ("attestation/generate", {}),
    }


def emit(frame):
    print(json.dumps(frame), flush=True)


def backend():
    thread = None
    pending = None
    observed = {}
    expected_callbacks = 2 if sys.argv[2] == "host-only" else 5
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        params = request.get("params", {})
        if method is None:
            assert request["id"] in callbacks(thread["cwd"])
            observed[request["id"]] = {key: value for key, value in request.items() if key in ("result", "error")}
            if len(observed) == expected_callbacks:
                emit({"id": pending["id"], "result": {"thread": thread, "observed": observed}})
                pending = None
            continue
        if method == "initialized":
            continue
        if method == "initialize":
            assert params["capabilities"]["experimentalApi"] is True
            result = {"userAgent": "callback-peer"}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "thread/start":
            thread = {"id": "callback-root", "cwd": params["cwd"], "status": {"type": "idle"}, "parentThreadId": None, "turns": []}
            result = {"thread": thread, "model": "fixture", "sandbox": {"type": "readOnly"}, "approvalPolicy": "on-request"}
        elif method == "thread/read" and params.get("includeTurns"):
            assert pending is None and not observed
            pending = request
            for callback_id, (callback, payload) in callbacks(thread["cwd"]).items():
                if expected_callbacks == 2 and callback_id not in ("auth", "attestation"):
                    continue
                emit({"id": callback_id, "method": callback, "params": payload})
            continue
        elif method == "thread/read":
            result = {"thread": thread}
        elif method in ("thread/list", "thread/loaded/list"):
            result = {"data": [], "nextCursor": None}
        elif method == "account/read":
            emit({"id": request["id"], "error": {"code": -32061, "message": "fixture account unavailable", "data": {"retryable": False, "nested": {"scope": "account"}}}})
            continue
        elif method in ("thread/backgroundTerminals/clean", "thread/unsubscribe"):
            result = {}
        else:
            raise AssertionError((method, params))
        emit({"id": request["id"], "result": result})


def exercise(binary, host, negotiated):
    args = [binary, "--codex-path", sys.executable, "--codex-arg", os.path.abspath(__file__),
            "--codex-arg", "backend", "--codex-arg", "all" if negotiated else "host-only",
            "--request-timeout-seconds", "8", "--interaction-timeout-seconds", "8"]
    if host:
        args.append("--allow-host-methods")
    if host and negotiated:
        args += ["--backend-capabilities", '{"requestAttestation":true}']
    process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8")
    received = queue.Queue()
    def reader():
        for line in process.stdout:
            received.put(json.loads(line))
        received.put(None)
    threading.Thread(target=reader, daemon=True).start()
    next_id = 0
    answers = {
        "permission": {"result": {"permissions": {"fileSystem": {"read": [os.getcwd()], "write": None}}, "scope": "turn", "strictAutoReview": True}},
        "form": {"result": {"action": "accept", "content": {"choice": {"persist": False}}, "_meta": {"selectedAction": "once", "opaque": [None, "unchanged"]}}},
        "tool": {"error": {"code": -32055, "message": "client tool declined", "data": {"scope": "turn", "retryable": False}}},
        "auth": {"result": {"accessToken": "fake-test-token-not-a-credential", "chatgptAccountId": "fixture-account", "chatgptPlanType": None}},
        "attestation": {"result": {"token": "fake-test-attestation-not-a-credential"}},
    }
    delivered = set()
    def send(frame):
        process.stdin.write(json.dumps({"jsonrpc": "2.0", **frame}) + "\n")
        process.stdin.flush()
    def rpc(method, params):
        nonlocal next_id
        next_id += 1
        send({"id": next_id, "method": method, "params": params})
        while True:
            frame = received.get(timeout=12)
            assert frame is not None, "adapter exited early"
            if frame.get("id") == next_id and "method" not in frame:
                return frame
            if frame.get("method") == "session/update":
                continue
            assert frame.get("method") == "_codex/serverRequest", frame
            envelope = frame["params"]
            callback_id = envelope["requestId"]
            backend_method, expected_params = callbacks(os.getcwd())[callback_id]
            assert envelope == {"version": 1, "sessionId": "callback-root" if callback_id in ("permission", "form", "tool") else None,
                                "requestId": callback_id, "method": backend_method, "params": expected_params}, envelope
            assert negotiated and (host or callback_id not in ("auth", "attestation")), frame
            assert callback_id not in delivered
            delivered.add(callback_id)
            send({"id": frame["id"], **answers[callback_id]})
    def initialize(metadata, top=None):
        params = {"protocolVersion": 2, "info": {"name": "callback-client", "version": "1"},
                  "capabilities": {"elicitation": {"form": {}, "url": {}}, "_meta": {"codex": metadata}}}
        if top is not None:
            params["_meta"] = {"codex": top}
        return rpc("initialize", params)
    try:
        if negotiated:
            conflict = initialize({"version": 1}, {"version": 1, "serverRequests": True})
            assert "conflicting" in str(conflict["error"]), conflict
            invalid = initialize({"version": 1, "rawServerRequests": ["*"]})
            assert "requires serverRequests" in str(invalid["error"]), invalid
            if host:
                missing = initialize({"version": 1})
                assert "require codex serverRequests" in str(missing["error"]), missing
            metadata = {"version": 1, "serverRequests": True, "rawServerRequests": ["item/permissions/requestApproval", "mcpServer/elicitation/request"]}
        else:
            metadata = {"version": 1, "serverRequests": False}
        initialized = initialize(metadata)
        assert initialized["result"]["capabilities"]["_meta"]["codex"]["hostMethods"] == host, initialized
        created = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": []})
        assert created["result"]["sessionId"] == "callback-root", created
        if not negotiated:
            login = rpc("_codex/request", {"version": 1, "method": "account/login/start", "params": {"type": "chatgptAuthTokens", "accessToken": "fake", "chatgptAccountId": "fixture"}})
            assert "requires serverRequests" in str(login["error"]), login
        observed = rpc("_codex/request", {"version": 1, "sessionId": "callback-root", "method": "thread/read", "params": {"threadId": "callback-root", "includeTurns": True}})["result"]["observed"]
        if negotiated:
            expected = dict(answers)
            if not host:
                for callback_id in ("auth", "attestation"):
                    expected[callback_id] = {"error": {"code": -32000, "message": "backend callback targets an unowned session"}}
            assert observed == expected, observed
            assert delivered == ({"permission", "form", "tool", "auth", "attestation"} if host else {"permission", "form", "tool"}), delivered
        else:
            assert observed == {callback_id: {"error": {"code": -32000, "message": "client does not support this backend interaction; negotiate codex serverRequests"}} for callback_id in ("auth", "attestation")}, observed
            assert not delivered, delivered
        account = rpc("_codex/request", {"version": 1, "method": "account/read", "params": {"refreshToken": False}})
        if host:
            assert account["error"] == {"code": -32061, "message": "fixture account unavailable", "data": {"retryable": False, "nested": {"scope": "account"}}}, account
        else:
            assert "require --allow-host-methods" in str(account["error"]), account
        assert "result" in rpc("session/close", {"sessionId": "callback-root"})
        process.stdin.close()
        assert process.wait(timeout=8) == 0, process.stderr.read()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)


if __name__ == "__main__":
    if sys.argv[1] == "backend":
        backend()
    else:
        exercise(sys.argv[1], host=False, negotiated=True)
        exercise(sys.argv[1], host=True, negotiated=True)
        exercise(sys.argv[1], host=True, negotiated=False)
        print("advanced callbacks: exact consent, capability negotiation, host gates, and bidirectional errors verified")
