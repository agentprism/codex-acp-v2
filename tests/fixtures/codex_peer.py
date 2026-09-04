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
child_history = []
last_turn_params = None
pending_callback = None
subscribed = False
child_active = False
child_turn = "child-turn"
pending_settings = False
completed_turn = None
streams = {}
stream_stops = []
parallel_threads = {}
pending_replay = None


def complete_turn(text="done", status="completed"):
    global active_turn, completed_turn
    item = {"type": "agentMessage", "id": f"answer-{active_turn}", "text": text, "phase": "final_answer"}
    if status == "completed":
        notify("item/agentMessage/delta", {"threadId": thread["id"], "turnId": active_turn, "itemId": item["id"], "delta": text})
        notify("item/completed", {"threadId": thread["id"], "turnId": active_turn, "item": item})
        history.append({"turnId": active_turn, "item": item})
    notify("turn/completed", {"threadId": thread["id"], "turn": {"id": active_turn, "status": status, "items": [], "error": None}})
    completed_turn = {"id": active_turn, "status": status, "items": []}
    active_turn = None
    thread["status"] = {"type": "idle"}


def session_response():
    # The real lifecycle response is not a full ThreadSettings snapshot.
    response = {key:value for key,value in settings.items() if key not in ("collaborationMode", "summary", "personality", "sandboxPolicy", "effort")}
    response["reasoningEffort"] = settings["effort"]
    return {"thread":thread, **response}

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
    elif scenario == "parallel":
        params = request.get("params", {})
        if method == "model/list":
            if pending_replay:
                root = pending_replay["params"]["threadId"]
                terminal = {"type":"commandExecution","id":"replay-tool","command":"echo","cwd":parallel_threads[root]["cwd"],"status":"completed","aggregatedOutput":"before\n","exitCode":None,"durationMs":None,"processId":"background","commandActions":[]}
                notify("item/commandExecution/outputDelta",{"threadId":root,"turnId":"saved-turn","itemId":"replay-tool","delta":"before\n"})
                notify("item/commandExecution/terminalInteraction",{"threadId":root,"turnId":"saved-turn","itemId":"replay-tool","processId":"background","stdin":"input not represented by history"})
                reply(pending_replay, {"data":[{"turnId":"saved-turn","item":{"type":"agentMessage","id":"saved-message","text":"retained history"}},{"turnId":"saved-turn","item":terminal}],"nextCursor":None})
                notify("item/commandExecution/outputDelta",{"threadId":root,"turnId":"saved-turn","itemId":"replay-tool","delta":"after\n"})
                pending_replay = None
            reply(request, {"data":[],"nextCursor":None})
        elif method == "thread/start":
            thread_id = f"parallel-{len(parallel_threads)+1}"
            parallel_threads[thread_id] = {"id":thread_id,"cwd":params["cwd"],"status":{"type":"idle"}}
            reply(request,{"thread":parallel_threads[thread_id],"model":"model-a","sandbox":{"type":"readOnly"}})
        elif method == "thread/read":
            reply(request,{"thread":parallel_threads[params["threadId"]]})
        elif method == "thread/resume":
            notify("thread/name/updated",{"threadId":params["threadId"],"threadName":"During replay"})
            reply(request,{"thread":parallel_threads[params["threadId"]],"model":"model-a","sandbox":{"type":"readOnly"}})
        elif method == "thread/items/list":
            pending_replay = request
            notify("fixture/replayBlocked",{"threadId":"parallel-2"})
        elif method == "turn/start":
            thread_id = params["threadId"]
            turn = {"id":"other-turn","status":"inProgress","items":[]}
            notify("turn/started",{"threadId":thread_id,"turn":turn})
            reply(request,{"turn":turn})
            notify("item/completed",{"threadId":thread_id,"turnId":"other-turn","item":{"type":"agentMessage","id":"other-answer","text":"other session remained responsive"}})
            notify("turn/completed",{"threadId":thread_id,"turn":{"id":"other-turn","status":"completed","items":[]}})
        elif method in ("thread/list","thread/loaded/list","thread/turns/list"):
            reply(request,{"data":[],"nextCursor":None})
        elif method in ("thread/unsubscribe","thread/backgroundTerminals/clean"):
            reply(request,{})
        else:
            send({"id":request["id"],"error":{"code":-32601,"message":f"parallel fixture does not implement {method}"}})
    elif scenario == "server":
        params = request.get("params", {})
        if method == "model/list":
            if pending_settings:
                notify("thread/settings/updated", {"threadId": thread["id"], "threadSettings": settings})
                pending_settings = False
            reply(request, {"data": [{"id": "model-a", "model": "model-a", "displayName": "Model A", "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": "balanced"}, {"reasoningEffort": "high", "description": "thorough"}]}], "nextCursor": None})
        elif method == "thread/start":
            subscribed = True
            thread = {"id": "session-1", "cwd": params["cwd"], "status": {"type": "idle"}, "turns": [], "createdAt": 1, "updatedAt": 2, "name": "Fixture", "preview": "", "creationParams": params}
            settings = {"model": "model-a", "reasoningEffort": "medium", "effort": "medium", "approvalPolicy": "on-request", "sandbox": {"type": "readOnly"}, "sandboxPolicy": {"type": "readOnly"}, "serviceTier": None, "cwd": params["cwd"]}
            if params.get("config", {}).get("audit_resume_settings"):
                settings["collaborationMode"] = {"mode":"plan", "settings":{"model":"model-a","reasoning_effort":"medium","developer_instructions":None}}
            notify("thread/started", {"thread": thread})
            reply(request, session_response())
        elif method == "thread/read":
            if params["threadId"] == "child":
                reply(request, {"thread": {"id": "child", "parentThreadId": thread["id"], "cwd": thread["cwd"], "status": {"type": "active" if child_active else "idle"}}})
            elif params["threadId"] == "unrelated":
                reply(request, {"thread": {"id": "unrelated", "parentThreadId": None, "cwd": thread["cwd"], "status": {"type": "idle"}}})
            else:
                reply(request, {"thread": thread, "lastTurnParams": last_turn_params, "streamStops": stream_stops, "activeStreams": list(streams), "observedSettings":settings})
        elif method == "thread/list":
            data = [thread] if thread else []
            if params.get("ancestorThreadId"):
                data = [{"id": "child", "parentThreadId": thread["id"]}] if child_history and not params.get("archived") else []
            reply(request, {"data": data, "nextCursor": None})
        elif method == "thread/loaded/list":
            reply(request, {"data": ([thread["id"]] if thread else []) + (["child"] if thread and thread.get("childSubscribed") else []), "nextCursor": None})
        elif method == "thread/turns/list":
            turns = [{"id": child_turn, "status": "inProgress" if child_active else "completed", "items": []}] if params["threadId"] == "child" else ([completed_turn] if completed_turn else [])
            reply(request, {"data": turns, "nextCursor": None})
        elif method == "thread/resume":
            assert not subscribed, "resume overrides require unsubscribing the existing thread first"
            subscribed = True
            thread["resumeParams"] = params
            if thread["creationParams"].get("config", {}).get("audit_resume_settings"):
                notify("thread/settings/updated", {"threadId":thread["id"], "threadSettings":settings})
                settings["model"] = "resumed-model"
            reply(request, session_response())
        elif method == "thread/items/list":
            reply(request, {"data": child_history if params["threadId"] == "child" else history, "nextCursor": None})
        elif method in ("thread/rollback", "thread/revert"):
            removed = history[-1]
            turns = list(dict.fromkeys(entry["turnId"] for entry in history))
            retained = turns[:-params["numTurns"]] if method == "thread/rollback" else turns[:turns.index(params["beforeTurnId"])]
            history = [entry for entry in history if entry["turnId"] in retained]
            completed_turn = {"id": retained[-1], "status": "completed", "items": []} if retained else None
            notify("item/completed", {"threadId":thread["id"],"turnId":removed["turnId"],"item":removed["item"]})
            if method == "thread/revert":
                settings["collaborationMode"] = {"mode":"plan", "settings":{"model":settings["model"],"reasoning_effort":settings["effort"],"developer_instructions":None}}
                notify("thread/settings/updated", {"threadId":thread["id"],"threadSettings":settings})
            reply(request, {"thread": thread})
            if method == "thread/revert":
                notify("thread/reverted", {"threadId": thread["id"]})
        elif method == "mcpServer/event/stream/start":
            streams[params["subscriptionId"]] = params["threadId"]
            notify("mcpServer/event/stream", {"subscriptionId": params["subscriptionId"], "notification": {"method": "notifications/message", "params": {"data": "early MCP event"}}})
            reply(request, {})
        elif method == "mcpServer/event/stream/stop":
            assert set(params) == {"subscriptionId"}, "authorization-only threadId must not be forwarded"
            streams.pop(params["subscriptionId"])
            stream_stops.append(params["subscriptionId"])
            reply(request, {})
        elif method == "thread/settings/update":
            previous_settings = dict(settings)
            settings.update({key: value for key, value in params.items() if key != "threadId"})
            if params.get("effort") == "high" and thread["creationParams"].get("config", {}).get("audit_defer_settings"):
                pending_settings = True
                notify("thread/settings/updated", {"threadId": thread["id"], "threadSettings": {**previous_settings, "summary": "detailed"}})
                notify("fixture/settingsQueued", {"threadId": thread["id"]})
            else:
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
                elif text == "child":
                    child_active = True
                    thread["childSubscribed"] = True
                    pending_callback = 800
                    notify("turn/started", {"threadId": "child", "turn": {"id": "child-turn", "status": "inProgress", "items": []}})
                    notify("item/started", {"threadId":"child","turnId":"child-turn","item":{"type":"commandExecution","id":"child-tool","command":"echo child","cwd":thread["cwd"],"status":"inProgress","commandActions":[],"aggregatedOutput":None,"exitCode":None}})
                    send({"id": pending_callback, "method": "item/commandExecution/requestApproval", "params": {"threadId": "child", "turnId": "child-turn", "itemId": "child-tool", "command": "echo child", "cwd": thread["cwd"], "availableDecisions": ["accept", "decline", "cancel"]}})
                elif text not in ("long", "cancel race"):
                    complete_turn(text)
        elif method == "turn/steer":
            assert params["expectedTurnId"] == active_turn
            reply(request, {"turnId": active_turn})
        elif method == "turn/interrupt":
            if params["threadId"] == thread["id"] and last_turn_params["input"][0]["text"] == "cancel race":
                complete_turn("finished before interrupt")
                send({"id": request["id"], "error": {"code": -32600, "message": "no active turn to interrupt"}})
                continue
            if params["threadId"] == "child":
                assert params["turnId"] == child_turn
                child_active = False
                thread["childInterrupted"] = True
                reply(request, {})
                notify("turn/completed", {"threadId": "child", "turn": {"id": child_turn, "status": "interrupted", "items": []}})
                continue
            assert params["turnId"] == active_turn
            reply(request, {})
            complete_turn(status="interrupted")
        elif method is None and request.get("id") == pending_callback:
            if "error" in request:
                result = {"error": request["error"]}
            else:
                result = request["result"]
            if pending_callback == 800:
                child_active = False
                item = {"type":"commandExecution","id":"child-tool","command":"echo child","cwd":thread["cwd"],"status":"declined","commandActions":[],"aggregatedOutput":None,"exitCode":None}
                child_history.append({"turnId":"child-turn","item":item})
                notify("item/completed", {"threadId":"child","turnId":"child-turn","item":item})
                notify("fixture/childCallback", {"threadId": thread["id"], "requestId": request["id"], "response": result})
                notify("turn/completed", {"threadId": "child", "turn": {"id": "child-turn", "status": "completed", "items": []}})
                pending_callback = 801
                send({"id": pending_callback, "method": "item/commandExecution/requestApproval", "params": {"threadId": "unrelated", "turnId": "unrelated-turn", "itemId": "unrelated-tool", "command": "echo unrelated", "cwd": thread["cwd"], "availableDecisions": ["accept", "decline", "cancel"]}})
                continue
            if pending_callback == 801:
                notify("fixture/unrelatedDenied", {"threadId": thread["id"], "response": result})
                pending_callback = None
                continue
            notify("fixture/callback", {"threadId": thread["id"], "response": result})
            pending_callback = None
            if active_turn is not None and result != {"decision": "cancel"}:
                complete_turn(json.dumps(result, sort_keys=True))
        elif method in ("thread/backgroundTerminals/clean", "thread/unsubscribe", "thread/archive"):
            if method == "thread/unsubscribe":
                if params["threadId"] == "child":
                    thread["childSubscribed"] = False
                else:
                    subscribed = False
            reply(request, {})
        elif method == "thread/goal/set":
            reply(request, {"goal": params, "opaque": {"preserved": True}})
        elif method == "thread/goal/get":
            reply(request, {"goal": None})
            if active_turn is not None:
                complete_turn("child work finished")
                child_turn = "child-background-turn"
                child_active = True
                notify("turn/started", {"threadId": "child", "turn": {"id": child_turn, "status": "inProgress", "items": []}})
        else:
            send({"id": request["id"], "error": {"code": -32601, "message": f"fixture does not implement {method}"}})
    else:
        raise RuntimeError(f"unknown scenario: {scenario}")
