//! control-tower: the machine's board (`store`), what it holds (`board`), the owner's alerts
//! (`inbox`), the dashboard (`ui`), the stuck-worker watch (`watch`), how posts reach a lead
//! (`listen`), the herdr plugin linking itself (`register`) and its actions opening the
//! dashboard (`open`). The `tower` binary in `main.rs` is the command line over these.

pub mod board;
pub mod deliver;
pub mod inbox;
pub mod listen;
pub mod open;
pub mod register;
pub mod store;
#[cfg(test)]
mod test_exe;
pub mod ui;
pub mod watch;
