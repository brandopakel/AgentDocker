//! The first four views a person reads through this module: the agents
//! (`ps`), the leases, the cards of a board (`task list`) and one card
//! (`task show`).
//!
//! Each is a function of what the daemon answered, one clock, and what
//! the stream is printed for, to the text that is printed: nothing here
//! asks the daemon, reads the time or looks at the terminal, so each can
//! be held to what it prints at any width, with colour and without.
//!
//! What a script reads is not made here. These are called only for a
//! stream that is printed for a person.

use std::collections::BTreeMap;

use agentdocker_core::usage::report::Row as UsageRow;
use agentdocker_core::{
    Board, CardTimes, Comment, Lease, Plan, ProjectFile, Task, TaskEntry, Tone, journal, short,
};
use chrono::{DateTime, TimeZone, Utc};

use super::table::{Cell, Column, Table};
use super::{Caps, FAINT, HEADING, paint, style_of, text};

/// "in 4m", or "expired", against the one clock a view is made with.
fn until(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let seconds = (at - now).num_seconds();
    if seconds <= 0 {
        "expired".to_owned()
    } else {
        format!("in {}", journal::span(seconds))
    }
}

/// A moment as a person in `zone` reads it, with its offset said: a
/// time with no zone is a guess for whoever reads it somewhere else. The
/// offset is the one `zone` had at that moment, so a summer moment read
/// in winter keeps summer's.
fn moment<Tz: TimeZone>(at: DateTime<Utc>, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    at.with_timezone(zone)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

/// Who an id is, by name where the names say, and otherwise the id as
/// short as an id is shown.
fn named(names: &BTreeMap<String, String>, id: &str) -> String {
    names
        .get(id)
        .cloned()
        .unwrap_or_else(|| short(id).to_owned())
}

/// The leases: what is held, by whom, and for how long, the time left in
/// its tone (running out, expired).
pub fn leases(leases: &[Lease], now: DateTime<Utc>, caps: Caps) -> String {
    if leases.is_empty() {
        return "no leases\n".to_owned();
    }
    let mut table = Table::new(vec![
        Column::identity("LEASE ID"),
        Column::always("RESOURCE").cut_to(16),
        Column::always("HOLDER"),
        Column::optional("MODE", 1),
        Column::always("EXPIRES"),
        Column::always("NOTE").cut_to(8),
    ]);
    for lease in leases {
        let left = (lease.expires_at - now).num_seconds();
        table.row(vec![
            lease.id.to_string().into(),
            crate::format::resource(&lease.resource).into(),
            lease.holder.short().into(),
            lease.mode.to_string().into(),
            Cell::toned(until(now, lease.expires_at), Tone::of_lease_left(left)),
            lease.note.clone().unwrap_or_default().into(),
        ]);
    }
    table.render(caps)
}

/// One card of a board's list, as its command worked it out.
pub struct CardRow<'a> {
    pub task: &'a Task,
    /// How long it has been in its lane, when that is known.
    pub in_lane: String,
    /// Who holds it, by name, or `-`.
    pub holder: String,
}

/// A board's cards: each with its lane in the lane's tone, an archived
/// one set back.
pub fn cards(rows: &[CardRow<'_>], caps: Caps) -> String {
    let mut table = Table::new(vec![
        Column::identity("CARD"),
        Column::always("LANE"),
        Column::optional("IN LANE", 1),
        Column::always("TITLE").cut_to(12),
        Column::always("HOLDER").cut_to(8),
    ]);
    for row in rows {
        let task = row.task;
        let lane = task.lane.as_deref().unwrap_or(task.column.as_str());
        let archived = task.archived_at.is_some();
        table.row(vec![
            task.handle().into(),
            Cell::toned(
                if archived {
                    format!("{lane}, archived")
                } else {
                    lane.to_owned()
                },
                Tone::of_lane(task.column),
            ),
            row.in_lane.clone().into(),
            task.title.clone().into(),
            row.holder.clone().into(),
        ]);
        if let Some(last) = table.rows.last_mut() {
            last.faint = archived;
        }
    }
    table.render(caps)
}

/// What a column of `ps` is to its row. The agent's id is the row's
/// identity; its name, its state, whether it can be told something and
/// what it is doing are always there, the longer of them shortened on a
/// narrow terminal; the rest a narrow terminal does without, the process
/// id first and the project last.
fn agent_column(header: &str) -> Column {
    match header {
        "AGENT ID" => Column::identity(header),
        "NAME" => Column::always(header).cut_to(8),
        "STATUS" => Column::always(header).cut_to(12),
        "INPUT" | "DOING" => Column::always(header).cut_to(10),
        "PROJECT" => Column::optional(header, 1),
        "TOKENS 24H" => Column::optional(header, 2),
        "BRANCH" => Column::optional(header, 3),
        "RUNTIME" => Column::optional(header, 4),
        "THREAD" | "LIVES IN" | "TEAM" => Column::optional(header, 5),
        "CREATED" => Column::optional(header, 6),
        "MODEL" => Column::optional(header, 7),
        "HEAD" => Column::optional(header, 8),
        _ => Column::optional(header, 9),
    }
}

/// The agents, from the rows `ps` worked out: `tones` is each row's
/// state's tone, rows from `set_back` on are listed beside the agents
/// without being one (a process nobody registered), and `unread` names
/// what could not be read, which is said: a column that is empty because
/// its answer did not come is not a column of nothing.
pub fn agents(
    headers: &[&str],
    rows: Vec<Vec<String>>,
    tones: &[Tone],
    set_back: usize,
    unread: &[&str],
    caps: Caps,
) -> String {
    let status = headers.iter().position(|header| *header == "STATUS");
    // A process nobody registered has no agent id: its process id is
    // what tells it from another and what `adopt` takes, so where one is
    // listed that column stays.
    let unregistered = set_back < rows.len();
    let mut table = Table::new(
        headers
            .iter()
            .map(|header| match *header {
                "PID" if unregistered => Column::always(header),
                header => agent_column(header),
            })
            .collect(),
    );
    for (at, row) in rows.into_iter().enumerate() {
        let mut cells: Vec<Cell> = row.into_iter().map(Cell::from).collect();
        if let (Some(status), Some(tone)) = (status, tones.get(at))
            && let Some(cell) = cells.get_mut(status)
        {
            cell.tone = *tone;
        }
        table.row(cells);
        if let Some(last) = table.rows.last_mut() {
            last.faint = at >= set_back;
        }
    }
    // Nothing matched: said, at any width, rather than a heading over
    // nothing (or, on a terminal too narrow for the heading, nothing).
    let mut out = if table.rows.is_empty() {
        "no agents\n".to_owned()
    } else {
        table.render(caps)
    };
    if !unread.is_empty() {
        out.push_str(&paint(
            &format!(
                "could not be read: {} (not known, which is not the same as none)",
                unread.join(", ")
            ),
            style_of(Tone::Attention),
            caps,
        ));
        out.push('\n');
    }
    out
}

/// One card and what the daemon answered beside it.
pub struct Card<'a> {
    pub task: &'a Task,
    pub board: &'a Board,
    pub comments: &'a [Comment],
    pub history: &'a [TaskEntry],
    pub blockers: &'a [Task],
    pub children: &'a [Task],
    pub lease: Option<&'a Lease>,
    pub plan: Option<&'a Plan>,
    pub files: &'a [ProjectFile],
    pub times: Option<&'a CardTimes>,
    pub usage: Option<&'a UsageRow>,
}

/// What was done to a card, in words: the kind of thing, and what it was
/// done with. Nothing the record says is left out.
fn action_words(entry: &TaskEntry) -> String {
    let Ok(serde_json::Value::Object(said)) = serde_json::to_value(&entry.action) else {
        return String::new();
    };
    let kind = said
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("changed")
        .replace('_', " ");
    let with: Vec<String> = said
        .iter()
        .filter(|(key, _)| key.as_str() != "kind")
        .map(|(key, value)| {
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            format!("{} {value}", key.replace('_', " "))
        })
        .collect();
    if with.is_empty() {
        kind
    } else {
        format!("{kind} ({})", with.join(", "))
    }
}

/// One card: what it is and where it stands first, then what done means
/// and what it says, then what it waits on, and the talk and the history
/// under headings of their own. Everything the plain view has is here;
/// every time has its zone, a holder and an author their name, and
/// nothing somebody wrote can act on the terminal.
pub fn card<Tz: TimeZone>(
    card: &Card<'_>,
    names: &BTreeMap<String, String>,
    zone: &Tz,
    caps: Caps,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let task = card.task;
    let safe = |words: &str| text::shown(words).into_owned();
    let mut out = String::new();
    let mut line = |words: String| {
        out.push_str(words.trim_end());
        out.push('\n');
    };

    line(format!(
        "{}  {}",
        paint(&task.handle(), HEADING, caps),
        paint(&safe(&task.title), HEADING, caps)
    ));
    let lane = card.board.lane_of(task);
    let mut standing = vec![paint(
        &safe(&lane.name),
        style_of(Tone::of_lane(task.column)),
        caps,
    )];
    standing.push(match &task.assignee {
        Some(holder) => format!("held by {}", safe(&named(names, holder.as_str()))),
        None => "nobody holds it".to_owned(),
    });
    if task.archived_at.is_some() {
        standing.push("archived".to_owned());
    }
    line(format!("  {}", standing.join(" · ")));
    line(format!("  board {}", safe(&card.board.slug)));
    if let Some(usage) = card.usage {
        line(format!("  {}", crate::format::card_usage(usage)));
    }
    if let Some(plan) = card.plan {
        line(format!("  plan  {}  {}", plan.id, safe(&plan.title)));
    }
    for file in card.files {
        line(format!(
            "  file  {}  {} ({} bytes)",
            file.id,
            safe(&file.name),
            file.size
        ));
    }
    for link in &task.links {
        line(format!("  link  {}", safe(&link.to_string())));
    }
    if let Some(times) = card.times {
        let lanes: Vec<String> = times
            .lanes
            .iter()
            .map(|spent| {
                let name = card
                    .board
                    .lane(&spent.lane)
                    .map_or(spent.lane.as_str(), |lane| lane.name.as_str());
                let stays = if spent.stays > 1 {
                    format!(" ×{}", spent.stays)
                } else {
                    String::new()
                };
                let now = if spent.current { " (now)" } else { "" };
                format!(
                    "{} {}{stays}{now}",
                    safe(name),
                    crate::format::span_secs(spent.seconds)
                )
            })
            .collect();
        if !lanes.is_empty() {
            line(format!("  time  {}", lanes.join(" · ")));
        }
        if times.incomplete {
            line(paint(
                "  (the card's history does not show all its time: what it does not show is in no lane)",
                FAINT,
                caps,
            ));
        }
        if let Some(seconds) = times.pull_to_done() {
            line(format!(
                "  pull to done {}",
                crate::format::span_secs(seconds)
            ));
        }
    }
    if let Some(lease) = card.lease {
        line(format!(
            "  lease {} until {}",
            lease.id,
            moment(lease.expires_at, zone)
        ));
    }

    let mut block = |heading: &str, words: &str| {
        if words.is_empty() {
            return;
        }
        line(String::new());
        line(paint(heading, HEADING, caps));
        for each in words.lines() {
            line(format!("  {}", safe(each)));
        }
    };
    block("Done when", &task.acceptance);
    block("Description", &task.description);

    if !card.blockers.is_empty() || !card.children.is_empty() {
        line(String::new());
    }
    for blocker in card.blockers {
        line(format!(
            "  waits on  {}  {}",
            blocker.handle(),
            safe(&blocker.title)
        ));
    }
    for child in card.children {
        line(format!(
            "  part      {}  {}",
            child.handle(),
            safe(&child.title)
        ));
    }

    if !card.comments.is_empty() {
        line(String::new());
        line(paint("Comments", HEADING, caps));
    }
    for comment in card.comments {
        line(paint(
            &format!(
                "  {}  {}",
                moment(comment.created_at, zone),
                safe(&named(names, &comment.author))
            ),
            FAINT,
            caps,
        ));
        for each in comment.body.lines() {
            line(format!("    {}", safe(each)));
        }
    }
    if !card.history.is_empty() {
        line(String::new());
        line(paint("History", HEADING, caps));
    }
    for entry in card.history {
        line(format!(
            "  {}  {}  {}",
            paint(&moment(entry.at, zone), FAINT, caps),
            safe(&named(names, &entry.by)),
            safe(&action_words(entry))
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WRITER: &str = "aaaaaaaaaaaa0000000000000000aaaa";
    const REVIEWER: &str = "bbbbbbbbbbbb0000000000000000bbbb";
    const PROJECT: &str = "cccccccccccc0000000000000000cccc";

    fn caps(width: Option<usize>, color: bool) -> Caps {
        Caps {
            human: true,
            color,
            width,
        }
    }

    fn at(clock: &str) -> DateTime<Utc> {
        clock.parse().unwrap()
    }

    fn now() -> DateTime<Utc> {
        at("2026-01-02T03:04:05Z")
    }

    fn utc() -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(0).unwrap()
    }

    fn names() -> BTreeMap<String, String> {
        BTreeMap::from([
            (WRITER.to_owned(), "writer".to_owned()),
            (REVIEWER.to_owned(), "reviewer".to_owned()),
        ])
    }

    /// What is printed, without its escapes.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find('\u{1b}') {
            out.push_str(&rest[..start]);
            let end = rest[start..]
                .find('m')
                .map_or(rest.len(), |m| start + m + 1);
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    fn lease(id: &str, resource: &str, holder: &str, left: i64, note: Option<&str>) -> Lease {
        let mut lease = json!({
            "id": id,
            "resource": resource,
            "holder": holder,
            "mode": "exclusive",
            "acquired_at": now() - chrono::Duration::seconds(120),
            "change_seq": 7,
            "expires_at": now() + chrono::Duration::seconds(left),
        });
        if let Some(note) = note {
            lease["note"] = json!(note);
        }
        serde_json::from_value(lease).unwrap()
    }

    fn held() -> Vec<Lease> {
        vec![
            lease(
                "lease-one",
                "path:/work/app/src/api",
                WRITER,
                270,
                Some("rewriting the router"),
            ),
            lease("lease-two", "task:local-build", REVIEWER, 30, None),
            lease(
                "lease-three",
                "task:AD-9",
                REVIEWER,
                -5,
                Some("left behind"),
            ),
        ]
    }

    /// The leases with room: every column, the time left in words.
    #[test]
    fn the_leases_with_room() {
        assert_eq!(
            leases(&held(), now(), caps(Some(120), false)),
            "\
LEASE ID      RESOURCE                 HOLDER         MODE        EXPIRES   NOTE
lease-one     path:/work/app/src/api   aaaaaaaaaaaa   exclusive   in 4m     rewriting the router
lease-two     task:local-build         bbbbbbbbbbbb   exclusive   in 30s
lease-three   task:AD-9                bbbbbbbbbbbb   exclusive   expired   left behind
"
        );
        assert_eq!(leases(&[], now(), caps(Some(120), true)), "no leases\n");
    }

    /// The time a lease has left is in its tone, and only that: one
    /// running out is attention, one expired alert, and a lease simply
    /// held is not coloured as news.
    #[test]
    fn a_leases_time_left_is_in_its_tone() {
        let said = leases(&held(), now(), caps(Some(120), true));
        assert!(said.contains("\u{1b}[33min 30s\u{1b}[0m"), "{said:?}");
        assert!(
            said.contains("\u{1b}[1m\u{1b}[31mexpired\u{1b}[0m"),
            "{said:?}"
        );
        assert!(said.contains("   in 4m   "), "{said:?}");
        assert_eq!(strip(&said), leases(&held(), now(), caps(Some(120), false)));
    }

    /// On a narrow terminal the mode goes first, then the resource and
    /// the note are shortened; the lease's id is whole, and every row is
    /// one line that fits.
    #[test]
    fn the_leases_on_a_narrow_terminal() {
        for width in [80, 66] {
            let said = leases(&held(), now(), caps(Some(width), false));
            assert!(said.contains("not shown at this width: MODE"), "{said}");
            for id in ["lease-one", "lease-two", "lease-three"] {
                assert!(said.contains(id), "{said}");
            }
            for line in said.lines().take(4) {
                assert!(text::width(line) <= width, "{width}: {line:?}");
            }
        }
        let narrow = leases(&held(), now(), caps(Some(66), false));
        assert!(narrow.contains('…'), "{narrow}");
    }

    fn task(key: &str, title: &str, column: &str, assignee: Option<&str>) -> Task {
        let mut task = json!({
            "id": format!("task-{key}"),
            "project": PROJECT,
            "key": key,
            "title": title,
            "acceptance": "It is done when it is done.",
            "column": column,
            "lane": column,
            "rank": "a0",
            "size": "s",
            "tags": ["cli"],
            "created_by": "user",
            "created_at": now(),
            "updated_at": now(),
            "lane_since": now(),
        });
        if let Some(assignee) = assignee {
            task["assignee"] = json!(assignee);
        }
        serde_json::from_value(task).unwrap()
    }

    /// A board's cards: each lane in its tone, an archived card set back
    /// and said to be archived, and with colour off the same text.
    #[test]
    fn the_cards_of_a_board() {
        let mut archived = task("AD-3", "An idea that was dropped", "cancelled", None);
        archived.archived_at = Some(now());
        let tasks = [
            task(
                "AD-1",
                "Give the router one error type",
                "in_progress",
                Some(WRITER),
            ),
            task("AD-2", "Say which lease is in the way", "blocked", None),
            archived,
            task("AD-4", "Ship it", "done", Some(REVIEWER)),
        ];
        let rows: Vec<CardRow<'_>> = tasks
            .iter()
            .zip(["2m", "", "", "3d"])
            .zip(["writer", "-", "-", "reviewer"])
            .map(|((task, in_lane), holder)| CardRow {
                task,
                in_lane: in_lane.to_owned(),
                holder: holder.to_owned(),
            })
            .collect();
        let plain = cards(&rows, caps(Some(120), false));
        assert_eq!(
            plain,
            "\
CARD   LANE                  IN LANE   TITLE                            HOLDER
AD-1   in_progress           2m        Give the router one error type   writer
AD-2   blocked                         Say which lease is in the way    -
AD-3   cancelled, archived             An idea that was dropped         -
AD-4   done                  3d        Ship it                          reviewer
"
        );
        let said = cards(&rows, caps(Some(120), true));
        assert!(said.contains("\u{1b}[36min_progress\u{1b}[0m"), "{said:?}");
        assert!(
            said.contains("\u{1b}[1m\u{1b}[31mblocked\u{1b}[0m"),
            "{said:?}"
        );
        assert!(said.contains("\u{1b}[32mdone\u{1b}[0m"), "{said:?}");
        assert!(
            said.contains("\u{1b}[2mcancelled, archived\u{1b}[0m"),
            "{said:?}"
        );
        assert_eq!(strip(&said), plain);

        // Narrow: the time in the lane goes, then the title is shortened;
        // the card's key and its lane are whole.
        let narrow = cards(&rows, caps(Some(56), false));
        assert!(
            narrow.contains("not shown at this width: IN LANE"),
            "{narrow}"
        );
        assert!(narrow.contains("AD-3   cancelled, archived   "), "{narrow}");
        assert!(narrow.contains('…'), "{narrow}");
        for line in narrow.lines().take(5) {
            assert!(text::width(line) <= 56, "{line:?}");
        }
    }

    const PS: [&str; 13] = [
        "AGENT ID",
        "NAME",
        "PROJECT",
        "BRANCH",
        "HEAD",
        "RUNTIME",
        "MODEL",
        "STATUS",
        "INPUT",
        "DOING",
        "TOKENS 24H",
        "PID",
        "CREATED",
    ];

    fn ps_rows() -> Vec<Vec<String>> {
        [
            [
                "aaaaaaaaaaaa",
                "writer",
                "app",
                "main",
                "fcfd61e",
                "claude-code",
                "-",
                "running",
                "Receiving",
                "editing src/api/router.rs",
                "15.4K",
                "4242",
                "2m ago",
            ],
            [
                "bbbbbbbbbbbb",
                "reviewer",
                "app",
                "main",
                "fcfd61e",
                "codex",
                "-",
                "exited (2)",
                "Can't receive messages",
                "-",
                "-",
                "-",
                "3h ago",
            ],
            [
                "-",
                "claude",
                "app",
                "-",
                "-",
                "claude-code",
                "-",
                "unadopted",
                "Not connected",
                "-",
                "-",
                "5151",
                "1m ago",
            ],
        ]
        .iter()
        .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
        .collect()
    }

    /// The agents with room: `ps` as it has been, a state in its tone,
    /// and a process nobody registered set back.
    #[test]
    fn the_agents_with_room() {
        let tones = [Tone::Positive, Tone::Attention];
        let plain = agents(&PS, ps_rows(), &tones, 2, &[], caps(Some(200), false));
        assert_eq!(
            plain,
            "\
AGENT ID       NAME       PROJECT   BRANCH   HEAD      RUNTIME       MODEL   STATUS       INPUT                    DOING                       TOKENS 24H   PID    CREATED
aaaaaaaaaaaa   writer     app       main     fcfd61e   claude-code   -       running      Receiving                editing src/api/router.rs   15.4K        4242   2m ago
bbbbbbbbbbbb   reviewer   app       main     fcfd61e   codex         -       exited (2)   Can't receive messages   -                           -            -      3h ago
-              claude     app       -        -         claude-code   -       unadopted    Not connected            -                           -            5151   1m ago
"
        );
        let said = agents(&PS, ps_rows(), &tones, 2, &[], caps(Some(200), true));
        assert!(said.contains("\u{1b}[32mrunning\u{1b}[0m"), "{said:?}");
        assert!(said.contains("\u{1b}[33mexited (2)\u{1b}[0m"), "{said:?}");
        assert!(said.contains("\u{1b}[2munadopted\u{1b}[0m"), "{said:?}");
        assert_eq!(strip(&said), plain);
    }

    /// At the width most terminals open with, and narrower, a row is
    /// still one line: the columns go in their order, the process id
    /// first and the project last, and what is left is the agent, its
    /// name, its state, whether it can be told something and what it is
    /// doing.
    #[test]
    fn the_agents_on_a_narrow_terminal() {
        let tones = [Tone::Positive, Tone::Attention];
        let eighty = agents(&PS, ps_rows(), &tones, 2, &[], caps(Some(80), false));
        assert!(
            eighty.contains(
                "not shown at this width: PROJECT, BRANCH, HEAD, RUNTIME, MODEL, TOKENS 24H, CREATED"
            ),
            "{eighty}"
        );
        assert!(
            eighty.starts_with("AGENT ID       NAME       STATUS       INPUT"),
            "{eighty}"
        );
        for line in eighty.lines().take(4) {
            assert!(text::width(line) <= 80, "{line:?}");
        }
        let wider = agents(&PS, ps_rows(), &tones, 2, &[], caps(Some(110), false));
        assert!(
            wider.contains("PROJECT"),
            "the project is the last to go: {wider}"
        );
        let heading = wider.lines().next().unwrap();
        assert!(
            heading.contains("PID") && !heading.contains("CREATED"),
            "{wider}"
        );
    }

    /// Two processes nobody registered, of one runtime, on a terminal
    /// too narrow for the table: each is still told from the other by the
    /// process id `adopt` takes, and each still says what it is.
    #[test]
    fn unregistered_processes_stay_apart_on_a_narrow_terminal() {
        let row = |pid: &str| -> Vec<String> {
            [
                "-",
                "claude-code-session",
                "app",
                "-",
                "-",
                "claude-code",
                "-",
                "unadopted",
                "Not connected",
                "-",
                "-",
                pid,
                "1m ago",
            ]
            .iter()
            .map(|cell| (*cell).to_owned())
            .collect()
        };
        for width in [20, 40] {
            let said = agents(
                &PS,
                vec![row("5151"), row("6262")],
                &[],
                0,
                &[],
                caps(Some(width), false),
            );
            for pid in ["5151", "6262"] {
                assert_eq!(said.matches(pid).count(), 1, "{width}: {said}");
            }
            assert_eq!(said.matches("unadopted").count(), 2, "{width}: {said}");
            assert_eq!(
                said.matches("claude-code-session").count(),
                2,
                "{width}: {said}"
            );
        }
        // With room for a table, the process id is in it though a
        // narrower table of agents alone would have left it out.
        let table = agents(
            &PS,
            vec![row("5151"), row("6262")],
            &[],
            0,
            &[],
            caps(Some(75), false),
        );
        assert!(table.lines().next().unwrap().contains("PID"), "{table}");
        assert!(table.contains("5151") && table.contains("6262"), "{table}");
    }

    /// A listing that matched nobody says so at any width: on a terminal
    /// too narrow for the table's heading it used to say nothing at all.
    #[test]
    fn no_agents_is_said_at_any_width() {
        for width in [Some(20), Some(80), Some(200), None] {
            assert_eq!(
                agents(&PS, Vec::new(), &[], 0, &[], caps(width, true)),
                "no agents\n",
                "{width:?}"
            );
        }
        let unread = ["the day's tokens"];
        let said = agents(&PS, Vec::new(), &[], 0, &unread, caps(Some(20), false));
        assert!(
            said.starts_with("no agents\ncould not be read: the day's tokens"),
            "{said}"
        );
    }

    /// What could not be read is said: an empty column whose answer did
    /// not come is not a column of nothing.
    #[test]
    fn an_answer_that_did_not_come_is_said() {
        let unread = ["what each is doing", "the day's tokens"];
        let said = agents(&PS, ps_rows(), &[], 3, &unread, caps(Some(200), true));
        assert!(
            said.ends_with(
                "\u{1b}[33mcould not be read: what each is doing, the day's tokens (not known, which is not the same as none)\u{1b}[0m\n"
            ),
            "{said:?}"
        );
        let all = agents(&PS, ps_rows(), &[], 3, &[], caps(Some(200), false));
        assert!(!all.contains("could not be read"));
    }

    fn board() -> Board {
        serde_json::from_value(json!({
            "project": PROJECT,
            "slug": "main",
            "name": "Main",
            "template": "software",
            "lanes": [
                {"slug": "backlog", "name": "Backlog", "category": "backlog"},
                {"slug": "ready", "name": "Ready", "category": "ready"},
                {"slug": "in_progress", "name": "In progress", "category": "in_progress"},
                {"slug": "done", "name": "Done", "category": "done"},
            ],
            "pull_source": "ready",
            "pull_target": "in_progress",
            "key_prefix": "AD",
            "last_key": 2,
            "created_at": now(),
            "updated_at": now(),
        }))
        .unwrap()
    }

    /// One card for a person: what it is and where it stands, what done
    /// means, what it says, then the talk and the history; a holder and
    /// an author by name, every time with its zone.
    #[test]
    fn one_card_for_a_person() {
        let mut card_task = task(
            "AD-1",
            "Give the router one error type",
            "in_progress",
            Some(WRITER),
        );
        card_task.description = "Three error enums say the same thing.\nOne is unused.".into();
        card_task.links = serde_json::from_value(json!([{"kind": "pr", "target": "#12"}])).unwrap();
        let comments: Vec<Comment> = serde_json::from_value(json!([{
            "id": "comment-one",
            "task": "task-AD-1",
            "project": PROJECT,
            "author": REVIEWER,
            "body": "Mapped: three enums.\nOne of them unused.",
            "created_at": now(),
        }]))
        .unwrap();
        let history: Vec<TaskEntry> = serde_json::from_value(json!([{
            "id": "entry-one",
            "task": "task-AD-1",
            "project": PROJECT,
            "at": now(),
            "by": WRITER,
            "action": {"kind": "pulled", "lane": "in_progress"},
        }]))
        .unwrap();
        let blockers = [task("AD-7", "Agree the error's wire form", "ready", None)];
        let board = board();
        let shown = Card {
            task: &card_task,
            board: &board,
            comments: &comments,
            history: &history,
            blockers: &blockers,
            children: &[],
            lease: None,
            plan: None,
            files: &[],
            times: None,
            usage: None,
        };
        let plain = card(&shown, &names(), &utc(), caps(Some(100), false));
        assert_eq!(
            plain,
            "\
AD-1  Give the router one error type
  In progress · held by writer
  board main
  link  pr:#12

Done when
  It is done when it is done.

Description
  Three error enums say the same thing.
  One is unused.

  waits on  AD-7  Agree the error's wire form

Comments
  2026-01-02 03:04 +00:00  reviewer
    Mapped: three enums.
    One of them unused.

History
  2026-01-02 03:04 +00:00  writer  pulled (lane in_progress)
"
        );
        let said = card(&shown, &names(), &utc(), caps(Some(100), true));
        assert!(
            said.starts_with(
                "\u{1b}[1mAD-1\u{1b}[0m  \u{1b}[1mGive the router one error type\u{1b}[0m\n"
            ),
            "{said:?}"
        );
        assert!(
            said.contains("  \u{1b}[36mIn progress\u{1b}[0m · held by writer\n"),
            "{said:?}"
        );
        assert_eq!(strip(&said), plain);

        // Read in another zone, the same moments say so.
        let east = chrono::FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let there = card(&shown, &names(), &east, caps(None, false));
        assert!(
            there.contains("  2026-01-02 08:34 +05:30  reviewer\n"),
            "{there}"
        );
    }

    /// What somebody wrote on a card cannot act on the terminal, and a
    /// holder nobody can name is its id as short as an id is shown.
    #[test]
    fn a_cards_words_cannot_act_on_the_terminal() {
        let mut card_task = task("AD-2", "clear\u{1b}[2J the screen", "ready", Some(WRITER));
        card_task.description = "line one\u{1b}[31m\nline\ttwo\u{202e}".into();
        card_task.acceptance = String::new();
        let board = board();
        let shown = Card {
            task: &card_task,
            board: &board,
            comments: &[],
            history: &[],
            blockers: &[],
            children: &[],
            lease: None,
            plan: None,
            files: &[],
            times: None,
            usage: None,
        };
        let said = card(&shown, &BTreeMap::new(), &utc(), caps(Some(100), false));
        assert_eq!(
            said,
            "\
AD-2  clear\\e[2J the screen
  Ready · held by aaaaaaaaaaaa
  board main

Description
  line one\\e[31m
  line\\ttwo\\u{202e}
"
        );
        assert!(!said.contains('\u{1b}') && !said.contains('\u{202e}'));
    }
}
