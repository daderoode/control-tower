//! Test only: an executable written so that Linux runs it. Both the lib's tests and
//! the integration tests use this one file (`tests/cli.rs` and `tests/herdr.rs` include it by
//! path).
//!
//! On Linux, running a file fails with "Text file busy" (ETXTBSY) while any process holds
//! it open for writing. A file a test writes itself is held open so, for a moment, by every
//! child another test thread forks in that moment, until that child's own exec; then
//! whoever runs the file (the test, or a tower it started) can be refused. macOS never
//! refuses. Here a child process (`/bin/sh`) writes the file: this process never holds it
//! open for writing, so nothing it forks can, and once that child has exited nothing does.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Makes `path` an executable (mode 755) holding `contents`: a script, or a binary's bytes
/// to copy it.
pub fn executable(path: &Path, contents: impl AsRef<[u8]>) {
    let mut sh = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("cannot start /bin/sh to write an executable");
    let mut stdin = sh.stdin.take().expect("its stdin is piped");
    stdin.write_all(contents.as_ref()).unwrap();
    drop(stdin);
    let status = sh.wait().unwrap();
    assert!(
        status.success(),
        "cannot write {}: {status}",
        path.display()
    );
}
