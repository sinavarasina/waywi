pub mod client;
pub mod stream;
pub mod types;

pub use client::SwayClient;
pub(crate) use stream::SwayListenerHandle;
pub use types::{Sway, SwayEvent, SwayOutput, SwayVersion, SwayWorkspace};
