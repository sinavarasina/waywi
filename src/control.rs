//! External control IPC domain: POSIX signals and single-instance local Unix sockets.

pub mod socket;
pub mod types;

pub(crate) use socket::ControlSocketRegistration;
pub use socket::{ControlRequest, resolve_socket_path, send_command, send_command_with_reply};
pub use types::{Control, Signal};
