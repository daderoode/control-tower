//! The Rex module: the terminal module interface over the `rex` command line (`path`), on
//! the local Rex server only. A lead starts in a new Rex session labelled with its name
//! (`rex new`), which Rex does not show in the app, so focus stays where it is; the session
//! runs the lead's command directly, each argument its own word, never through a shell
//! string. The handle is the session's id, and undo kills it (`rex kill`).
//!
//! For now `n` is all this module does: the watch's look (`agents`) and delivery (`prompt`)
//! still use herdr until #110 and #111 build them here.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Agent, Handle, LeadStart, Sent, Terminal, Unread};
use crate::ui::runner::{Outcome, Run, run_now};
use crate::watch::{find_exe, not_found, terminal_timeout};

/// The rex executable as this process's environment names it (`path_in`).
pub fn path() -> PathBuf {
    path_in(|name| std::env::var_os(name))
}

/// The rex executable as `env` names it: `TOWER_REX` if set; else `rex` in `REX_BIN_DIR`,
/// where Rex keeps it and which Rex sets in its own tabs, when one is there; else `rex` on
/// the PATH (Rex does not put it there).
pub fn path_in(env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(p) = env("TOWER_REX").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let in_bin_dir = env("REX_BIN_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("rex"))
        .filter(|p| p.is_absolute() && find_exe(p, None).is_some());
    in_bin_dir.unwrap_or_else(|| PathBuf::from("rex"))
}

/// Rex as the terminal module, run from `exe`.
pub struct Rex {
    exe: PathBuf,
}

impl Rex {
    pub fn new(exe: PathBuf) -> Rex {
        Rex { exe }
    }

    /// Runs `rex <args>`, bounded by `terminal_timeout`.
    fn run(&self, db: &Path, args: &[&str]) -> Outcome {
        run_now(&Run {
            exe: self.exe.clone(),
            args: args.iter().map(|a| a.to_string()).collect(),
            db: db.to_path_buf(),
            stdin: None,
            timeout: terminal_timeout(),
        })
    }
}

/// What `rex <command>` ending in `outcome` gave: its stdout when it exited 0, else what
/// went wrong.
fn stdout(command: &str, outcome: Outcome) -> Result<String> {
    match outcome {
        Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } => Ok(stdout),
        Outcome::Exited {
            code,
            stdout,
            stderr,
        } => {
            let how = code.map_or("was ended by a signal".into(), |c| format!("exited {c}"));
            let said = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            bail!("rex {command} {how}: {}", said.trim())
        }
        Outcome::TimedOut(after) => bail!(
            "rex {command} did not finish in {} s and was stopped",
            after.as_secs()
        ),
        Outcome::Failed(why) => bail!("{why}"),
    }
}

#[derive(Deserialize)]
struct Created {
    session_id: String,
}

/// Not built yet: what the watch's look and delivery are told until #110 and #111.
const NOT_YET: &str = "the Rex module does not list or reach workers in this build";

impl Terminal for Rex {
    fn name(&self) -> &'static str {
        "Rex"
    }

    fn exe(&self) -> &Path {
        &self.exe
    }

    fn missing(&self, path: Option<&OsStr>) -> Option<String> {
        match find_exe(&self.exe, path) {
            Some(_) => None,
            None => Some(format!(
                "{}; Rex keeps it in REX_BIN_DIR, which Rex sets in its own tabs; set TOWER_REX \
                 to its full path if it is elsewhere",
                not_found("rex", &self.exe, path)
            )),
        }
    }

    fn agents(&self, _db: &Path, _timeout: Duration) -> Result<Vec<Agent>, Unread> {
        Err(Unread::Failed(NOT_YET.into()))
    }

    fn prompt(&self, _db: &Path, _name: &str, _text: &str, _timeout: Duration) -> Sent {
        Sent::Failed(NOT_YET.into())
    }

    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle> {
        let env = format!("TOWER_DB={}", lead.board);
        let mut args = vec![
            "new", lead.name, "-c", lead.dir, "--json", "--", "env", &env,
        ];
        args.extend(lead.command.iter().map(String::as_str));
        // Rex may have made the session before it stopped, or before a reply this cannot
        // read; with no id it cannot be killed here.
        let left = || {
            format!(
                "a Rex session labelled {:?} may be left: rex ls, then rex kill its id",
                lead.name
            )
        };
        let outcome = self.run(db, &args);
        let timed_out = matches!(outcome, Outcome::TimedOut(_));
        let reply =
            stdout("new", outcome).map_err(|e| if timed_out { e.context(left()) } else { e })?;
        let created: Created = serde_json::from_str(&reply)
            .with_context(|| format!("rex new said something unexpected: {}", reply.trim()))
            .with_context(left)?;
        Ok(Handle(created.session_id))
    }

    fn undo(&self, db: &Path, handle: &Handle) -> Result<String> {
        stdout("kill", self.run(db, &["kill", &handle.0]))?;
        Ok(format!("killed Rex session {}", handle.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_exe::executable;

    /// `path_in` over an environment holding just `vars`.
    fn path_with(vars: &[(&str, &OsStr)]) -> PathBuf {
        path_in(|name| (vars.iter().find(|(k, _)| *k == name)).map(|(_, v)| v.to_os_string()))
    }

    #[test]
    fn rex_is_tower_rex_else_in_rex_bin_dir_else_on_the_path() {
        let bin = tempfile::tempdir().unwrap();
        let dir = bin.path().as_os_str();
        let in_bin = bin.path().join("rex");
        // No rex in REX_BIN_DIR yet: rex on the PATH.
        assert_eq!(path_with(&[("REX_BIN_DIR", dir)]), PathBuf::from("rex"));
        executable(&in_bin, "#!/bin/sh\n");
        assert_eq!(path_with(&[("REX_BIN_DIR", dir)]), in_bin);
        // TOWER_REX first; an empty one counts as unset.
        let tower_rex = ("TOWER_REX", OsStr::new("/opt/x/rex"));
        assert_eq!(
            path_with(&[tower_rex, ("REX_BIN_DIR", dir)]),
            PathBuf::from("/opt/x/rex")
        );
        let empty = ("TOWER_REX", OsStr::new(""));
        assert_eq!(path_with(&[empty, ("REX_BIN_DIR", dir)]), in_bin);
        // Neither, or an empty REX_BIN_DIR: rex on the PATH.
        assert_eq!(path_with(&[]), PathBuf::from("rex"));
        let empty_dir = ("REX_BIN_DIR", OsStr::new(""));
        assert_eq!(path_with(&[empty_dir]), PathBuf::from("rex"));
        // A relative REX_BIN_DIR is none, even one that reaches that rex from here: it would
        // name another file wherever the dashboard runs.
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative = format!(
            "{up}{}",
            bin.path().to_str().unwrap().trim_start_matches('/')
        );
        assert!(
            Path::new(&relative).join("rex").is_file(),
            "{relative} reaches the rex"
        );
        let relative_dir = ("REX_BIN_DIR", OsStr::new(&relative));
        assert_eq!(path_with(&[relative_dir]), PathBuf::from("rex"));
        // The not-found line names what was tried and where Rex keeps it.
        let none = Rex::new("rex".into());
        assert_eq!(
            none.missing(Some(OsStr::new("/nowhere"))).unwrap(),
            "rex not found: tried rex on PATH /nowhere; Rex keeps it in REX_BIN_DIR, which Rex \
             sets in its own tabs; set TOWER_REX to its full path if it is elsewhere"
        );
        assert_eq!(Rex::new(in_bin).missing(None), None);
    }

    #[test]
    fn a_started_lead_is_its_new_sessions_id_and_undo_kills_that_session() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rex");
        executable(
            &exe,
            "#!/bin/sh\nd=$(dirname \"$0\")\n(IFS='|'; echo \"$*\") >> \"$d/calls.log\"\n\
             [ -e \"$d/fail\" ] && { echo 'Error: session target \"session:7\" not found' >&2; \
             exit 3; }\n[ \"$1\" = new ] && echo '{\"session_id\":\"session:7\"}'\nexit 0\n",
        );
        let rex = Rex::new(exe);
        let db = dir.path().join("board.db");
        let command = ["claude", "-n", "Comics Lead", "/control-tower:lead M2"].map(String::from);
        let lead = LeadStart {
            name: "Comics Lead",
            dir: "/srv/shop",
            board: "/b/board.db",
            command: &command,
        };
        let handle = rex.start_lead(&db, &lead).unwrap();
        assert_eq!(handle, Handle("session:7".into()));
        assert_eq!(
            rex.undo(&db, &handle).unwrap(),
            "killed Rex session session:7"
        );
        let log = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "new|Comics Lead|-c|/srv/shop|--json|--|env|TOWER_DB=/b/board.db|claude|-n|\
                 Comics Lead|/control-tower:lead M2",
                "kill|session:7",
            ]
        );
        // A session Rex no longer has: undo says so.
        std::fs::write(dir.path().join("fail"), "").unwrap();
        assert_eq!(
            format!("{:#}", rex.undo(&db, &handle).unwrap_err()),
            "rex kill exited 3: Error: session target \"session:7\" not found"
        );
    }
}
