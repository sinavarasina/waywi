//! External control IPC domain: POSIX signals and single-instance local Unix sockets.

pub mod socket;
pub mod types;

pub(crate) use socket::{accept_commands, bind_control_listener};
pub use socket::{resolve_socket_path, send_command};
pub use types::{Control, Signal};
