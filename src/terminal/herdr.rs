//! The herdr module: the terminal module interface over the `herdr` command line. The
//! workers are herdr's named agents (`herdr agent list`; unnamed agents are no one's), a
//! text reaches one by `herdr agent prompt` to exactly that name, and a lead starts in a new
//! herdr workspace, out of focus, whose root pane runs its command (`workspace create`, then
//! `pane run`), the workspace closed again when the pane would not run it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Agent, Handle, LeadStart, Sent, Status, Terminal, Unread};
use crate::ui::runner::{Outcome, Run, run_now};
use crate::ui::writer::sh_quote;
use crate::watch::{find_exe, not_found, terminal_timeout};

/// The herdr executable: `TOWER_HERDR` if set, else `herdr` on the PATH.
pub fn path() -> PathBuf {
    std::env::var_os("TOWER_HERDR")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from("herdr"), PathBuf::from)
}

/// The one line saying herdr cannot be found, naming exactly what was tried.
pub fn missing(herdr: &Path, path: Option<&OsStr>) -> String {
    format!(
        "{}; set TOWER_HERDR to its full path if it is elsewhere (said once, until herdr is \
         found)",
        not_found("herdr", herdr, path)
    )
}

/// herdr as the terminal module, run from `exe`.
pub struct Herdr {
    exe: PathBuf,
}

impl Herdr {
    pub fn new(exe: PathBuf) -> Herdr {
        Herdr { exe }
    }
}

#[derive(Deserialize)]
struct ListReply {
    result: ListResult,
}

#[derive(Deserialize)]
struct ListResult {
    agents: Vec<RawAgent>,
}

#[derive(Deserialize)]
struct RawAgent {
    name: Option<String>,
    agent_status: String,
}

/// herdr's live named agents from `herdr agent list`'s JSON. Unnamed agents are no one's.
pub fn parse_agents(json: &str) -> Result<Vec<Agent>> {
    let reply: ListReply =
        serde_json::from_str(json).context("herdr agent list said something unexpected")?;
    Ok(reply
        .result
        .agents
        .into_iter()
        .filter_map(|a| {
            Some(Agent {
                name: a.name?,
                status: Status::of(&a.agent_status, None),
            })
        })
        .collect())
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

#[derive(Deserialize)]
struct Created {
    result: CreatedResult,
}

#[derive(Deserialize)]
struct CreatedResult {
    root_pane: Pane,
    workspace: Workspace,
}

#[derive(Deserialize)]
struct Pane {
    pane_id: String,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
}

/// The new workspace's id and its root pane's, from `herdr workspace create`'s JSON.
fn parse_created(json: &str) -> Result<(String, String)> {
    let c: Created =
        serde_json::from_str(json).context("herdr workspace create said something unexpected")?;
    Ok((c.result.workspace.workspace_id, c.result.root_pane.pane_id))
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

impl Terminal for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn exe(&self) -> &Path {
        &self.exe
    }

    fn missing(&self, path: Option<&OsStr>) -> Option<String> {
        match find_exe(&self.exe, path) {
            Some(_) => None,
            None => Some(missing(&self.exe, path)),
        }
    }

    fn agents(&self, db: &Path, timeout: Duration) -> Result<Vec<Agent>, Unread> {
        let outcome = run_now(&Run {
            exe: self.exe.clone(),
            args: ["agent", "list"].map(String::from).to_vec(),
            db: db.to_path_buf(),
            stdin: None,
            timeout,
        });
        match outcome {
            Outcome::Exited {
                code: Some(0),
                stdout,
                ..
            } => parse_agents(&stdout).map_err(|e| Unread::Failed(format!("{e:#}"))),
            Outcome::TimedOut(after) => Err(Unread::Failed(format!(
                "herdr agent list did not finish in {} s",
                after.as_secs()
            ))),
            Outcome::Exited { stderr, stdout, .. } => {
                let said = said(stdout, stderr);
                let why = format!("herdr agent list failed: {}", said.trim());
                if said.contains("server_not_running") {
                    Err(Unread::NotRunning(why))
                } else {
                    Err(Unread::Failed(why))
                }
            }
            Outcome::Failed(why) => Err(Unread::Failed(why)),
        }
    }

    fn prompt(&self, db: &Path, name: &str, text: &str, timeout: Duration) -> Sent {
        // No `--` before the text: herdr 0.9.5's `agent prompt` refuses it ("unknown
        // option"). Tower's texts start "control-tower:" or "no post", never "-".
        let outcome = run_now(&Run {
            exe: self.exe.clone(),
            args: ["agent", "prompt", name, text].map(String::from).to_vec(),
            db: db.to_path_buf(),
            stdin: None,
            timeout,
        });
        match outcome {
            Outcome::Exited { code: Some(0), .. } => Sent::Delivered,
            Outcome::TimedOut(after) => Sent::Unsure(format!(
                "herdr agent prompt did not finish in {} s and was stopped",
                after.as_secs()
            )),
            Outcome::Exited {
                code,
                stdout,
                stderr,
            } => {
                let said = said(stdout, stderr);
                let why = format!("herdr agent prompt {}: {}", how(code), said.trim());
                // herdr stopped a prompt that may have arrived.
                if said.contains("agent_prompt_stalled") || said.contains("\"timeout\"") {
                    Sent::Unsure(why)
                } else {
                    Sent::Failed(why)
                }
            }
            Outcome::Failed(why) => Sent::Failed(why),
        }
    }

    fn start_lead(&self, db: &Path, lead: &LeadStart) -> Result<Handle> {
        let env = format!("TOWER_DB={}", lead.board);
        let create = [
            "workspace",
            "create",
            "--label",
            lead.name,
            "--cwd",
            lead.dir,
            "--env",
            &env,
            "--no-focus",
        ];
        let (workspace, pane) = parse_created(&run(&self.exe, db, &create)?)?;
        let handle = Handle(workspace);
        let line = shell_line(lead.command);
        if let Err(e) = run(&self.exe, db, &["pane", "run", &pane, &line]) {
            let closed = match self.undo(db, &handle) {
                Ok(closed) => closed,
                Err(c) => format!("could not close workspace {} either: {c:#}", handle.0),
            };
            return Err(e.context(closed));
        }
        Ok(handle)
    }

    fn undo(&self, db: &Path, handle: &Handle) -> Result<String> {
        run(&self.exe, db, &["workspace", "close", &handle.0])?;
        Ok(format!("closed workspace {}", handle.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_herdrs_agent_list_keeping_named_agents() {
        let json = r#"{"id":"cli:agent:list","result":{"agents":[
            {"agent":"claude","agent_status":"idle","name":"dev-1","pane_id":"w1:p1"},
            {"agent":"claude","agent_status":"working","pane_id":"w1:p3"},
            {"agent":"codex","agent_status":"later","name":"qa","pane_id":"w1:p4"}],
            "type":"agent_list"}}"#;
        assert_eq!(
            parse_agents(json).unwrap(),
            [
                Agent {
                    name: "dev-1".into(),
                    status: Status::Idle
                },
                Agent {
                    name: "qa".into(),
                    status: Status::Unknown
                }
            ]
        );
        assert!(parse_agents(r#"{"error":"no server"}"#).is_err());
    }
}
