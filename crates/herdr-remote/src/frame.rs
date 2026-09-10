//! Multiplexed framing over one byte pipe (the SSH exec channel).
//!
//! Frame = varint length + 1 kind byte + payload. Kinds:
//! * `C` control — an NDJSON `RequestEnvelope` (client → runner)
//! * `R` reply   — an NDJSON `ResponseEnvelope`  (runner → client)
//! * `E` event   — an NDJSON `EventEnvelope`     (runner → client)
//!
//! Bounded buffering: oversized or malformed frames tear the connection (the
//! bridge reconnects) rather than buffering unboundedly.

use herdr_protocol::{DaemonEvent, RequestEnvelope, ResponseEnvelope};

/// Hard cap on a single frame (control replies and event batches stay far below
/// this; log replays are capped at 256 KiB by the daemon already).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Control(RequestEnvelope),
    Reply(ResponseEnvelope),
    Event(DaemonEvent),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame exceeds maximum size ({0})")]
    Oversized(usize),
    #[error("malformed frame header")]
    Malformed,
    #[error("stream closed mid-frame")]
    Eof,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Write one frame: varint length + kind + payload.
pub fn write_frame<W: std::io::Write>(w: &mut W, frame: &Frame) -> Result<(), FrameError> {
    let (kind, payload): (u8, Vec<u8>) = match frame {
        Frame::Control(env) => (b'C', serde_json::to_vec(env)?),
        Frame::Reply(env) => (b'R', serde_json::to_vec(env)?),
        Frame::Event(ev) => (b'E', serde_json::to_vec(ev)?),
    };
    if payload.len() > MAX_FRAME {
        return Err(FrameError::Oversized(payload.len()));
    }
    write_varint(w, payload.len() as u64 + 1)?;
    w.write_all(&[kind])?;
    w.write_all(&payload)?;
    w.flush()?;
    Ok(())
}

/// Incremental reader: feed bytes, get complete frames. Owns its buffer so split
/// TCP/pipe writes are handled naturally.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes and drain all complete frames.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, FrameError> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(len) = try_read_varint(&self.buf)? else {
                break; // need more header bytes
            };
            if len == 0 || len as usize > MAX_FRAME {
                return Err(FrameError::Oversized(len as usize));
            }
            let total = self.buf.len();
            // Payload includes the kind byte; frame body = len bytes.
            if total < varint_len(len) + len as usize {
                break; // need more body bytes
            }
            let header = varint_len(len);
            let mut body: Vec<u8> = self.buf.drain(..header + len as usize).collect();
            body.drain(..header);
            let kind = body.remove(0);
            let frame = match kind {
                b'C' => Frame::Control(serde_json::from_slice(&body)?),
                b'R' => Frame::Reply(serde_json::from_slice(&body)?),
                b'E' => Frame::Event(serde_json::from_slice(&body)?),
                _ => return Err(FrameError::Malformed),
            };
            out.push(frame);
        }
        Ok(out)
    }
}

fn write_varint<W: std::io::Write>(w: &mut W, mut v: u64) -> std::io::Result<()> {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            w.write_all(&[byte])?;
            return Ok(());
        }
        w.write_all(&[byte | 0x80])?;
    }
}

fn varint_len(v: u64) -> usize {
    (64 - v.max(1).leading_zeros() as usize).div_ceil(7)
}

/// Read one varint from the buffer head; `None` if incomplete.
fn try_read_varint(buf: &[u8]) -> Result<Option<u64>, FrameError> {
    let mut value = 0u64;
    let mut shift = 0;
    for &b in buf {
        value |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(Some(value));
        }
        shift += 7;
        if shift > 63 {
            return Err(FrameError::Malformed);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::{AgentState, Response};

    fn roundtrip(frames: &[Frame]) {
        let mut buf = Vec::new();
        for f in frames {
            write_frame(&mut buf, f).unwrap();
        }
        let mut reader = FrameReader::new();
        let parsed = reader.feed(&buf).unwrap();
        assert_eq!(&parsed, frames);
    }

    #[test]
    fn frames_roundtrip_all_kinds() {
        roundtrip(&[
            Frame::Control(RequestEnvelope {
                id: 1,
                cmd: herdr_protocol::ClientCommand::Ping,
            }),
            Frame::Reply(ResponseEnvelope {
                id: 1,
                resp: Response::Pong {
                    version: "0.1.0".into(),
                    uptime_ms: 5,
                    agents: 0,
                },
            }),
            Frame::Event(DaemonEvent::StateChange {
                agent_id: "abc".into(),
                state: AgentState::Blocked,
            }),
        ]);
    }

    #[test]
    fn split_feeds_assemble_frames() {
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            &Frame::Event(DaemonEvent::AgentOutput {
                agent_id: "x".into(),
                payload: "hello".into(),
            }),
        )
        .unwrap();

        let mut reader = FrameReader::new();
        // Feed one byte at a time.
        let mut all = Vec::new();
        for b in &buf {
            all.extend(reader.feed(&[*b]).unwrap());
        }
        assert_eq!(all.len(), 1);
        assert!(matches!(
            &all[0],
            Frame::Event(DaemonEvent::AgentOutput { payload, .. }) if payload == "hello"
        ));
    }

    #[test]
    fn oversized_frame_is_rejected_not_buffered() {
        let mut reader = FrameReader::new();
        let mut buf = Vec::new();
        write_varint(&mut buf, (MAX_FRAME as u64) + 2).unwrap();
        buf.push(b'E');
        assert!(matches!(reader.feed(&buf), Err(FrameError::Oversized(_))));
    }

    #[test]
    fn multiple_frames_in_one_feed() {
        let mut buf = Vec::new();
        for i in 0..5 {
            write_frame(
                &mut buf,
                &Frame::Reply(ResponseEnvelope {
                    id: i,
                    resp: Response::Ok,
                }),
            )
            .unwrap();
        }
        let mut reader = FrameReader::new();
        assert_eq!(reader.feed(&buf).unwrap().len(), 5);
    }

    #[test]
    fn unknown_kind_is_malformed() {
        let mut reader = FrameReader::new();
        let mut buf = Vec::new();
        write_varint(&mut buf, 2).unwrap();
        buf.push(b'Z');
        buf.push(b'{');
        buf.push(b'}');
        assert!(matches!(reader.feed(&buf), Err(FrameError::Malformed)));
    }
}
