//! `MasterPty` / `Child` adapters wrapping the raw Win32 handles conhost
//! hands us during the termhost handoff.

mod child;
mod io;
mod master;

pub use child::TermHostChild;
pub use io::create_anon_pipe;
pub use master::RawHandlesMasterPty;
