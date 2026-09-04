use std::{
    convert::Infallible,
    sync::{Arc, atomic::Ordering},
};

use agent_client_protocol::{Error, schema::v2};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::post,
};
use serde_json::{Value, json};

use super::{Endpoint, MAX_FRAME};

pub(super) async fn serve(
    listener: tokio::net::TcpListener,
    token: String,
    endpoint: Arc<Endpoint>,
) {
    let mut shutdown = endpoint.shutdown.subscribe();
    let app = Router::new()
        .route(
            &format!("/{token}"),
            post(message).get(events).delete(disconnect),
        )
        .layer(DefaultBodyLimit::max(MAX_FRAME))
        .layer(middleware::from_fn_with_state(endpoint.clone(), limit))
        .with_state(endpoint.clone());
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|closed| *closed).await;
        })
        .await;
    if result.is_err() {
        tracing::warn!("native MCP HTTP listener failed");
        endpoint.shutdown.send_replace(true);
        let _ = endpoint.disconnect().await;
    }
}

async fn limit(State(endpoint): State<Arc<Endpoint>>, request: Request, next: Next) -> Response {
    if *endpoint.shutdown.borrow() {
        return StatusCode::GONE.into_response();
    }
    if endpoint.http_initialized.load(Ordering::SeqCst)
        && request
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some(endpoint.http_session_id.as_str())
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(_permit) = endpoint.requests.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    // Bound body reads, client work, and response construction. SSE body polling
    // has its own single-stream permit and shutdown signal below.
    let timeout = endpoint.timeout;
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

async fn message(State(endpoint): State<Arc<Endpoint>>, Json(frame): Json<Value>) -> Response {
    let initialize = frame["method"] == "initialize";
    let (frames, batch) = match frame {
        Value::Array(frames) if !frames.is_empty() && frames.len() <= 32 => (frames, true),
        Value::Object(_) => (vec![frame], false),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let results =
        futures::future::join_all(frames.into_iter().map(|frame| relay(&endpoint, frame))).await;
    let mut responses = Vec::new();
    for result in results {
        match result {
            Ok(Some(response)) => responses.push(response),
            Ok(None) => {}
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    }
    if responses.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    if initialize
        && responses
            .iter()
            .all(|response| response.get("result").is_some())
    {
        endpoint.http_initialized.store(true, Ordering::SeqCst);
    }
    let mut response = if batch {
        Json(Value::Array(responses)).into_response()
    } else {
        Json(responses.remove(0)).into_response()
    };
    if let Ok(session_id) = endpoint.http_session_id.parse() {
        response.headers_mut().insert("mcp-session-id", session_id);
    }
    response
}

async fn relay(endpoint: &Endpoint, frame: Value) -> Result<Option<Value>, Error> {
    if frame["jsonrpc"] != "2.0" {
        return Err(Error::invalid_request());
    }
    let id = frame.get("id").cloned();
    if id
        .as_ref()
        .is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64()))
    {
        return Err(Error::invalid_request());
    }
    let Some(method) = frame.get("method") else {
        let id = id
            .and_then(|id| id.as_str().map(str::to_owned))
            .ok_or_else(Error::invalid_request)?;
        let reply = endpoint
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
            .ok_or_else(Error::invalid_request)?;
        let response = if let Some(error) = frame.get("error") {
            let code = error["code"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(Error::invalid_request)?;
            let message = error["message"]
                .as_str()
                .ok_or_else(Error::invalid_request)?;
            Err(Error::new(code, message).data(error.get("data").cloned()))
        } else {
            Ok(frame
                .get("result")
                .cloned()
                .ok_or_else(Error::invalid_request)?)
        };
        let _ = reply.send(response);
        return Ok(None);
    };
    let method = method.as_str().ok_or_else(Error::invalid_request)?;
    let params = match frame.get("params") {
        Some(Value::Object(params)) => Some(params.clone()),
        None | Some(Value::Null) => None,
        _ => return Err(Error::invalid_params()),
    };
    let Some(id) = id else {
        endpoint.client.send_notification(
            v2::MessageMcpNotification::new(endpoint.connection_id.clone(), method).params(params),
        )?;
        return Ok(None);
    };
    let mut shutdown = endpoint.shutdown.subscribe();
    let response = tokio::select! {
        response = tokio::time::timeout(endpoint.timeout,
            endpoint.client.send_request(v2::MessageMcpRequest::new(endpoint.connection_id.clone(), method).params(params)).block_task()) =>
            response.unwrap_or_else(|_| Err(Error::new(-32000,"native MCP request timed out"))),
        _ = shutdown.wait_for(|closed| *closed) => Err(Error::new(-32800,"native MCP connection closed")),
    };
    let response = match response {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
    };
    if serde_json::to_vec(&response)?.len() > MAX_FRAME {
        return Ok(Some(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"native MCP response exceeds 1 MiB"}}),
        ));
    }
    Ok(Some(response))
}

async fn events(State(endpoint): State<Arc<Endpoint>>, request: Request) -> Response {
    if request.method() != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Ok(permit) = endpoint.streams.clone().try_acquire_owned() else {
        return StatusCode::CONFLICT.into_response();
    };
    let shutdown = endpoint.shutdown.subscribe();
    let stream = futures::stream::unfold(
        (endpoint, shutdown, permit),
        |(endpoint, mut shutdown, permit)| async move {
            let mut incoming = endpoint.incoming.lock().await;
            let value = tokio::select! {
                value = incoming.recv() => value?,
                _ = shutdown.wait_for(|closed| *closed) => return None,
            };
            drop(incoming);
            let event = Event::default().event("message").data(value.to_string());
            Some((Ok::<_, Infallible>(event), (endpoint, shutdown, permit)))
        },
    );
    Sse::new(stream).into_response()
}

async fn disconnect(State(endpoint): State<Arc<Endpoint>>) -> StatusCode {
    endpoint.shutdown.send_replace(true);
    endpoint
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    if endpoint.disconnect().await.is_err() {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::NO_CONTENT
    }
}
