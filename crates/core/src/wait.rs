//! Who is waiting for what, and whether that can ever finish.
//!
//! `claim --wait` used to be a retry loop and nothing more: on a conflict
//! the caller slept until an overlapping lease cleared, then raced every
//! other sleeper for it. Two things were missing. A newcomer could take a
//! resource somebody had been waiting on for minutes, and a set of agents
//! each holding what the next one wants had no way of finding out — they
//! all simply waited for their TTLs.
//!
//! Making waiting a fact rather than a sleep fixes both, and gives the
//! daemon a third thing for free: an agent that is blocked is blocked on
//! a *named resource held by a named agent*, which is a far better answer
//! than guessing at what its terminal last printed.
//!
//! Pure: no clock of its own, no I/O. The daemon passes `now` and keeps
//! the queue under its one state lock.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, LeaseMode, ResourceKey};

/// A ticket: arrival order across the whole queue, so that waiters on
/// overlapping resources share one order rather than each having their
/// own. Handed out by [`WaitQueue::join`] and given back to leave.
pub type Ticket = u64;

/// How far ahead of arrival order a waiter asks to be served: higher
/// first, 0 when it does not say. From [`MIN_PRIORITY`] to
/// [`MAX_PRIORITY`]; the daemon refuses anything else.
pub type Priority = i8;

/// The lowest priority a claim may ask for.
pub const MIN_PRIORITY: Priority = -9;

/// The highest priority a claim may ask for.
pub const MAX_PRIORITY: Priority = 9;

/// Whether a claim may ask for this priority.
pub fn valid_priority(priority: Priority) -> bool {
    (MIN_PRIORITY..=MAX_PRIORITY).contains(&priority)
}

/// One request waiting for a resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub ticket: Ticket,
    pub agent: AgentId,
    pub resource: ResourceKey,
    pub mode: LeaseMode,
    pub since: DateTime<Utc>,
    /// Where the claim asked to stand: higher is served first, arrival
    /// decides among equals. 0 from a daemon predating it.
    #[serde(default)]
    pub priority: Priority,
}

impl Waiter {
    /// The one order the queue serves in: higher priority first, then
    /// arrival. Tickets are unique, so no two waiters share a key and the
    /// order is total.
    fn order(&self) -> (Reverse<Priority>, Ticket) {
        (Reverse(self.priority), self.ticket)
    }

    /// Whether this waiter is served before `other`.
    pub fn ahead_of(&self, other: &Waiter) -> bool {
        self.order() < other.order()
    }
}

/// Waiters in the order they will be served: higher priority first, then
/// arrival, oldest first.
#[derive(Debug, Default)]
pub struct WaitQueue {
    /// Kept in [`Waiter::ahead_of`] order, so [`Self::all`] reads as the
    /// queue is served.
    waiters: Vec<Waiter>,
    /// Waits ended to break a ring of waits, by ticket, with the ring:
    /// kept until the claim that was waiting reads it or leaves, so it
    /// can say why its wait ended.
    broken: HashMap<Ticket, Vec<Blocked>>,
    next_ticket: Ticket,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a place in the queue: behind every waiter of the same or a
    /// higher priority, ahead of every lower one. The ticket identifies
    /// this waiter for as long as it waits; the caller gives it back to
    /// [`Self::leave`].
    pub fn join(
        &mut self,
        agent: AgentId,
        resource: ResourceKey,
        mode: LeaseMode,
        priority: Priority,
        now: DateTime<Utc>,
    ) -> Ticket {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        let waiter = Waiter {
            ticket,
            agent,
            resource,
            mode,
            since: now,
            priority,
        };
        let at = self.waiters.partition_point(|w| w.ahead_of(&waiter));
        self.waiters.insert(at, waiter);
        ticket
    }

    /// Give up a place, whether the wait ended in a lease, a timeout, or
    /// a client that went away. `None` when the ticket was not waiting —
    /// it left already, or its wait was ended to break a ring
    /// ([`Self::break_ring`]), which this forgets.
    pub fn leave(&mut self, ticket: Ticket) -> Option<Waiter> {
        self.broken.remove(&ticket);
        let at = self.waiters.iter().position(|w| w.ticket == ticket)?;
        Some(self.waiters.remove(at))
    }

    /// End a wait to break a ring of waits it is in: it leaves the queue,
    /// and [`Self::deadlocked`] says so with the ring until the claim that
    /// was waiting leaves. `None` when the ticket was not waiting.
    pub fn break_ring(&mut self, ticket: Ticket, cycle: Vec<Blocked>) -> Option<Waiter> {
        let at = self.waiters.iter().position(|w| w.ticket == ticket)?;
        self.broken.insert(ticket, cycle);
        Some(self.waiters.remove(at))
    }

    /// The ring this ticket's wait was ended to break, if it was.
    pub fn deadlocked(&self, ticket: Ticket) -> Option<&[Blocked]> {
        self.broken.get(&ticket).map(Vec::as_slice)
    }

    /// Whether it is this waiter's turn: no waiter served before it
    /// ([`Waiter::ahead_of`]) wants something that overlaps and would
    /// exclude it.
    ///
    /// Two shared waiters do not block each other, so a queue of readers
    /// is not serialised by its own fairness rule. An unknown ticket is
    /// not next — it is nobody.
    pub fn is_next(&self, ticket: Ticket) -> bool {
        self.position(ticket) == Some(0)
    }

    /// How many waiters are ahead of this one *and in its way*. Zero
    /// means it is next; `None` means it is not queued.
    ///
    /// [`Self::is_next`] is this being zero, and it has to be: this
    /// number is published as `lease_waiting.position` and shown as a
    /// queue place, so a shared waiter that may proceed at once must not
    /// be told it is second.
    pub fn position(&self, ticket: Ticket) -> Option<usize> {
        let mine = self.waiters.iter().find(|w| w.ticket == ticket)?;
        Some(
            self.waiters
                .iter()
                .filter(|other| other.ahead_of(mine) && blocks(other, mine))
                .count(),
        )
    }

    /// What this agent is waiting for, if anything. An agent waits for
    /// one thing at a time — a claim holds its connection — so the
    /// oldest is the answer.
    pub fn waiting_for(&self, agent: &AgentId) -> Option<&Waiter> {
        self.waiters
            .iter()
            .filter(|w| w.agent == *agent)
            .min_by_key(|w| w.ticket)
    }

    /// Every waiter, in the order they will be served.
    pub fn all(&self) -> &[Waiter] {
        &self.waiters
    }

    pub fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }
}

/// Whether `other` stands in `mine`'s way: it wants something that
/// overlaps, and at least one of them wants it exclusively.
fn blocks(other: &Waiter, mine: &Waiter) -> bool {
    other.resource.overlaps(&mine.resource)
        && (other.mode == LeaseMode::Exclusive || mine.mode == LeaseMode::Exclusive)
}

/// One step of a deadlock: an agent, what it is waiting for, and who
/// holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    pub agent: AgentId,
    pub resource: ResourceKey,
    pub held_by: AgentId,
}

/// Would `requester` waiting for `resource` close a cycle?
///
/// The graph has an edge from each waiter to every holder of what it
/// waits for. A cycle through the requester means every agent in it is
/// waiting for something another member holds, so none of them can ever
/// proceed: TTLs would eventually break it, but only after everybody has
/// wasted the whole of one.
///
/// The answer is the cycle itself, starting and ending at the requester,
/// so the refusal can say what the problem is rather than that there is
/// one.
///
/// `holders` says who holds each resource; `waits` says what each agent
/// is already waiting for. Both are the daemon's own tables, passed in
/// rather than borrowed, so this stays pure and testable.
pub fn deadlock(
    requester: &AgentId,
    resource: &ResourceKey,
    holders: &[(ResourceKey, AgentId)],
    waits: &[(AgentId, ResourceKey)],
) -> Option<Vec<Blocked>> {
    let waiting: HashMap<&AgentId, &ResourceKey> = waits.iter().map(|(a, r)| (a, r)).collect();
    // Depth-first from the requester, following "waiting for something
    // this agent holds". A path back to the requester is the cycle.
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(requester.clone());
    walk(
        requester, resource, requester, holders, &waiting, &mut path, &mut seen,
    )
    .then_some(path)
}

/// One step of the search: from `from`, which wants `want`, to every
/// agent holding something that overlaps it.
fn walk(
    from: &AgentId,
    want: &ResourceKey,
    requester: &AgentId,
    holders: &[(ResourceKey, AgentId)],
    waiting: &HashMap<&AgentId, &ResourceKey>,
    path: &mut Vec<Blocked>,
    seen: &mut HashSet<AgentId>,
) -> bool {
    for (held, holder) in holders {
        if !held.overlaps(want) {
            continue;
        }
        // Holding something you also want is not a deadlock; it is a
        // renewal, and the lease table settles it.
        if holder == from {
            continue;
        }
        path.push(Blocked {
            agent: from.clone(),
            resource: want.clone(),
            held_by: holder.clone(),
        });
        if holder == requester {
            return true;
        }
        if seen.insert(holder.clone())
            && let Some(next) = waiting.get(holder)
            && walk(holder, next, requester, holders, waiting, path, seen)
        {
            return true;
        }
        path.pop();
    }
    false
}

/// The wait a ring is broken at, as [`victim`] chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Victim {
    /// Its step in the cycle: what it waits for and who holds that.
    pub step: usize,
    /// The queued wait to end; `None` for the claim that found the ring,
    /// which has not joined the queue and is refused instead.
    pub ticket: Option<Ticket>,
}

/// Which wait in a ring to end so the rest can go on.
///
/// `cycle` is what [`deadlock`] found: its first step is the claim that
/// would close the ring by waiting (the newcomer, asking at `priority`),
/// every later step a wait already in `waiters`. The rule: the lowest
/// priority; among equals the youngest, by latest arrival, the newcomer
/// counting as the youngest of all. So where every priority is equal the
/// newcomer is the victim, as it always was before priorities.
///
/// A later step's wait is the waiter with that step's agent and resource;
/// should an agent wait twice for one resource, each wait is a candidate
/// and the rule picks between them. A step no waiter matches has nothing
/// to end and is passed over.
pub fn victim(cycle: &[Blocked], priority: Priority, waiters: &[Waiter]) -> Victim {
    let queued = cycle
        .iter()
        .enumerate()
        .skip(1)
        .flat_map(|(step, blocked)| {
            waiters
                .iter()
                .filter(move |w| w.agent == blocked.agent && w.resource == blocked.resource)
                .map(move |w| (step, w.priority, Some(w.ticket)))
        });
    // The largest key is the lowest priority, then the newcomer, then
    // the latest ticket; no two candidates share one.
    let (step, _, ticket) = queued
        .chain(std::iter::once((0, priority, None)))
        .max_by_key(|&(_, priority, ticket)| (Reverse(priority), ticket.is_none(), ticket))
        .expect("the newcomer is always a candidate");
    Victim { step, ticket }
}

/// What an agent is actually doing, as far as the working set can say.
///
/// Not a terminal heuristic. A multiplexer has to guess from what a pane
/// last printed; the daemon knows, because every one of these comes from
/// something it recorded: the wait queue says who is blocked and on
/// what, the registry says when an agent last acted through the daemon,
/// and the status says whether it is there at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Activity {
    /// The process is present, but no fresh activity evidence is available.
    Unknown,
    /// Registered, no process yet.
    Starting,
    /// Acted through the daemon within the activity window — claimed,
    /// released, observed, reported a tool run, changed a file it holds.
    Working { since: DateTime<Utc> },
    /// Waiting on a claim. The one state where a multiplexer can only
    /// say "blocked" and this can say what by, and who has it.
    Blocked {
        resource: ResourceKey,
        held_by: Vec<AgentId>,
        since: DateTime<Utc>,
    },
    /// Explicitly reported quiet by an adapter; silence alone cannot prove it.
    Idle { since: DateTime<Utc> },
    /// Stopped for the person, by its adapter's report: a permission
    /// prompt or a question the provider put in its own terminal. Only
    /// the person can move it on, from there. `reason` says which, when
    /// the adapter could tell, and `detail` what the prompt is about in
    /// one short line (see [`wait_detail`]).
    ///
    /// Or, when the opt-in judge is on, read that way from its quiet
    /// screen: `judged` then says so and how sure the judge was. A
    /// judgment only ever raises working, idle or unknown to this; it
    /// never stands over another state (see `crate::judgment::raise`).
    Waiting {
        since: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<WaitReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        judged: Option<Judged>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Not running any more.
    Finished,
}

impl Activity {
    /// A word for a column.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Starting => "starting",
            Self::Working { .. } => "working",
            Self::Blocked { .. } => "blocked",
            Self::Idle { .. } => "idle",
            Self::Waiting { .. } => "waiting",
            Self::Finished => "finished",
        }
    }
}

/// Explicit adapter observations; process liveness and MCP configuration are
/// not turn observations. No prompts or transcripts are stored, and of a
/// tool's arguments only a permission prompt's one bounded line
/// ([`ActivityObservation::detail`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedActivity {
    Working,
    Idle,
    /// The provider stopped for the person (a permission prompt, a
    /// question dialog) until they answer in its terminal.
    Waiting,
}

/// What a provider stopped for the person for, as its adapter read the
/// provider's own event kind. The kind, never the prompt's words; what
/// the prompt is about travels separately, as a bounded detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// A permission prompt: a tool run it wants allowed.
    Permission,
    /// A question or a choice it put to the person.
    Question,
    /// Words typed at its prompt and never sent. Only the judge reads
    /// this, from the screen; no adapter reports it.
    Draft,
    /// A kind this build does not know, from a newer adapter.
    #[serde(other)]
    Other,
}

/// A wait read from the agent's screen by the opt-in judge rather than
/// reported by its adapter: how sure the judge was, in thousandths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judged {
    pub permille: u16,
}

impl Judged {
    /// `92%`, for a person.
    pub fn percent(self) -> String {
        format!("{}%", (u32::from(self.permille.min(1000)) + 5) / 10)
    }
}

/// Who made an observation, when it was not the agent's own adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySource {
    /// The daemon, reading the herdr pane that holds the agent's provider
    /// session: herdr watches the terminal, so its word on whether the
    /// agent is working, waiting or idle outranks contact.
    Herdr,
    /// The daemon, reading Codex's shared app-server, which hosts the
    /// agent's conversation: the server says what the conversation itself
    /// is doing, so its word outranks contact and, while it lasts, herdr's.
    CodexServer,
    /// A source this build does not know, from a newer daemon.
    #[serde(other)]
    Other,
}

impl ActivitySource {
    /// Whether this source's current word on an agent keeps `other`'s off
    /// its record. Codex's shared server reports the conversation's own
    /// state — a running turn, an approval or a question it stopped for —
    /// where herdr infers it from what the terminal shows, so where both
    /// speak for one agent the server wins. No other pair is ranked, and a
    /// source this build does not know ranks with none.
    pub fn outranks(self, other: ActivitySource) -> bool {
        matches!((self, other), (Self::CodexServer, Self::Herdr))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityObservation {
    pub activity: ReportedActivity,
    pub observed_at: DateTime<Utc>,
    /// For a wait, what for, when the adapter could tell. A daemon
    /// predating it ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<WaitReason>,
    /// For a wait, what it is about in one short line — the tool a
    /// permission prompt is for and a summary of its input — as
    /// [`wait_detail`] leaves it. Only a wait carries one. A daemon
    /// predating it ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Who observed it, when not the agent's own adapter; `None` for a
    /// report the agent's hooks, MCP server or CLI made. Only the daemon
    /// sets it. A daemon predating it ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ActivitySource>,
}

/// The most characters a wait's detail keeps.
pub const WAIT_DETAIL_CHARS: usize = 120;

/// How long after a detailed wait a plain one for the same reason is
/// taken for the same prompt. Claude Code says a permission prompt twice:
/// its `PermissionRequest` hook as the dialog opens, with the tool and its
/// input, and a `Notification` six seconds later, without them.
pub const WAIT_DETAIL_CARRY_SECS: i64 = 60;

/// `text` as a wait's detail is kept and shown: one line, `<Tool>: <what>`
/// or a tool alone, never worth a secret. The one gate every detail passes,
/// the hooks adapter's and any other client's alike, so a client that
/// sends a raw command gets exactly the hook's treatment.
///
/// Its first line, without control or invisible formatting characters,
/// runs of spaces as one. None when that whole line reads like a credential
/// ([`crate::memory::secret_like`]), judged before anything is cut. Then
/// what follows the tool's name is kept only where that tool's argument is
/// known to be safe to show, and free text never is:
/// - a file tool's path ([`PATH_TOOLS`]) and `WebFetch`'s address, up to a
///   flag that takes a credential or a `user:password@host`, every address
///   reduced to scheme, host and path ([`scrubbed`]);
/// - a shell command ([`SHELL_TOOLS`]) as [`command_detail`] reduces it:
///   its program alone, or a few programs' own subcommands;
/// - any other tool — a search (`Grep`, `Glob`, `WebSearch`), whose query
///   or pattern is free text a short plain password slips through, an MCP
///   tool, one this build does not know — as its name alone.
///
/// A line with no tool is read as a command. Cut to [`WAIT_DETAIL_CHARS`]
/// with an ellipsis.
pub fn wait_detail(text: &str) -> Option<String> {
    let line = text
        .split(['\n', '\r'])
        .map(|line| {
            line.chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .filter(|c| !c.is_control() && !invisible(*c))
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .find(|line| !line.is_empty())?;
    if crate::memory::secret_like(&line) {
        return None;
    }
    let mut kept = match line.split_once(": ").filter(|(tool, _)| tool_name(tool)) {
        Some((tool, what)) => {
            let what = if PATH_TOOLS.contains(&tool) || tool == "WebFetch" {
                Some(scrubbed(what))
            } else if SHELL_TOOLS.contains(&tool) {
                command_detail(what)
            } else {
                None
            };
            match what.filter(|what| !what.is_empty()) {
                Some(what) => format!("{tool}: {what}"),
                None => tool.to_owned(),
            }
        }
        None => command_detail(&line)?,
    };
    if kept.chars().count() > WAIT_DETAIL_CHARS {
        kept = kept.chars().take(WAIT_DETAIL_CHARS - 1).collect::<String>();
        kept = kept.trim_end().to_owned();
        kept.push('…');
    }
    Some(kept)
}

/// The tools whose argument is a file's path.
pub const PATH_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "Read", "NotebookEdit"];

/// The tools whose argument is a shell command.
pub const SHELL_TOOLS: &[&str] = &["Bash", "PowerShell"];

/// Whether what precedes `: ` in a detail is a tool's name.
fn tool_name(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

/// The words of `line` before the first that may carry a credential: a
/// flag that takes one ([`credential_flag`]) or a `user:password@host`
/// outside an address. An address keeps only its scheme, host and path
/// ([`address`]).
fn scrubbed(line: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for word in line.split(' ') {
        if credential_flag(word) || userinfo(word) {
            break;
        }
        kept.push(if word.contains("://") {
            address(word)
        } else {
            word.to_owned()
        });
    }
    kept.join(" ")
}

/// Words that, anywhere in a command, say a credential is in it or near
/// it: such a command is never shown.
pub const CREDENTIAL_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "bearer",
    "credential",
    "private_key",
    "private-key",
];

/// Parts of a flag's name that say its value is a credential, or names
/// whose it is: `--user`, `--password`, `--header`, `--token`, `--key`,
/// `-Credential`…
const CREDENTIAL_FLAG_PARTS: &[&str] = &[
    "user", "pass", "pwd", "token", "key", "auth", "header", "cookie", "secret", "cred", "bearer",
    "login", "session", "cert", "identity",
];

/// Single-letter flags that commonly take a credential or its holder:
/// `-u user:password`, `-p<password>`, `-P`, `-H 'Authorization: …'`,
/// `-k`/`-K` keys. In a cluster or with a value attached, any of them.
const CREDENTIAL_FLAG_LETTERS: &[char] = &['u', 'p', 'P', 'H', 'k', 'K'];

/// The programs a detail may show one word of besides their name, and
/// the words: each program's own subcommands, written out literally. A
/// word in that place is shown only when it is exactly one of these, and
/// nothing after it ever is — what follows a subcommand may be a value the
/// person supplies and the program passes on (`cargo run -- <args>`, `npm
/// run <script> <args>`, `go run <file> <args>`), and neither a word's
/// shape nor its position can tell a subcommand from a value. Every other
/// program is shown alone: an argument in an unknown position may be a
/// password (`smbclient //server/share <password>`, `isql <dsn> <user>
/// <password>`). Kept small on purpose.
const SUBCOMMANDS: &[(&str, &[&str])] = &[
    (
        "cargo",
        &[
            "build", "check", "clippy", "test", "run", "fmt", "doc", "bench", "install", "update",
            "add", "remove", "tree", "metadata", "nextest",
        ],
    ),
    (
        "git",
        &[
            "status", "diff", "log", "show", "add", "commit", "push", "pull", "fetch", "merge",
            "rebase", "checkout", "switch", "branch", "stash", "worktree", "reset", "restore",
            "tag", "remote", "clone", "init",
        ],
    ),
    (
        "go",
        &["build", "test", "run", "vet", "mod", "get", "install"],
    ),
    (
        "npm",
        &[
            "install", "ci", "test", "run", "build", "add", "remove", "exec",
        ],
    ),
    (
        "pnpm",
        &[
            "install", "ci", "test", "run", "build", "add", "remove", "exec",
        ],
    ),
    (
        "yarn",
        &[
            "install", "ci", "test", "run", "build", "add", "remove", "exec",
        ],
    ),
];

/// Whether `word` is a flag that takes, or is, a credential: a long name
/// with a [`CREDENTIAL_FLAG_PARTS`] part (`--password=…` by its name), or
/// a single-dash flag with a [`CREDENTIAL_FLAG_LETTERS`] letter
/// (`-phunter2`, `-uroot`).
fn credential_flag(word: &str) -> bool {
    let Some(flag) = word.strip_prefix('-') else {
        return false;
    };
    let name = flag.split('=').next().unwrap_or_default();
    let long = name.strip_prefix('-');
    let bare = long.unwrap_or(name);
    if bare.is_empty() {
        return false;
    }
    let lower = bare.to_ascii_lowercase();
    CREDENTIAL_FLAG_PARTS
        .iter()
        .any(|part| lower.contains(part))
        || (long.is_none() && bare.chars().any(|c| CREDENTIAL_FLAG_LETTERS.contains(&c)))
}

/// Whether `word`, outside an address, has a user and password before an
/// `@`: `alice:hunter2@host`.
fn userinfo(word: &str) -> bool {
    !word.contains("://")
        && word
            .rsplit_once('@')
            .is_some_and(|(before, _)| before.contains(':'))
}

/// An address without what may carry a credential: the user and password
/// before the host, the query and the fragment.
pub fn address(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}://{host}{path}")
}

/// Whether `word` is plain enough to show as it is: letters, digits and
/// the punctuation of paths, versions and names, nothing a shell would
/// expand, quote or split on.
fn plain_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/~+,:%@-".contains(c))
}

/// A shell command as a wait's detail may show it: an allowlist, with
/// arguments left out by default.
///
/// Nothing at all when the command reads like a secret or names a
/// [`CREDENTIAL_WORDS`] word. Otherwise the program of its first line — by
/// its name, without its directory, `NAME=value` assignments before it
/// skipped unseen — and, for the few programs in [`SUBCOMMANDS`], the word
/// that directly follows it when that word is exactly one of the program's
/// own subcommands. Nothing after that, ever. A program a shell could not
/// read plainly, or a flag in the program's place, shows nothing.
pub fn command_detail(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    if crate::memory::secret_like(command) || CREDENTIAL_WORDS.iter().any(|w| lower.contains(w)) {
        return None;
    }
    let line = command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut words = line.split_whitespace().skip_while(|word| assignment(word));
    let program = words
        .next()
        .filter(|word| !word.starts_with('-') && plain_word(word) && !userinfo(word))?;
    let name = program.rsplit('/').find(|part| !part.is_empty())?;
    let known = name.strip_suffix(".exe").unwrap_or(name);
    let subcommand = SUBCOMMANDS
        .iter()
        .find(|(program, _)| *program == known)
        .and_then(|(_, subcommands)| {
            let word = words.next()?;
            subcommands.iter().find(|subcommand| **subcommand == word)
        });
    Some(match subcommand {
        Some(subcommand) => format!("{name} {subcommand}"),
        None => name.to_owned(),
    })
}

/// `NAME=value` before a program: an environment assignment.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Characters that change how the text around them reads without showing
/// themselves: bidirectional controls, zero-width marks and joiners, the
/// soft hyphen, the byte-order mark.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

impl ActivityObservation {
    /// The observation as the daemon keeps it after `previous`: a detail
    /// only on a wait, and only as [`wait_detail`] leaves it; and a plain
    /// wait — one that said nothing of what it is about — for the same
    /// reason that follows a detailed one within [`WAIT_DETAIL_CARRY_SECS`]
    /// keeps that detail, since it is the same prompt said again without
    /// it. A wait whose detail was left out (it read like a secret) is a
    /// prompt of its own and carries nothing over.
    pub fn settled(mut self, previous: Option<&ActivityObservation>) -> Self {
        if self.activity != ReportedActivity::Waiting {
            self.detail = None;
            return self;
        }
        let plain = self.detail.is_none();
        self.detail = self.detail.as_deref().and_then(wait_detail);
        if plain
            && let Some(previous) = previous.filter(|previous| {
                previous.activity == ReportedActivity::Waiting
                    && previous.reason == self.reason
                    && previous.observed_at <= self.observed_at
                    && self.observed_at - previous.observed_at
                        <= chrono::Duration::seconds(WAIT_DETAIL_CARRY_SECS)
            })
        {
            self.detail = previous.detail.clone();
        }
        self
    }

    /// A missed hook must not leave a permanent working/idle assertion. Long
    /// silent turns become unknown until another provider event arrives.
    /// A wait for the person is silent by nature and may last as long as
    /// they are away, so it holds for twelve hours unless the next event
    /// replaces it. An observation a source renews while it still holds
    /// (herdr's, Codex's shared server's) lasts five minutes whatever it
    /// says, so a wait it stopped renewing — the pane released, herdr or
    /// the server gone — does not stand for hours.
    pub fn current(&self, now: DateTime<Utc>) -> Option<Activity> {
        let age = now - self.observed_at;
        let lasts = match (self.activity, self.source) {
            (ReportedActivity::Waiting, None) => chrono::Duration::hours(12),
            _ => chrono::Duration::minutes(5),
        };
        if age < chrono::Duration::zero() || age >= lasts {
            return None;
        }
        Some(match self.activity {
            ReportedActivity::Working => Activity::Working {
                since: self.observed_at,
            },
            ReportedActivity::Idle => Activity::Idle {
                since: self.observed_at,
            },
            ReportedActivity::Waiting => Activity::Waiting {
                since: self.observed_at,
                reason: self.reason,
                judged: None,
                detail: self.detail.clone(),
            },
        })
    }

    /// Whether this observation, standing on an agent's record, keeps
    /// `next` off it at `now`: while it is current, a source that outranks
    /// `next`'s ([`ActivitySource::outranks`]) keeps that source's word out.
    /// Past its expiry it keeps nothing out. The agent's own report (no
    /// source) is never kept off and keeps nothing off: between it and a
    /// source's word, the newer stands.
    pub fn keeps_off(&self, next: &ActivityObservation, now: DateTime<Utc>) -> bool {
        match (self.source, next.source) {
            (Some(standing), Some(coming)) => {
                standing.outranks(coming) && self.current(now).is_some()
            }
            _ => false,
        }
    }
}

/// One agent's activity, with enough of its identity to show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub agent: AgentId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::ProjectId>,
    pub activity: Activity,
    /// Durable queue size. None when talking to a daemon predating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_inputs: Option<usize>,
    /// Of those, the ones no receipt from the current process covers: an
    /// earlier receipt says nothing about them. None on older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_receipt: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait with neither a judgment nor a detail keeps the wire shape it
    /// had before either field: both are left out, not written as null.
    #[test]
    fn a_plain_wait_writes_neither_judged_nor_detail() {
        let since = now();
        let wait = Activity::Waiting {
            since,
            reason: None,
            judged: None,
            detail: None,
        };
        let value = serde_json::to_value(&wait).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"state": "waiting", "since": since})
        );
        let back: Activity = serde_json::from_value(value).unwrap();
        assert_eq!(back, wait);
    }

    #[test]
    fn activity_observations_expire_instead_of_guessing_idle() {
        let now = now();
        for activity in [ReportedActivity::Working, ReportedActivity::Idle] {
            let observation = ActivityObservation {
                activity,
                observed_at: now,
                reason: None,
                detail: None,
                source: None,
            };
            assert!(observation.current(now).is_some());
            assert!(
                observation
                    .current(now + chrono::Duration::seconds(299))
                    .is_some()
            );
            assert_eq!(
                observation.current(now + chrono::Duration::minutes(5)),
                None
            );
            assert_eq!(
                observation.current(now - chrono::Duration::seconds(1)),
                None
            );
        }
    }

    #[test]
    fn a_wait_for_the_person_outlasts_a_quiet_turn_but_not_a_day() {
        let now = now();
        let observation = ActivityObservation {
            activity: ReportedActivity::Waiting,
            observed_at: now,
            reason: Some(WaitReason::Permission),
            detail: None,
            source: None,
        };
        assert_eq!(
            observation.current(now + chrono::Duration::hours(2)),
            Some(Activity::Waiting {
                since: now,
                reason: Some(WaitReason::Permission),
                judged: None,
                detail: None,
            })
        );
        assert_eq!(observation.current(now + chrono::Duration::hours(12)), None);
        assert_eq!(
            observation.current(now - chrono::Duration::seconds(1)),
            None
        );
        assert_eq!(
            Activity::Waiting {
                since: now,
                reason: None,
                judged: None,
                detail: None,
            }
            .label(),
            "waiting"
        );
    }

    /// A report without a reason reads and writes as it did before the
    /// field, and a reason a newer adapter names is a plain wait here, not
    /// a refused report.
    #[test]
    fn a_wait_reason_is_optional_and_tolerates_newer_kinds() {
        let now = now();
        let before = serde_json::json!({"activity": "waiting", "observed_at": now});
        let read: ActivityObservation = serde_json::from_value(before.clone()).unwrap();
        assert_eq!(read.reason, None);
        assert_eq!(serde_json::to_value(&read).unwrap(), before);
        let named = serde_json::json!({
            "activity": "waiting", "observed_at": now, "reason": "permission"
        });
        let read: ActivityObservation = serde_json::from_value(named.clone()).unwrap();
        assert_eq!(read.reason, Some(WaitReason::Permission));
        assert_eq!(serde_json::to_value(&read).unwrap(), named);
        let newer = serde_json::json!({
            "activity": "waiting", "observed_at": now, "reason": "sign_in"
        });
        let read: ActivityObservation = serde_json::from_value(newer).unwrap();
        assert_eq!(read.reason, Some(WaitReason::Other));
        assert_eq!(
            read.current(now),
            Some(Activity::Waiting {
                since: now,
                reason: Some(WaitReason::Other),
                judged: None,
                detail: None,
            })
        );
    }

    /// A detail is one short line the person can read: the first line
    /// with text, no control or invisible formatting characters, spaces
    /// collapsed, cut with an ellipsis — and nothing at all when it reads
    /// like a credential.
    #[test]
    fn a_wait_detail_is_one_bounded_printable_line_and_never_a_secret() {
        assert_eq!(
            wait_detail("Bash: git push\u{202E} origin\tmain\nrm -rf /").as_deref(),
            Some("Bash: git push")
        );
        assert_eq!(
            wait_detail("\n  \nEdit: src/lib.rs").as_deref(),
            Some("Edit: src/lib.rs")
        );
        assert_eq!(wait_detail("\u{200B}\u{0007}\n \r"), None);
        let long = format!("Read: {}", "a ".repeat(200));
        let cut = wait_detail(&long).unwrap();
        assert_eq!(cut.chars().count(), WAIT_DETAIL_CHARS);
        assert!(cut.ends_with('…'));
        for secret in [
            "Bash: curl -H 'token: ghp_abcdefghijklmnopqrstuvwxyz0123456789'",
            "Bash: export KEY=x9Fk2LmQ7vR3tY8wZ1aB4cD6eG0hJ5nP",
        ] {
            assert_eq!(wait_detail(secret), None, "{secret}");
        }
    }

    /// A command shows its program and, for a few programs, one word more
    /// when it is exactly one of their own subcommands — nothing after it,
    /// ever: an argument in an unknown position may be a password, flagged
    /// or not, and what follows a subcommand may be a value the program
    /// passes on. Neither shape nor position can tell.
    #[test]
    fn a_command_shows_its_program_and_only_known_subcommands() {
        for (command, shown) in [
            // Positional passwords, which no credential word or shape gives
            // away.
            ("smbclient //server/share hunter2", "smbclient"),
            ("isql localdb alice hunter2", "isql"),
            ("mysql -u alice db hunter2", "mysql"),
            ("mysql db hunter2", "mysql"),
            ("tool run hunter2", "tool"),
            ("redis-cli -a hunter2 ping", "redis-cli"),
            // Flagged ones and passwords in addresses.
            ("curl -u alice:hunter2 https://example.invalid", "curl"),
            ("curl --user=alice:hunter2 https://example.invalid", "curl"),
            ("curl https://alice:hunter2@example.invalid/data", "curl"),
            ("mysql -phunter2 -e SELECT", "mysql"),
            ("mysql -uroot -phunter2", "mysql"),
            ("sshpass -p hunter2 ssh host", "sshpass"),
            ("scp alice:hunter2@host:/f .", "scp"),
            ("psql postgresql://alice:hunter2@db/app", "psql"),
            ("DB_PASS=hunter2 psql -h db", "psql"),
            ("tool 'alice:hunter2'", "tool"),
            ("echo hunter2 | sudo -S true", "echo"),
            ("docker login -u alice hunter2", "docker"),
            ("Get-Item -Path C:/hunter2", "Get-Item"),
            ("/opt/tools/smbclient //server/share hunter2", "smbclient"),
            // A known program's own subcommand, and nothing after it.
            ("cargo run hunter2", "cargo run"),
            ("cargo run -- alice hunter2", "cargo run"),
            ("cargo nextest run -p hunter2", "cargo nextest"),
            ("npm run deploy hunter2", "npm run"),
            ("pnpm exec tool hunter2", "pnpm exec"),
            ("go run main.go hunter2", "go run"),
            ("yarn deploy hunter2", "yarn"),
            ("yarn run deploy hunter2", "yarn run"),
            ("make deploy DB_PASS=hunter2", "make"),
            ("make deploy hunter2", "make"),
            ("git push origin main", "git push"),
            ("git config somename hunter2", "git"),
            ("git hunter2", "git"),
            (
                "git clone https://alice:hunter2@example.invalid/o/r.git",
                "git clone",
            ),
            ("git -C /work status", "git"),
            ("git checkout Hunter2", "git checkout"),
            ("/usr/bin/git status", "git status"),
            ("cargo test -p core", "cargo test"),
            ("cargo login hunter2", "cargo"),
            ("cargo.exe build --release", "cargo.exe build"),
            ("npm run build", "npm run"),
            ("yarn npm login hunter2", "yarn"),
            ("go test ./...", "go test"),
            ("Cargo Run hunter2", "Cargo"),
            ("rm -rf target", "rm"),
            ("./scripts/verify.sh check", "verify.sh"),
        ] {
            assert_eq!(command_detail(command).as_deref(), Some(shown), "{command}");
            assert!(!shown.contains("hunter2"), "{command}");
        }
        for command in [
            "PGPASSWORD=hunter2 psql",
            "curl -H 'Authorization: Bearer abc' https://example.invalid",
            "export GITHUB_TOKEN=abc",
            "echo ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "ls\nexport KEY=x9Fk2LmQ7vR3tY8wZ1aB4cD6eG0hJ5nP",
            "'quoted program' arg",
            "-u alice:hunter2",
            "   \n  ",
        ] {
            assert_eq!(command_detail(command), None, "{command}");
        }
    }

    /// Whoever sends a detail, it gets the hooks adapter's treatment: what
    /// follows a shell tool or no tool at all is reduced as a command, a
    /// path or an address keeps nothing of a credential, and a search, an
    /// MCP tool or an unknown one shows its name alone — its query or
    /// pattern is free text.
    #[test]
    fn any_detail_gets_the_hooks_treatment() {
        for (sent, kept) in [
            ("Bash: redis-cli -a hunter2 ping", Some("Bash: redis-cli")),
            (
                "Bash: smbclient //server/share hunter2",
                Some("Bash: smbclient"),
            ),
            (
                "PowerShell: isql localdb alice hunter2",
                Some("PowerShell: isql"),
            ),
            (
                "Bash: curl -u alice:hunter2 https://example.invalid",
                Some("Bash: curl"),
            ),
            (
                "Bash: curl https://alice:hunter2@example.invalid/data?k=v",
                Some("Bash: curl"),
            ),
            ("Bash: mysql -phunter2 -e SELECT", Some("Bash: mysql")),
            ("Bash: PGPASSWORD=hunter2 psql", Some("Bash")),
            ("Bash: git push origin main", Some("Bash: git push")),
            ("Bash: cargo run hunter2", Some("Bash: cargo run")),
            (
                "PowerShell: npm run deploy hunter2",
                Some("PowerShell: npm run"),
            ),
            ("cargo run hunter2", Some("cargo run")),
            ("Bash: rm -rf target", Some("Bash: rm")),
            ("Bash", Some("Bash")),
            ("redis-cli -a hunter2 ping", Some("redis-cli")),
            (
                "mcp__fixture__run: tool run hunter2",
                Some("mcp__fixture__run"),
            ),
            ("Edit: src/lib.rs", Some("Edit: src/lib.rs")),
            ("Read: alice:hunter2@host/f", Some("Read")),
            (
                "WebFetch: https://alice:hunter2@example.invalid/a?k=v",
                Some("WebFetch: https://example.invalid/a"),
            ),
            ("Grep: fn main", Some("Grep")),
            ("Grep: hunter2", Some("Grep")),
            ("Grep: password=hunter2", Some("Grep")),
            ("Glob: **/hunter2*", Some("Glob")),
            ("WebSearch: alice hunter2", Some("WebSearch")),
            ("ToolSearch: hunter2", Some("ToolSearch")),
            ("-u alice:hunter2", None),
        ] {
            assert_eq!(wait_detail(sent).as_deref(), kept, "{sent}");
        }
    }

    /// Claude Code says a permission prompt twice: as the dialog opens,
    /// with what it is for, and six seconds later without. The second keeps
    /// the first's detail; anything else replaces it, and only a wait
    /// carries one at all.
    #[test]
    fn a_plain_wait_after_a_detailed_one_keeps_its_detail() {
        let now = now();
        let observation =
            |activity, seconds: i64, reason, detail: Option<&str>| ActivityObservation {
                activity,
                observed_at: now + chrono::Duration::seconds(seconds),
                reason,
                detail: detail.map(str::to_owned),
                source: None,
            };
        let permission = Some(WaitReason::Permission);
        let asked = observation(
            ReportedActivity::Waiting,
            0,
            permission,
            Some("Bash: cargo test\nsecond line"),
        )
        .settled(None);
        assert_eq!(asked.detail.as_deref(), Some("Bash: cargo test"));
        let again =
            observation(ReportedActivity::Waiting, 6, permission, None).settled(Some(&asked));
        assert_eq!(again.detail.as_deref(), Some("Bash: cargo test"));
        assert_eq!(
            again.current(again.observed_at),
            Some(Activity::Waiting {
                since: again.observed_at,
                reason: permission,
                judged: None,
                detail: Some("Bash: cargo test".into())
            })
        );
        // Another prompt, a question, or one long after is not the same.
        let other = observation(ReportedActivity::Waiting, 7, permission, Some("Edit: a.rs"))
            .settled(Some(&asked));
        assert_eq!(other.detail.as_deref(), Some("Edit: a.rs"));
        let question = observation(
            ReportedActivity::Waiting,
            6,
            Some(WaitReason::Question),
            None,
        )
        .settled(Some(&asked));
        assert_eq!(question.detail, None);
        let late = observation(
            ReportedActivity::Waiting,
            WAIT_DETAIL_CARRY_SECS + 1,
            permission,
            None,
        )
        .settled(Some(&asked));
        assert_eq!(late.detail, None);
        let working =
            observation(ReportedActivity::Working, 8, None, Some("Bash: ls")).settled(Some(&asked));
        assert_eq!(working.detail, None);
        let after_work =
            observation(ReportedActivity::Waiting, 9, permission, None).settled(Some(&working));
        assert_eq!(after_work.detail, None);
        let secret = observation(
            ReportedActivity::Waiting,
            10,
            permission,
            Some("Bash: echo sk-abcdefghijklmnopqrstuvwxyz"),
        )
        .settled(None);
        assert_eq!(secret.detail, None);
        let secret_after = observation(
            ReportedActivity::Waiting,
            11,
            permission,
            Some("Bash: echo sk-abcdefghijklmnopqrstuvwxyz"),
        )
        .settled(Some(&asked));
        assert_eq!(secret_after.detail, None, "its own prompt, said unseen");
        // Older readers and writers: no field, no change.
        let plain = serde_json::json!({"activity": "waiting", "observed_at": now});
        let read: ActivityObservation = serde_json::from_value(plain.clone()).unwrap();
        assert_eq!(read.detail, None);
        assert_eq!(serde_json::to_value(&read).unwrap(), plain);
    }

    /// A wait herdr reported holds only while herdr keeps renewing it, so
    /// one it stopped renewing does not stand for hours; a source a newer
    /// daemon names reads as another source, not a refused record.
    #[test]
    fn a_herdr_observation_lasts_only_as_long_as_herdr_renews_it() {
        let now = now();
        let waiting = ActivityObservation {
            activity: ReportedActivity::Waiting,
            observed_at: now,
            reason: Some(WaitReason::Other),
            detail: None,
            source: Some(ActivitySource::Herdr),
        };
        assert!(
            waiting
                .current(now + chrono::Duration::seconds(299))
                .is_some()
        );
        assert_eq!(waiting.current(now + chrono::Duration::minutes(5)), None);

        let plain = serde_json::json!({"activity": "idle", "observed_at": now});
        let read: ActivityObservation = serde_json::from_value(plain.clone()).unwrap();
        assert_eq!(read.source, None);
        assert_eq!(serde_json::to_value(&read).unwrap(), plain);
        let herdr = serde_json::json!({"activity": "idle", "observed_at": now, "source": "herdr"});
        let read: ActivityObservation = serde_json::from_value(herdr.clone()).unwrap();
        assert_eq!(read.source, Some(ActivitySource::Herdr));
        assert_eq!(serde_json::to_value(&read).unwrap(), herdr);
        let newer = serde_json::json!({"activity": "idle", "observed_at": now, "source": "zellij"});
        let read: ActivityObservation = serde_json::from_value(newer).unwrap();
        assert_eq!(read.source, Some(ActivitySource::Other));
    }

    /// What Codex's shared server said lasts five minutes whatever it says,
    /// a wait included, like herdr's: the watch renews it while the server
    /// still says it. It travels as `codex_server`, which a reader that
    /// predates it takes for another source.
    #[test]
    fn a_codex_server_observation_lasts_five_minutes_whatever_it_says() {
        let now = now();
        for (activity, reason) in [
            (ReportedActivity::Waiting, Some(WaitReason::Permission)),
            (ReportedActivity::Waiting, Some(WaitReason::Question)),
            (ReportedActivity::Working, None),
            (ReportedActivity::Idle, None),
        ] {
            let observation = ActivityObservation {
                activity,
                observed_at: now,
                reason,
                detail: None,
                source: Some(ActivitySource::CodexServer),
            };
            assert!(
                observation
                    .current(now + chrono::Duration::seconds(299))
                    .is_some(),
                "{activity:?} holds inside five minutes"
            );
            assert_eq!(
                observation.current(now + chrono::Duration::minutes(5)),
                None,
                "{activity:?} lapses at five minutes"
            );
        }
        let wire = serde_json::json!({
            "activity": "waiting", "observed_at": now, "reason": "question", "source": "codex_server"
        });
        let read: ActivityObservation = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(read.source, Some(ActivitySource::CodexServer));
        assert_eq!(serde_json::to_value(&read).unwrap(), wire);
    }

    /// Where Codex's shared server and herdr both speak for one agent, the
    /// server's current word keeps herdr's off the record, never the other
    /// way, and once it lapses herdr's counts again. The agent's own report
    /// is never kept off and keeps nothing off, and no source ranks with
    /// itself or with one this build does not know.
    #[test]
    fn a_current_codex_server_word_keeps_herdrs_off_and_nothing_else() {
        let now = now();
        let said =
            |source: Option<ActivitySource>, activity: ReportedActivity| ActivityObservation {
                activity,
                observed_at: now,
                reason: None,
                detail: None,
                source,
            };
        let server = said(Some(ActivitySource::CodexServer), ReportedActivity::Idle);
        let herdr = said(Some(ActivitySource::Herdr), ReportedActivity::Working);
        let own = said(None, ReportedActivity::Working);
        let unknown = said(Some(ActivitySource::Other), ReportedActivity::Working);
        let soon = now + chrono::Duration::seconds(30);
        let lapsed = now + chrono::Duration::minutes(5);

        assert!(ActivitySource::CodexServer.outranks(ActivitySource::Herdr));
        assert!(!ActivitySource::Herdr.outranks(ActivitySource::CodexServer));
        assert!(server.keeps_off(&herdr, soon), "the server's word wins");
        assert!(
            !server.keeps_off(&herdr, lapsed),
            "past its expiry the server is no longer saying"
        );
        assert!(!herdr.keeps_off(&server, soon), "herdr never keeps it off");
        assert!(!server.keeps_off(&own, soon), "the agent's own report");
        assert!(!own.keeps_off(&herdr, soon));
        assert!(!own.keeps_off(&server, soon));
        assert!(!server.keeps_off(&server, soon), "a source's own renewal");
        assert!(!herdr.keeps_off(&herdr, soon));
        assert!(!server.keeps_off(&unknown, soon));
        assert!(!unknown.keeps_off(&herdr, soon));
    }

    fn agent(name: &str) -> AgentId {
        AgentId::from(name)
    }

    fn key(k: &str) -> ResourceKey {
        ResourceKey::new(k)
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn the_oldest_waiter_on_an_overlapping_resource_goes_first() {
        let mut queue = WaitQueue::new();
        let first = queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, 0, now());
        let second = queue.join(agent("b"), key("task:x"), LeaseMode::Exclusive, 0, now());
        assert!(queue.is_next(first));
        assert!(!queue.is_next(second), "a newcomer cannot jump the queue");
        assert_eq!(queue.position(first), Some(0));
        assert_eq!(queue.position(second), Some(1));

        // When the head leaves — claimed, timed out, or gone — the next
        // one is next.
        queue.leave(first);
        assert!(queue.is_next(second));
        assert_eq!(queue.position(second), Some(0));
    }

    #[test]
    fn an_unrelated_resource_is_not_in_the_way() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, 0, now());
        let other = queue.join(agent("b"), key("task:y"), LeaseMode::Exclusive, 0, now());
        assert!(queue.is_next(other));
        assert_eq!(queue.position(other), Some(0));
    }

    #[test]
    fn a_hierarchical_key_is_in_the_way_of_what_it_contains() {
        let mut queue = WaitQueue::new();
        let dir = queue.join(
            agent("a"),
            key("path:/repo/src"),
            LeaseMode::Exclusive,
            0,
            now(),
        );
        let file = queue.join(
            agent("b"),
            key("path:/repo/src/lib.rs"),
            LeaseMode::Exclusive,
            0,
            now(),
        );
        assert!(queue.is_next(dir));
        assert!(!queue.is_next(file), "the directory covers the file");
    }

    #[test]
    fn readers_do_not_queue_behind_each_other() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Shared, 0, now());
        let second = queue.join(agent("b"), key("task:x"), LeaseMode::Shared, 0, now());
        assert!(
            queue.is_next(second),
            "two shared waiters can be satisfied together"
        );
        // And the number published as its queue place has to agree: a
        // waiter that may proceed at once must not be shown as second.
        assert_eq!(queue.position(second), Some(0));
        // A writer behind them still waits, behind both of them.
        let writer = queue.join(agent("c"), key("task:x"), LeaseMode::Exclusive, 0, now());
        assert!(!queue.is_next(writer));
        assert_eq!(queue.position(writer), Some(2));
    }

    #[test]
    fn a_ticket_that_left_is_nobody() {
        let mut queue = WaitQueue::new();
        let ticket = queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, 0, now());
        assert_eq!(queue.leave(ticket).map(|w| w.agent), Some(agent("a")));
        assert!(queue.leave(ticket).is_none(), "and only leaves once");
        assert!(!queue.is_next(ticket));
        assert_eq!(queue.position(ticket), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn waiting_for_names_the_one_thing_an_agent_waits_on() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, 0, now());
        assert_eq!(
            queue.waiting_for(&agent("a")).map(|w| w.resource.clone()),
            Some(key("task:x"))
        );
        assert!(queue.waiting_for(&agent("b")).is_none());
    }

    #[test]
    fn two_agents_each_holding_what_the_other_wants_is_a_deadlock() {
        // a holds x and wants y; b holds y and is about to want x.
        let holders = [(key("task:x"), agent("a")), (key("task:y"), agent("b"))];
        let waits = [(agent("a"), key("task:y"))];
        let cycle = deadlock(&agent("b"), &key("task:x"), &holders, &waits)
            .expect("b waiting for x closes the cycle");
        assert_eq!(
            cycle,
            vec![
                Blocked {
                    agent: agent("b"),
                    resource: key("task:x"),
                    held_by: agent("a"),
                },
                Blocked {
                    agent: agent("a"),
                    resource: key("task:y"),
                    held_by: agent("b"),
                },
            ]
        );
    }

    #[test]
    fn three_agents_round_a_ring_is_a_deadlock() {
        let holders = [
            (key("task:x"), agent("a")),
            (key("task:y"), agent("b")),
            (key("task:z"), agent("c")),
        ];
        let waits = [(agent("a"), key("task:y")), (agent("b"), key("task:z"))];
        let cycle = deadlock(&agent("c"), &key("task:x"), &holders, &waits).expect("a ring");
        assert_eq!(cycle.len(), 3);
        assert_eq!(cycle[0].agent, agent("c"));
        assert_eq!(cycle.last().unwrap().held_by, agent("c"));
    }

    #[test]
    fn plain_contention_is_not_a_deadlock() {
        // a holds x and wants nothing; b waiting for x simply waits.
        let holders = [(key("task:x"), agent("a"))];
        assert!(deadlock(&agent("b"), &key("task:x"), &holders, &[]).is_none());
        // Even when a is waiting, for something nobody holds.
        let waits = [(agent("a"), key("task:free"))];
        assert!(deadlock(&agent("b"), &key("task:x"), &holders, &waits).is_none());
    }

    #[test]
    fn wanting_what_you_already_hold_is_a_renewal_not_a_cycle() {
        let holders = [(key("task:x"), agent("a"))];
        assert!(deadlock(&agent("a"), &key("task:x"), &holders, &[]).is_none());
    }

    #[test]
    fn a_cycle_through_overlapping_paths_is_still_a_cycle() {
        // Hierarchical keys: holding a directory blocks a file under it.
        let holders = [
            (key("path:/repo/src"), agent("a")),
            (key("path:/repo/tests"), agent("b")),
        ];
        let waits = [(agent("a"), key("path:/repo/tests/it.rs"))];
        let cycle = deadlock(&agent("b"), &key("path:/repo/src/lib.rs"), &holders, &waits)
            .expect("the directories overlap what each wants");
        assert_eq!(cycle.len(), 2);
    }

    #[test]
    fn a_long_chain_that_does_not_close_terminates() {
        // A chain of a hundred waiters, none of which reaches back.
        let holders: Vec<_> = (0..100)
            .map(|i| (key(&format!("task:{i}")), agent(&format!("a{i}"))))
            .collect();
        let waits: Vec<_> = (0..99)
            .map(|i| (agent(&format!("a{i}")), key(&format!("task:{}", i + 1))))
            .collect();
        assert!(deadlock(&agent("newcomer"), &key("task:0"), &holders, &waits).is_none());
    }

    #[test]
    fn a_higher_priority_is_served_before_an_earlier_arrival() {
        let mut queue = WaitQueue::new();
        let early = queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, 0, now());
        let urgent = queue.join(agent("b"), key("task:x"), LeaseMode::Exclusive, 5, now());
        let late = queue.join(agent("c"), key("task:x"), LeaseMode::Exclusive, 0, now());
        let low = queue.join(agent("d"), key("task:x"), LeaseMode::Exclusive, -3, now());
        assert!(queue.is_next(urgent), "priority orders ahead of arrival");
        assert!(!queue.is_next(early));
        assert_eq!(queue.position(urgent), Some(0));
        assert_eq!(queue.position(early), Some(1));
        assert_eq!(queue.position(late), Some(2), "arrival among equals");
        assert_eq!(queue.position(low), Some(3));
        let served: Vec<Ticket> = queue.all().iter().map(|w| w.ticket).collect();
        assert_eq!(served, vec![urgent, early, late, low], "listed as served");

        // The head leaving makes the oldest of the next priority next.
        queue.leave(urgent);
        assert!(queue.is_next(early));
        assert_eq!(queue.position(late), Some(1));
    }

    #[test]
    fn readers_of_any_priority_still_do_not_queue_behind_each_other() {
        let mut queue = WaitQueue::new();
        let low = queue.join(agent("a"), key("task:x"), LeaseMode::Shared, -2, now());
        let high = queue.join(agent("b"), key("task:x"), LeaseMode::Shared, 7, now());
        assert!(
            queue.is_next(low),
            "a shared waiter ahead does not block it"
        );
        assert!(queue.is_next(high));
        assert_eq!(queue.position(low), Some(0));
        assert_eq!(queue.position(high), Some(0));
        // A writer between them in priority waits behind the higher reader
        // only; the lower reader now waits behind the writer.
        let writer = queue.join(agent("c"), key("task:x"), LeaseMode::Exclusive, 1, now());
        assert_eq!(queue.position(writer), Some(1));
        assert!(!queue.is_next(writer));
        assert_eq!(queue.position(low), Some(1));
        assert!(!queue.is_next(low));
        assert_eq!(queue.position(high), Some(0));
    }

    #[test]
    fn position_and_is_next_agree_for_every_waiter() {
        let mut queue = WaitQueue::new();
        let tickets: Vec<Ticket> = [
            ("a", "task:x", LeaseMode::Exclusive, 0),
            ("b", "task:x", LeaseMode::Shared, 2),
            ("c", "path:/repo", LeaseMode::Exclusive, -1),
            ("d", "path:/repo/src", LeaseMode::Shared, 3),
            ("e", "task:x", LeaseMode::Shared, 2),
            ("f", "task:y", LeaseMode::Exclusive, -9),
        ]
        .into_iter()
        .map(|(a, k, mode, priority)| queue.join(agent(a), key(k), mode, priority, now()))
        .collect();
        for ticket in tickets {
            assert_eq!(
                queue.is_next(ticket),
                queue.position(ticket) == Some(0),
                "ticket {ticket}"
            );
        }
    }

    #[test]
    fn a_wait_ended_to_break_a_ring_leaves_the_queue_and_says_why() {
        let mut queue = WaitQueue::new();
        let victim = queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, 0, now());
        let behind = queue.join(agent("b"), key("task:y"), LeaseMode::Exclusive, 0, now());
        let ring = vec![Blocked {
            agent: agent("a"),
            resource: key("task:y"),
            held_by: agent("c"),
        }];
        assert_eq!(
            queue.break_ring(victim, ring.clone()).map(|w| w.agent),
            Some(agent("a"))
        );
        assert!(queue.is_next(behind), "it is out of everybody's way");
        assert_eq!(queue.deadlocked(victim), Some(ring.as_slice()));
        assert_eq!(queue.deadlocked(behind), None);
        // Leaving afterwards forgets the ring and is not a second leaving.
        assert!(queue.leave(victim).is_none());
        assert_eq!(queue.deadlocked(victim), None);
        assert!(queue.break_ring(victim, ring).is_none(), "nobody to end");
    }

    /// A two-agent ring as `deadlock` reports it: `b` would wait for `x`,
    /// held by `a`, which waits for `y`, held by `b`.
    fn ring() -> Vec<Blocked> {
        vec![
            Blocked {
                agent: agent("b"),
                resource: key("task:x"),
                held_by: agent("a"),
            },
            Blocked {
                agent: agent("a"),
                resource: key("task:y"),
                held_by: agent("b"),
            },
        ]
    }

    #[test]
    fn with_equal_priorities_the_newcomer_is_the_victim() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, 0, now());
        assert_eq!(
            victim(&ring(), 0, queue.all()),
            Victim {
                step: 0,
                ticket: None
            }
        );
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, 4, now());
        assert_eq!(victim(&ring(), 4, queue.all()).ticket, None);
    }

    #[test]
    fn an_older_lower_priority_wait_is_ended_for_a_higher_newcomer() {
        let mut queue = WaitQueue::new();
        let older = queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, -1, now());
        assert_eq!(
            victim(&ring(), 3, queue.all()),
            Victim {
                step: 1,
                ticket: Some(older)
            }
        );
        // And a lower newcomer is still refused itself.
        assert_eq!(victim(&ring(), -2, queue.all()).ticket, None);
    }

    #[test]
    fn among_equal_priorities_the_youngest_wait_is_the_victim() {
        // c would wait for x (held by a); a waits for y (held by b); b
        // waits for z (held by c). a arrived before b.
        let cycle = vec![
            Blocked {
                agent: agent("c"),
                resource: key("task:x"),
                held_by: agent("a"),
            },
            Blocked {
                agent: agent("a"),
                resource: key("task:y"),
                held_by: agent("b"),
            },
            Blocked {
                agent: agent("b"),
                resource: key("task:z"),
                held_by: agent("c"),
            },
        ];
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, 1, now());
        let b = queue.join(agent("b"), key("task:z"), LeaseMode::Exclusive, 1, now());
        // An unrelated waiter of the lowest priority is not in the ring.
        queue.join(agent("d"), key("task:w"), LeaseMode::Exclusive, -9, now());
        assert_eq!(
            victim(&cycle, 5, queue.all()),
            Victim {
                step: 2,
                ticket: Some(b)
            },
            "the younger of the two equal, lower waits"
        );
        // With the newcomer as low as they are, it is the youngest of all.
        assert_eq!(victim(&cycle, 1, queue.all()).ticket, None);
        // A lower priority outweighs arrival.
        let mut queue = WaitQueue::new();
        let a2 = queue.join(agent("a"), key("task:y"), LeaseMode::Exclusive, 0, now());
        queue.join(agent("b"), key("task:z"), LeaseMode::Exclusive, 1, now());
        assert_eq!(victim(&cycle, 1, queue.all()).ticket, Some(a2));
    }

    /// A queue written by a daemon predating priorities reads as all 0.
    #[test]
    fn a_waiter_without_a_priority_reads_as_zero() {
        let wire = serde_json::json!({
            "ticket": 3, "agent": "a", "resource": "task:x", "mode": "exclusive",
            "since": now(),
        });
        let waiter: Waiter = serde_json::from_value(wire).unwrap();
        assert_eq!(waiter.priority, 0);
        assert!(valid_priority(MIN_PRIORITY) && valid_priority(MAX_PRIORITY));
        assert!(!valid_priority(MIN_PRIORITY - 1) && !valid_priority(MAX_PRIORITY + 1));
    }
}
