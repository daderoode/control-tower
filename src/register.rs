//! The lead skill registers itself: brew's post_install cannot write ~/.claude, so the
//! brew-installed watch adds the skill's marketplace to Claude Code, through `claude_path`,
//! when Claude Code has none (`decide_skill`, `skill_dir_of` and `register_skill`, below). A
//! `tower` that is not a brew install (a `cargo build`) registers nothing. The marketplace
//! is added at the formula's `opt` share path. `claude plugin marketplace add` silently
//! repoints a marketplace of the same name, so it is run only when there is none, or when it
//! is the GitHub source the 0.1.3 README added: anything else, such as a dev checkout, is
//! reported and left alone.
//!
//! `installed` says whether the running `tower` is the brew install, and `claude` runs a
//! bounded `claude` for the watch, the hooks and the terminal module.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ui::runner::{Outcome, Run, run_now};

/// The formula's name, and the lead skill's marketplace's in Claude Code.
pub const PLUGIN_ID: &str = "control-tower";

/// The brew prefix of a `tower` at `exe` (resolved): `<prefix>` of
/// `<prefix>/Cellar/control-tower/<version>/bin/tower`, `None` for any other place.
fn brew_prefix_of(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let version = bin.parent()?;
    let formula = version.parent()?;
    let cellar = formula.parent()?;
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n == name);
    if !(named(bin, "bin") && named(formula, PLUGIN_ID) && named(cellar, "Cellar")) {
        return None;
    }
    Some(cellar.parent()?.to_path_buf())
}

/// Whether the running `tower` is the brew install: its resolved path is in the Cellar.
/// Only that one creates or upgrades the default board.
pub fn installed() -> bool {
    (std::env::current_exe().and_then(|e| e.canonicalize()))
        .is_ok_and(|e| brew_prefix_of(&e).is_some())
}

/// The skill's plugin, as `claude plugin list` names it.
pub const SKILL_PLUGIN: &str = "control-tower@control-tower";

/// The GitHub source 0.1.3 told people to add; the brew path replaces it.
pub const SKILL_REPO: &str = "daderoode/control-tower";

/// How long the watch waits to try registering the skill again after it failed.
pub const RETRY_SKILL: Duration = Duration::from_secs(600);

/// How long one `claude` call may run before it is stopped: a `claude` may hang, on the
/// network say, and must not hold the registration forever.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(30);

/// Where Claude Code's `control-tower` marketplace comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Directory(PathBuf),
    Github(String),
    /// Any other kind (a git URL, say), by its kind.
    Other(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Directory(dir) => write!(f, "{}", dir.display()),
            Source::Github(repo) => write!(f, "GitHub {repo}"),
            Source::Other(kind) => write!(f, "a {kind} source"),
        }
    }
}

/// What to do about the skill, given the brew marketplace directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDecision {
    /// Added at the brew directory with the plugin installed.
    Already,
    /// Added at the brew directory, plugin missing: install it.
    Install,
    /// No marketplace, or the old GitHub one: add the brew directory, then install.
    Add,
    /// Added from somewhere else: leave it.
    Elsewhere(Source),
}

/// `found` is where Claude Code's `control-tower` marketplace comes from, if it has one;
/// `installed` whether the plugin is installed; `brew` the brew marketplace directory, as
/// Claude records it (unresolved).
pub fn decide_skill(found: Option<&Source>, installed: bool, brew: &Path) -> SkillDecision {
    match found {
        None => SkillDecision::Add,
        Some(Source::Directory(dir)) if dir == brew => {
            if installed {
                SkillDecision::Already
            } else {
                SkillDecision::Install
            }
        }
        // GitHub names ignore case, so DadeRoode/control-tower is the same repo.
        Some(Source::Github(repo)) if repo.eq_ignore_ascii_case(SKILL_REPO) => SkillDecision::Add,
        Some(other) => SkillDecision::Elsewhere(other.clone()),
    }
}

/// What `register_skill` found or did, as its log line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRegistered {
    /// This `tower` is not a brew install: nothing to register.
    NotBrew,
    Already(PathBuf),
    Installed(PathBuf),
    /// Added at the brew directory, replacing `from` if it was the old GitHub source.
    Added {
        brew: PathBuf,
        from: Option<Source>,
    },
    Elsewhere {
        source: Source,
        brew: PathBuf,
    },
}

impl fmt::Display for SkillRegistered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkillRegistered::NotBrew => write!(
                f,
                "not registering the lead skill: this tower is not a brew install"
            ),
            SkillRegistered::Already(dir) => write!(
                f,
                "the lead skill {SKILL_PLUGIN} is registered at {}",
                dir.display()
            ),
            SkillRegistered::Installed(dir) => write!(
                f,
                "installed the lead skill {SKILL_PLUGIN} from {}",
                dir.display()
            ),
            SkillRegistered::Added { brew, from: None } => write!(
                f,
                "registered the lead skill {SKILL_PLUGIN} at {}",
                brew.display()
            ),
            SkillRegistered::Added {
                brew,
                from: Some(from),
            } => write!(
                f,
                "registered the lead skill {SKILL_PLUGIN} at {}, in place of {from}",
                brew.display()
            ),
            SkillRegistered::Elsewhere { source, brew } => write!(
                f,
                "the lead skill's marketplace {PLUGIN_ID} is added from {source}, not the brew \
                 {}; left as it is. To use the brew one: claude plugin marketplace add {} && \
                 claude plugin install {SKILL_PLUGIN}",
                brew.display(),
                brew.display()
            ),
        }
    }
}

/// The brew marketplace directory of a `tower` at `exe` (resolved, so inside the Cellar),
/// through the `opt` link every upgrade repoints:
/// `<prefix>/Cellar/control-tower/<version>/bin/tower` gives
/// `<prefix>/opt/control-tower/share/control-tower/claude`. `None` for any other place.
pub fn skill_dir_of(exe: &Path) -> Option<PathBuf> {
    let prefix = brew_prefix_of(exe)?;
    Some(
        prefix
            .join("opt")
            .join(PLUGIN_ID)
            .join("share")
            .join(PLUGIN_ID)
            .join("claude"),
    )
}

/// The brew marketplace directory of the running `tower`, if it is a brew install.
pub fn skill_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    skill_dir_of(&exe)
}

/// The `claude` to ask: `TOWER_CLAUDE`, else `claude` on the PATH. The brew service has
/// only the PATH its formula gives it, so `TOWER_CLAUDE` is for a watch run by hand.
pub fn claude_path() -> PathBuf {
    std::env::var_os("TOWER_CLAUDE")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from("claude"), PathBuf::from)
}

#[derive(Deserialize)]
struct RawMarketplace {
    name: String,
    source: String,
    path: Option<PathBuf>,
    repo: Option<String>,
}

/// Where `claude plugin marketplace list --json` has `control-tower` from, if anywhere.
pub fn parse_marketplace(json: &str) -> Result<Option<Source>> {
    let list: Vec<RawMarketplace> = serde_json::from_str(json)
        .context("claude plugin marketplace list said something unexpected")?;
    let Some(m) = list.into_iter().find(|m| m.name == PLUGIN_ID) else {
        return Ok(None);
    };
    Ok(Some(match (m.source.as_str(), m.path, m.repo) {
        ("directory", Some(path), _) => Source::Directory(path),
        ("github", _, Some(repo)) => Source::Github(repo),
        _ => Source::Other(m.source),
    }))
}

#[derive(Deserialize)]
struct RawInstalled {
    id: String,
}

/// Whether `claude plugin list --json` has the skill's plugin, enabled or not.
pub fn parse_installed(json: &str) -> Result<bool> {
    let list: Vec<RawInstalled> =
        serde_json::from_str(json).context("claude plugin list said something unexpected")?;
    Ok(list.iter().any(|p| p.id == SKILL_PLUGIN))
}

/// Runs `claude <args>`, bounded by `CLAUDE_TIMEOUT`, unless `stop` is set; its stdout, or
/// an error naming the command and what claude said.
pub fn claude(exe: &Path, db: &Path, stop: &AtomicBool, args: &[&str]) -> Result<String> {
    claude_within(exe, db, stop, args, CLAUDE_TIMEOUT)
}

/// `claude`, bounded by `timeout` instead: for a caller with its own deadline, like a
/// SessionStart hook, which asks more than once within the hook's own limit.
pub fn claude_within(
    exe: &Path,
    db: &Path,
    stop: &AtomicBool,
    args: &[&str],
    timeout: Duration,
) -> Result<String> {
    let command = args.iter().take(3).copied().collect::<Vec<_>>().join(" ");
    if stop.load(Ordering::Relaxed) {
        bail!("stopped before claude {command}");
    }
    let outcome = run_now(&Run {
        exe: exe.to_path_buf(),
        args: args.iter().map(|a| a.to_string()).collect(),
        db: db.to_path_buf(),
        env: Vec::new(),
        stdin: None,
        timeout,
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
        } => {
            let said = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            let how = code.map_or("was ended by a signal".into(), |c| format!("exited {c}"));
            bail!("claude {command} {how}: {}", said.trim())
        }
        Outcome::TimedOut { after, .. } => bail!(
            "claude {command} did not finish in {} s and was stopped",
            after.as_secs()
        ),
        Outcome::Failed(why) => bail!("{why}"),
    }
}

/// Registers the skill's marketplace at `brew` (the brew marketplace directory, `None` for
/// a `tower` that is not a brew install) with the Claude Code at `exe`, and installs its
/// plugin, when Claude Code has no `control-tower` marketplace or has the old GitHub one.
/// Each claude call is bounded, and none starts once `stop` is set. Fails when the brew
/// install has no marketplace, or claude cannot be asked or refuses; a failed add installs
/// nothing.
pub fn register_skill(
    exe: &Path,
    db: &Path,
    brew: Option<&Path>,
    stop: &AtomicBool,
) -> Result<SkillRegistered> {
    let Some(brew) = brew else {
        return Ok(SkillRegistered::NotBrew);
    };
    if !brew.join(".claude-plugin/marketplace.json").is_file() {
        bail!(
            "the brew install has no lead skill marketplace at {}; brew reinstall \
             control-tower restores it",
            brew.display()
        );
    }
    let path = std::env::var_os("PATH");
    if crate::terminal::find_exe(exe, path.as_deref()).is_none() {
        bail!(
            "{}; link it into Homebrew's bin (ln -s <its path> \"$(brew --prefix)/bin/claude\") \
             and run brew services restart control-tower",
            crate::terminal::not_found("claude", exe, path.as_deref())
        );
    }
    let dir = brew
        .to_str()
        .context("the brew marketplace directory is not UTF-8")?;
    let found = parse_marketplace(&claude(
        exe,
        db,
        stop,
        &["plugin", "marketplace", "list", "--json"],
    )?)?;
    let installed = match &found {
        Some(Source::Directory(d)) if d == brew => {
            parse_installed(&claude(exe, db, stop, &["plugin", "list", "--json"])?)?
        }
        _ => false,
    };
    let install = || claude(exe, db, stop, &["plugin", "install", SKILL_PLUGIN]);
    Ok(match decide_skill(found.as_ref(), installed, brew) {
        SkillDecision::Already => SkillRegistered::Already(brew.to_path_buf()),
        SkillDecision::Install => {
            install()?;
            SkillRegistered::Installed(brew.to_path_buf())
        }
        SkillDecision::Add => {
            claude(exe, db, stop, &["plugin", "marketplace", "add", dir])?;
            install()?;
            SkillRegistered::Added {
                brew: brew.to_path_buf(),
                from: found,
            }
        }
        SkillDecision::Elsewhere(source) => SkillRegistered::Elsewhere {
            source,
            brew: brew.to_path_buf(),
        },
    })
}

/// The watch's skill registration: tried until it succeeds, at most every `RETRY_SKILL`
/// after a failure, and a failure said once until what it says changes. Each try runs on a
/// thread of its own, so a slow or hung claude never holds the watch's looks; one try at a
/// time. Once `stop` is set (SIGTERM) a try starts no more claude calls.
pub struct Skill {
    claude: PathBuf,
    brew: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    done: bool,
    failed: Option<(Instant, String)>,
    /// The try under way, if any: its outcome arrives here.
    running: Option<Receiver<Result<SkillRegistered>>>,
}

impl Skill {
    pub fn new(claude: PathBuf, brew: Option<PathBuf>, stop: Arc<AtomicBool>) -> Skill {
        Skill {
            claude,
            brew,
            stop,
            done: false,
            failed: None,
            running: None,
        }
    }

    /// Never waits: takes the outcome of the try under way if it has landed, else starts a
    /// try if one is due at `now`. The line to log, if any.
    pub fn tick(&mut self, db: &Path, now: Instant) -> Option<String> {
        if let Some(rx) = &self.running {
            let outcome = match rx.try_recv() {
                Ok(outcome) => outcome,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!(
                    "the registration stopped without an answer"
                )),
            };
            self.running = None;
            return self.landed(outcome, now);
        }
        let due = self
            .failed
            .as_ref()
            .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= RETRY_SKILL);
        if self.done || !due {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let (claude, brew, stop) = (self.claude.clone(), self.brew.clone(), self.stop.clone());
        let db = db.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(register_skill(&claude, &db, brew.as_deref(), &stop));
        });
        self.running = Some(rx);
        None
    }

    /// Whether a try is under way.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Records a try's outcome, seen at `now`; the line to log, if any.
    fn landed(&mut self, outcome: Result<SkillRegistered>, now: Instant) -> Option<String> {
        match outcome {
            Ok(done) => {
                self.done = true;
                Some(done.to_string())
            }
            // Stopping is not a failure to report: the watch says it stopped.
            Err(_) if self.stop.load(Ordering::Relaxed) => None,
            Err(e) => {
                let said = format!("{e:#}");
                let again = self.failed.as_ref().is_some_and(|(_, s)| *s == said);
                self.failed = Some((now, said.clone()));
                (!again).then(|| {
                    format!(
                        "could not register the lead skill, trying again every {} min (said \
                         once until it changes): {said}",
                        RETRY_SKILL.as_secs() / 60
                    )
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_brew_prefix_only_for_a_cellar_tower() {
        assert_eq!(
            brew_prefix_of(Path::new(
                "/opt/homebrew/Cellar/control-tower/0.2.0/bin/tower"
            )),
            Some(PathBuf::from("/opt/homebrew"))
        );
        assert_eq!(
            brew_prefix_of(Path::new(
                "/usr/local/Cellar/control-tower/0.2.0_1/bin/tower"
            )),
            Some(PathBuf::from("/usr/local"))
        );
        for not in [
            "/Users/me/control-tower/target/release/tower",
            "/Users/me/.cargo/bin/tower",
            "/opt/homebrew/Cellar/other/1.0/bin/tower",
            "/opt/homebrew/Cellar/control-tower/0.2.0/libexec/tower",
            "/tower",
            "tower",
        ] {
            assert_eq!(brew_prefix_of(Path::new(not)), None, "{not}");
        }
    }

    #[test]
    fn decides_the_skill_by_where_its_marketplace_comes_from() {
        let brew = Path::new("/opt/homebrew/opt/control-tower/share/control-tower/claude");
        let at = |p: &Path| Source::Directory(p.to_path_buf());
        assert_eq!(decide_skill(None, false, brew), SkillDecision::Add);
        assert_eq!(
            decide_skill(Some(&at(brew)), true, brew),
            SkillDecision::Already
        );
        assert_eq!(
            decide_skill(Some(&at(brew)), false, brew),
            SkillDecision::Install
        );
        // Claude records the path as added: a trailing slash is the same place.
        let slash = PathBuf::from(format!("{}/", brew.display()));
        assert_eq!(
            decide_skill(Some(&at(&slash)), true, brew),
            SkillDecision::Already
        );
        let old = Source::Github(SKILL_REPO.into());
        assert_eq!(decide_skill(Some(&old), true, brew), SkillDecision::Add);
        let cased = Source::Github("DadeRoode/Control-Tower".into());
        assert_eq!(decide_skill(Some(&cased), true, brew), SkillDecision::Add);
        for other in [
            at(Path::new("/Users/me/control-tower")),
            // The resolved Cellar path is deleted by the next upgrade: not the brew one.
            at(Path::new(
                "/opt/homebrew/Cellar/control-tower/0.1.4/share/control-tower/claude",
            )),
            Source::Github("someone/control-tower".into()),
            Source::Other("git".into()),
        ] {
            assert_eq!(
                decide_skill(Some(&other), true, brew),
                SkillDecision::Elsewhere(other.clone())
            );
        }
    }

    #[test]
    fn finds_the_skill_dir_through_opt_only_for_a_cellar_tower() {
        assert_eq!(
            skill_dir_of(Path::new(
                "/opt/homebrew/Cellar/control-tower/0.2.0/bin/tower"
            )),
            Some(PathBuf::from(
                "/opt/homebrew/opt/control-tower/share/control-tower/claude"
            ))
        );
        for not in [
            "/Users/me/control-tower/target/release/tower",
            "/opt/homebrew/Cellar/other/1.0/bin/tower",
            "tower",
        ] {
            assert_eq!(skill_dir_of(Path::new(not)), None, "{not}");
        }
    }

    #[test]
    fn parses_claudes_marketplace_and_plugin_lists() {
        // As claude 2.1.309 prints them.
        let dir = r#"[{"name":"other","source":"directory","path":"/x/other","installLocation":"/x/other"},
            {"name":"control-tower","source":"directory","path":"/x/claude","installLocation":"/x/claude"}]"#;
        assert_eq!(
            parse_marketplace(dir).unwrap(),
            Some(Source::Directory("/x/claude".into()))
        );
        let gh = r#"[{"name":"control-tower","source":"github","repo":"daderoode/control-tower",
            "installLocation":"/h/.claude/plugins/marketplaces/control-tower"}]"#;
        assert_eq!(
            parse_marketplace(gh).unwrap(),
            Some(Source::Github(SKILL_REPO.into()))
        );
        let git = r#"[{"name":"control-tower","source":"git","url":"https://x/y.git"}]"#;
        assert_eq!(
            parse_marketplace(git).unwrap(),
            Some(Source::Other("git".into()))
        );
        assert_eq!(parse_marketplace("[]").unwrap(), None);
        assert!(parse_marketplace("Not logged in").is_err());

        let plugins = r#"[{"id":"control-tower@control-tower","version":"0.1.3","enabled":false}]"#;
        assert!(parse_installed(plugins).unwrap());
        assert!(!parse_installed(r#"[{"id":"control-tower@elsewhere"}]"#).unwrap());
        assert!(!parse_installed("[]").unwrap());
        assert!(parse_installed("{}").is_err());
    }

    /// A fake claude in a temp dir: the two lists print `markets.json` and `plugins.json`
    /// (the marketplace list held until `release` exists, then marking `finished`, when
    /// `slow` exists),
    /// `add` and `install` succeed unless `refuse-add` or `refuse-install` exists, and every
    /// call is appended to `calls.log`.
    fn fake_claude() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("claude");
        crate::test_exe::executable(&exe, "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$*\" >> \"$d/calls.log\"\n\
             case \"$*\" in\n\
             \"plugin marketplace list --json\") if [ -e \"$d/slow\" ]; then HOLD; : > \"$d/finished\"; fi; cat \"$d/markets.json\" ;;\n\
             \"plugin list --json\") cat \"$d/plugins.json\" ;;\n\
             \"plugin marketplace add \"*) if [ -e \"$d/refuse-add\" ]; then echo 'Path does not exist' >&2; exit 1; fi ;;\n\
             \"plugin install \"*) if [ -e \"$d/refuse-install\" ]; then echo 'Not logged in' >&2; exit 1; fi ;;\n\
             *) echo \"unexpected: $*\" >&2; exit 2 ;;\nesac\n"
                .replace("HOLD", &crate::watch::hold_until_released()));
        (dir, exe)
    }

    /// A brew marketplace directory at `rel` under `dir`, as the formula installs it.
    fn marketplace(dir: &Path, rel: &str) -> PathBuf {
        let brew = dir.join(rel);
        std::fs::create_dir_all(brew.join(".claude-plugin")).unwrap();
        std::fs::write(brew.join(".claude-plugin/marketplace.json"), "{}").unwrap();
        brew
    }

    fn markets(dir: &Path, json: &str) {
        std::fs::write(dir.join("markets.json"), json).unwrap();
    }

    fn installed(dir: &Path, yes: bool) {
        let json = if yes {
            r#"[{"id":"control-tower@control-tower","enabled":true}]"#
        } else {
            "[]"
        };
        std::fs::write(dir.join("plugins.json"), json).unwrap();
    }

    /// The add and install calls made since the last look, which are forgotten.
    fn changes(dir: &Path) -> Vec<String> {
        let log = dir.join("calls.log");
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        calls
            .lines()
            .filter(|c| !c.ends_with("--json"))
            .map(String::from)
            .collect()
    }

    #[test]
    fn registers_the_skill_once_and_never_over_another_marketplace() {
        let (dir, exe) = fake_claude();
        let d = dir.path();
        let db = d.join("board.db");
        let brew = marketplace(d, "opt/control-tower/share/control-tower/claude");
        let go = AtomicBool::new(false);
        let at = |p: &Path| {
            format!(
                r#"[{{"name":"control-tower","source":"directory","path":"{}"}}]"#,
                p.display()
            )
        };
        let add = format!("plugin marketplace add {}", brew.display());
        let install = format!("plugin install {SKILL_PLUGIN}");

        // Not a brew install: claude is not asked at all.
        assert_eq!(
            register_skill(&exe, &db, None, &go).unwrap(),
            SkillRegistered::NotBrew
        );
        assert!(!d.join("calls.log").exists());

        markets(d, "[]");
        assert_eq!(
            register_skill(&exe, &db, Some(&brew), &go).unwrap(),
            SkillRegistered::Added {
                brew: brew.clone(),
                from: None
            }
        );
        assert_eq!(changes(d), [add.clone(), install.clone()]);

        markets(d, &at(&brew));
        installed(d, true);
        assert_eq!(
            register_skill(&exe, &db, Some(&brew), &go).unwrap(),
            SkillRegistered::Already(brew.clone())
        );
        assert!(changes(d).is_empty());

        installed(d, false);
        assert_eq!(
            register_skill(&exe, &db, Some(&brew), &go).unwrap(),
            SkillRegistered::Installed(brew.clone())
        );
        assert_eq!(changes(d), [install.as_str()]);

        // The GitHub source 0.1.3 added moves to the brew path.
        markets(
            d,
            r#"[{"name":"control-tower","source":"github","repo":"daderoode/control-tower"}]"#,
        );
        let got = register_skill(&exe, &db, Some(&brew), &go).unwrap();
        assert_eq!(
            got,
            SkillRegistered::Added {
                brew: brew.clone(),
                from: Some(Source::Github(SKILL_REPO.into()))
            }
        );
        assert!(
            got.to_string()
                .contains("in place of GitHub daderoode/control-tower"),
            "{got}"
        );
        assert_eq!(changes(d), [add.clone(), install.clone()]);

        // A checkout, or any other GitHub repo, is left alone: no add, no install.
        let dev = d.join("checkout");
        for other in [
            at(&dev),
            r#"[{"name":"control-tower","source":"github","repo":"someone/control-tower"}]"#.into(),
        ] {
            markets(d, &other);
            let got = register_skill(&exe, &db, Some(&brew), &go).unwrap();
            assert!(matches!(got, SkillRegistered::Elsewhere { .. }), "{got:?}");
            assert!(got.to_string().contains("left as it is"), "{got}");
            assert!(got.to_string().contains(&add), "{got}");
            assert!(changes(d).is_empty());
        }
    }

    #[test]
    fn a_claude_that_fails_is_an_error_and_a_failed_add_installs_nothing() {
        let (dir, exe) = fake_claude();
        let d = dir.path();
        let db = d.join("board.db");
        let brew = marketplace(d, "claude-dir");
        let go = AtomicBool::new(false);
        // No markets.json: the list prints nothing JSON.
        assert!(register_skill(&exe, &db, Some(&brew), &go).is_err());

        markets(d, "[]");
        std::fs::write(d.join("refuse-add"), "").unwrap();
        let err = register_skill(&exe, &db, Some(&brew), &go).unwrap_err();
        assert!(
            format!("{err:#}").contains("Path does not exist"),
            "{err:#}"
        );
        assert_eq!(
            changes(d),
            [format!("plugin marketplace add {}", brew.display())]
        );

        std::fs::remove_file(d.join("refuse-add")).unwrap();
        std::fs::write(d.join("refuse-install"), "").unwrap();
        let err = register_skill(&exe, &db, Some(&brew), &go).unwrap_err();
        assert!(
            format!("{err:#}").contains(
                "claude plugin install control-tower@control-tower exited 1: Not logged in"
            ),
            "{err:#}"
        );

        changes(d);
        let missing = d.join("no-claude");
        let err = register_skill(&missing, &db, Some(&brew), &go).unwrap_err();
        let said = format!("{err:#}");
        assert!(
            said.starts_with(&format!("claude not found: tried {}", missing.display())),
            "{said}"
        );
        // The brew service cannot be given TOWER_CLAUDE: the advice is to link claude.
        assert!(!said.contains("TOWER_CLAUDE"), "{said}");
        assert!(
            said.contains("brew services restart control-tower"),
            "{said}"
        );
        assert_eq!(changes(d), Vec::<String>::new());

        // A brew install without its marketplace asks claude nothing.
        let empty = d.join("empty");
        let err = register_skill(&exe, &db, Some(&empty), &go).unwrap_err();
        assert!(
            format!("{err:#}").contains("no lead skill marketplace at"),
            "{err:#}"
        );
        assert!(changes(d).is_empty());

        // Once the watch is stopping, no claude call starts.
        let stopping = AtomicBool::new(true);
        let err = register_skill(&exe, &db, Some(&brew), &stopping).unwrap_err();
        assert!(
            format!("{err:#}").contains("stopped before claude"),
            "{err:#}"
        );
        assert!(changes(d).is_empty());
    }

    /// Ticks `skill` at `now` until the try it started, if any, has landed: the line to log.
    fn settle(skill: &mut Skill, db: &Path, now: Instant) -> Option<String> {
        let mut said = skill.tick(db, now);
        let deadline = Instant::now() + crate::watch::TEST_WAIT;
        while skill.busy() {
            assert!(Instant::now() < deadline, "the try never landed");
            std::thread::sleep(Duration::from_millis(10));
            said = skill.tick(db, now);
        }
        said
    }

    #[test]
    fn the_watch_retries_the_skill_every_ten_minutes_and_says_a_failure_once() {
        let (dir, exe) = fake_claude();
        let d = dir.path();
        let db = d.join("board.db");
        let brew = marketplace(d, "claude-dir");
        let stop = Arc::new(AtomicBool::new(false));
        let mut skill = Skill::new(exe.clone(), Some(brew.clone()), Arc::clone(&stop));
        let t0 = Instant::now();
        markets(d, "[]");
        std::fs::write(d.join("refuse-install"), "").unwrap();

        let said = settle(&mut skill, &db, t0).unwrap();
        assert!(said.contains("could not register the lead skill"), "{said}");
        assert_eq!(changes(d).len(), 2);
        // Not due yet: claude is not run.
        assert_eq!(settle(&mut skill, &db, t0 + RETRY_SKILL / 2), None);
        assert!(changes(d).is_empty());
        // Due, failing the same way: tried, not said again.
        assert_eq!(settle(&mut skill, &db, t0 + RETRY_SKILL), None);
        assert_eq!(changes(d).len(), 2);
        // A different failure is said.
        std::fs::remove_file(d.join("markets.json")).unwrap();
        let said = settle(&mut skill, &db, t0 + RETRY_SKILL * 2).unwrap();
        assert!(said.contains("marketplace list exited 1"), "{said}");

        std::fs::remove_file(d.join("refuse-install")).unwrap();
        markets(d, "[]");
        let said = settle(&mut skill, &db, t0 + RETRY_SKILL * 3).unwrap();
        assert!(said.starts_with("registered the lead skill"), "{said}");
        // Done: never asked again.
        assert_eq!(settle(&mut skill, &db, t0 + RETRY_SKILL * 9), None);
        changes(d);
        assert_eq!(settle(&mut skill, &db, t0 + RETRY_SKILL * 10), None);
        assert!(changes(d).is_empty() && !d.join("calls.log").exists());

        // A Skill stopping mid-try says nothing: the watch logs that it stopped.
        let mut stopped = Skill::new(exe, Some(brew.clone()), Arc::new(AtomicBool::new(true)));
        assert_eq!(settle(&mut stopped, &db, t0), None);
        assert!(changes(d).is_empty());

        let mut not_brew = Skill::new(d.join("no-claude"), None, Arc::clone(&stop));
        let said = settle(&mut not_brew, &db, t0).unwrap();
        assert!(said.contains("not a brew install"), "{said}");
        assert_eq!(settle(&mut not_brew, &db, t0 + RETRY_SKILL), None);
    }

    #[test]
    fn a_slow_claude_never_holds_the_watch_and_one_try_runs_at_a_time() {
        let (dir, exe) = fake_claude();
        let d = dir.path();
        let db = d.join("board.db");
        let brew = marketplace(d, "claude-dir");
        markets(d, "[]");
        std::fs::write(d.join("slow"), "").unwrap();
        let mut skill = Skill::new(exe, Some(brew), Arc::new(AtomicBool::new(false)));
        let t0 = Instant::now();

        // The list is held until released: tick returns while it is, and keeps doing so.
        assert_eq!(skill.tick(&db, t0), None);
        assert!(skill.busy());
        // Even past a retry's wait, no second try starts while one runs.
        assert_eq!(skill.tick(&db, t0 + RETRY_SKILL * 5), None);

        // Released only now, after both ticks returned: the held claude then finishes and
        // the try registers. Had a tick waited for the try, the claude would have run into
        // CLAUDE_TIMEOUT before any release, been stopped short of `finished`, and the try
        // failed.
        std::fs::write(d.join("release"), "").unwrap();
        let said = settle(&mut skill, &db, t0);
        assert!(said.unwrap().starts_with("registered the lead skill"));
        assert!(
            d.join("finished").exists(),
            "claude was stopped before the release"
        );
        let calls = std::fs::read_to_string(d.join("calls.log")).unwrap();
        assert_eq!(
            calls.matches("plugin marketplace list").count(),
            1,
            "{calls}"
        );
        assert_eq!(skill.tick(&db, t0 + RETRY_SKILL * 9), None);
        assert!(!skill.busy());
    }
}
