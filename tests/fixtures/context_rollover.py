"""Exercise contextCompaction notification semantics, not a model budget algorithm."""

import json
import os
import queue
import subprocess
import sys
import threading

# Protocol streams are UTF-8 even when Windows uses a legacy console code page.
sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8")


def emit(frame):
    print(json.dumps(frame), flush=True)


def backend():
    thread = None
    turns = 0
    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        params = request.get("params", {})
        if method == "initialize":
            result = {"userAgent": "context-peer"}
        elif method == "initialized":
            continue
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "thread/start":
            assert thread is None, "adapter replaced the ACP session after a context reset"
            thread = {"id": "stable-session", "cwd": params["cwd"], "status": {"type": "idle"}, "parentThreadId": None}
            result = {"thread": thread, "model": "fixture", "sandbox": {"type": "readOnly"}}
        elif method == "turn/start":
            turns += 1
            assert turns <= 2
            assert params == {"threadId": "stable-session", "input": [{"type": "text", "text": "before-window" if turns == 1 else "after-window", "text_elements": []}]}, params
            turn = {"id": f"turn-{turns}", "status": "inProgress", "items": []}
            emit({"method": "turn/started", "params": {"threadId": thread["id"], "turn": turn}})
            emit({"id": request["id"], "result": {"turn": turn}})
            if turns == 1:
                emit({"method": "item/agentMessage/delta", "params": {"threadId": thread["id"], "turnId": turn["id"], "itemId": "visible-answer", "delta": "Before reset"}})
                emit({"method": "thread/tokenUsage/updated", "params": {"threadId": thread["id"], "turnId": turn["id"], "tokenUsage": {"last": {"totalTokens": 950}, "total": {"totalTokens": 950}, "modelContextWindow": 1000}}})
                for notification in ("item/started", "item/completed"):
                    emit({"method": notification, "params": {"threadId": thread["id"], "turnId": turn["id"], "item": {"id": "context-reset", "type": "contextCompaction"}}})
                emit({"method": "thread/tokenUsage/updated", "params": {"threadId": thread["id"], "turnId": turn["id"], "tokenUsage": {"last": {"totalTokens": 12}, "total": {"totalTokens": 962}, "modelContextWindow": 1000}}})
            item = {"type": "agentMessage", "id": "visible-answer" if turns == 1 else "next-answer", "text": "Authoritative answer" if turns == 1 else "Next window answer"}
            emit({"method": "item/completed", "params": {"threadId": thread["id"], "turnId": turn["id"], "item": item}})
            emit({"method": "turn/completed", "params": {"threadId": thread["id"], "turn": {"id": turn["id"], "status": "completed", "items": [], "error": None}}})
            continue
        elif method in ("thread/list", "thread/loaded/list"):
            result = {"data": [], "nextCursor": None}
        elif method in ("thread/backgroundTerminals/clean", "thread/unsubscribe"):
            result = {}
        else:
            raise AssertionError(f"adapter performed unexpected context/history operation: {request}")
        emit({"id": request["id"], "result": result})


def probe(binary):
    process = subprocess.Popen([binary, "--codex-path", sys.executable,
        "--codex-arg", os.path.abspath(__file__), "--codex-arg", "backend",
        "--request-timeout-seconds", "8"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8")
    received = queue.Queue()
    def read():
        for line in process.stdout:
            received.put(json.loads(line))
        received.put(None)
    threading.Thread(target=read, daemon=True).start()
    updates = []
    raw = []
    next_id = 0
    def receive():
        frame = received.get(timeout=12)
        if frame is None:
            process.wait(timeout=8)
            raise AssertionError(f"adapter exited before context workflow completed: {process.stderr.read()}")
        if frame.get("method") == "session/update":
            assert frame["params"]["sessionId"] == "stable-session"
            updates.append(frame["params"]["update"])
        elif frame.get("method") == "_codex/event":
            raw.append(frame["params"])
        else:
            assert "method" not in frame, f"context reset invented a client interaction or transcript reset: {frame}"
        return frame
    def rpc(method, params):
        nonlocal next_id
        next_id += 1
        process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}) + "\n")
        process.stdin.flush()
        while True:
            frame = receive()
            if frame.get("id") == next_id and "method" not in frame:
                assert "error" not in frame, frame
                return frame["result"]
    try:
        rpc("initialize", {"protocolVersion": 2, "info": {"name": "context-probe", "version": "1"}, "capabilities": {"_meta": {"codex": {"version": 1, "events": ["item/started", "item/completed"], "sessionReset": True}}}})
        created = rpc("session/new", {"cwd": os.getcwd(), "mcpServers": []})
        assert created["sessionId"] == "stable-session"
        for prompt in ("before-window", "after-window"):
            start = len(updates)
            rpc("session/prompt", {"sessionId": "stable-session", "prompt": [{"type": "text", "text": prompt}]})
            while not any(update.get("state") == "idle" for update in updates[start:]):
                receive()
        messages = {update["messageId"]: update["content"] for update in updates if update.get("sessionUpdate") == "agent_message"}
        assert messages == {"visible-answer": [{"type": "text", "text": "Authoritative answer"}], "next-answer": [{"type": "text", "text": "Next window answer"}]}, messages
        assert [update for update in updates if update.get("sessionUpdate") == "usage_update"] == [{"sessionUpdate": "usage_update", "used": 950, "size": 1000}, {"sessionUpdate": "usage_update", "used": 12, "size": 1000}]
        resets = [event for event in raw if event["params"].get("item", {}).get("type") == "contextCompaction"]
        assert len(resets) == 2 and all(event["sessionId"] == "stable-session" for event in resets), resets
        rpc("session/close", {"sessionId": "stable-session"})
        process.stdin.close()
        assert process.wait(timeout=8) == 0, process.stderr.read()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)


if __name__ == "__main__":
    backend() if sys.argv[1] == "backend" else probe(sys.argv[1])
