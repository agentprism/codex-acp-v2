//! ACP stdio framing with hard byte limits before JSON deserialization.

use std::io;
use std::time::Duration;

use agent_client_protocol::{ConnectTo, Lines, Role};
use futures::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use futures::{sink, stream};

/// A capped replacement for the SDK's otherwise unbounded stdio line reader.
///
/// Limits apply to both incoming and outgoing JSON lines, excluding their newline.
/// Oversized or invalid UTF-8 input closes the connection explicitly. Blocking I/O
/// runs outside Tokio's blocking pool so an open stdin cannot stall runtime exit.
#[derive(Debug)]
pub struct BoundedStdio {
    max_frame_bytes: usize,
}

impl BoundedStdio {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self { max_frame_bytes }
    }
}

impl<Counterpart: Role> ConnectTo<Counterpart> for BoundedStdio {
    async fn connect_to(
        self,
        peer: impl ConnectTo<Counterpart::Counterpart>,
    ) -> Result<(), agent_client_protocol::Error> {
        if self.max_frame_bytes == 0 {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("ACP max_frame_bytes must be positive"));
        }
        let limit = self.max_frame_bytes;
        let reader = BufReader::new(blocking::Unblock::new(std::io::stdin()));
        let incoming = stream::try_unfold(reader, move |mut reader| async move {
            bounded_line(&mut reader, limit)
                .await
                .map(|line| line.map(|line| (line, reader)))
        });
        let writer = blocking::Unblock::new(std::io::stdout());
        let outgoing = sink::unfold(writer, move |mut writer, line: String| async move {
            if line.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ACP outbound frame exceeds max_frame_bytes",
                ));
            }
            tokio::time::timeout(Duration::from_secs(30), async {
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ACP stdout write timed out"))??;
            Ok::<_, io::Error>(writer)
        });
        ConnectTo::<Counterpart>::connect_to(Lines::new(outgoing, incoming), peer).await
    }
}

async fn bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_count = newline.unwrap_or(available.len());
        if bytes.len().saturating_add(content_count) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACP inbound frame exceeds max_frame_bytes",
            ));
        }
        bytes.extend_from_slice(&available[..content_count]);
        let consumed = content_count + usize::from(newline.is_some());
        reader.consume_unpin(consumed);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACP input is not UTF-8"))
}
