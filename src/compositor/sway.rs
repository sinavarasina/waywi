pub mod client;
pub mod stream;
pub mod types;

pub use client::{SwayClient, encode_frame};
pub(crate) use stream::SwayListenerHandle;
pub use stream::parse_event_frame;
pub use types::{Sway, SwayEvent, SwayOutput, SwayVersion, SwayWorkspace, WorkspaceRef};
