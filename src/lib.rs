//! control-tower: the machine's board (`store`), what it holds (`board`), the owner's alerts
//! (`inbox`), the dashboard (`ui`), the stuck-worker watch (`watch`), how posts reach a lead
//! (`listen`), the one interface to the terminal (`terminal`) and the lead skill registering
//! itself with Claude Code (`register`). The `tower` binary in `main.rs` is the command line
//! over these.

pub mod board;
pub mod deliver;
pub mod inbox;
pub mod listen;
pub mod register;
pub mod store;
pub mod terminal;
#[cfg(test)]
mod test_exe;
pub mod ui;
pub mod watch;
