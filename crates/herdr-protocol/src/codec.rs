//! NDJSON line codec for the herdr wire protocol.
//!
//! Three envelope shapes share one socket:
//! * [`RequestEnvelope`]  — client → daemon: `{"id":..,"cmd":{..}}`
//! * [`ResponseEnvelope`] — daemon → client, tied to a request: `{"id":..,"resp":{..}}`
//! * [`EventEnvelope`]    — daemon → client, async: `{"event":{..}}`

use serde::{Deserialize, Serialize};

use crate::types::{ClientCommand, DaemonEvent, Response};

/// Maximum size of a single protocol line (1 MiB). Larger lines are rejected.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub cmd: ClientCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub id: u64,
    pub resp: Response,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    #[serde(rename = "event")]
    pub event: DaemonEvent,
}

/// Failures produced while feeding lines into [`Decoder`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Input contained a NUL byte — never valid in this protocol.
    #[error("NUL byte in input")]
    NulByte,
    /// Line exceeded [`MAX_LINE_BYTES`].
    #[error("line exceeds {MAX_LINE_BYTES} byte limit")]
    LineTooLong,
    /// Line was not valid JSON or not a valid request envelope.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl DecodeError {
    /// What the daemon should send back to the client for this error.
    pub fn to_response(&self) -> Response {
        Response::Err(self.to_string())
    }
}

/// Incremental NDJSON decoder: push raw bytes, pull complete request lines.
///
/// Tolerates `\r\n`. Rejects oversized lines without aborting the connection
/// (the daemon replies with a single `Err` and keeps reading).
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes from the socket; returns one result per complete line, in order.
    /// Malformed lines yield `Err` for that line only — the connection stays usable.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<RequestEnvelope, DecodeError>> {
        for &b in bytes {
            if b == 0 {
                self.buf.clear();
                return vec![Err(DecodeError::NulByte)];
            }
            self.buf.push(b);
        }
        if self.buf.len() > MAX_LINE_BYTES {
            // Drop the buffer but keep the connection usable.
            self.buf.clear();
            return vec![Err(DecodeError::LineTooLong)];
        }

        let mut out = Vec::new();
        let mut start = 0usize;
        let data = &self.buf[..];
        let mut consumed = 0usize;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                let mut line = &data[start..i];
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                if !line.is_empty() {
                    out.push(Self::parse_request(line));
                }
                start = i + 1;
                consumed = start;
            }
        }
        self.buf.drain(..consumed);
        out
    }

    fn parse_request(line: &[u8]) -> Result<RequestEnvelope, DecodeError> {
        let s = std::str::from_utf8(line)
            .map_err(|e| DecodeError::InvalidRequest(format!("invalid utf-8: {e}")))?;
        serde_json::from_str(s).map_err(|e| DecodeError::InvalidRequest(e.to_string()))
    }

    /// Number of bytes currently buffered awaiting a newline.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Encoding helpers.
#[derive(Debug, Default, Clone, Copy)]
pub struct Encoder;

impl Encoder {
    /// Serialize a request envelope to one NDJSON line (with trailing newline).
    pub fn encode_request(env: &RequestEnvelope) -> String {
        let mut s = serde_json::to_string(env).expect("request serialize is infallible");
        s.push('\n');
        s
    }

    /// Serialize a response envelope to one NDJSON line (with trailing newline).
    pub fn encode_response(env: &ResponseEnvelope) -> String {
        let mut s = serde_json::to_string(env).expect("response serialize is infallible");
        s.push('\n');
        s
    }

    /// Serialize an event envelope to one NDJSON line (with trailing newline).
    pub fn encode_event(env: &EventEnvelope) -> String {
        let mut s = serde_json::to_string(env).expect("event serialize is infallible");
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentState, ClientCommand, Response};

    #[test]
    fn request_roundtrip_across_chunks() {
        let env = RequestEnvelope {
            id: 7,
            cmd: ClientCommand::SendInput {
                agent_id: "abc".into(),
                text: "line with \n newline? no — JSON escapes it".into(),
                raw: true,
            },
        };
        let line = Encoder::encode_request(&env);
        assert!(line.ends_with('\n'));

        let mut dec = Decoder::new();
        // Split into awkward chunks to prove incremental decoding.
        let bytes = line.as_bytes();
        let mut got = Vec::new();
        for chunk in bytes.chunks(7) {
            got.extend(dec.push(chunk).into_iter().flatten());
        }
        assert_eq!(got, vec![env]);
        assert_eq!(dec.pending(), 0);
    }

    #[test]
    fn multiple_lines_in_one_push() {
        let mut dec = Decoder::new();
        let l1 = Encoder::encode_request(&RequestEnvelope {
            id: 1,
            cmd: ClientCommand::Ping,
        });
        let l2 = Encoder::encode_request(&RequestEnvelope {
            id: 2,
            cmd: ClientCommand::List,
        });
        let got: Vec<_> = dec
            .push(format!("{l1}{l2}").as_bytes())
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, 1);
        assert_eq!(got[1].id, 2);
    }

    #[test]
    fn crlf_and_blank_lines_tolerated() {
        let mut dec = Decoder::new();
        let got: Vec<_> = dec
            .push(b"\r\n{\"id\":3,\"cmd\":\"Ping\"}\r\n\n")
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(
            got,
            vec![RequestEnvelope {
                id: 3,
                cmd: ClientCommand::Ping
            }]
        );
    }

    #[test]
    fn partial_line_held_back() {
        let mut dec = Decoder::new();
        assert!(dec.push(b"{\"id\":4,").is_empty());
        assert_eq!(dec.pending(), 8);
        let got = dec.push(b"\"cmd\":\"Ping\"}\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap().cmd, ClientCommand::Ping);
    }

    #[test]
    fn invalid_json_yields_error_not_panic() {
        let mut dec = Decoder::new();
        let results = dec.push(b"not json\n");
        assert!(matches!(results[0], Err(DecodeError::InvalidRequest(_))));
        // Decoder stays usable afterwards.
        let got: Vec<_> = dec
            .push(b"{\"id\":5,\"cmd\":\"Ping\"}\n")
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn wrong_shape_rejected() {
        let mut dec = Decoder::new();
        let results = dec.push(b"{\"nope\":1}\n");
        assert!(matches!(results[0], Err(DecodeError::InvalidRequest(_))));
    }

    #[test]
    fn nul_byte_rejected() {
        let mut dec = Decoder::new();
        let results = dec.push(b"ab\0c\n");
        assert_eq!(results[0], Err(DecodeError::NulByte));
    }

    #[test]
    fn oversized_line_rejected_but_recoverable() {
        let mut dec = Decoder::new();
        let big = "x".repeat(MAX_LINE_BYTES + 1);
        let results = dec.push(big.as_bytes());
        assert_eq!(results[0], Err(DecodeError::LineTooLong));
        assert_eq!(dec.pending(), 0);
        let got: Vec<_> = dec
            .push(b"{\"id\":6,\"cmd\":\"Ping\"}\n")
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn good_line_after_bad_line_still_parsed() {
        let mut dec = Decoder::new();
        let mut results = dec.push(b"garbage\n{\"id\":8,\"cmd\":\"Ping\"}\n");
        assert!(results[0].is_err());
        let good = results.remove(1).unwrap();
        assert_eq!(good.id, 8);
    }

    #[test]
    fn envelope_encoding_shapes() {
        let resp = Encoder::encode_response(&ResponseEnvelope {
            id: 9,
            resp: Response::Err("bad".into()),
        });
        assert!(resp.contains("\"id\":9"));
        assert!(resp.contains("\"resp\":{\"Err\":\"bad\"}"));

        let ev = Encoder::encode_event(&EventEnvelope {
            event: DaemonEvent::StateChange {
                agent_id: "a".into(),
                state: AgentState::Idle,
            },
        });
        assert!(ev.starts_with("{\"event\":"));
    }
}
