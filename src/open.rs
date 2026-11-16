//! The two herdr actions that open the dashboard: `tower space`, the dashboard in a
//! herdr workspace of its own labelled `control-tower`, and `tower popup`, the dashboard in
//! herdr's popup over the work, 85% of the terminal each way.
//!
//! `space` finds the dashboard first: a pane labelled `control-tower` in `herdr pane list`
//! (herdr labels a plugin pane with its manifest title) that `herdr plugin pane focus`
//! confirms is this plugin's `ui` pane, which focuses it and its workspace. Any pane can
//! carry the label (a renamed shell, the shell herdr restores after a restart), so the label
//! alone is never enough. With no confirmed dashboard it always creates a workspace of
//! its own and opens the dashboard there; it never adopts a workspace by its label, and the
//! only pane it closes is the new workspace's first shell, so the dashboard fills it.
//!
//! The dashboard it opens is told it is in its own space (`TOWER_UI_MODE=space`): there `q`
//! returns to the workspace `space` was last invoked from and the dashboard keeps running
//! (`back`). herdr gives an action that workspace as `HERDR_WORKSPACE_ID`; `space` records it
//! beside the board (`from_path`) on every press, so a press from anywhere updates where `q`
//! goes. Workspace ids repeat across herdr servers sharing one board, so the record keeps
//! one workspace per server, keyed by its socket (`HERDR_SOCKET_PATH`), which the action and
//! the dashboard's pane both get.
//!
//! What to do is decided from herdr's JSON, parsed with serde; every herdr call goes through
//! the bounded `watch::herdr`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::register::PLUGIN_ID;
use crate::watch::herdr;

/// The label of the dashboard's own workspace, and of its pane (the manifest pane's title).
pub const LABEL: &str = "control-tower";
/// The dashboard's pane entrypoint in `herdr-plugin.toml`.
pub const ENTRYPOINT: &str = "ui";
/// The environment variable telling `tower ui` where it runs: `space` in its own space,
/// `popup` in herdr's popup.
pub const MODE_VAR: &str = "TOWER_UI_MODE";
/// `MODE_VAR`'s value in the dashboard's own space.
pub const SPACE_MODE: &str = "space";
/// The environment variable naming the herdr server's socket, in an action and in a pane.
pub const SOCKET_VAR: &str = "HERDR_SOCKET_PATH";
/// The popup's width and height, of the terminal.
pub const POPUP_SIZE: &str = "85%";

#[derive(Deserialize)]
struct Reply<T> {
    result: T,
}

#[derive(Deserialize)]
struct PaneList {
    panes: Vec<PaneInfo>,
}

/// One pane of `herdr pane list`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize)]
struct Created {
    workspace: Id,
    root_pane: PaneId,
}

#[derive(Deserialize)]
struct Id {
    workspace_id: String,
}

#[derive(Deserialize)]
struct PaneId {
    pane_id: String,
}

#[derive(Deserialize)]
struct Focused {
    plugin_pane: PluginPane,
}

#[derive(Deserialize)]
struct PluginPane {
    plugin_id: String,
    entrypoint: String,
}

fn parse<T: for<'de> Deserialize<'de>>(json: &str, what: &str) -> Result<T> {
    serde_json::from_str::<Reply<T>>(json)
        .map(|r| r.result)
        .with_context(|| format!("herdr {what} said something unexpected: {}", json.trim()))
}

/// The panes in `herdr pane list`'s JSON.
pub fn parse_panes(json: &str) -> Result<Vec<PaneInfo>> {
    Ok(parse::<PaneList>(json, "pane list")?.panes)
}

/// The panes among `panes` that may be the dashboard: those labelled `control-tower`.
pub fn candidates(panes: &[PaneInfo]) -> impl Iterator<Item = &PaneInfo> {
    panes.iter().filter(|p| p.label.as_deref() == Some(LABEL))
}

/// Whether `herdr plugin pane focus`'s reply is this plugin's dashboard pane.
pub fn is_dashboard(json: &str) -> Result<bool> {
    let f: Focused = parse(json, "plugin pane focus")?;
    Ok(f.plugin_pane.plugin_id == PLUGIN_ID && f.plugin_pane.entrypoint == ENTRYPOINT)
}

/// Whether a failed herdr call failed with the error `code`, as herdr's JSON names it.
fn failed_with(e: &anyhow::Error, code: &str) -> bool {
    format!("{e:#}").contains(&format!("\"code\":\"{code}\""))
}

/// Focuses `pane` if it is the dashboard, which focuses its workspace too, and says whether
/// it was. A pane that is not a plugin pane, or that has closed since `pane list`, is not
/// the dashboard; any other failure is an error.
fn focus_dashboard(exe: &Path, db: &Path, pane: &str) -> Result<bool> {
    match herdr(exe, db, &["plugin", "pane", "focus", pane]) {
        Ok(json) => is_dashboard(&json),
        Err(e) if failed_with(&e, "plugin_pane_not_found") || failed_with(&e, "pane_not_found") => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Where `space` records the workspace it was invoked from, for `back`: beside the board.
/// One line per herdr server, its socket and the workspace separated by a tab.
pub fn from_path(db: &Path) -> PathBuf {
    db.with_extension("space-from")
}

/// Where `space` was pressed: the herdr server, by its socket (empty if herdr gave none),
/// and the workspace, if herdr gave one.
#[derive(Debug, Clone, Copy)]
pub struct Press<'a> {
    pub socket: &'a str,
    pub workspace: Option<&'a str>,
}

/// The record's lines as (socket, workspace), none when there is no record, and the lines
/// that are not socket, tab, workspace.
type Record = (Vec<(String, String)>, Vec<String>);

/// Reads the record at `path`.
fn read_record(path: &Path) -> Result<Record> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let mut record = Record::default();
    for l in String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|l| !l.is_empty())
    {
        match l.split_once('\t') {
            Some((socket, ws)) if !ws.is_empty() && !ws.contains('\t') => {
                record.0.push((socket.to_string(), ws.to_string()))
            }
            _ => record.1.push(l.to_string()),
        }
    }
    Ok(record)
}

/// Records where `press` came from for its herdr server, unless it came from the
/// dashboard's own `space`; with no workspace the server's line goes, so its `q` stays put
/// rather than going somewhere stale. Other servers' lines are kept. The record only says
/// where `q` goes, so a line it cannot read is dropped with a warning rather than stopping
/// `space`. It is replaced whole (a temporary file renamed over it), never half written.
fn record_from(db: &Path, press: Press, space: Option<&str>) -> Result<()> {
    if press.workspace.is_some() && press.workspace == space {
        return Ok(());
    }
    for (what, value) in [
        ("socket", press.socket),
        ("workspace", press.workspace.unwrap_or("")),
    ] {
        if value.contains(['\t', '\n']) {
            anyhow::bail!("herdr gave a {what} with a tab or line break: {value:?}");
        }
    }
    let path = from_path(db);
    let (mut lines, bad) = read_record(&path)?;
    for l in bad {
        eprintln!(
            "tower: dropped a line of {} that is not socket, tab, workspace: {l:?}",
            path.display()
        );
    }
    lines.retain(|(socket, _)| socket != press.socket);
    if let Some(ws) = press.workspace {
        lines.push((press.socket.to_string(), ws.to_string()));
    }
    let text: String = lines.iter().map(|(s, w)| format!("{s}\t{w}\n")).collect();
    let tmp = path.with_extension(format!("space-from.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .with_context(|| format!("could not write {}", path.display()))
}

/// `herdr plugin pane open` for the dashboard as a tab of `workspace`, focused, and
/// told by `MODE_VAR` it is in its own space.
fn open_in(exe: &Path, db: &Path, workspace: &str) -> Result<String> {
    let mode = format!("{MODE_VAR}={SPACE_MODE}");
    herdr(
        exe,
        db,
        &[
            "plugin",
            "pane",
            "open",
            "--plugin",
            PLUGIN_ID,
            "--entrypoint",
            ENTRYPOINT,
            "--placement",
            "tab",
            "--workspace",
            workspace,
            "--env",
            &mode,
            "--focus",
        ],
    )
}

/// The `space` action, pressed at `from`: focuses the dashboard in its own workspace,
/// creating the workspace and opening it there when there is no dashboard, and says what it
/// did.
pub fn space(exe: &Path, db: &Path, from: Press) -> Result<String> {
    let panes = parse_panes(&herdr(exe, db, &["pane", "list"])?)?;
    for p in candidates(&panes) {
        // Only the dashboard's workspace says whether to record, so this comes after the
        // check, which has done nothing but focus the dashboard.
        if focus_dashboard(exe, db, &p.pane_id)? {
            record_from(db, from, Some(&p.workspace_id))?;
            return Ok(format!("focused the {LABEL} workspace {}", p.workspace_id));
        }
    }
    // Recorded before anything is made, so a failure leaves nothing half done.
    record_from(db, from, None)?;
    let created = herdr(
        exe,
        db,
        &["workspace", "create", "--label", LABEL, "--no-focus"],
    )?;
    let created: Created = parse(&created, "workspace create")?;
    let ws = created.workspace.workspace_id;
    open_in(exe, db, &ws).with_context(|| {
        format!("created the {LABEL} workspace {ws} but could not open the dashboard in it")
    })?;
    // The shell the workspace started with goes, with its tab, so the dashboard fills it.
    herdr(exe, db, &["pane", "close", &created.root_pane.pane_id])?;
    herdr(exe, db, &["workspace", "focus", &ws])?;
    Ok(format!(
        "created the {LABEL} workspace {ws} with the dashboard"
    ))
}

/// `q` in the dashboard's own space, on the herdr server whose socket is `socket`: focuses
/// the workspace `space` was last pressed in on that server, and returns it. `None` when
/// there is none to go to, recorded or still open in herdr: the dashboard stays put. A
/// record line it cannot read is an error, which the next `space` press clears.
pub fn back(exe: &Path, db: &Path, socket: &str) -> Result<Option<String>> {
    let path = from_path(db);
    let (lines, bad) = read_record(&path)?;
    if let Some(l) = bad.first() {
        anyhow::bail!(
            "{} has a line that is not socket, tab, workspace: {l:?}; the next tower space \
             press drops it",
            path.display()
        );
    }
    let Some((_, ws)) = lines.into_iter().find(|(s, _)| s == socket) else {
        return Ok(None);
    };
    match herdr(exe, db, &["workspace", "focus", &ws]) {
        Ok(_) => Ok(Some(ws)),
        Err(e) if failed_with(&e, "workspace_not_found") => Ok(None),
        Err(e) => Err(e),
    }
}

/// The `popup` action: the dashboard in herdr's popup, `POPUP_SIZE` each way, told it is in
/// a popup through `MODE_VAR`.
pub fn popup(exe: &Path, db: &Path) -> Result<String> {
    let mode = format!("{MODE_VAR}=popup");
    herdr(
        exe,
        db,
        &[
            "plugin",
            "pane",
            "open",
            "--plugin",
            PLUGIN_ID,
            "--entrypoint",
            ENTRYPOINT,
            "--placement",
            "popup",
            "--width",
            POPUP_SIZE,
            "--height",
            POPUP_SIZE,
            "--env",
            &mode,
        ],
    )?;
    Ok("opened the dashboard in a popup".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_candidates_are_the_panes_labelled_control_tower() {
        let json = r#"{"id":"cli:pane:list","result":{"type":"pane_list","panes":[
            {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","cwd":"/"},
            {"pane_id":"w5:p3","workspace_id":"w5","tab_id":"w5:t2","label":"control-tower","cwd":"/"},
            {"pane_id":"w6:p1","workspace_id":"w6","tab_id":"w6:t1","label":"Control-Tower","cwd":"/"}]}}"#;
        let panes = parse_panes(json).unwrap();
        let ids: Vec<&str> = candidates(&panes).map(|p| p.pane_id.as_str()).collect();
        assert_eq!(ids, ["w5:p3"]);
    }

    #[test]
    fn only_this_plugins_ui_pane_is_the_dashboard() {
        let reply = |plugin: &str, entry: &str| {
            format!(
                r#"{{"id":"cli:plugin","result":{{"plugin_pane":{{"entrypoint":"{entry}","pane":{{"pane_id":"w2:p2"}},"plugin_id":"{plugin}"}},"type":"plugin_pane_focused"}}}}"#
            )
        };
        assert!(is_dashboard(&reply("control-tower", "ui")).unwrap());
        assert!(!is_dashboard(&reply("control-tower", "other")).unwrap());
        assert!(!is_dashboard(&reply("renamer", "ui")).unwrap());
        assert!(is_dashboard(r#"{"result":{"type":"ok"}}"#).is_err());
    }
}
