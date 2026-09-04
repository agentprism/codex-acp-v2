"""Cross-platform release check through the packaged default and a local model."""

import json
from pathlib import Path
import signal
import sys
import tempfile
import threading
from http.server import ThreadingHTTPServer

from installed_workflow import Client, Model, message


class SmokeModel(Model):
    observed = []

    def do_POST(self):
        size = int(self.headers.get("Content-Length", 0))
        assert 0 < size <= 2 * 1024 * 1024
        request = json.loads(self.rfile.read(size))
        self.observed.append(request)
        self.send_items(message("bundled-smoke-message", "Packaged backend completed this turn."), "bundled-smoke-response", 0)


def smoke(binary):
    server = ThreadingHTTPServer(("127.0.0.1", 0), SmokeModel)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="codex-acp-bundle-") as temporary:
        directory = Path(temporary)
        (directory / "profile").mkdir()
        workspace = directory / "workspace"
        workspace.mkdir()
        client = Client(binary, directory, f"http://127.0.0.1:{server.server_port}/v1")
        try:
            assert client.initialized["protocolVersion"] == 2, client.initialized
            result = client.rpc("session/new", {
                "cwd": str(workspace), "mcpServers": [],
                "_meta": {"codex": {"thread": {
                    "model": "workflow-model", "modelProvider": "workflow",
                    "sandbox": "read-only", "approvalPolicy": "never",
                }}},
            })
            session = result["sessionId"]
            assert any(option["configId"] == "model" for option in result["configOptions"]), result
            client.idle_since(0)
            start = len(client.events)
            client.rpc("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "bundled-default-input-marker"}]})
            updates = client.idle_since(start)
            responses = [update for update in updates if update.get("sessionUpdate") == "agent_message"]
            assert responses and responses[-1]["content"] == [{"type": "text", "text": "Packaged backend completed this turn."}], responses
            assert len(SmokeModel.observed) == 1
            request = SmokeModel.observed[0]
            user_text = "\n".join(block.get("text", "") for item in request["input"] if item.get("role") == "user" for block in item.get("content", []))
            assert request["model"] == "workflow-model" and user_text.count("bundled-default-input-marker") == 1, request
            client.rpc("session/close", {"sessionId": session})

            start = len(client.events)
            client.rpc("session/resume", {"sessionId": session, "cwd": str(workspace), "mcpServers": []})
            assert not any(update.get("sessionUpdate") == "agent_message" for update in client.events[start:]), client.events[start:]
            client.rpc("session/close", {"sessionId": session})
            start = len(client.events)
            client.rpc("session/resume", {"sessionId": session, "cwd": str(workspace), "mcpServers": [], "replayFrom": {"type": "start"}})
            replay = [update for update in client.events[start:] if update.get("sessionUpdate") == "agent_message"]
            assert len(replay) == 1 and replay[0]["content"] == responses[-1]["content"], replay
            assert len(SmokeModel.observed) == 1, "client replay must not trigger inference"
            client.rpc("session/close", {"sessionId": session})
            client.shutdown()
            print("Packaged default: ACP v2, local inference, durable replay, no backend override, poisoned Codex PATH: passed.")
        finally:
            if client.process.poll() is None:
                if sys.platform != "win32":
                    import os
                    os.killpg(client.process.pid, signal.SIGTERM)
                else:
                    client.process.terminate()
                client.process.wait(timeout=10)
            server.shutdown()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: bundled_smoke.py ABSOLUTE_ADAPTER_PATH")
    smoke(sys.argv[1])
