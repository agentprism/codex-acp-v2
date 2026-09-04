use std::{convert::Infallible, sync::Arc};

use agent_client_protocol::{Error, schema::v2};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::post,
};
use serde_json::{Value, json};

use super::{Endpoint, MAX_FRAME, listener::Listener};

pub(super) async fn serve(
    listener: tokio::net::TcpListener,
    token: String,
    listener_state: Arc<Listener>,
) {
    let mut shutdown = listener_state.shutdown.subscribe();
    let app = Router::new()
        .route(
            &format!("/{token}"),
            post(message).get(events).delete(disconnect),
        )
        .layer(DefaultBodyLimit::max(MAX_FRAME))
        .layer(middleware::from_fn_with_state(
            listener_state.clone(),
            limit,
        ))
        .with_state(listener_state.clone());
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|closed| *closed).await;
        })
        .await;
    if result.is_err() {
        tracing::warn!("native MCP HTTP listener failed");
        let _ = listener_state.close().await;
    }
}

async fn limit(State(listener): State<Arc<Listener>>, request: Request, next: Next) -> Response {
    if *listener.shutdown.borrow() {
        return StatusCode::GONE.into_response();
    }
    let Ok(_permit) = listener.requests.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    // Bound body reads, client work, and response construction. SSE body polling
    // has its own single-stream permit and shutdown signal below.
    let timeout = listener.manager.timeout;
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

async fn message(
    State(listener): State<Arc<Listener>>,
    headers: HeaderMap,
    Json(frame): Json<Value>,
) -> Response {
    let endpoint = if let Some(session_id) = session_id(&headers) {
        let Some(endpoint) = listener.endpoint(session_id).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if frame["method"] == "initialize" || *endpoint.shutdown.borrow() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        endpoint
    } else {
        if frame["jsonrpc"] != "2.0"
            || frame["method"] != "initialize"
            || !frame
                .get("id")
                .is_some_and(|id| id.is_string() || id.is_i64() || id.is_u64())
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let lease = match listener.connect().await {
            Ok(lease) => lease,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
        let endpoint = match lease.endpoint() {
            Ok(endpoint) => endpoint,
            Err(_) => return StatusCode::GONE.into_response(),
        };
        let mut shutdown = listener.shutdown.subscribe();
        let result = tokio::select! {
            result = relay(&endpoint, frame) => result,
            _ = shutdown.wait_for(|closed| *closed) => return StatusCode::GONE.into_response(),
        };
        let response = match result {
            Ok(Some(response)) => response,
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        if response.get("result").is_none() {
            let _ = lease.close().await;
            return Json(response).into_response();
        }
        if listener.register(lease).await.is_err() {
            return StatusCode::GONE.into_response();
        }
        let mut response = Json(response).into_response();
        if let Ok(session_id) = endpoint.http_session_id.parse() {
            response.headers_mut().insert("mcp-session-id", session_id);
        }
        return response;
    };
    let (frames, batch) = match frame {
        Value::Array(frames) if !frames.is_empty() && frames.len() <= 32 => (frames, true),
        Value::Object(_) => (vec![frame], false),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    if frames.iter().any(|frame| frame["method"] == "initialize") {
        return StatusCode::BAD_REQUEST.into_response();
    }
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
    let payload = if batch {
        Value::Array(responses)
    } else {
        responses.remove(0)
    };
    if !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_FRAME) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut response = Json(payload).into_response();
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

async fn events(State(listener): State<Arc<Listener>>, headers: HeaderMap) -> Response {
    let Some(id) = session_id(&headers) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(endpoint) = listener.endpoint(id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if *endpoint.shutdown.borrow() {
        return StatusCode::GONE.into_response();
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

async fn disconnect(State(listener): State<Arc<Listener>>, headers: HeaderMap) -> StatusCode {
    let Some(id) = session_id(&headers) else {
        return StatusCode::BAD_REQUEST;
    };
    if listener.endpoint(id).await.is_none() {
        return StatusCode::NOT_FOUND;
    }
    if listener.disconnect(id).await.is_err() {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::NO_CONTENT
    }
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
}
