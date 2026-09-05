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
        Self::start_with_timeout(negotiated, "5").await
    }

    async fn start_with_timeout(negotiated: bool, timeout: &str) -> Self {
        Self::start_fixture(negotiated, timeout, "server").await
    }

    async fn start_fixture(negotiated: bool, timeout: &str, scenario: &str) -> Self {
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_codex-acp-v2"));
        command.args([
            "--codex-path",
            python,
            "--codex-arg",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_peer.py"),
            "--codex-arg",
            scenario,
            "--request-timeout-seconds",
            timeout,
            "--interaction-timeout-seconds",
            "5",
        ]);
        Self::connect(command, directory, negotiated).await
    }

    async fn connect(mut command: Command, directory: tempfile::TempDir, negotiated: bool) -> Self {
        let mut child = command
            .env_remove("CODEX_APP_SERVER_PATH")
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
            initialize["_meta"] = json!({"codex":{"version":1,"events":["*"],"serverRequests":true,"sessionReset":true}});
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
            .unwrap_or_else(|_| {
                panic!(
                    "timed out reading ACP frame; pending activity: {:?}",
                    self.backlog
                )
            })
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
async fn slow_replay_does_not_block_another_sessions_events_or_prompt() {
    let mut client = Client::start_fixture(true, "5", "parallel").await;
    let first = client.new_session(Value::Null).await;
    let second = client.new_session(Value::Null).await;
    client
        .rpc("session/close", json!({"sessionId":first}))
        .await;
    client.send(json!({"jsonrpc":"2.0","id":100,"method":"session/resume","params":{"sessionId":first,"cwd":client.directory.path(),"mcpServers":[],"replayFrom":{"type":"start"}}})).await;
    // The fixture holds the history response while a same-session notification
    // waits behind replay. The second session must still receive its events.
    tokio::time::timeout(
        Duration::from_secs(1),
        client.matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "fixture/replayBlocked"
        }),
    )
    .await
    .expect("one session's replay blocked the connection event pump");
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":second,"prompt":[{"type":"text","text":"work during replay"}]}),
        )
        .await;
    let finished = tokio::time::timeout(
        Duration::from_secs(1),
        client.matching(|frame| is_state(frame, "idle") && frame["params"]["sessionId"] == second),
    )
    .await
    .expect("independent session did not finish before replay was released");
    assert_eq!(finished["params"]["update"]["stopReason"], "end_turn");
    client
        .rpc(
            "_codex/request",
            json!({"version":1,"method":"model/list","params":{}}),
        )
        .await;
    let resumed = client
        .matching(|frame| frame["id"] == 100 && frame.get("method").is_none())
        .await;
    assert!(resumed.get("error").is_none());
    let replay = client
        .matching(|frame| {
            frame["params"]["sessionId"] == first
                && frame["params"]["update"]["messageId"] == "saved-message"
        })
        .await;
    assert_eq!(
        replay["params"]["update"]["content"],
        json!([{"type":"text","text":"retained history"}])
    );
    let output = client
        .matching(|frame| frame["params"]["update"]["sessionUpdate"] == "terminal_output_chunk")
        .await;
    assert_eq!(
        output["params"]["update"]["data"], "YWZ0ZXIK",
        "pre-snapshot output must not append twice, but post-snapshot background output must survive"
    );
    let interaction = client
        .matching(|frame| frame["params"]["update"]["sessionUpdate"] == "tool_call_content_chunk")
        .await;
    assert_eq!(
        interaction["params"]["update"]["content"]["content"]["text"],
        "Input sent to process background: input not represented by history"
    );
    let raw = client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "item/commandExecution/outputDelta"
                && frame["params"]["params"]["delta"] == "before\n"
        })
        .await;
    assert_eq!(
        raw["params"]["sessionId"], first,
        "native snapshot reconciliation must preserve the raw event stream"
    );
    client.shutdown().await;
}

#[tokio::test]
async fn resume_reconciles_full_settings_with_partial_lifecycle_responses() {
    let mut client = Client::start(true).await;
    let id = client
        .new_session(json!({"config":{"audit_resume_settings":true}}))
        .await;
    // A cold lifecycle response cannot reveal the collaboration mode. Choosing
    // default must write a preset, not acknowledge an invented cache no-op.
    let chosen = client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"mode","type":"id","value":"default"}),
        )
        .await;
    assert_eq!(current(&chosen["configOptions"], "mode"), "default");
    let backend = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(
        backend["observedSettings"]["collaborationMode"]["mode"], "default",
        "the backend started in unreported plan mode; choosing default must actually apply it"
    );
    client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"mode","type":"id","value":"plan"}),
        )
        .await;
    let resumed = client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    assert_eq!(
        (
            current(&resumed["configOptions"], "mode"),
            current(&resumed["configOptions"], "model")
        ),
        (json!("plan"), json!("resumed-model")),
        "a full settings event supplies omitted mode while the later response supplies the model"
    );
    let changed = client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"mode","type":"id","value":"default"}),
        )
        .await;
    assert_eq!(current(&changed["configOptions"], "mode"), "default");
    client.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn resume_accepts_directory_aliases_without_allowing_a_different_workspace() {
    let mut client = Client::start(true).await;
    let id = client.new_session(Value::Null).await;
    let paths = tempfile::tempdir().unwrap();
    let alias = paths.path().join("workspace-alias");
    std::os::unix::fs::symlink(client.directory.path(), &alias).unwrap();
    client.rpc("session/close", json!({"sessionId":id})).await;

    // Codex can retain a canonical cwd while clients use macOS /var aliases or
    // Windows short names. Exercise that distinction through the public API.
    client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":alias,"mcpServers":[]}),
        )
        .await;
    for different in [paths.path().to_owned(), paths.path().join("missing")] {
        let rejected = client
            .request(
                "session/resume",
                json!({"sessionId":id,"cwd":different,"mcpServers":[]}),
            )
            .await;
        assert_eq!(
            rejected["error"],
            json!({
                "code":-32602,
                "message":"Invalid params",
                "data":"resume cwd must match the stored session cwd"
            })
        );
    }
    let backend = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(
        backend["thread"]["resumeParams"]["cwd"],
        json!(alias),
        "rejected working directories must not reach thread/resume"
    );
    client.shutdown().await;
}

#[tokio::test]
async fn cancellation_racing_completion_keeps_the_connection_usable() {
    let mut client = Client::start(true).await;
    let id = client.new_session(Value::Null).await;
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"cancel race"}]}),
        )
        .await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    let completed = client
        .matching(|frame| {
            is_state(frame, "idle") && frame["params"]["update"]["stopReason"] == "end_turn"
        })
        .await;
    assert_eq!(completed["params"]["sessionId"], id);
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"fast"}]}),
        )
        .await;
    let answer = client
        .matching(|frame| {
            frame["params"]["update"]["messageId"] == "answer-turn-2"
                && frame["params"]["update"]["sessionUpdate"] == "agent_message"
        })
        .await;
    assert_eq!(
        answer["params"]["update"]["content"],
        json!([{"type":"text","text":"fast answer"}])
    );
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"missing"}}))
        .await;
    client.rpc("session/list", json!({})).await;
    client.shutdown().await;
}

#[tokio::test]
async fn queued_extension_settings_are_reconciled_before_native_configuration() {
    let mut client = Client::start_with_timeout(true, "1").await;
    let id = client
        .new_session(json!({"config":{"audit_defer_settings":true}}))
        .await;
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"long"}]}),
        )
        .await;
    client.send(json!({"jsonrpc":"2.0","id":100,"method":"_codex/request","params":{"version":1,"sessionId":id,"method":"thread/settings/update","params":{"threadId":id,"effort":"high"}}})).await;
    client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "fixture/settingsQueued"
        })
        .await;
    client
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    tokio::time::timeout(
        Duration::from_millis(500),
        client.matching(|frame| {
            is_state(frame, "idle") && frame["params"]["update"]["stopReason"] == "cancelled"
        }),
    )
    .await
    .expect("cancellation must not wait for pending session settings to reconcile");
    // An unrelated snapshot cannot certify this mutation. Timing out after the
    // backend's queued acknowledgment must not discard its unresolved state.
    let extension = client
        .matching(|frame| frame["id"] == 100 && frame.get("method").is_none())
        .await;
    assert!(extension.get("error").is_some());
    client.send(json!({"jsonrpc":"2.0","id":101,"method":"session/set_config_option","params":{"sessionId":id,"configId":"effort","type":"id","value":"medium"}})).await;
    client
        .rpc(
            "_codex/request",
            json!({"version":1,"method":"model/list","params":{}}),
        )
        .await;
    let native = client
        .matching(|frame| frame["id"] == 101 && frame.get("method").is_none())
        .await;
    assert_eq!(
        current(&native["result"]["configOptions"], "effort"),
        "medium"
    );
    client.rpc("session/close", json!({"sessionId":id})).await;
    let resumed = client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    assert_eq!(current(&resumed["configOptions"], "effort"), "medium");
    client.shutdown().await;
}

#[tokio::test]
async fn session_owned_mcp_streams_route_early_events_stop_and_close_without_host_access() {
    let mut client = Client::start(true).await;
    let id = client.new_session(Value::Null).await;
    let start = |subscription: &str| json!({"version":1,"sessionId":id,"method":"mcpServer/event/stream/start","params":{"threadId":id,"subscriptionId":subscription,"server":"tools","name":"watch","arguments":{}}});
    client.rpc("_codex/request", start("watch-1")).await;
    let event = client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "mcpServer/event/stream"
        })
        .await;
    assert_eq!(event["params"]["sessionId"], id);
    assert_eq!(
        event["params"]["params"]["notification"]["params"]["data"],
        "early MCP event"
    );
    let duplicate = client.request("_codex/request", start("watch-1")).await;
    assert!(duplicate.get("error").is_some());
    client.rpc("_codex/request",json!({"version":1,"sessionId":id,"method":"mcpServer/event/stream/stop","params":{"subscriptionId":"watch-1"}})).await;
    client.rpc("_codex/request", start("watch-2")).await;
    client.rpc("session/close", json!({"sessionId":id})).await;
    client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    let state = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(state["streamStops"], json!(["watch-1", "watch-2"]));
    assert_eq!(state["activeStreams"], json!([]));
    client.shutdown().await;
}

#[tokio::test]
async fn history_mutations_publish_reset_boundaries_and_authoritative_replay() {
    let mut client = Client::start(true).await;
    let id = client.new_session(Value::Null).await;
    client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"mode","type":"id","value":"default"}),
        )
        .await;
    for text in ["keep", "remove"] {
        client
            .rpc(
                "session/prompt",
                json!({"sessionId":id,"prompt":[{"type":"text","text":text}]}),
            )
            .await;
        client.matching(|frame| is_state(frame, "idle")).await;
    }
    for (method, params) in [
        ("thread/rollback", json!({"threadId":id,"numTurns":1})),
        (
            "thread/revert",
            json!({"threadId":id,"beforeTurnId":"turn-1"}),
        ),
    ] {
        client.backlog.clear();
        client
            .rpc(
                "_codex/request",
                json!({"version":1,"sessionId":id,"method":method,"params":params}),
            )
            .await;
        // The fixture re-emits the deleted final snapshot immediately before the
        // mutation response. Wait for raw delivery to prove native filtering has
        // run before inspecting the rebuilt transcript.
        client
            .matching(|frame| {
                frame["method"] == "_codex/event" && frame["params"]["method"] == "item/completed"
            })
            .await;
        let boundaries: Vec<_> = client
            .backlog
            .iter()
            .filter(|frame| frame["method"] == "_codex/sessionReset")
            .collect();
        assert_eq!(boundaries.len(), 2);
        assert_eq!(boundaries[0]["params"]["phase"], "start");
        assert_eq!(boundaries[1]["params"]["phase"], "complete");
        assert_eq!(
            boundaries[0]["params"]["revision"],
            boundaries[1]["params"]["revision"]
        );
        let replayed: Vec<_> = client
            .backlog
            .iter()
            .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message")
            .map(|frame| frame["params"]["update"]["messageId"].clone())
            .collect();
        assert_eq!(
            replayed,
            if method == "thread/rollback" {
                vec![json!("answer-turn-1")]
            } else {
                vec![]
            }
        );
    }
    client
        .rpc(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"mode","type":"id","value":"default"}),
        )
        .await;
    let backend = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(
        backend["observedSettings"]["collaborationMode"]["mode"], "default",
        "revert restored plan mode; a later native setting must not use the pre-revert no-op cache"
    );
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"fresh"}]}),
        )
        .await;
    client.matching(|frame| is_state(frame, "idle")).await;
    client.shutdown().await;
}

#[tokio::test]
async fn oversized_acp_input_fails_without_waiting_for_peer_eof() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-acp-v2"))
        .args([
            "--codex-path",
            python,
            "--codex-arg",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_peer.py"),
            "--codex-arg",
            "server",
            "--max-frame-bytes",
            "4096",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(&vec![b'x'; 4097]).await.unwrap();
    input.flush().await.unwrap();
    // Keep stdin open: the byte limit must reject the frame before a newline/EOF.
    let output = tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .expect("oversized ACP frame did not terminate the adapter")
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("ACP inbound frame exceeds max_frame_bytes")
    );
}

#[tokio::test]
#[ignore = "requires installed Codex and Python 3; isolated profile and local mock provider"]
async fn installed_codex_supports_real_protocol_catalog_and_session_lifecycle() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut provider = Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/responses_peer.py"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut provider_output = BufReader::new(provider.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), provider_output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let provider_url = serde_json::from_str::<Value>(&line).unwrap()["url"].clone();
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("codex-profile");
    std::fs::create_dir(&profile).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_codex-acp-v2"));
    command
        .args(["--request-timeout-seconds", "20"])
        .arg("--codex-path")
        .arg(std::env::var_os("CODEX_PATH").unwrap_or_else(|| "codex".into()))
        .env("CODEX_HOME", profile)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY");
    let provider_config = json!({"name":"Local smoke","base_url":provider_url,"wire_api":"responses","requires_openai_auth":false,"supports_websockets":false});
    for (key, value) in provider_config.as_object().unwrap() {
        command
            .arg("--codex-arg=-c")
            .arg(format!("--codex-arg=model_providers.smoke.{key}={value}"));
    }
    let mut client = Client::connect(command, directory, true).await;
    let id = client
        .new_session(json!({"sandbox":"read-only","approvalPolicy":"never","model":"smoke-model","modelProvider":"smoke"}))
        .await;
    let read = client.rpc("_codex/request", json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id,"includeTurns":false}})).await;
    assert_eq!(read["thread"]["id"], id);
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"deterministic-smoke-marker"}]}),
        )
        .await;
    let message = client
        .matching(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message")
        .await;
    assert_eq!(
        message["params"]["update"]["content"],
        json!([{"type":"text","text":"Local Responses smoke succeeded."}])
    );
    client.matching(|frame| is_state(frame, "idle")).await;
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), provider_output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"path":"/v1/responses","model":"smoke-model","markerCount":1})
    );
    client.rpc("session/close", json!({"sessionId":id})).await;
    client
        .rpc(
            "session/resume",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    let fork = client
        .rpc(
            "session/fork",
            json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[]}),
        )
        .await;
    assert_ne!(
        fork["sessionId"], id,
        "fork must create an independent Codex thread"
    );
    client
        .rpc("session/close", json!({"sessionId":fork["sessionId"]}))
        .await;
    client.rpc("session/delete", json!({"sessionId":id})).await;
    let listed = client
        .rpc("session/list", json!({"cwd":client.directory.path()}))
        .await;
    assert!(
        !listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["sessionId"] == id),
        "soft-deleted sessions must disappear from the standard listing"
    );
    client.shutdown().await;
    provider.kill().await.unwrap();
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
    let additional = client.directory.path().join("additional-root");
    client.rpc("session/resume", json!({"sessionId":id,"cwd":client.directory.path(),"additionalDirectories":[additional],"mcpServers":[{"type":"stdio","name":"client_context","command":"context-tool","args":["serve"],"env":[{"name":"MODE","value":"test"}]}]})).await;
    let read = client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":id}}),
        )
        .await;
    assert_eq!(
        read["thread"]["resumeParams"]["runtimeWorkspaceRoots"],
        json!([client.directory.path(), additional])
    );
    assert_eq!(
        read["thread"]["resumeParams"]["config"]["mcp_servers"],
        json!({"client_context":{"command":"context-tool","args":["serve"],"env":{"MODE":"test"}}})
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
    let started = client
        .matching(|frame| {
            frame["method"] == "_codex/event" && frame["params"]["method"] == "thread/started"
        })
        .await;
    assert_eq!(
        started["params"]["sessionId"], id,
        "thread creation events preceding the backend response must survive session registration"
    );
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

    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"dynamic"}]}),
        )
        .await;
    let callback = client
        .matching(|frame| frame["method"] == "_codex/serverRequest")
        .await;
    let callback_error = json!({"code":-32042,"message":"client lookup failed","data":{"retryable":false,"detail":[1,2]}});
    client
        .send(json!({"jsonrpc":"2.0","id":callback["id"],"error":callback_error}))
        .await;
    let observed = client
        .matching(|frame| {
            frame["method"] == "_codex/event" && frame["params"]["method"] == "fixture/callback"
        })
        .await;
    assert_eq!(
        observed["params"]["params"]["response"],
        json!({"error":callback_error})
    );
    client.matching(|frame| is_state(frame, "idle")).await;

    client.backlog.clear();
    client
        .rpc(
            "session/prompt",
            json!({"sessionId":id,"prompt":[{"type":"text","text":"child"}]}),
        )
        .await;
    let permission = client
        .matching(|frame| frame["method"] == "session/request_permission")
        .await;
    assert_eq!(
        permission["params"]["sessionId"], id,
        "verified child approvals belong to the ACP root session"
    );
    let decline = permission["params"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["kind"] == "reject_once")
        .unwrap()["optionId"]
        .clone();
    client.send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{"outcome":{"outcome":"selected","optionId":decline}}})).await;
    let child_response = client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "fixture/childCallback"
        })
        .await;
    assert_eq!(
        child_response["params"]["params"],
        json!({"threadId":id,"requestId":800,"response":{"decision":"decline"}})
    );
    let rejected = client
        .matching(|frame| {
            frame["method"] == "_codex/event"
                && frame["params"]["method"] == "fixture/unrelatedDenied"
        })
        .await;
    assert!(
        rejected["params"]["params"]["response"]
            .get("error")
            .is_some(),
        "an unrelated thread callback must not be approved or forwarded"
    );
    assert!(
        !client.backlog.iter().any(|frame| is_state(frame, "idle")),
        "child completion must not finish root foreground work"
    );
    assert!(
        !client
            .backlog
            .iter()
            .any(|frame| frame["method"] == "session/request_permission"),
        "unrelated callback must not appear in the root session"
    );
    assert!(
        client
            .backlog
            .iter()
            .filter(|frame| is_state(frame, "running"))
            .count()
            <= 2,
        "child lifecycle must not emit additional root running transitions"
    );
    let child = client.rpc("_codex/request", json!({"version":1,"sessionId":id,"method":"thread/read","params":{"threadId":"child"}})).await;
    assert_eq!(
        child["thread"]["id"], "child",
        "authorized child history requests must preserve their backend target"
    );
    client
        .rpc(
            "_codex/request",
            json!({"version":1,"sessionId":id,"method":"thread/goal/get","params":{"threadId":id}}),
        )
        .await;
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
    let threads = client
        .rpc(
            "_codex/request",
            json!({"version":1,"method":"thread/list","params":{}}),
        )
        .await;
    assert_eq!(
        threads["data"][0]["childSubscribed"], false,
        "closing the root must unsubscribe its verified descendants"
    );
    assert_eq!(
        threads["data"][0]["childInterrupted"], true,
        "closing the root must stop outstanding child foreground work"
    );
    client.backlog.clear();
    client.rpc("session/resume",json!({"sessionId":id,"cwd":client.directory.path(),"mcpServers":[],"replayFrom":{"type":"start"}})).await;
    let child_tools: Vec<_> = client
        .backlog
        .iter()
        .filter(|frame| {
            frame["params"]["update"]["sessionUpdate"] == "tool_call_update"
                && frame["params"]["update"]["toolCallId"] == "codex-child:child:child-tool"
        })
        .collect();
    assert_eq!(
        child_tools.len(),
        1,
        "child tool entities shown live must also survive full root replay"
    );
    assert_eq!(child_tools[0]["params"]["update"]["status"], "failed");
    assert!(
        !client
            .backlog
            .iter()
            .any(|frame| frame["params"]["update"]["messageId"]
                .as_str()
                .is_some_and(|id| id.starts_with("codex-child:"))),
        "child assistant messages are not parent chat messages"
    );
    client.shutdown().await;
}
