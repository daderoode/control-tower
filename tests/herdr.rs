//! `tower space` against a real, throwaway herdr. Ignored by default, as it needs herdr:
//! `cargo test --test herdr -- --ignored` (README, Testing against herdr).
//!
//! It never touches the owner's herdr: its own `herdr --session <name> server` in a short
//! folder under /tmp (herdr's socket path has a length limit), every `HERDR_*` variable
//! unset, a temp HOME and TOWER_DB, a fake `claude` first on PATH, and every process under a
//! time limit (`perl -e 'alarm N'`). The folder, with herdr's session folder in it, goes
//! afterwards, pass or fail.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

#[path = "../src/test_exe.rs"]
mod test_exe;

const HERDR_VARS: [&str; 6] = [
    "HERDR_BIN_PATH",
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
];

/// A throwaway herdr server and its folder, stopped and removed on drop.
struct Herdr {
    exe: PathBuf,
    dir: PathBuf,
    session: String,
    server: Child,
}

impl Herdr {
    fn start() -> Herdr {
        let exe =
            control_tower::watch::find_exe(Path::new("herdr"), std::env::var_os("PATH").as_deref())
                .expect("herdr is not on PATH; this test needs it");
        let session = format!("ct{}", std::process::id());
        let dir = PathBuf::from(format!("/tmp/{session}"));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["home", "bin", "plugin/target/release"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        // The fake claude: no pane can reach the real one.
        let claude = dir.join("bin/claude");
        test_exe::executable(
            &claude,
            "#!/bin/sh\necho fake claude running\nexec sleep 300\n",
        );
        // The plugin as checked in, without its build step, running this build's tower.
        let manifest = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"),
        )
        .unwrap();
        let mut in_build = false;
        let manifest: String = (manifest.lines())
            .filter(|l| {
                in_build = (in_build || *l == "[[build]]") && !l.starts_with("[[panes]]");
                !in_build
            })
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(!manifest.contains("cargo"), "{manifest}");
        std::fs::write(dir.join("plugin/herdr-plugin.toml"), manifest).unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_tower"),
            dir.join("plugin/target/release/tower"),
        )
        .unwrap();
        let mut server = Command::new("perl");
        server
            .args(["-e", "alarm 300; exec @ARGV"])
            .arg(&exe)
            .args(["--session", &session, "server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let server = isolate(&mut server, &dir, &exe).spawn().unwrap();
        let h = Herdr {
            exe,
            dir,
            session,
            server,
        };
        h.wait("the server answers", || {
            h.try_call(&["workspace", "list"]).is_ok()
        });
        let plugin = h.dir.join("plugin");
        h.call(&["plugin", "link", plugin.to_str().unwrap()]);
        h
    }

    /// One herdr call, bounded: what it printed, or what it said when it failed.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new("perl");
        cmd.args(["-e", "alarm 15; exec @ARGV"])
            .arg(&self.exe)
            .args(["--session", &self.session])
            .args(args);
        let out = isolate(&mut cmd, &self.dir, &self.exe).output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            return Err(format!("{said}{}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(said)
    }

    /// One herdr call's JSON (null when it prints nothing, as `pane run`).
    fn try_call(&self, args: &[&str]) -> Result<Value, String> {
        let said = self.run(args)?;
        if said.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&said).map_err(|e| format!("{e}: {said}"))
    }

    fn call(&self, args: &[&str]) -> Value {
        self.try_call(args)
            .unwrap_or_else(|e| panic!("herdr {}: {e}", args.join(" ")))
    }

    fn wait(&self, what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn workspaces(&self) -> Vec<String> {
        let list = self.call(&["workspace", "list"]);
        let mut ids: Vec<String> = (list["result"]["workspaces"].as_array().unwrap().iter())
            .map(|w| w["workspace_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    fn focused(&self) -> String {
        let list = self.call(&["workspace", "list"]);
        (list["result"]["workspaces"].as_array().unwrap().iter())
            .find(|w| w["focused"] == true)
            .map(|w| w["workspace_id"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }

    fn panes_in(&self, workspace: &str) -> Vec<Value> {
        let list = self.call(&["pane", "list"]);
        (list["result"]["panes"].as_array().unwrap().iter())
            .filter(|p| p["workspace_id"] == workspace)
            .cloned()
            .collect()
    }

    fn exists(&self, pane: &str) -> bool {
        self.try_call(&["pane", "get", pane]).is_ok()
    }

    fn shows(&self, pane: &str, text: &str) -> bool {
        self.run(&["pane", "read", pane, "--source", "visible"])
            .is_ok_and(|said| said.contains(text))
    }

    /// Presses prefix+a in `workspace`: herdr runs the `space` action with the focused
    /// workspace, and this waits until it has finished, which must be cleanly.
    fn press(&self, workspace: &str) {
        self.call(&["workspace", "focus", workspace]);
        let invoked = self.call(&[
            "plugin",
            "action",
            "invoke",
            "space",
            "--plugin",
            "control-tower",
        ]);
        let id = invoked["result"]["log"]["log_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut log = Value::Null;
        self.wait("the space action finishes", || {
            let logs = self.call(&["plugin", "log", "list"]);
            log = (logs["result"]["logs"].as_array().unwrap().iter())
                .find(|l| l["log_id"] == id.as_str())
                .cloned()
                .unwrap_or(Value::Null);
            log["status"] != "running"
        });
        assert_eq!(log["exit_code"], 0, "{log}");
    }
}

impl Drop for Herdr {
    fn drop(&mut self) {
        let _ = self.try_call(&["server", "stop"]);
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `cmd` with no `HERDR_*` from the herdr running the tests, and the throwaway's HOME, PATH
/// (the fake claude first), board and herdr.
fn isolate<'a>(cmd: &'a mut Command, dir: &Path, herdr: &Path) -> &'a mut Command {
    for var in HERDR_VARS {
        cmd.env_remove(var);
    }
    cmd.env("HOME", dir.join("home"))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", dir.join("bin").display()),
        )
        .env("TOWER_DB", dir.join("board.db"))
        .env("TOWER_HERDR", herdr)
        .env_remove("TOWER_UI_MODE")
}

/// What this cannot see: an attached client's view. `focused()` reads the server's flag from
/// `workspace list`, and this herdr has no client attached. In herdr 0.9.5 the two can
/// disagree: `plugin pane focus` moves the server's focused workspace but not a client's view,
/// so this passed while prefix+a left the open dashboard out of view (issue #3). No herdr
/// 0.9.5 call reports what a client shows (`api snapshot` has only the server's focus, and
/// `status client` describes the client binary), so the view is not asserted here. What keeps
/// the fix, `workspace focus` after the confirming `plugin pane focus`, is the herdr calls
/// `tests/cli.rs` asserts for a press that finds the dashboard.
#[test]
#[ignore = "needs herdr: cargo test --test herdr -- --ignored"]
fn space_leaves_labelled_panes_alone_and_q_returns_with_the_dashboard_running() {
    let h = Herdr::start();
    // w1: a plain shell renamed control-tower. w2: a workspace labelled control-tower with a
    // session running in it.
    h.call(&["workspace", "create", "--label", "shop", "--no-focus"]);
    h.call(&[
        "workspace",
        "create",
        "--label",
        "control-tower",
        "--no-focus",
    ]);
    h.call(&["pane", "rename", "w1:p1", "control-tower"]);
    let claude = h.dir.join("bin/claude");
    h.call(&["pane", "run", "w2:p1", claude.to_str().unwrap()]);
    h.wait("the session runs in w2", || {
        h.shows("w2:p1", "fake claude running")
    });

    h.press("w1");
    assert!(h.exists("w1:p1"), "the renamed shell was closed");
    assert!(
        h.shows("w2:p1", "fake claude running"),
        "the session in w2 was closed"
    );
    let all = h.workspaces();
    assert_eq!(all.len(), 3, "{all:?}");
    let space = all
        .iter()
        .find(|w| *w != "w1" && *w != "w2")
        .unwrap()
        .clone();
    let panes = h.panes_in(&space);
    assert_eq!(panes.len(), 1, "{panes:?}");
    let dash = panes[0]["pane_id"].as_str().unwrap().to_string();
    let confirmed = h.call(&["plugin", "pane", "focus", &dash]);
    let p = &confirmed["result"]["plugin_pane"];
    assert_eq!(
        (&p["plugin_id"], &p["entrypoint"]),
        (&"control-tower".into(), &"ui".into())
    );
    assert_eq!(h.focused(), space);

    // A second press, from w2, reuses the dashboard; q goes back to w2 and leaves it running.
    h.press("w2");
    assert_eq!(h.workspaces(), all);
    assert_eq!(h.focused(), space);
    h.wait("the dashboard draws", || h.shows(&dash, "q"));
    h.call(&["pane", "send-keys", &dash, "q"]);
    h.wait("q returns to w2", || h.focused() == "w2");
    assert!(h.exists(&dash), "q closed the dashboard");

    // With w2 gone, q stays put and says so.
    h.call(&["workspace", "close", "w2"]);
    h.call(&["workspace", "focus", &space]);
    h.call(&["pane", "send-keys", &dash, "q"]);
    h.wait("q says there is nowhere to go", || {
        h.shows(&dash, "no workspace to return to")
    });
    assert_eq!(h.focused(), space);

    // Ctrl-C closes it; the renamed shell is still there.
    h.call(&["pane", "send-keys", &dash, "ctrl+c"]);
    h.wait("Ctrl-C closes the dashboard", || !h.exists(&dash));
    assert!(h.exists("w1:p1"));
}
