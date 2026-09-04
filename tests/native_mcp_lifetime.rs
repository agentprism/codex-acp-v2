//! Native resources must close on ACP EOF without relying on process exit.

use std::{io, sync::Arc, time::Duration};

use agent_client_protocol::{Agent, Client, Lines, Responder, V2ConnectionTo, schema::v2};
use codex_acp_v2::mcp::{McpLeases, McpManager};
use futures::{SinkExt, StreamExt, channel::mpsc};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn acp_eof_releases_native_listener_while_the_runtime_keeps_running() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let leases = Arc::new(tokio::sync::Mutex::new(McpLeases::default()));
        let retained = Arc::downgrade(&leases);
        let manager = McpManager::new(Duration::from_secs(2));
        let (mut input, incoming) = mpsc::channel::<String>(16);
        let (outgoing, mut output) = mpsc::channel::<String>(16);
        let transport = Lines::new(
            outgoing.sink_map_err(io::Error::other),
            incoming.map(Ok::<_, io::Error>),
        );
        let agent = Agent.v2().on_receive_request(
            async |request: v2::InitializeRequest,
                   responder: Responder<v2::InitializeResponse>,
                   _connection: V2ConnectionTo<Client>| {
                responder.respond(v2::InitializeResponse::new(
                    request.protocol_version,
                    v2::Implementation::new("lifetime-probe", "1"),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        ).on_receive_request(
            async move |request: v2::NewSessionRequest,
                        responder: Responder<v2::NewSessionResponse>,
                        connection: V2ConnectionTo<Client>| {
                let prepared = manager.prepare(&request.mcp_servers, &connection).await
                    .map_err(|error| agent_client_protocol::Error::internal_error().data(error.to_string()))?;
                let servers = serde_json::to_value(&prepared.servers)?;
                *leases.lock().await = prepared.leases;
                responder.respond(v2::NewSessionResponse::new("session").meta(
                    serde_json::from_value::<v2::Meta>(json!({"url":servers[0]["url"]}))?,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        );
        let task = tokio::spawn(agent.connect_to(transport));
        input.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":2,"info":{"name":"probe","version":"1"},"capabilities":{}}}).to_string()).await.unwrap();
        let response: Value = serde_json::from_str(&output.next().await.unwrap()).unwrap();
        assert!(response.get("result").is_some(), "{response}");
        input.send(json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/workspace","mcpServers":[{"type":"acp","name":"native","serverId":"provider"}]}}).to_string()).await.unwrap();
        let response: Value = serde_json::from_str(&output.next().await.unwrap()).unwrap();
        assert!(response.get("result").is_some(), "{response}");
        let url = response["result"]["_meta"]["url"].as_str().unwrap();
        let (address, token) = url.strip_prefix("http://").unwrap().split_once('/').unwrap();
        let address = address.to_owned();
        let path = format!("/{token}");
        let http_address = address.clone();
        let initialized = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(&http_address).await.unwrap();
            let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}).to_string();
            stream.write_all(format!("POST {path} HTTP/1.1\r\nHost: {http_address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        });
        let connect: Value = serde_json::from_str(&output.next().await.unwrap()).unwrap();
        assert_eq!(connect["method"], "mcp/connect");
        input.send(json!({"jsonrpc":"2.0","id":connect["id"],"result":{"connectionId":"provider-1"}}).to_string()).await.unwrap();
        let initialize: Value = serde_json::from_str(&output.next().await.unwrap()).unwrap();
        assert_eq!(initialize["params"]["method"], "initialize");
        input.send(json!({"jsonrpc":"2.0","id":initialize["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"provider","version":"1"}}}).to_string()).await.unwrap();
        initialized.await.unwrap();

        // Close only ACP input. The HTTP listener and Tokio runtime stay alive
        // unless the connection-owned lease actually releases its resources.
        input.close().await.unwrap();
        task.await.unwrap().unwrap();
        assert!(retained.upgrade().is_none(), "ACP handler state retained a lease after EOF");
        // Refusing a closed loopback port can take several SYN retries on Windows.
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if tokio::net::TcpStream::connect(&address).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("native listener survived ACP EOF in a live runtime");
    }).await.expect("native lifetime probe timed out");
}
