//! The Rex module: the terminal module interface over the `rex` command line (`path`), on
//! the local Rex server only, never starting one (`--autostart=false`).
//!
//! The workers (`agents`) are the Claude sessions running in Rex's terminal blocks: every
//! session's blocks, each terminal block's `program_status` root record (the one without an
//! id; child records are a session's background shells and subagents), kept when its
//! `effective_app` is `claude-code` with an owner pid, joined by that pid to `claude agents
//! --json`. A worker's name is its Claude session's name, its status the root record's
//! state and kind, its `msg` the record's. A block with no Claude record, or whose pid
//! Claude does not list, is no one. One look is one call per session and per terminal
//! block, all within the look's time limit; any call that fails fails the whole look, so a
//! Rex half read never reads as workers gone.
//!
//! For now the look is all this module does: delivery (`prompt`) and the dashboard's `n`
//! (`start_lead`) still use herdr until #111 and #112 build them here.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Agent, Handle, LeadStart, Sent, Status, Terminal, Unread, find_exe, not_found};
use crate::ui::runner::{Outcome, Run, run_now};

/// The creator of Rex's terminal blocks: only they have a `program_status`.
const TERMINAL_CREATOR: &str = "com.superlogical.terminal";
/// The terminal block method listing what its programs reported.
const PROGRAM_STATUS: &str = "com.superlogical.terminal.program_status";
/// The app Claude Code reports itself as.
const CLAUDE_CODE: &str = "claude-code";

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

/// Rex as the terminal module, run from `exe`, asking the `claude` at `claude` for the
/// names of the Claude sessions it finds.
pub struct Rex {
    exe: PathBuf,
    claude: PathBuf,
}

impl Rex {
    pub fn new(exe: PathBuf, claude: PathBuf) -> Rex {
        Rex { exe, claude }
    }

    /// `rex api call <method> <input>` on the local server, bounded by what is left of the
    /// look before `deadline` (`limit`, for its message): what it printed, read as `T`.
    fn call<T: DeserializeOwned>(
        &self,
        db: &Path,
        method: &str,
        input: serde_json::Value,
        deadline: Instant,
        limit: Duration,
    ) -> Result<T, Unread> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(out_of_time(limit));
        }
        let outcome = run_now(&Run {
            exe: self.exe.clone(),
            args: [
                "--autostart=false",
                "api",
                "call",
                method,
                &input.to_string(),
            ]
            .map(String::from)
            .to_vec(),
            db: db.to_path_buf(),
            stdin: None,
            timeout: left,
        });
        let command = format!("rex api call {method}");
        let stdout = match outcome {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => stdout,
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
                return Err(Unread::Failed(format!("{command} {how}: {}", said.trim())));
            }
            Outcome::TimedOut(_) => return Err(out_of_time(limit)),
            Outcome::Failed(why) => return Err(Unread::Failed(why)),
        };
        serde_json::from_str(&stdout).map_err(|e| {
            Unread::Failed(format!(
                "{command} said something unexpected ({e}): {}",
                stdout.trim()
            ))
        })
    }
}

/// A look that ran past its limit.
fn out_of_time(limit: Duration) -> Unread {
    Unread::Failed(format!(
        "Rex did not answer within {} s and the look was stopped",
        limit.as_secs()
    ))
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Option<Vec<Session>>,
}

#[derive(Deserialize)]
struct Session {
    session_id: String,
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Option<Vec<Block>>,
}

#[derive(Deserialize)]
struct Block {
    block_id: Option<String>,
    creator_name: Option<String>,
}

#[derive(Deserialize)]
struct Records {
    records: Vec<Record>,
}

/// One `program_status` record, as far as the look reads it.
#[derive(Deserialize)]
struct Record {
    id: Option<String>,
    state: String,
    kind: Option<String>,
    msg: Option<String>,
    effective_app: Option<String>,
    owner: Option<Owner>,
}

#[derive(Deserialize)]
struct Owner {
    pid: i64,
}

/// A Claude session as `claude agents --json` lists it, as far as the look reads it.
#[derive(Deserialize)]
struct Listed {
    name: Option<String>,
    pid: Option<i64>,
}

/// A block's Claude: the pid its root record names, with the record's status and msg, when
/// that record is Claude Code's. `None` for a block with no records, or whose root record
/// is another program's or names no owner. Child records (with an id) are never read.
fn claude_in(records: Vec<Record>) -> Option<(i64, Status, Option<String>)> {
    let root = records.into_iter().find(|r| r.id.is_none())?;
    if root.effective_app.as_deref() != Some(CLAUDE_CODE) {
        return None;
    }
    let pid = root.owner?.pid;
    Some((pid, Status::of(&root.state, root.kind.as_deref()), root.msg))
}

/// The workers: each block's Claude (`found`, in Rex's order) that `claude agents --json`
/// (`listed`) lists by its pid, named as Claude names it. A pid Claude does not list, or
/// lists with no name, is no one.
fn join(found: Vec<(i64, Status, Option<String>)>, listed: &[Listed]) -> Vec<Agent> {
    (found.into_iter())
        .filter_map(|(pid, status, msg)| {
            let name = (listed.iter()).find(|l| l.pid == Some(pid))?.name.clone()?;
            Some(Agent { name, status, msg })
        })
        .collect()
}

/// Not built yet: what delivery and `n` would be told, though they use herdr until #111 and
/// #112.
const NOT_YET: &str = "the Rex module does not reach workers or start leads in this build";

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
                "{}; Rex keeps it in REX_BIN_DIR, which Rex sets in its own tabs: link it into \
                 Homebrew's bin (ln -s \"$REX_BIN_DIR/rex\" \"$(brew --prefix)/bin/rex\") and \
                 run brew services restart control-tower, or set TOWER_REX to its full path",
                not_found("rex", &self.exe, path)
            )),
        }
    }

    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread> {
        let deadline = Instant::now() + timeout;
        let none = serde_json::json!({});
        let sessions: Sessions = self.call(db, "session.list", none, deadline, timeout)?;
        let mut found = Vec::new();
        for s in sessions.sessions.unwrap_or_default() {
            let input = serde_json::json!({ "session_id": s.session_id });
            let blocks: Blocks = self.call(db, "session.list_blocks", input, deadline, timeout)?;
            for b in blocks.blocks.unwrap_or_default() {
                let Some(block) = b.block_id else { continue };
                if b.creator_name.as_deref() != Some(TERMINAL_CREATOR) {
                    continue;
                }
                let input = serde_json::json!({
                    "session_id": s.session_id, "block_id": block, "args": {},
                });
                let records: Records = self.call(db, PROGRAM_STATUS, input, deadline, timeout)?;
                found.extend(claude_in(records.records));
            }
        }
        // No Claude in Rex: nothing to name, and claude is not asked.
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(out_of_time(timeout));
        }
        let args = ["agents", "--json"];
        let not_stopped = AtomicBool::new(false);
        let json = crate::register::claude_within(&self.claude, db, &not_stopped, &args, left)
            .map_err(|e| Unread::Failed(format!("{e:#}")))?;
        let listed: Vec<Listed> = serde_json::from_str(&json).map_err(|e| {
            Unread::Failed(format!(
                "claude agents --json said something unexpected: {e}"
            ))
        })?;
        Ok(join(found, &listed))
    }

    fn prompt(&self, _db: &Path, _name: &str, _text: &str, _timeout: Duration) -> Sent {
        Sent::Failed(NOT_YET.into())
    }

    fn start_lead(&self, _db: &Path, _lead: &LeadStart) -> Result<Handle> {
        bail!(NOT_YET)
    }

    fn undo(&self, _db: &Path, _handle: &Handle) -> Result<String> {
        bail!(NOT_YET)
    }
}

/// Test only: a fake `rex` and `claude` in a temp folder, and the Rex module over them.
/// Rex's server is files: `sessions` lists one session id per line, `<session>.blocks` one
/// block per line as "<block id> <creator>", and `<block>.json` is that block's
/// `program_status` records (none when it is missing). `claude.json` is what `claude
/// agents --json` prints. Each call's words go to `calls.log`, one call per line joined by
/// `|`; a call whose method or session or block has a file `fail-<it>` exits 1 saying so.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::{Path, PathBuf};

    use super::Rex;

    /// A session's blocks, each (block id, creator, its `program_status` records).
    pub type Blocks<'a> = [(&'a str, &'a str, &'a str)];

    pub struct FakeRex {
        pub dir: PathBuf,
    }

    /// The fake rex: answers `--autostart=false api call <method> <json>` from the files.
    const REX: &str = r#"#!/bin/sh
d=$(dirname "$0")
(IFS='|'; echo "$*") >> "$d/calls.log"
[ "$1" = "--autostart=false" ] && [ "$2 $3" = "api call" ] || { echo "unexpected: $*" >&2; exit 2; }
m=$4
field() { printf '%s' "$2" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
s=$(field session_id "$5"); b=$(field block_id "$5")
for it in "$m" "$s" "$b"; do
  [ -n "$it" ] && [ -e "$d/fail-$it" ] && { cat "$d/fail-$it" >&2; exit 1; }
done
case "$m" in
session.list)
  printf '{"sessions":['; sep=
  while read -r id; do printf '%s{"session_id":"%s","label":"work"}' "$sep" "$id"; sep=,; done < "$d/sessions"
  echo ']}' ;;
session.list_blocks)
  printf '{"blocks":['; sep=
  [ -e "$d/$s.blocks" ] && while read -r id creator; do
    printf '%s{"block_id":"%s","label":"shell","creator_name":"%s","placement":"placed"}' "$sep" "$id" "$creator"; sep=,
  done < "$d/$s.blocks"
  echo ']}' ;;
com.superlogical.terminal.program_status)
  if [ -e "$d/$b.json" ]; then cat "$d/$b.json"; else echo '{"records":[]}'; fi ;;
*) echo "unexpected method: $m" >&2; exit 2 ;;
esac
"#;

    /// The fake claude: `agents --json` prints `claude.json`.
    const CLAUDE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "claude|$*" >> "$d/calls.log"
[ "$*" = "agents --json" ] || { echo "unexpected: $*" >&2; exit 2; }
cat "$d/claude.json"
"#;

    impl FakeRex {
        /// The fakes in `dir`, with an empty Rex and an empty Claude list.
        pub fn new(dir: &Path) -> FakeRex {
            crate::test_exe::executable(&dir.join("rex"), REX);
            crate::test_exe::executable(&dir.join("claude"), CLAUDE);
            let f = FakeRex {
                dir: dir.to_path_buf(),
            };
            f.write("sessions", "");
            f.write("claude.json", "[]");
            f
        }

        pub fn module(&self) -> Rex {
            Rex::new(self.dir.join("rex"), self.dir.join("claude"))
        }

        pub fn write(&self, file: &str, contents: &str) {
            std::fs::write(self.dir.join(file), contents).unwrap();
        }

        /// The calls made so far, oldest first.
        pub fn calls(&self) -> Vec<String> {
            (std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default())
                .lines()
                .map(String::from)
                .collect()
        }

        /// One root record of Claude Code's, owned by `pid`, in `state` (and `kind`, `msg`
        /// when given), as Claude Code reports itself.
        pub fn claude_record(
            pid: i64,
            state: &str,
            kind: Option<&str>,
            msg: Option<&str>,
        ) -> String {
            let mut r = serde_json::json!({
                "app": "claude-code", "effective_app": "claude-code", "state": state,
                "owner": {"pid": pid, "name": "claude"},
                "updated_at": "2026-01-02T03:04:05Z",
            });
            if let Some(kind) = kind {
                r["kind"] = kind.into();
            }
            if let Some(msg) = msg {
                r["msg"] = msg.into();
            }
            r.to_string()
        }

        /// Rex's whole server from now on: `sessions`, each with its blocks as
        /// (block id, creator, `program_status` records joined as a JSON array's items).
        pub fn server(&self, sessions: &[(&str, &Blocks)]) {
            let ids: Vec<&str> = sessions.iter().map(|(s, _)| *s).collect();
            self.write("sessions", &lines(&ids));
            for (s, blocks) in sessions {
                let rows: Vec<String> = (blocks.iter())
                    .map(|(b, creator, _)| format!("{b} {creator}"))
                    .collect();
                self.write(&format!("{s}.blocks"), &lines(&rows));
                for (b, _, records) in blocks.iter() {
                    self.write(
                        &format!("{b}.json"),
                        &format!(r#"{{"records":[{records}]}}"#),
                    );
                }
            }
        }

        /// `claude agents --json` from now on: each (name, pid) an interactive session.
        pub fn claude(&self, sessions: &[(&str, i64)]) {
            let list: Vec<serde_json::Value> = (sessions.iter())
                .map(|(name, pid)| {
                    serde_json::json!({
                        "id": format!("a{pid}"), "cwd": "/somewhere", "kind": "interactive",
                        "startedAt": 1, "sessionId": format!("sess-{pid}"), "name": name,
                        "pid": pid, "status": "idle",
                    })
                })
                .collect();
            self.write("claude.json", &serde_json::Value::Array(list).to_string());
        }
    }

    fn lines(rows: &[impl AsRef<str>]) -> String {
        rows.iter().map(|r| format!("{}\n", r.as_ref())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRex;
    use super::*;
    use crate::terminal::Kind;
    use crate::test_exe::executable;

    const TERM: &str = TERMINAL_CREATOR;

    fn agents(f: &FakeRex) -> Result<Vec<Agent>, Unread> {
        f.module()
            .agents(&f.dir.join("board.db"), crate::watch::TEST_WAIT)
    }

    fn agent(name: &str, status: Status, msg: Option<&str>) -> Agent {
        Agent {
            name: name.into(),
            status,
            msg: msg.map(String::from),
        }
    }

    /// `path_in` over an environment holding just `vars`.
    fn path_with(vars: &[(&str, &OsStr)]) -> PathBuf {
        path_in(|name| (vars.iter().find(|(k, _)| *k == name)).map(|(_, v)| v.to_os_string()))
    }

    #[test]
    fn the_workers_are_claude_sessions_in_rex_by_their_root_record_named_by_claude() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let blocked = FakeRex::claude_record(
            41,
            "blocked",
            Some("permission"),
            Some("approve Bash: touch notes.txt"),
        );
        // dev-1 waits on a background shell: its root is working, its child blocked.
        let busy = format!(
            "{},{}",
            FakeRex::claude_record(42, "working", None, None),
            r#"{"id":"shell-1","app":"zsh","effective_app":"zsh","state":"blocked","kind":"question","owner":{"pid":43,"name":"zsh"},"updated_at":"2026-01-02T03:04:05Z"}"#
        );
        let other_app = r#"{"app":"cargo","effective_app":"cargo","state":"working","owner":{"pid":44,"name":"cargo"},"updated_at":"2026-01-02T03:04:05Z"}"#;
        let no_owner = r#"{"app":"claude-code","effective_app":"claude-code","state":"idle","updated_at":"2026-01-02T03:04:05Z"}"#;
        let unknown = FakeRex::claude_record(46, "error", None, None);
        let unlisted = FakeRex::claude_record(47, "blocked", Some("question"), None);
        let child_only = r#"{"id":"task","app":"claude-code","effective_app":"claude-code","state":"blocked","owner":{"pid":48,"name":"claude"},"updated_at":"2026-01-02T03:04:05Z"}"#;
        f.server(&[
            (
                "session:a",
                &[
                    ("block:1", TERM, &blocked),
                    ("block:2", TERM, &busy),
                    ("block:3", TERM, ""),
                    ("block:4", TERM, other_app),
                ],
            ),
            (
                "session:b",
                &[
                    ("block:5", TERM, no_owner),
                    ("block:6", TERM, &unknown),
                    ("block:7", TERM, &unlisted),
                    ("block:8", TERM, child_only),
                    ("block:9", "com.superlogical.viewer", "not records"),
                ],
            ),
            ("session:c", &[]),
        ]);
        f.claude(&[
            ("qa-1", 41),
            ("dev-1", 42),
            ("ghost", 44),
            ("dev-2", 46),
            ("x", 48),
        ]);
        assert_eq!(
            agents(&f).unwrap(),
            [
                agent(
                    "qa-1",
                    Status::Blocked(Some(Kind::Permission)),
                    Some("approve Bash: touch notes.txt")
                ),
                agent("dev-1", Status::Working, None),
                agent("dev-2", Status::Unknown, None),
            ]
        );
        // One call per session and per terminal block, then claude once; the viewer block
        // has no program_status and is never asked.
        let calls = f.calls();
        let methods: Vec<&str> = (calls.iter())
            .map(|c| c.split('|').nth(3).unwrap_or(c))
            .collect();
        assert_eq!(
            methods,
            [
                "session.list",
                "session.list_blocks",
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                "session.list_blocks",
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                PROGRAM_STATUS,
                "session.list_blocks",
                "claude|agents --json",
            ]
        );
        assert_eq!(
            calls[2],
            r#"--autostart=false|api|call|com.superlogical.terminal.program_status|{"args":{},"block_id":"block:1","session_id":"session:a"}"#
        );
    }

    #[test]
    fn a_rex_with_no_claude_is_no_workers_and_claude_is_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        assert_eq!(agents(&f).unwrap(), []);
        f.server(&[("session:a", &[("block:1", TERM, "")])]);
        assert_eq!(agents(&f).unwrap(), []);
        assert!(!f.calls().iter().any(|c| c.starts_with("claude")));
        // Claude, but Claude lists none of their pids: no one.
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 99)]);
        assert_eq!(agents(&f).unwrap(), []);
    }

    #[test]
    fn any_call_that_fails_fails_the_whole_look_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = FakeRex::new(dir.path());
        let idle = FakeRex::claude_record(41, "idle", None, None);
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.claude(&[("dev-1", 41)]);
        assert_eq!(agents(&f).unwrap().len(), 1);
        let failed = |f: &FakeRex| match agents(f) {
            Err(Unread::Failed(why)) => why,
            other => panic!("{other:?}"),
        };
        for (it, method) in [
            ("session.list", "session.list"),
            ("session:a", "session.list_blocks"),
            ("block:1", PROGRAM_STATUS),
        ] {
            f.write(&format!("fail-{it}"), "Error: no such thing");
            assert_eq!(
                failed(&f),
                format!("rex api call {method} exited 1: Error: no such thing")
            );
            std::fs::remove_file(dir.path().join(format!("fail-{it}"))).unwrap();
        }
        f.write("block:1.json", "not json");
        assert!(
            failed(&f).starts_with(&format!(
                "rex api call {PROGRAM_STATUS} said something unexpected"
            )),
            "{}",
            failed(&f)
        );
        f.server(&[("session:a", &[("block:1", TERM, &idle)])]);
        f.write("claude.json", "not json");
        assert!(failed(&f).starts_with("claude agents --json said something unexpected"));
        std::fs::remove_file(dir.path().join("claude")).unwrap();
        assert!(failed(&f).contains("cannot run"), "{}", failed(&f));
        std::fs::remove_file(dir.path().join("rex")).unwrap();
        assert!(failed(&f).contains("cannot run"), "{}", failed(&f));
    }

    #[test]
    fn a_look_past_its_time_limit_is_stopped_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rex");
        executable(&exe, "#!/bin/sh\nexec sleep 5\n");
        let rex = Rex::new(exe, dir.path().join("claude"));
        let began = Instant::now();
        let got = rex.agents(&dir.path().join("board.db"), Duration::from_secs(1));
        assert_eq!(
            got,
            Err(Unread::Failed(
                "Rex did not answer within 1 s and the look was stopped".into()
            ))
        );
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "{:?}",
            began.elapsed()
        );
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
        // The not-found line names what was tried and how to link it.
        let none = Rex::new("rex".into(), "claude".into());
        assert_eq!(
            none.missing(Some(OsStr::new("/nowhere"))).unwrap(),
            "rex not found: tried rex on PATH /nowhere; Rex keeps it in REX_BIN_DIR, which Rex \
             sets in its own tabs: link it into Homebrew's bin (ln -s \"$REX_BIN_DIR/rex\" \
             \"$(brew --prefix)/bin/rex\") and run brew services restart control-tower, or set \
             TOWER_REX to its full path"
        );
        assert_eq!(Rex::new(in_bin, "claude".into()).missing(None), None);
    }
}
