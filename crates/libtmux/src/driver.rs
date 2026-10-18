//! Runtime and process adapters used by the shared client.

mod async_io_runtime;
mod process;
pub mod runtime;

pub use async_io_runtime::AsyncIoRuntime;
pub use process::{Process, Reader, Writer};
pub use runtime::{AbortHandle, Runtime, RuntimeExt, TaskError, TaskHandle};

pub(crate) use process::StdProcess;
