"""Deterministic JSONL app-server peer; never makes network or model requests."""

import json
import sys


def send(frame):
    print(json.dumps(frame), flush=True)


def notify(method, params):
    send({"method": method, "params": params})


def reply(request, result):
    send({"id": request["id"], "result": result})


scenario = sys.argv[1]
initialized = False
cancelled_request = None
thread = None
settings = None
active_turn = None
turn_count = 0
history = []
last_turn_params = None
pending_callback = None


def complete_turn(text="done", status="completed"):
    global active_turn
    item = {"type": "agentMessage", "id": f"answer-{active_turn}", "text": text, "phase": "final"}
    if status == "completed":
        notify("item/agentMessage/delta", {"threadId": thread["id"], "turnId": active_turn, "itemId": item["id"], "delta": text})
        notify("item/completed", {"threadId": thread["id"], "turnId": active_turn, "item": item})
        history.append({"turnId": active_turn, "item": item})
    notify("turn/completed", {"threadId": thread["id"], "turn": {"id": active_turn, "status": status, "items": [], "error": None}})
    active_turn = None
    thread["status"] = {"type": "idle"}

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        assert request["params"]["capabilities"]["experimentalApi"] is True
        reply(request, {"userAgent": "fake-codex", "platformOs": "linux"})
    elif method == "initialized":
        initialized = True
    elif not initialized:
        raise RuntimeError("request arrived before initialized")
    elif scenario == "transport":
        if method == "exercise":
            notify("thread/started", {"thread": {"id": "thread-1"}})
            send({"id": "approval-1", "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread-1"}})
            reply(request, {"accepted": True})
        elif method == "cancel-me":
            cancelled_request = request
            notify("waiting", {})
        elif method == "after-cancel":
            reply(cancelled_request, {"late": True})
            reply(request, {"stillResponsive": True})
        elif method == "rpc-error":
            send({"id": request["id"], "error": {"code": -32001, "message": "policy", "data": {"scope": "turn"}}})
        elif method is None and request.get("id") == "approval-1":
            notify("approval-received", request["result"])
        else:
            raise RuntimeError(f"unexpected request: {method}")
    elif scenario == "overflow":
        for number in range(8):
            notify("item/delta", {"number": number})
    elif scenario == "oversize":
        sys.stdout.write("x" * 4096)
        sys.stdout.flush()
    elif scenario == "server":
        params = request.get("params", {})
        if method == "model/list":
            reply(request, {"data": [{"id": "model-a", "model": "model-a", "displayName": "Model A", "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": "balanced"}, {"reasoningEffort": "high", "description": "thorough"}]}], "nextCursor": None})
        elif method == "thread/start":
            thread = {"id": "session-1", "cwd": params["cwd"], "status": {"type": "idle"}, "turns": [], "createdAt": 1, "updatedAt": 2, "name": "Fixture", "preview": "", "creationParams": params}
            settings = {"model": "model-a", "reasoningEffort": "medium", "effort": "medium", "approvalPolicy": "on-request", "sandbox": {"type": "readOnly"}, "sandboxPolicy": {"type": "readOnly"}, "serviceTier": None, "cwd": params["cwd"]}
            notify("thread/started", {"thread": thread})
            reply(request, {"thread": thread, **settings})
        elif method == "thread/read":
            reply(request, {"thread": thread, "lastTurnParams": last_turn_params})
        elif method == "thread/list":
            reply(request, {"data": [thread] if thread else [], "nextCursor": None})
        elif method == "thread/resume":
            reply(request, {"thread": thread, **settings})
        elif method == "thread/items/list":
            reply(request, {"data": history, "nextCursor": None})
        elif method == "thread/settings/update":
            settings.update({key: value for key, value in params.items() if key != "threadId"})
            notify("thread/settings/updated", {"threadId": thread["id"], "threadSettings": settings})
            reply(request, {})
        elif method == "turn/settings/update":
            reply(request, {"status": "applied", "received": params})
        elif method == "turn/start":
            turn_count += 1
            active_turn = f"turn-{turn_count}"
            last_turn_params = params
            text = params["input"][0]["text"]
            thread["status"] = {"type": "active"}
            turn = {"id": active_turn, "status": "inProgress", "items": [], "error": None}
            notify("turn/started", {"threadId": thread["id"], "turn": turn})
            user_item = {"type": "userMessage", "id": f"user-{active_turn}", "content": params["input"]}
            history.append({"turnId": active_turn, "item": user_item})
            notify("item/completed", {"threadId": thread["id"], "turnId": active_turn, "item": user_item})
            if text == "fast":
                complete_turn("fast answer")
                reply(request, {"turn": turn})
            else:
                reply(request, {"turn": turn})
                if text in ("approval", "cancel approval"):
                    pending_callback = f"approval-{active_turn}"
                    item = {"type": "commandExecution", "id": f"tool-{active_turn}", "command": "echo fixture", "cwd": thread["cwd"], "status": "inProgress", "commandActions": [], "aggregatedOutput": None, "exitCode": None, "durationMs": None}
                    notify("item/started", {"threadId": thread["id"], "turnId": active_turn, "item": item})
                    send({"id": pending_callback, "method": "item/commandExecution/requestApproval", "params": {"threadId": thread["id"], "turnId": active_turn, "itemId": item["id"], "command": "echo fixture", "cwd": thread["cwd"], "availableDecisions": ["accept", "decline", "cancel"]}})
                elif text == "dynamic":
                    pending_callback = 700
                    send({"id": pending_callback, "method": "item/tool/call", "params": {"threadId": thread["id"], "turnId": active_turn, "callId": "dynamic-1", "tool": "client_lookup", "arguments": {"query": "value"}}})
                elif text != "long":
                    complete_turn(text)
        elif method == "turn/steer":
            assert params["expectedTurnId"] == active_turn
            reply(request, {"turnId": active_turn})
        elif method == "turn/interrupt":
            assert params["turnId"] == active_turn
            reply(request, {})
            complete_turn(status="interrupted")
        elif method is None and request.get("id") == pending_callback:
            if "error" in request:
                result = {"error": request["error"]}
            else:
                result = request["result"]
            notify("fixture/callback", {"threadId": thread["id"], "response": result})
            pending_callback = None
            if active_turn is not None and result != {"decision": "cancel"}:
                complete_turn(json.dumps(result, sort_keys=True))
        elif method in ("thread/backgroundTerminals/clean", "thread/unsubscribe", "thread/archive"):
            reply(request, {})
        elif method == "thread/goal/set":
            reply(request, {"goal": params, "opaque": {"preserved": True}})
        else:
            send({"id": request["id"], "error": {"code": -32601, "message": f"fixture does not implement {method}"}})
    else:
        raise RuntimeError(f"unknown scenario: {scenario}")
