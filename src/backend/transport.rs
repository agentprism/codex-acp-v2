use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::*;

pub(super) async fn spawn(
    mut options: BackendOptions,
) -> Result<(Backend, mpsc::Receiver<BackendEvent>), BackendError> {
    if options.event_capacity < 2
        || options.outbound_capacity == 0
        || options.max_in_flight == 0
        || options.max_frame_bytes == 0
        || options.request_timeout.is_zero()
    {
        return Err(BackendError::Configuration("capacities, frame size, and timeout must be positive; event_capacity must be at least 2".into()));
    }
    let capabilities = options
        .capabilities
        .as_object_mut()
        .ok_or_else(|| BackendError::Configuration("capabilities must be an object".into()))?;
    capabilities.insert("experimentalApi".into(), Value::Bool(true));
    let mut child = Command::new(&options.executable)
        .args(&options.args)
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            BackendError::Io(format!(
                "could not spawn {}: {error}",
                options.executable.display()
            ))
        })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| BackendError::Io("missing child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BackendError::Io("missing child stdout".into()))?;
    let (outgoing, outgoing_rx) = mpsc::channel(options.outbound_capacity);
    let (events, events_rx) = mpsc::channel(options.event_capacity);
    let terminal = events
        .clone()
        .reserve_owned()
        .await
        .map_err(|_| BackendError::Io("could not reserve terminal event".into()))?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (finished_tx, finished) = watch::channel(false);
    let state = Arc::new(Mutex::new(State::default()));
    let writer = tokio::spawn(write_frames(stdin, outgoing_rx));
    let reader = tokio::spawn(read_frames(
        stdout,
        events,
        Arc::clone(&state),
        options.max_frame_bytes,
    ));
    tokio::spawn(supervise(
        child,
        reader,
        writer,
        shutdown_rx,
        finished_tx,
        terminal,
        Arc::clone(&state),
    ));
    let backend = Backend(Arc::new(Inner {
        outgoing,
        state,
        next_id: AtomicU64::new(1),
        shutdown,
        finished,
        request_timeout: options.request_timeout,
        max_frame_bytes: options.max_frame_bytes,
        max_in_flight: options.max_in_flight,
    }));
    let initialized = backend.request("initialize", json!({
        "clientInfo": {"name": "codex_acp_v2", "title": "Codex ACP v2", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": options.capabilities,
    })).await;
    if let Err(error) = initialized {
        let _ = backend.shutdown().await;
        return Err(error);
    }
    backend.notify("initialized", json!({})).await?;
    Ok((backend, events_rx))
}

async fn write_frames(
    mut stdin: ChildStdin,
    mut outgoing: mpsc::Receiver<Vec<u8>>,
) -> Result<(), BackendError> {
    while let Some(frame) = outgoing.recv().await {
        stdin
            .write_all(&frame)
            .await
            .map_err(|error| BackendError::Io(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| BackendError::Io(error.to_string()))?;
    }
    Ok(())
}

async fn read_frames(
    stdout: ChildStdout,
    events: mpsc::Sender<BackendEvent>,
    state: Arc<Mutex<State>>,
    max_frame_bytes: usize,
) -> Result<(), BackendError> {
    let mut reader = BufReader::new(stdout);
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| BackendError::Io(error.to_string()))?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(())
            } else {
                Err(BackendError::Protocol("EOF inside a JSON line".into()))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(count) > max_frame_bytes.saturating_add(1) {
            return Err(BackendError::Limit(
                "inbound frame exceeds max_frame_bytes".into(),
            ));
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_none() {
            continue;
        }
        let value: Value = serde_json::from_slice(&frame)
            .map_err(|_| BackendError::Protocol("invalid JSON".into()))?;
        frame.clear();
        dispatch(value, &events, &state)?;
    }
}

fn dispatch(
    value: Value,
    events: &mpsc::Sender<BackendEvent>,
    state: &Mutex<State>,
) -> Result<(), BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| BackendError::Protocol("expected a JSON object".into()))?;
    if let Some(method) = object.get("method") {
        let method = method
            .as_str()
            .ok_or_else(|| BackendError::Protocol("method must be a string".into()))?
            .to_owned();
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let event = if let Some(id) = object.get("id") {
            if !valid_id(id) {
                return Err(BackendError::Protocol(
                    "request IDs must be strings or integers".into(),
                ));
            }
            BackendEvent::ServerRequest {
                id: id.clone(),
                method,
                params,
            }
        } else {
            BackendEvent::Notification { method, params }
        };
        return events.try_send(event).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => BackendError::Limit(
                "backend event queue overflow; connection terminated to prevent silent event loss"
                    .into(),
            ),
            mpsc::error::TrySendError::Closed(_) => {
                BackendError::Disconnected("event receiver closed".into())
            }
        });
    }
    let id = object.get("id").and_then(Value::as_u64).ok_or_else(|| {
        BackendError::Protocol("response has no matching numeric request ID".into())
    })?;
    let result = match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(BackendError::Rpc(
            serde_json::from_value(error.clone())
                .map_err(|_| BackendError::Protocol("malformed RPC error".into()))?,
        )),
        _ => {
            return Err(BackendError::Protocol(
                "response must contain exactly one result or error".into(),
            ));
        }
    };
    // A request future may have been cancelled or timed out before its reply arrived.
    if let Some(reply) = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending
        .remove(&id)
    {
        let _ = reply.send(result);
    }
    Ok(())
}

async fn supervise(
    mut child: Child,
    mut reader: JoinHandle<Result<(), BackendError>>,
    mut writer: JoinHandle<Result<(), BackendError>>,
    mut shutdown: watch::Receiver<bool>,
    finished: watch::Sender<bool>,
    terminal: mpsc::OwnedPermit<BackendEvent>,
    state: Arc<Mutex<State>>,
) {
    let reason = tokio::select! {
        _ = shutdown.changed() => "backend shut down".to_owned(),
        result = &mut reader => task_reason("stdout", result),
        result = &mut writer => task_reason("stdin", result),
        result = child.wait() => {
            // A peer may exit immediately after its last reply. Drain buffered stdout
            // before failing pending requests, but do not wait on inherited pipes forever.
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut reader).await;
            match result {
                Ok(status) => format!("Codex process exited with {status}"),
                Err(error) => format!("could not wait for Codex: {error}"),
            }
        },
    };
    {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disconnected = Some(reason.clone());
        for (_, reply) in state.pending.drain() {
            let _ = reply.send(Err(BackendError::Disconnected(reason.clone())));
        }
    }
    reader.abort();
    writer.abort(); // Drops stdin so Codex can gracefully terminate.
    terminal.send(BackendEvent::Disconnected { message: reason });
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    let _ = finished.send(true);
}

fn task_reason(
    stream: &str,
    result: Result<Result<(), BackendError>, tokio::task::JoinError>,
) -> String {
    match result {
        Ok(Ok(())) => format!("Codex {stream} closed"),
        Ok(Err(error)) => error.to_string(),
        Err(error) => format!("Codex {stream} task stopped: {error}"),
    }
}
