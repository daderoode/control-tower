//! What is left of herdr until #114 removes it: where its executable is (`path`), a
//! bounded run of it (`run`, for its plugin's self-linking and `tower space` and `tower
//! popup`), and a command as one shell line (`shell_line`). It is no terminal module: the
//! workers, delivery and `n` are Rex's.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::terminal_timeout;
use crate::ui::runner::{Outcome, Run, run_now};
use crate::ui::writer::sh_quote;

/// The herdr executable: `TOWER_HERDR` if set, else `herdr` on the PATH.
pub fn path() -> PathBuf {
    std::env::var_os("TOWER_HERDR")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from("herdr"), PathBuf::from)
}

/// What a failed run said: its stderr, else its stdout.
fn said(stdout: String, stderr: String) -> String {
    if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    }
}

/// How a run that exited non-zero ended, for its message.
fn how(code: Option<i32>) -> String {
    code.map_or("was ended by a signal".into(), |c| format!("exited {c}"))
}

/// Runs `herdr <args>` from `exe`, bounded by `terminal_timeout`, and returns its stdout,
/// or what went wrong.
pub fn run(exe: &Path, db: &Path, args: &[&str]) -> Result<String> {
    // Named by its command alone: a prompt's text is in the action's own log line.
    let command = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    let outcome = run_now(&Run {
        exe: exe.to_path_buf(),
        args: args.iter().map(|a| a.to_string()).collect(),
        db: db.to_path_buf(),
        env: Vec::new(),
        stdin: None,
        timeout: terminal_timeout(),
    });
    match outcome {
        Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(stdout),
        Outcome::Exited {
            code,
            stderr,
            stdout,
        } => bail!(
            "herdr {command} {}: {}",
            how(code),
            said(stdout, stderr).trim()
        ),
        Outcome::TimedOut(after) => bail!(
            "herdr {command} did not finish in {} s and was stopped",
            after.as_secs()
        ),
        Outcome::Failed(why) => bail!("{why}"),
    }
}

/// `words` as one line a POSIX shell reads back as those words: a word of plain characters
/// as it is, any other in single quotes (`sh_quote`).
pub fn shell_line(words: &[String]) -> String {
    let plain = |w: &str| {
        !w.is_empty() && (w.chars()).all(|c| c.is_ascii_alphanumeric() || "-_./:@%+,".contains(c))
    };
    (words.iter())
        .map(|w| if plain(w) { w.clone() } else { sh_quote(w) })
        .collect::<Vec<_>>()
        .join(" ")
}
