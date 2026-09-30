//! fs_terminal — 终端会话状态机与字节管道（spec §2.2）。
pub mod decode;
pub mod error;
pub mod flow;
pub mod grid;
pub mod history;
pub mod pipe;
pub mod record;
pub mod ring;
pub mod sessionlog;
pub mod zmodem;
pub mod zsession;
pub use error::Error;
pub use pipe::{PipeOpts, SessionPipe};
