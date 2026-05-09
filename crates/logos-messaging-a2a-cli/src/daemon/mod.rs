//! IPC layer between short-lived `lmao` CLI commands and a long-running
//! `lmao agent run` process.
//!
//! Today, every CLI invocation (`task send`, `presence peers`, `info`, …)
//! spins up its own embedded Logos Messaging node, dials the gossip mesh
//! from cold (~5 s), runs one operation, and exits. That's wasteful and
//! awkward for any orchestration on top of LMAO.
//!
//! When `lmao agent run` is up, it now also binds a Unix domain socket
//! (default `~/.cache/lmao/lmao.sock`, override with `--daemon-socket`).
//! Other commands check that socket first: if a daemon is listening,
//! they send a [`Request`] and wait for a [`Response`] over the socket
//! using the daemon's already-connected node and storage backend; if
//! not, they fall back to the original spin-up-ephemeral-node path.
//!
//! Wire format: `u32` little-endian length prefix, then a UTF-8 JSON
//! [`Request`] / [`Response`]. No streaming yet — every command is one
//! request, one response. Streaming surfaces (`task stream`) currently
//! buffer all chunks server-side and return them in a single response.

pub mod client;
mod frame;
pub mod protocol;
pub mod server;

pub use client::DaemonClient;
pub use protocol::{default_socket_path, Request, Response};
pub use server::DaemonServer;
