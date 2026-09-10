//! # herdr-protocol
//!
//! The single source of truth for the wire contract between the herdr daemon and every
//! client (CLI, TUI, desktop). Depends only on `serde`/`serde_json` so any client can
//! compile against it without pulling in the daemon's runtime.
//!
//! Framing (see `specs/01-architecture.md` §5):
//! * Requests: `{"id": u64, "cmd": ClientCommand}` — one per line.
//! * Replies:  `{"id": u64, "resp": Response}`   — exactly one per request.
//! * Events:   `{"event": DaemonEvent}`          — asynchronous, interleaved.
//! * One JSON value per `\n`, max [`MAX_LINE_BYTES`].

pub mod codec;
pub mod profile;
pub mod socket;
pub mod types;

pub use codec::{
    Decoder, Encoder, EventEnvelope, RequestEnvelope, ResponseEnvelope, MAX_LINE_BYTES,
};
pub use profile::AgentProfile;
pub use socket::default_socket_path;
pub use types::{AgentId, AgentInfo, AgentState, ClientCommand, DaemonEvent, RemoteHost, Response};

/// Version reported by `Ping`/`Pong`.
pub const PROTOCOL_VERSION: &str = env!("CARGO_PKG_VERSION");
