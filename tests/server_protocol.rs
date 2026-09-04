//! Public ACP JSON-RPC checks against the real adapter and a deterministic child.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    backlog: Vec<Value>,
    next_id: u64,
    directory: tempfile::TempDir,
}

impl Client {
    async fn start(negotiated: bool) -> Self {
        let python = if cfg!(windows) { "python" } else { "python3" };
        assert!(
            Command::new(python)
                .arg("--version")
                .stdout(Stdio::null())
                .status()
                .await
                .is_ok(),
            "protocol tests require Python 3 ({python}) for the deterministic app-server fixture"
        );
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_codex-acp-v2"))
            .args([
                "--codex-path",
                python,
                "--codex-arg",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_peer.py"),
                "--codex-arg",
                "server",
                "--request-timeout-seconds",
                "5",
                "--interaction-timeout-seconds",
                "5",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            input: Some(input),
            output,
            backlog: Vec::new(),
            next_id: 1,
            directory,
        };
        let mut initialize = json!({"protocolVersion":2,"info":{"name":"test-client","version":"1"},"capabilities":{}});
        if negotiated {
            initialize["_meta"] =
                json!({"codex":{"version":1,"events":["*"],"serverRequests":true}});
        }
        let response = client.rpc("initialize", initialize).await;
        assert_eq!(response["protocolVersion"], 2);
        client
    }

    async fn send(&mut self, frame: Value) {
        let mut encoded = serde_json::to_vec(&frame).unwrap();
        encoded.push(b'\n');
        self.input
            .as_mut()
            .unwrap()
            .write_all(&encoded)
            .await
            .unwrap();
        self.input.as_mut().unwrap().flush().await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let mut line = String::new();
        let size = tokio::time::timeout(Duration::from_secs(8), self.output.read_line(&mut line))
            .await
            .expect("timed out reading ACP frame")
            .unwrap();
        assert_ne!(
            size, 0,
            "adapter exited while a protocol result was expected; queued frames: {:?}",
            self.backlog
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("invalid ACP JSON on stdout: {error}: {line}"))
    }

    async fn matching(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.backlog.iter().position(&predicate) {
            return self.backlog.remove(index);
        }
        loop {
            let frame = self.next().await;
            if predicate(&frame) {
                return frame;
            }
            assert!(
                self.backlog.len() < 2048,
                "unexpectedly unbounded protocol activity"
            );
            self.backlog.push(frame);
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        self.matching(|frame| frame["id"] == id && frame.get("method").is_none())
            .await
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params).await;
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    async fn new_session(&mut self, metadata: Value) -> String {
        let mut params = json!({"cwd":self.directory.path(),"mcpServers":[]});
        if !metadata.is_null() {
            params["_meta"] = json!({"codex":{"thread":metadata}});
        }
        let result = self.rpc("session/new", params).await;
        let id = result["sessionId"].as_str().unwrap().to_owned();
        self.matching(|frame| is_state(frame, "idle")).await;
        id
    }

    async fn shutdown(mut self) {
        self.input.take();
        let status = tokio::time::timeout(Duration::from_secs(8), self.child.wait())
            .await
            .expect("adapter failed to shut down after EOF")
            .unwrap();
        assert!(status.success(), "adapter exited unsuccessfully: {status}");
    }
}

fn is_state(frame: &Value, state: &str) -> bool {
    frame["method"] == "session/update"
        && frame["params"]["update"]["sessionUpdate"] == "state_update"
        && frame["params"]["update"]["state"] == state
}

fn current(options: &Value, id: &str) -> Value {
    options
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["configId"] == id)
        .unwrap()["currentValue"]
        .clone()
}

#[tokio::test]
async fn standard_lifecycle_streams_approvals_cancels_and_replays_without_feeding_history_back() {
    let mut client = Client::start(true).await;
    let id = client.new_session(Value::Null).await;
    let configuration = client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"effort","type":"id","value":"high"}),
        )
        .await;
    assert_eq!(current(&configuration["configOptions"], "effort"), "high");

    client.rpc("session/prompt", json!({"sessionId":id,"prompt":[{"type":"text","text":"approval"}],"_meta":{"codex":{"turn":{"outputSchema":{"type":"object"},"serviceTierForTurn":"priority"}}}})).await;
    let permission = client
        .matching(|frame| frame["method"] == "session/request_permission")
        .await;
    let decline = permission["params"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["kind"] == "reject_once")
        .unwrap()["optionId"]
        .clone();
    client.send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{"outcome":{"outcome":"selected","optionId":decline}}})).await;
    client.matching(|frame| is_state(frame, "idle")).await;
    let callback = client
        .matching(|frame| {
            frame["method"] == "_codex/event" && frame["params"]["method"] == "fixture/callback"
        })
        .await;
    assert_eq!(
        callback["params"]["params"]["response"],
        json!({"decision":"decline"})
    );

    let read = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(
        read["lastTurnParams"]["outputSchema"],
        json!({"type":"object"})
    );
    assert_eq!(read["lastTurnParams"]["serviceTierForTurn"], "priority");
    assert!(
        read["lastTurnParams"].get("effort").is_none(),
        "session configuration must not be silently repeated as a one-turn override"
    );

    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"cancel approval"}]}),
        )
        .await;
    client
        .matching(|frame| frame["method"] == "session/request_permission")
        .await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    let cancelled = client
        .matching(|frame| {
            is_state(frame, "idle") && frame["params"]["update"]["stopReason"] == "cancelled"
        })
        .await;
    assert_eq!(cancelled["params"]["sessionId"], id);
    client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "fixture/callback"
                && frame["params"]["params"]["response"] == json!({"decision":"cancel"})
        })
        .await;

    client.rpc("session/close", json!({"sessionId":id})).await;
    client.backlog.clear();
    client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    assert!(
        !client
            .backlog
            .iter()
            .any(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message"),
        "resume without replayFrom must not replay history"
    );
    client.rpc("session/close", json!({"sessionId":id})).await;
    client.backlog.clear();
    client.rpc("session/resume", json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[],"replayFrom":{"type":"start"}})).await;
    let messages: Vec<_> = client
        .backlog
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message")
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "one final assistant message should be replayed exactly once"
    );
    assert_eq!(
        messages[0]["params"]["update"]["messageId"],
        "answer-turn-1"
    );
    client.rpc("session/close", json!({"sessionId":id})).await;
    client.shutdown().await;
}

#[tokio::test]
async fn extensions_are_negotiated_bidirectional_and_share_authoritative_session_state() {
    let mut unnegotiated = Client::start(false).await;
    let rejected = unnegotiated
        .request(
            "_codex/request",
            json!({"version":1,"method":"model/list","params":{}}),
        )
        .await;
    assert!(rejected.get("error").is_some());
    unnegotiated.shutdown().await;

    let mut client = Client::start(true).await;
    let tools = json!([{"name":"client_lookup","description":"Client lookup","inputSchema":{"type":"object"}}]);
    let id = client.new_session(json!({"dynamicTools":tools})).await;
    let goal = json!({"threadId":id,"objective":"test goal","tokenBudget":1000});
    let result = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/goal/set","params":goal}),
        )
        .await;
    assert_eq!(result, json!({"goal":goal,"opaque":{"preserved":true}}));
    client.rpc("_codex/request", json!({"version":1,"sessionId":id,"method":"thread/settings/update","params":{"threadId":id,"effort":"high"}})).await;
    let updated = client
        .matching(|frame| frame["params"]["update"]["sessionUpdate"] == "config_option_update")
        .await;
    assert_eq!(
        current(&updated["params"]["update"]["configOptions"], "effort"),
        "high"
    );
    for params in [
        json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":"other-session"}}),
        json!({"version":1,"method":"process/spawn","params":{"command":"anything"}}),
        json!({"version":1,"method":"thread/start","params":{}}),
    ] {
        assert!(
            client
                .request("_codex/request", params)
                .await
                .get("error")
                .is_some()
        );
    }

    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"dynamic"}]}),
        )
        .await;
    let callback = client
        .matching(|frame| frame["method"] == "_codex/serverRequest")
        .await;
    assert_eq!(callback["params"]["method"], "item/tool/call");
    assert_eq!(callback["params"]["requestId"], 700);
    let callback_result =
        json!({"success":true,"contentItems":[{"type":"inputText","text":"client value"}]});
    client
        .send(json!({"jsonrpc":"2.0","id":callback["id"],"result":callback_result}))
        .await;
    let observed = client
        .matching(|frame| {
            frame["method"] == "_codex/event" && frame["params"]["method"] == "fixture/callback"
        })
        .await;
    assert_eq!(observed["params"]["params"]["response"], callback_result);
    client.matching(|frame| is_state(frame, "idle")).await;

    // Complete-before-accept must not resurrect a finished turn and steer the next prompt.
    for _ in 0..2 {
        client
            .rpc(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":"fast"}]}),
            )
            .await;
        client.matching(|frame| is_state(frame, "idle")).await;
    }
    client.rpc("session/close", json!({"sessionId":id})).await;
    client.shutdown().await;
}
