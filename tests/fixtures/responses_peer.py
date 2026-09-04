"""Loopback-only Responses endpoint for the opt-in installed-Codex smoke test."""

import json
from http.server import BaseHTTPRequestHandler, HTTPServer


class ResponsesHandler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        size = int(self.headers["Content-Length"])
        if size > 2 * 1024 * 1024:
            self.send_error(413)
            return
        request = json.loads(self.rfile.read(size))
        marker_count = sum(
            block.get("text", "").count("deterministic-smoke-marker")
            for item in request.get("input", [])
            for block in item.get("content", [])
            if isinstance(block, dict)
        )
        print(json.dumps({"path": self.path, "model": request["model"], "markerCount": marker_count}), flush=True)
        events = [
            {"type": "response.created", "response": {"id": "smoke-response"}},
            {"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "id": "smoke-message", "phase": "final", "content": [{"type": "output_text", "text": "Local Responses smoke succeeded."}]}},
            {"type": "response.completed", "response": {"id": "smoke-response", "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "input_tokens_details": None, "output_tokens_details": None}}},
        ]
        body = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()


server = HTTPServer(("127.0.0.1", 0), ResponsesHandler)
print(json.dumps({"url": f"http://127.0.0.1:{server.server_port}/v1"}), flush=True)
server.serve_forever()
