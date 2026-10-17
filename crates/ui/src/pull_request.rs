//! A branch's pull request and its checks, as one small chip on an agent's
//! row. Asked of GitHub through the `gh` the person installed and signed in
//! to — this reads no token and stores nothing — on a background thread,
//! each lookup bounded by a timeout. A project whose remote is not GitHub,
//! a default branch, a missing or signed-out `gh`: no chip.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a branch's pull request is asked about again.
pub const REFRESH: Duration = Duration::from_secs(120);
/// How old an answer may be when the window regains focus before it is
/// asked again: coming back to the window is when CI is looked at, but a
/// quick switch away and back is not a reason to ask.
pub const FOCUS_REFRESH: Duration = Duration::from_secs(15);
/// The most branches one round asks about; the rest wait for the next.
pub const ROUND: usize = 8;
/// How long one `gh` call may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// A round unanswered this long is taken as lost, so one dropped answer
/// cannot stop the chips for good: its calls run side by side, each
/// bounded by [`TIMEOUT`].
const ROUND_LOST: Duration = Duration::from_secs(60);

/// What the chip says about the pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Open, with no checks reported.
    Open,
    Passed,
    Running,
    Failed,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub status: Status,
    /// The pull request's head commit, not the current branch ref. Missing
    /// identity cannot establish that the checkout's current work was merged.
    pub head: Option<String>,
}

impl PullRequest {
    pub fn merged_head_matches(&self, head: Option<&str>) -> bool {
        self.status == Status::Merged
            && self
                .head
                .as_deref()
                .zip(head)
                .is_some_and(|(merged, current)| !merged.is_empty() && merged == current)
    }

    /// The chip's words for assistive technology and the pointer.
    pub fn spoken(&self) -> String {
        let state = match self.status {
            Status::Open => "open",
            Status::Passed => "checks passed",
            Status::Running => "checks running",
            Status::Failed => "checks failed",
            Status::Merged => "merged",
        };
        format!("Pull request #{}, {state}", self.number)
    }
}

/// The branches no pull request is looked up for: a project's own line.
pub fn is_default_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "develop")
}

/// One branch of one project.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub root: PathBuf,
    pub branch: String,
}

impl Key {
    /// The branch worth asking about, or `None` for a default branch or a
    /// name `gh` could not be handed safely.
    pub fn new(root: &Path, branch: &str) -> Option<Self> {
        (!branch.is_empty()
            && !branch.starts_with('-')
            && !branch.chars().any(char::is_control)
            && !is_default_branch(branch))
        .then(|| Key {
            root: root.to_owned(),
            branch: branch.to_owned(),
        })
    }
}

#[derive(Default)]
struct Entry {
    pull: Option<PullRequest>,
    asked: Option<Instant>,
}

/// What is known per branch, and whether a round of lookups is out.
#[derive(Default)]
pub struct Cache {
    entries: BTreeMap<Key, Entry>,
    /// When the round now out was started.
    in_flight: Option<Instant>,
}

impl Cache {
    pub fn get(&self, key: &Key) -> Option<&PullRequest> {
        self.entries.get(key).and_then(|e| e.pull.as_ref())
    }

    /// The branches to ask about now, oldest answer first, at most
    /// [`ROUND`]: those never answered, and those answered longer ago than
    /// [`REFRESH`] — or [`FOCUS_REFRESH`] when the window has just regained
    /// focus. Branches no longer wanted are forgotten. One round at a time:
    /// nothing is due while one is out (until it is lost), and a non-empty
    /// answer starts one.
    pub fn due(&mut self, wanted: &BTreeSet<Key>, now: Instant, focus: bool) -> Vec<Key> {
        self.entries.retain(|key, _| wanted.contains(key));
        if self
            .in_flight
            .is_some_and(|since| now.saturating_duration_since(since) < ROUND_LOST)
        {
            return Vec::new();
        }
        let bound = if focus { FOCUS_REFRESH } else { REFRESH };
        let mut due: Vec<(Option<Instant>, &Key)> = wanted
            .iter()
            .map(|key| (self.entries.get(key).and_then(|e| e.asked), key))
            .filter(|(asked, _)| asked.is_none_or(|at| now.saturating_duration_since(at) >= bound))
            .collect();
        due.sort();
        let due: Vec<Key> = due
            .into_iter()
            .take(ROUND)
            .map(|(_, key)| key.clone())
            .collect();
        self.in_flight = (!due.is_empty()).then_some(now);
        due
    }

    /// A round's answers. A failed lookup keeps what was known before, so
    /// a network blip does not take a chip away; either way the branch is
    /// not asked about again until the bound has passed.
    pub fn answered(
        &mut self,
        answers: Vec<(Key, Result<Option<PullRequest>, String>)>,
        now: Instant,
    ) {
        self.in_flight = None;
        for (key, answer) in answers {
            let entry = self.entries.entry(key).or_default();
            entry.asked = Some(now);
            if let Ok(pull) = answer {
                entry.pull = pull;
            }
        }
    }
}

/// Ask about every branch of a round at once, each on its own thread.
pub fn lookup_all(keys: Vec<Key>) -> Vec<(Key, Result<Option<PullRequest>, String>)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = keys.iter().map(|key| scope.spawn(|| lookup(key))).collect();
        keys.iter()
            .cloned()
            .zip(handles)
            .map(|(key, handle)| {
                let answer = handle
                    .join()
                    .unwrap_or_else(|_| Err("the lookup stopped".into()));
                (key, answer)
            })
            .collect()
    })
}

/// The pull request of one branch: `Ok(None)` when the project is not on
/// GitHub or the branch has none; `Err` when `gh` could not say.
pub fn lookup(key: &Key) -> Result<Option<PullRequest>, String> {
    let Some(repo) = github_repository(&key.root) else {
        return Ok(None);
    };
    let gh = gh().ok_or("gh is not installed")?;
    let argv = vec![
        gh.to_string_lossy().into_owned(),
        "pr".into(),
        "list".into(),
        "--repo".into(),
        repo,
        format!("--head={}", key.branch),
        "--state".into(),
        "all".into(),
        "--limit".into(),
        "5".into(),
        "--json".into(),
        "number,state,url,headRefOid,statusCheckRollup".into(),
    ];
    let quiet = Some(std::ffi::OsStr::new("1"));
    let output = agentdocker_host::command::run_with_env(
        &key.root,
        &argv,
        TIMEOUT,
        &[
            ("GH_PROMPT_DISABLED", quiet),
            ("GH_NO_UPDATE_NOTIFIER", quiet),
            ("GH_SPINNER_DISABLED", quiet),
            ("NO_COLOR", quiet),
        ],
    )
    .map_err(|e| e.to_string())?;
    if !output.success {
        return Err(output.text.trim().chars().take(200).collect());
    }
    parse(&output.stdout)
}

/// `gh` where a shell would find it, or where Homebrew puts it: an app
/// started from the Dock or Finder has a minimal `PATH`.
fn gh() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
            "/usr/bin",
        ]
        .map(PathBuf::from),
    );
    agentdocker_host::command::find_program(&dirs, "gh")
}

#[derive(serde::Deserialize)]
struct RawPull {
    number: u64,
    state: String,
    url: String,
    #[serde(default, rename = "headRefOid")]
    head: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    checks: Option<Vec<RawCheck>>,
}

/// A check run (`status`, then `conclusion`) or a commit status (`state`).
#[derive(serde::Deserialize)]
struct RawCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Passed,
    Running,
    Failed,
}

impl RawCheck {
    fn outcome(&self) -> Outcome {
        if let Some(state) = self.state.as_deref() {
            return match state {
                "SUCCESS" => Outcome::Passed,
                "ERROR" | "FAILURE" => Outcome::Failed,
                _ => Outcome::Running,
            };
        }
        if self.status.as_deref() != Some("COMPLETED") {
            return Outcome::Running;
        }
        match self.conclusion.as_deref().unwrap_or_default() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" | "STALE" => Outcome::Passed,
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                Outcome::Failed
            }
            _ => Outcome::Running,
        }
    }
}

/// `gh pr list --json number,state,url,headRefOid,statusCheckRollup` read as the chip:
/// the open pull request if there is one, else the newest if it merged,
/// else none — a closed one says nothing. Any failed check is a failure,
/// then any unfinished one is running; skipped and neutral ones pass.
pub fn parse(json: &str) -> Result<Option<PullRequest>, String> {
    let mut pulls: Vec<RawPull> =
        serde_json::from_str(json).map_err(|e| format!("unreadable answer from gh: {e}"))?;
    pulls.sort_by_key(|p| std::cmp::Reverse(p.number));
    let chosen = match pulls.iter().position(|p| p.state == "OPEN") {
        Some(open) => pulls.swap_remove(open),
        None => match pulls.into_iter().next() {
            Some(newest) if newest.state == "MERGED" => newest,
            _ => return Ok(None),
        },
    };
    if !is_github_url(&chosen.url) {
        return Ok(None);
    }
    let status = if chosen.state == "MERGED" {
        Status::Merged
    } else {
        match chosen
            .checks
            .unwrap_or_default()
            .iter()
            .map(RawCheck::outcome)
            .max()
        {
            None => Status::Open,
            Some(Outcome::Passed) => Status::Passed,
            Some(Outcome::Running) => Status::Running,
            Some(Outcome::Failed) => Status::Failed,
        }
    };
    Ok(Some(PullRequest {
        number: chosen.number,
        url: chosen.url,
        status,
        head: chosen.head,
    }))
}

/// Only a GitHub page is opened from a chip.
pub fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `owner/name` of the project's GitHub repository, read from its git
/// configuration without running git. Remotes are preferred as `gh`
/// prefers them: `upstream`, `github`, `origin`, then any other.
pub fn github_repository(root: &Path) -> Option<String> {
    let (_, common) = agentdocker_host::vcs::git_dirs(root)?;
    let config = common.join("config");
    let metadata = std::fs::metadata(&config).ok()?;
    if !metadata.is_file() || metadata.len() > 256 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(config).ok()?;
    let mut remotes: Vec<(String, String)> = remotes(&text)
        .into_iter()
        .filter_map(|(name, url)| Some((name, github_repo(&url)?)))
        .collect();
    let rank = |name: &str| match name {
        "upstream" => 0,
        "github" => 1,
        "origin" => 2,
        _ => 3,
    };
    remotes.sort_by_key(|(name, _)| rank(name));
    remotes.into_iter().next().map(|(_, repo)| repo)
}

/// Each remote's name and first URL in a git configuration file.
fn remotes(config: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut remote: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            remote = line
                .strip_prefix('[')
                .and_then(|l| l.strip_suffix(']'))
                .and_then(|section| {
                    let (kind, name) = section.split_once(char::is_whitespace)?;
                    kind.eq_ignore_ascii_case("remote")
                        .then(|| name.trim().trim_matches('"').to_owned())
                });
            continue;
        }
        let Some(name) = &remote else { continue };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") && !found.iter().any(|(n, _)| n == name) {
            found.push((name.clone(), value.trim().trim_matches('"').to_owned()));
        }
    }
    found
}

/// `owner/name` of a github.com remote URL in any of the forms git takes
/// (`git@github.com:o/n.git`, `https://github.com/o/n`, `ssh://…`), or
/// `None` for another host.
pub fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => url.split_once(':')?,
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next()?.to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "github.com" | "www.github.com" | "ssh.github.com"
    ) {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let part = |s: &str| {
        !s.is_empty()
            && !s.starts_with(['-', '.'])
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (part(owner) && part(name)).then(|| format!("{owner}/{name}"))
}

/// Open a pull request's page in the person's browser: its URL as one
/// argument, never in a shell string.
pub fn open(url: &str) -> Result<(), String> {
    if !is_github_url(url) {
        return Err("Only a GitHub pull request page can be opened from here.".into());
    }
    #[cfg(target_os = "macos")]
    {
        let output = agentdocker_host::command::run(
            Path::new("/"),
            &["/usr/bin/open".to_owned(), url.to_owned()],
            Duration::from_secs(8),
        )
        .map_err(|e| format!("Could not open the pull request: {e}"))?;
        if !output.success {
            return Err(format!(
                "Could not open the pull request: {}",
                output.text.trim()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        // xdg-open may wait for the browser it starts; reap it whenever
        // it ends rather than holding it to a timeout that would kill it.
        let mut child = std::process::Command::new("xdg-open")
            .arg(url)
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Could not open the pull request: {e}"))?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("Opening a pull request is not available on this platform yet.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(number: u64, state: &str, checks: &str) -> String {
        format!(
            r#"{{"number":{number},"state":"{state}","url":"https://github.com/o/n/pull/{number}","statusCheckRollup":[{checks}]}}"#
        )
    }
    const PASSED: &str = r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}"#;
    const SKIPPED: &str =
        r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}"#;
    const QUEUED: &str = r#"{"__typename":"CheckRun","status":"QUEUED","conclusion":""}"#;
    const FAILED: &str = r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}"#;
    const STATUS_OK: &str = r#"{"__typename":"StatusContext","state":"SUCCESS"}"#;
    const STATUS_PENDING: &str = r#"{"__typename":"StatusContext","state":"PENDING"}"#;
    const STATUS_ERROR: &str = r#"{"__typename":"StatusContext","state":"ERROR"}"#;

    fn status(json: &str) -> Option<Status> {
        parse(json).unwrap().map(|p| p.status)
    }

    #[test]
    fn checks_read_as_passed_running_failed_or_merged() {
        let list = |items: &[String]| format!("[{}]", items.join(","));
        assert_eq!(
            status(&list(&[pull(
                340,
                "OPEN",
                &[PASSED, SKIPPED, STATUS_OK].join(",")
            )])),
            Some(Status::Passed)
        );
        assert_eq!(
            status(&list(&[pull(340, "OPEN", &[PASSED, QUEUED].join(","))])),
            Some(Status::Running)
        );
        assert_eq!(
            status(&list(&[pull(
                340,
                "OPEN",
                &[PASSED, STATUS_PENDING].join(",")
            )])),
            Some(Status::Running)
        );
        // A failure outranks anything still running.
        assert_eq!(
            status(&list(&[pull(340, "OPEN", &[QUEUED, FAILED].join(","))])),
            Some(Status::Failed)
        );
        assert_eq!(
            status(&list(&[pull(340, "OPEN", STATUS_ERROR)])),
            Some(Status::Failed)
        );
        assert_eq!(status(&list(&[pull(340, "OPEN", "")])), Some(Status::Open));
        assert_eq!(
            status(&list(&[pull(340, "MERGED", FAILED)])),
            Some(Status::Merged)
        );
        let read = parse(&list(&[pull(340, "OPEN", PASSED)])).unwrap().unwrap();
        assert_eq!(read.number, 340);
        assert_eq!(read.url, "https://github.com/o/n/pull/340");
        assert_eq!(read.spoken(), "Pull request #340, checks passed");
        // A null rollup is no checks.
        assert_eq!(
            status(
                r#"[{"number":7,"state":"OPEN","url":"https://github.com/o/n/pull/7","statusCheckRollup":null}]"#
            ),
            Some(Status::Open)
        );
    }

    #[test]
    fn merged_state_applies_only_to_the_known_pull_request_head() {
        let head = "1111111111111111111111111111111111111111";
        let newer = "2222222222222222222222222222222222222222";
        let mut raw: serde_json::Value = serde_json::from_str(&pull(340, "MERGED", "")).unwrap();
        raw["headRefOid"] = head.into();
        let read = parse(&serde_json::json!([raw.clone()]).to_string())
            .unwrap()
            .unwrap();
        assert!(read.merged_head_matches(Some(head)));
        assert!(!read.merged_head_matches(Some(newer)));
        assert!(!read.merged_head_matches(None));
        raw["state"] = "OPEN".into();
        assert!(
            !parse(&serde_json::json!([raw]).to_string())
                .unwrap()
                .unwrap()
                .merged_head_matches(Some(head))
        );
        for unknown in [None, Some("".into())] {
            let unknown = PullRequest {
                head: unknown,
                ..read.clone()
            };
            assert!(!unknown.merged_head_matches(Some(head)));
            assert!(!unknown.merged_head_matches(None));
        }
        let legacy = parse(&format!("[{}]", pull(340, "MERGED", "")))
            .unwrap()
            .unwrap();
        assert_eq!(legacy.status, Status::Merged);
        assert!(!legacy.merged_head_matches(Some(head)));
    }

    #[test]
    fn the_open_pull_request_wins_and_a_closed_one_says_nothing() {
        let both = format!(
            "[{},{}]",
            pull(12, "MERGED", PASSED),
            pull(30, "OPEN", QUEUED)
        );
        let read = parse(&both).unwrap().unwrap();
        assert_eq!((read.number, read.status), (30, Status::Running));
        // The newest merged one, when none is open.
        let merged = format!(
            "[{},{}]",
            pull(12, "MERGED", PASSED),
            pull(9, "CLOSED", PASSED)
        );
        assert_eq!(parse(&merged).unwrap().unwrap().number, 12);
        assert_eq!(status(&format!("[{}]", pull(9, "CLOSED", PASSED))), None);
        // A newer closed one outranks an older merged one: nothing.
        let closed_last = format!(
            "[{},{}]",
            pull(12, "MERGED", PASSED),
            pull(20, "CLOSED", PASSED)
        );
        assert_eq!(status(&closed_last), None);
    }

    #[test]
    fn empty_and_malformed_answers_show_nothing() {
        assert_eq!(parse("[]").unwrap(), None);
        assert!(parse("").is_err());
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"number":1}"#).is_err());
        assert!(parse(r#"[{"number":"x","state":"OPEN","url":"u"}]"#).is_err());
        // Only a GitHub page is offered to open.
        assert_eq!(
            parse(r#"[{"number":1,"state":"OPEN","url":"file:///etc/passwd"}]"#).unwrap(),
            None
        );
        assert!(!is_github_url("https://github.com/o/n/pull/1 --x"));
        assert!(open("javascript:alert(1)").is_err());
    }

    #[test]
    fn default_branches_and_unsafe_names_are_not_looked_up() {
        let root = Path::new("/fixture/alpha");
        for branch in ["main", "master", "develop", "", "-x", "a\nb"] {
            assert!(Key::new(root, branch).is_none(), "{branch:?}");
        }
        assert!(Key::new(root, "claude/row-status").is_some());
        assert!(Key::new(root, "maintenance").is_some());
    }

    #[test]
    fn github_remotes_in_every_form_and_no_other_host() {
        for url in [
            "git@github.com:brandopakel/AgentDocker.git",
            "https://github.com/brandopakel/AgentDocker",
            "https://github.com/brandopakel/AgentDocker.git/",
            "https://token@github.com/brandopakel/AgentDocker.git",
            "ssh://git@github.com/brandopakel/AgentDocker.git",
            "ssh://git@ssh.github.com:443/brandopakel/AgentDocker.git",
        ] {
            assert_eq!(
                github_repo(url).as_deref(),
                Some("brandopakel/AgentDocker"),
                "{url}"
            );
        }
        for url in [
            "git@gitlab.com:o/n.git",
            "https://github.com.evil.example/o/n",
            "https://github.com/o",
            "https://github.com/o/n/extra",
            "/srv/git/n.git",
            "https://github.com/-o/n",
        ] {
            assert_eq!(github_repo(url), None, "{url}");
        }
    }

    #[test]
    fn the_repository_comes_from_the_preferred_github_remote() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let write = |config: &str| std::fs::write(dir.path().join(".git/config"), config).unwrap();
        write(
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:me/fork.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[remote \"upstream\"]\n\turl = https://github.com/them/project.git\n",
        );
        assert_eq!(
            github_repository(dir.path()).as_deref(),
            Some("them/project")
        );
        write("[remote \"origin\"]\n\turl = git@gitlab.com:me/project.git\n");
        assert_eq!(github_repository(dir.path()), None);
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(github_repository(plain.path()), None);
    }

    #[test]
    fn a_branch_is_asked_about_at_most_every_two_minutes_and_on_focus() {
        let mut cache = Cache::default();
        let start = Instant::now();
        let alpha = Key::new(Path::new("/a"), "one").unwrap();
        let beta = Key::new(Path::new("/a"), "two").unwrap();
        let wanted = BTreeSet::from([alpha.clone(), beta.clone()]);
        let due = cache.due(&wanted, start, false);
        assert_eq!(due, vec![alpha.clone(), beta.clone()]);
        // One round at a time, until it is taken as lost.
        assert!(cache.due(&wanted, start, true).is_empty());
        let mut lost = Cache::default();
        assert_eq!(lost.due(&wanted, start, false).len(), 2);
        assert!(
            lost.due(&wanted, start + Duration::from_secs(59), false)
                .is_empty()
        );
        assert_eq!(lost.due(&wanted, start + ROUND_LOST, false).len(), 2);
        let open = PullRequest {
            number: 1,
            url: "https://github.com/o/n/pull/1".into(),
            status: Status::Passed,
            head: None,
        };
        cache.answered(
            vec![
                (alpha.clone(), Ok(Some(open.clone()))),
                (beta.clone(), Err("gh is not installed".into())),
            ],
            start,
        );
        assert_eq!(cache.get(&alpha), Some(&open));
        assert_eq!(cache.get(&beta), None);
        // Answered, failed or not, a branch waits for the bound.
        assert!(
            cache
                .due(&wanted, start + Duration::from_secs(60), false)
                .is_empty()
        );
        assert!(
            cache
                .due(&wanted, start + Duration::from_secs(10), true)
                .is_empty()
        );
        // Focus regained asks again sooner than the sweep would.
        let focus = cache.due(&wanted, start + FOCUS_REFRESH, true);
        assert_eq!(focus.len(), 2);
        // A failed lookup keeps the last answer.
        cache.answered(
            vec![(alpha.clone(), Err("timed out".into()))],
            start + FOCUS_REFRESH,
        );
        assert_eq!(cache.get(&alpha), Some(&open));
        // The sweep asks again once two minutes have passed, oldest first.
        let later = start + FOCUS_REFRESH + REFRESH;
        assert_eq!(
            cache.due(&wanted, later, false),
            vec![beta.clone(), alpha.clone()]
        );
        cache.answered(vec![(alpha.clone(), Ok(None))], later);
        assert_eq!(cache.get(&alpha), None, "a branch whose PR went away");
        // A branch nobody shows any more is forgotten.
        let only_beta = BTreeSet::from([beta.clone()]);
        let _ = cache.due(&only_beta, later, false);
        assert!(!cache.entries.contains_key(&alpha));
    }

    #[test]
    fn a_round_is_bounded() {
        let mut cache = Cache::default();
        let wanted: BTreeSet<Key> = (0..20)
            .map(|i| Key::new(Path::new("/a"), &format!("b{i}")).unwrap())
            .collect();
        assert_eq!(cache.due(&wanted, Instant::now(), false).len(), ROUND);
    }
}
