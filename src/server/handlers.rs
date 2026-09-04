use std::sync::Arc;

use agent_client_protocol::{
    Agent, Client, JsonRpcMessage, JsonRpcRequest, Responder, UntypedMessage, V2ConnectionTo,
    schema::{ProtocolVersion, v2},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json, value::to_raw_value};

use super::{EventReceiver, Server, rpc_error};
use crate::extensions::Negotiation;

// Every potentially blocking operation runs outside the ACP dispatch callback.
// Permission replies, event projection and cancellation remain independently live.
macro_rules! handler {
    ($builder:expr, $server:ident, $req:ty, $resp:ty, |$agent:ident, $request:ident, $cx:ident| $body:expr) => {{
        let server = $server.clone();
        $builder.on_receive_request(
            async move |$request: $req,
                        responder: Responder<$resp>,
                        connection: V2ConnectionTo<Client>| {
                let $agent = server.clone();
                let $cx = connection.clone();
                let permit = match $agent.request_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        return responder.respond_with_error(agent_client_protocol::Error::new(
                            -32000,
                            "too many in-flight ACP requests",
                        ));
                    }
                };
                connection.spawn(async move {
                    let _permit = permit;
                    match $body.await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_error(rpc_error(error)),
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
    }};
}

pub(super) async fn serve(server: Server, events: EventReceiver) -> Result<()> {
    let transport = crate::acp_transport::BoundedStdio::new(server.options.backend.max_frame_bytes);
    let initialize_server = server.clone();
    let builder = Agent.v2().name("codex-acp-v2").on_receive_request(
        async move |request: v2::InitializeRequest,
                    responder: Responder<v2::InitializeResponse>,
                    connection: V2ConnectionTo<Client>| {
            let server = initialize_server.clone();
            let events = events.clone();
            let event_connection = connection.clone();
            connection.spawn(async move {
                let result = server.initialize(request).await;
                match result {
                    Err(error) => responder.respond_with_error(rpc_error(error)),
                    Ok(response) => {
                        responder.respond(response)?;
                        if let Some((receiver, registrations)) = events.lock().await.take() {
                            let pump_connection = event_connection.clone();
                            event_connection.spawn(async move {
                                server
                                    .event_pump(receiver, registrations, pump_connection)
                                    .await
                                    .map_err(rpc_error)
                            })?;
                        }
                        Ok(())
                    }
                }
            })
        },
        agent_client_protocol::on_receive_request!(),
    );
    let builder = handler!(
        builder,
        server,
        v2::NewSessionRequest,
        v2::NewSessionResponse,
        |server, request, cx| server.new_session(request, &cx)
    );
    let builder = handler!(
        builder,
        server,
        v2::ListSessionsRequest,
        v2::ListSessionsResponse,
        |server, request, _cx| server.list_sessions(request)
    );
    let builder = handler!(
        builder,
        server,
        v2::ResumeSessionRequest,
        v2::ResumeSessionResponse,
        |server, request, cx| server.resume_session(request, &cx)
    );
    let builder = handler!(
        builder,
        server,
        v2::ForkSessionRequest,
        v2::ForkSessionResponse,
        |server, request, cx| server.fork_session(request, &cx)
    );
    let builder = handler!(
        builder,
        server,
        v2::CloseSessionRequest,
        v2::CloseSessionResponse,
        |server, request, cx| server.close(&request.session_id.to_string(), &cx)
    );
    let builder = handler!(
        builder,
        server,
        v2::DeleteSessionRequest,
        v2::DeleteSessionResponse,
        |server, request, cx| server.delete(&request.session_id.to_string(), &cx)
    );
    let builder = handler!(
        builder,
        server,
        v2::PromptRequest,
        v2::PromptResponse,
        |server, request, _cx| server.prompt(request)
    );
    let builder = handler!(
        builder,
        server,
        v2::SetSessionConfigOptionRequest,
        v2::SetSessionConfigOptionResponse,
        |server, request, _cx| server.set_config(request)
    );
    let builder = handler!(
        builder,
        server,
        ExtensionRequest,
        Value,
        |server, request, cx| server.extension(request.0, &cx)
    );
    let mcp_server = server.clone();
    let builder = builder.on_receive_request(
        async move |request: v2::MessageMcpRequest,
                    responder: Responder<v2::MessageMcpResponse>,
                    connection: V2ConnectionTo<Client>| {
            let server = mcp_server.clone();
            let permit = match server.request_slots.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    return responder.respond_with_error(agent_client_protocol::Error::new(
                        -32000,
                        "too many in-flight ACP requests",
                    ));
                }
            };
            let cancellation = responder.cancellation();
            connection.spawn(async move {
                let _permit = permit;
                match cancellation
                    .run_until_cancelled(server.mcp.request(request))
                    .await
                {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            })
        },
        agent_client_protocol::on_receive_request!(),
    );
    let mcp = server.mcp.clone();
    let builder = builder.on_receive_notification(
        async move |request: v2::MessageMcpNotification, connection: V2ConnectionTo<Client>| {
            let mcp = mcp.clone();
            connection.spawn(async move {
                if let Err(error) = mcp.notify(request).await {
                    tracing::warn!(error = %error, "native MCP notification failed; endpoint closed");
                }
                Ok(())
            })
        }, agent_client_protocol::on_receive_notification!());
    builder
        .on_receive_notification(
            async move |request: v2::CancelSessionNotification,
                        connection: V2ConnectionTo<Client>| {
                let server = server.clone();
                let permit = match server.request_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        server
                            .notification_failure(
                                &connection,
                                &request.session_id.to_string(),
                                "session/cancel",
                                anyhow::anyhow!("too many in-flight ACP requests"),
                            )
                            .await
                            .map_err(rpc_error)?;
                        return Ok(());
                    }
                };
                let task_connection = connection.clone();
                connection.spawn(async move {
                    let _permit = permit;
                    let id = request.session_id.to_string();
                    if let Err(error) = server.cancel(&id, &task_connection).await {
                        server
                            .notification_failure(&task_connection, &id, "session/cancel", error)
                            .await
                            .map_err(rpc_error)?;
                    }
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(transport)
        .await?;
    Ok(())
}

impl Server {
    async fn initialize(&self, request: v2::InitializeRequest) -> Result<v2::InitializeResponse> {
        ensure!(!self.state.lock().await.initialized, "already initialized");
        let negotiation = Negotiation::from_initialize_meta(
            &serde_json::to_value(&request.meta)?,
            &serde_json::to_value(&request.capabilities.meta)?,
        )?;
        let backend_capabilities = &self.options.backend.capabilities;
        let custom_callbacks = backend_capabilities["requestAttestation"] == true
            || backend_capabilities["mcpServerOpenaiFormElicitation"] == true
            || backend_capabilities["extensions"]
                .as_object()
                .is_some_and(|extensions| !extensions.is_empty());
        ensure!(
            !custom_callbacks
                || negotiation
                    .as_ref()
                    .is_some_and(|negotiation| negotiation.server_requests),
            "configured backend capabilities require codex serverRequests negotiation"
        );
        let models = self.models().await?;
        let mut state = self.state.lock().await;
        state.capabilities = request.capabilities;
        state.extensions = negotiation.is_some();
        state.negotiation = negotiation;
        state.models = models;
        state.initialized = true;
        let session = v2::SessionCapabilities::new()
            .delete(v2::SessionDeleteCapabilities::new())
            .fork(v2::SessionForkCapabilities::new())
            .additional_directories(v2::SessionAdditionalDirectoriesCapabilities::new())
            .mcp(
                v2::McpCapabilities::new()
                    .stdio(v2::McpStdioCapabilities::new())
                    .http(v2::McpHttpCapabilities::new())
                    .acp(v2::McpAcpCapabilities::new()),
            )
            .prompt(
                v2::PromptCapabilities::new()
                    .image(v2::PromptImageCapabilities::new())
                    .audio(v2::PromptAudioCapabilities::new())
                    .embedded_context(v2::PromptEmbeddedContextCapabilities::new()),
            );
        let metadata =
            serde_json::from_value::<v2::Meta>(json!({"codex":self.policy.capabilities()}))?;
        Ok(v2::InitializeResponse::new(
            ProtocolVersion::V2,
            v2::Implementation::new("codex-acp-v2", env!("CARGO_PKG_VERSION")),
        )
        .capabilities(
            v2::AgentCapabilities::new()
                .session(session)
                .meta(metadata.clone()),
        )
        .meta(metadata))
    }
}

/// Narrow SDK routing adapter matching only underscore-prefixed methods.
#[derive(Clone, Debug)]
struct ExtensionRequest(v2::ExtRequest);

impl JsonRpcMessage for ExtensionRequest {
    fn matches_method(method: &str) -> bool {
        method.starts_with('_')
    }
    fn method(&self) -> &str {
        &self.0.method
    }
    fn to_untyped_message(&self) -> agent_client_protocol::Result<UntypedMessage> {
        UntypedMessage::new(self.method(), &self.0)
    }
    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> agent_client_protocol::Result<Self> {
        if !Self::matches_method(method) {
            return Err(agent_client_protocol::Error::method_not_found());
        }
        Ok(Self(v2::ExtRequest::new(
            method.to_owned(),
            Arc::from(to_raw_value(params)?),
        )))
    }
}

impl JsonRpcRequest for ExtensionRequest {
    type Response = Value;
}
