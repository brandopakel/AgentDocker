# Using the desktop

`agentdocker ui` opens the native Iced app. It connects to the local daemon and
restores the last project and appearance. No browser, container engine or cloud
account is required to organize local agent work.

The screens carry names, content and the labels of controls. A state is a
labelled chip: an icon and its word, with any fix beside it — **Restart…**,
**Set up**, **Resume delivery**, **Answer**, **Reconnect here**,
**Connection**. Healthy states are chips too (*Idle*, *Working*,
*Delivered*, *Receiving messages*), so nothing is told by a colour alone. A
chip's words are its accessible name; its tooltip, when it has one, only adds
detail (a time, a reason, who), and is its accessible value. A chip is one
line of fixed height that fits its row, and the row's name is cut before the
chip is. A failure is the alert mark: a chip with the alert icon and the head
of its words (all of them its detail), or, for an error that reads as a
sentence, the amber mark with its first line beside it. What just
happened shows as a toast at the window's foot for eight seconds, and only
when the screen does not already show it.

After a send, **Queued · N sessions need attention** appears when recipients
have missing or stale input receivers, paused delivery, ended sessions or provider
limits. **Details** beside it (read aloud as *Delivery details*) lists them, one
line each with what stands in the way and **Open**; the fix is in the session's
panel. These are the facts when the message was queued, not a receipt.

A session's panel says how to get it taking messages in as few words as do it:
the one fix AgentDocker decided for it, the same one `ps --input-details` and a
send's warning name, and one action for it, never two. A live Claude Code
session shows the one command to paste — into its own folder, its own
conversation, with the AgentDocker channel — whole, on a step of tone with a
copy button that copies exactly it, and one line (*Quit it, then run this in a
terminal.*). A Claude Code session nothing of which has reached AgentDocker
shows `agentdocker setup claude-code` instead (*Run this, then restart it.*):
without setup the resume command would fail. A Codex session in a terminal
takes messages at its next prompt and nothing can wake it there, so its panel
says so and offers **Start a live Codex** (the launch form, for Codex in this
project); one that never reached AgentDocker shows `codex resume` with its
conversation when it is known. A `codex resume` terminal whose conversation runs
in Codex's shared app-server gets a receiver through that server's control socket
and then reads as any bound Codex session; a terminal Codex stopped there for an
approval or a question shows **waiting for you**. Any other session gets one line.
The details stay with that conversation or thread even if you switch while
sending. In a small pane the feedback scrolls below the input, keeping typing
and Send visible while every delivery action remains reachable.
Session **Details** and expanded tool connection details show the same fix.
Nothing is restarted or resent by opening these controls.
Once a Claude Code session has ended, its fix is **Reconnect here**, on its row
in the Earlier group and in its **Details**, in place of the command: it asks
the daemon to bring the session back under its own record — its
own tool with `--resume` and its conversation, in its own checkout, with the
AgentDocker channel, under its name — so what was queued for it, its aliases,
questions and cards stay its own; the daemon checks the launch against the
record and refuses when the process is back, a checkout differs or somebody is
attached, and the reason appears as the page's alert line. When the list shows the
session running, its pane opens in this window and Claude's own consent prompt
appears there for you to accept; a session that ends at once stays in the list
with its exit. A live session gets the command instead, because resuming a
conversation a live process still holds would start a second one, and the app
does not exit a session it does not own; where Claude Code is not installed
here, an ended session gets the command too. One press is one resume; a second
press while the first is unanswered does nothing. Nothing is sent or
acknowledged on the session's behalf.

## Projects

**Jump to…** at the top of the sidebar, or ⌘K (Ctrl+K on Linux and Windows) from
anywhere, finds an agent, a place, a project or an action by typing a few
letters: Enter goes there, Escape steps back. With nothing typed it leads with
the agents that call for you, loudest first, so ⌘K then Enter reaches the most
urgent; typing finds any live agent by its name, its project or the word for
where it stands (*waiting*, *failed*, *working*). On an agent, ⌘Enter goes to
its terminal instead: the highlighted row shows a terminal glyph, and its
spoken name says so. The palette has no key footer, and a row's folder or
standing word is searched and spoken rather than drawn. Initials work (*cc* for Claude
Code), and three or more letters in order find what nothing else did.
**Launch agent…** in it lists the installed tools.
Agents in a forgotten project stay out of Jump to and out of **Next agent that
needs you** until that project is listed again.

**Back** and **Forward**, the two arrows beside the mark at the top of the
sidebar, go through the places you went in this window, as a browser does: a
project's Chat, Agents with the session that was open, Board with the board
and the card that were open, an agent's terminal (ready to type into), a
thread beside a chat, a conversation in Messages or, in a narrow window,
the list of them, To-do, Tools, Settings. Pointing at an arrow says where it goes, by the project and
the screen or what was open there (*Back to* a project's name *· Chat*); with
nowhere to go it is greyed out. ⌘[ and ⌘] (Ctrl+[ and Ctrl+] on Linux and
Windows), a keyboard's own Back and Forward keys and the mouse's side buttons
do the same, from a text field too. Only going somewhere counts: hovering,
menus, typing, searching, filters and scrolling do not, and nor does
choosing the place you are already on. Going somewhere new after going back
drops what was ahead. A place whose project you removed, whose session is no
longer listed or whose card was archived is passed over. The window keeps
the last hundred places and forgets them when it closes. A session you
launch counts once its terminal opens, and so does a notification's message
once it is on screen.

With no saved selection, the app opens on **All projects**, with sessions grouped
under their project names. A saved project or Other sessions view is restored.
**Projects** in the sidebar returns to All projects. Choosing a project opens
its shared **Chat**, with the agents working there beside it. Under each of your
own five newest messages in a conversation, one chip says where it stands
with the agents it went to, refreshed with the screen: the word for its
loudest state — a problem before a wait, a wait before a delivery, a delivery
before an answer — in that state's tone (*Paused*, *Not delivered*, *Waiting*,
*Delivered*, *Answered*), and, when it went to several, how many the word is
true of (*Delivered · 2*, *Waiting · 1 of 3*). Pointing at it lists each
recipient's own line, which is also the chip's accessible value. Those lines
say *answered*, *delivered*, *received by the model*, *shown to its session,
not confirmed*, or why it still waits — *nothing connects its session to
AgentDocker*, *waiting for its next prompt*, *waits until it checks in* (a
browser agent, or a session away for hours), *waiting for you to stop typing
in its terminal*, *delivery paused*, *session ended before taking it*. Nothing says *read*: the daemon cannot know it. **Agents** opens the
session list and **Board** the project's cards. **More** opens a menu of History,
Files in use, Usage, AgentDocker commands and the project's Pin and Forget; the tab takes the name of the screen on view and is underlined only
while one of those screens is open. **Pause…**, the project terminal and
**Open in…** (when an editor is installed), as icons named in their tooltips,
and **Launch agent…**, the one labelled action, stay in the header on every
project screen; the chevron
beside Launch agent lists the installed tools and opens the form with one
chosen. The form is a dialog over the window, with no title, that starts from the prompt:
what the agent should work on, sent as its first message once it is launched
(optional), the tools as chips under it, its switches on one row, and **More
options** for its name and arguments, each field named by its placeholder. Launching from Board or History goes to
Agents with it open. A session's Details shows its
short ID with a copy icon named **Copy session ID**. The Commands screen offers **Previous** and
**Next** only once a command has run, and shows no output box before then.
Opening an agent takes you to its project and selects that session.

**Open project terminal**, in the project header, opens a fresh native terminal
in that project's folder. It is separate from **More → AgentDocker commands**,
which runs AgentDocker commands rather than a project shell. Clicking a live agent
in the chat's agent list goes to its terminal; the chevron beside it opens its
details. An agent AgentDocker started opens its terminal inside the app. One
started elsewhere is found where it runs: in a tmux or herdr pane, that pane is
selected first; then, on macOS, its Terminal or iTerm tab is brought forward
(macOS asks once to let AgentDocker control that app), and any other host —
Ghostty, an editor's terminal, a JetBrains IDE — is brought to the front. One
started through AgentDocker's own command in a terminal (`agentdocker codex` in
Terminal) is in that terminal, not in AgentDocker: its action reads **Go to
Terminal** and selects its tab. One found running under AgentDocker itself opens
in the app's own terminal for the session that holds it, and the app says so when
none does; AgentDocker is never offered as the place to go, nor brought forward. Ghostty
is not asked for the agent's tab: its scripting names each terminal's
folder but no tty or process, and a folder is not an identity (an agent can work
in another folder than its tab reports, and that folder can be another tab's). The
standing chip beside the name in its details says in its detail what it is
doing, its tool and where it runs (*in Terminal (ttys005)*, *in
tmux:%3*), and its terminal action is named for it (**Go to Terminal**, **Go to
tmux pane**). Nothing is typed into the agent, and no second copy is started.
An agent without a terminal to reach opens its details instead.

An agent's row, in the chat's agent list, the session list, the command
palette and a board card's holder, is one line: its mark, its name, then its
**standing** chip, the same everywhere it appears — **Needs you** (amber; a
question, its own prompt, or messages it cannot take, which the chip's detail
names), **Failed** (red, until you look), **Working**, **Waiting on a lease**,
**Starting**, **Finished** (the accent, *finished since you looked* in its
detail), **Running** (live, with no fresh report of what it is doing),
**Idle** (only when its tool reported it quiet) and **Ended**. The name is set
semibold when something there
is new or waits on you, and is cut before the chip is. "Finished since you
looked" means its turn ended, or the session did, while its project was not
on screen in a focused window; going to its terminal, opening its details or
opening its project clears it, as does the agent starting to work again;
**Mark unread** in a session's panel brings **Finished** back, to come back
to it later. In the session list a second chip follows: the time left to
answer (*4m left*, red in the last fifth of its window) while it waits on
you, else the **pull request chip**; in the chat's list it is the pull
request chip, and what the agent is doing and for how long is the name's
tooltip. The pull request chip names the pull request of the agent's branch
and its checks in a word — **#&lt;n&gt; · Passed**, **Running**, **Failed**,
**Merged**, or **Open** with none reported — and opens it in the
browser. It is read with the `gh` you are signed in to, in the background, at
most every two minutes and again when the window regains focus, for live
agents and those that ended within a day. A default branch (main, master,
develop), a project whose remote is not on GitHub, a closed pull request, or a
missing or signed-out `gh` shows no chip.

An agent you gave **roles** shows them after its name in the session list as
coloured labels, in its project's palette order — two, then **+N** — and its
spoken name reads them. Roles are yours to give and nothing has one until you
do, so a project that never uses them shows nothing. In a session's panel the
tag beside its labels (**Roles**) opens its project's roles, each ticked where
the agent holds it; pressing one gives or takes it, and several agents can hold
the same role. **Edit roles…** in that menu edits the palette: a role's dot is
its next colour (Claude Code's eight), its cross deletes it — pressed twice, as
the second press takes it from every agent holding it and says how many — and
**New role** adds one by name. The person is given no roles. With a daemon that
has no palette, none of this is shown.
**Open in…** (the code icon beside the terminal icon in the header) opens the project's folder in an editor
installed on this computer: a small menu lists the ones found (the one chosen
last first, remembered with the workspace preferences), and with only one
installed the button names it (**Open in Zed**) and opens it at once. With
none installed there is no button. On macOS the editors are the known
applications in `/Applications`, `~/Applications` and `~/Applications/JetBrains
Toolbox` — Visual Studio Code (and Insiders), Cursor, Windsurf, Zed (and
Preview), Sublime Text, Nova, BBEdit, Xcode, Fleet and the JetBrains IDEs
(IntelliJ IDEA and CE, RustRover, PyCharm and CE, GoLand, WebStorm, CLion,
PhpStorm, RubyMine, DataGrip, Android Studio) — opened with `open -a`; on
Linux, their commands on `PATH` (`code`, `cursor`, `windsurf`, `zed`, `subl`,
`idea`, `rustrover`, `pycharm`, `goland`, `webstorm`, `clion`), given the
folder. The folder is one argument, never shell text. They are found once, as
the window opens. A session working in a worktree of its own offers the same
**Open in…** in its Details, under its folder, for that worktree. The command
palette has one **Open in** row per editor for the selected project.

**Needs you**, a card headed by its chip and count (*Needs you · n*), shows
only what waits on you: questions, each row the asker, the ask, the
time-left chip (*4m left*, *Expired*) and **Answer** (a question from a
session that has since ended carries a *Session ended* chip, and its spoken
line says it was asked before the session ended), and sessions stopped at a
permission prompt or question in their own terminal, each the agent's name
with a chip for the errand — **Wants permission**, **Has a question**, or
**Waiting for you** when its hooks did not say which — read aloud as *Claude
Code wants permission in its terminal*, *has a question in its terminal* or
*is waiting for you in its terminal*. A permission prompt whose hooks said
what it is for shows that one line after the name in the quiet ink, clipped to
the row (*Bash: cargo test*, *Edit: src/lib.rs*), and the spoken line ends with
it (*… wants permission in its terminal: Bash: cargo test*); it is never an
answer, and nothing that reads like a secret is shown. **Go to Terminal** takes
you there. A
session the opt-in screen judge read as waiting from its quiet screen, rather
than one its hooks reported, reads the same way with a second chip beside the
errand, **Judged · 87%** (its detail: read from its quiet screen, how likely,
and that nothing it reported says so), and an errand of its own, **Unsent
draft**, for words left typed at its prompt; it is read aloud with the
session's own name, e.g. *Claude Code seems to want permission in its
terminal, judged from its screen, 87% likely*, or *seems to have a question*
or *seems to have an unsent draft*. The row goes when the screen changes or
can no longer be read (a second agent claims its herdr session, its pane
moves, is replaced or closes, or herdr or tmux stays silent for about twenty
seconds), when the agent's own hooks report something newer, when another
process takes its place, or when the judge is turned off. Its notification says the same with
that name (e.g. *Claude Code seems to want permission*, *Judged from its
screen, 87% likely*). Seen across
projects, each row starts with its project's name (*&lt;project&gt; · Claude Code · Badger*).
A session that is blocked by its provider, not receiving messages,
or ended with messages no model took is not a decision for you, so it is not
here: its row's **Needs you** chip marks it, with the reason in the chip's
detail, and opening it shows the details — a provider's limit is a chip with
its name (*Usage limit*) and when it lifts on your own clock printed beside it
(*resets in 42 min (15:00)*), or for a passing provider outage, which
AgentDocker tries again by itself, *Provider unavailable* and *tries again at
14:31*, beside **Resume delivery**; an ended session's kept messages are a
chip with their count (*3 not delivered*); **Review delivery** opens a live
session's paused delivery and **Dismiss** puts away an ended one's notice,
which leaves the messages as they are. Finished sessions keep their
**Finished** chip on the row.
A project's row in the rail shows its loudest agent's chip — *Needs you*
counts the agents that need you when more than one does (*Needs you · 2*) —
and its name in semibold when something there is new; how many agents run is
not shown on the row, since it is not a reason to look (the project tile's
tooltip, and its accessible name, give the folder and how many are live or
new; the row's ⋯ menu does not repeat the folder). A project with no
live agent shows no chip. Question previews use at
most 80 characters from the first line. Answer opens and reveals the exact
question without submitting or changing drafts; full approval details remain in
Inbox, where an approval card also has a line (*Why not?*) for words to go with **Deny** —
why not, or what to do instead — sent to the agent as your message once the
denial is accepted, opening with what was refused (*Denied `rm -rf build`: use
the fixture script instead*); if that message is itself refused (a pause, a full
work queue), its words go into the agent's message box instead and a toast says
it was not sent; if its answer is lost (a dropped connection), they go there
too, and the toast says it may have been sent, so check the conversation before
sending again. Until the daemon takes it the note is saved with the drafts as
that message-box text, so a window closed meanwhile shows it there on
reopening, never sent again by itself (the provider's own answer is still
exactly Deny; Allow sends no words). A command a live session asks to run is answered in the row itself:
**Deny** and **Allow once**, and **Allow for session** where Codex offers it
and the window is wide enough, send the same exact answer as the Inbox card, with
the command in the row's words; file changes, which must be looked at first,
and every other question open with **Answer**. The first three items are shown; **Show N more** expands the same list and
**Show fewer** collapses it. When nobody needs an answer or delivery review, the
same card, without a heading, can offer
**Connect** for discovered processes and **Set up** for installed tools, one
tool and its control to a row. Tools also keeps the full setup controls.

A vendor's browser extension (Claude, ChatGPT) has its own Tools row, whose
chip says **Installed in Chrome**, with the rest in its detail: *its sessions
reach here only through the connector, which is not running*, or *the
connector is ready; add it to your Claude account and its clients appear
here* while `agentdocker connector` serves on this machine, or, once a
consent has made an agent, *clients of your Claude account reach here as one
agent through the connector* (a consent is
the account's: the side panel, the apps and a terminal reaching the account's
connectors all come in as that agent, and nothing tells them apart) — with
the browser, profile and version of
each copy under **Details**, and the connector: its MCP
URL, whose tooltip says where in the vendor's settings to add it, and the
pairing code the consent page asks for (one consent each; the page then asks
which project the agent joins), or, when none serves, a *Not running* chip with
**Tailscale** and **Cloudflare** to start one, each saying on the pointer what
its address does. The row offers no **Set up**: what the extension is doing runs
in the browser and on the vendor's side, and only the connector's consent
makes a session of it. The bridge the browser launches for a command-line
tool is that tool's helper; **Connect** never
offers it and adopting it by pid is refused with what it is.

The Claude Code row offers **Wake terminal sessions** while the shell's startup
file lacks the channel flag: it previews `agentdocker setup --shell`, one
reviewable change (a `claude` function that adds the flag) applied with
**Connect** and taken back with **Undo**, and the Shell line in its **Details**
says what terminal launches do (*At their next prompt*, *Wake on
messages*), with the reason on the pointer.

The sidebar remembers projects found through agents and folders you add yourself.
**Add project…** lets you browse or enter an existing folder. It pins the project
without starting an agent, changing integrations or creating files. Later sessions
in the same repository appear there automatically. Linked worktrees share a
project and retain their session checkout details.

Quiet projects remain available. An unavailable folder stays selected and shows
the alert mark (*Project folder unavailable*) beside **Check again**. Each project row has its own **⋯** menu, shown when the
pointer is on the row or the row is selected: **Rename…**
gives the entry a name of your own in the sidebar and All projects headings (an empty name goes back to the
folder's, which is the field's placeholder), **Pin**/**Unpin**, and **Remove from list**, which keeps the folder
off the list even when its sessions are discovered again, until you add it
again or open one of its sessions (from All projects, say): going there is
asking for it, so the project is listed again, unpinned, with that session
open (the list of removed folders is bounded like the project list, at 512;
at the bound a removal is refused with a message and the project stays, so
nothing removed earlier comes back on its own). **More → Forget project** does the same for the selected project.
A removed folder is also out of AgentDocker's hands: its hooks leave the
sessions working there alone — no read-before-write guard, no leases, no
messages or journal put in front of the model — until it is listed again (the
desktop keeps the list in `uncoordinated.json` in the AgentDocker home, which
every hook reads first, at most 1 MiB; a list past that is not written).
A session already registered there still gives back its leases at a turn's
end and its record at its end. When the list cannot be written, the window
says the hooks may still coordinate those sessions and tries again every
30 seconds until it can. None
of these deletes files or stops sessions. Two projects with one name are told
apart by the tile's tooltip, which gives each folder, and by the row's spoken
name, which adds the parent folder. A folder discovered because an
agent ran there leaves the list by itself once it no longer exists, and a
folder under the per-user temporary directory (where test fixtures come and
go) is never listed by discovery, only by a pin; a pinned folder stays
either way. Shared scratch roots such as `/tmp` and `/var/tmp` remain
discoverable, including when Linux reports one as its default temporary
directory, but a folder discovered there and never pinned is not listed
among your projects: it sits in a **Temporary** fold under them (a
fixture's workspace, a trial's worktree). Until you toggle it the fold is
automatic — open while one of them has a live session or is the selected
project, closed otherwise — and from then on it is as you left it, closable
even while one runs or is selected; a chip on the fold counts the live sessions (*2 live*). Pinning
one moves it up. Two projects with one folder name (four `workspace`
fixtures, say) are read aloud with the folder above each first —
`workspace · app-a · /tmp` — so the part that tells
them apart comes before the rest. Sessions whose
project is unknown, or was only the filesystem root or your home folder (where
a session runs when nobody chose a folder), appear under **Other sessions**:
discovery never makes `/` or the home folder a project, and entries it made
before are dropped, though you can still add either yourself. Opening such a
session goes to Other sessions. Opening any other session goes to its project
and lists it when it is not listed yet (one under the per-user temporary
directory, listed only by a pin, opens on All projects instead); only a list
already holding 512 projects refuses, saying it is full and to forget an old
project first.

A session that asked something shows **Answer**, which opens that exact
question, as the project chat's **Answer N** (read aloud *Answer N questions*)
does for the oldest waiting one. A copy shows the toast *Copied to the clipboard*.

**Current** shows live sessions; **Needs input** shows this project's
unanswered, unexpired questions, including questions from a session that has
since finished; nothing else. Ended sessions are not a tab: they sit in one collapsed
**Earlier (n)** group under the current ones, including previous runs with the
same name, and a search that finds one opens the group. The group opens on
its newest eight;
**Show N older** adds eight more each time, and closing the
group forgets how far it was opened. When only an earlier
session matches, its result appears without a contradictory empty-state card.
Search applies to the
selected project, both filters and the Earlier group. Switching projects
returns to Current. Nothing is deleted when a row moves to Earlier.

Session rows show the session's small mark — its tool's logo (Claude, OpenAI,
Gemini, Copilot, Cursor, Windsurf, VS Code, OpenCode), or a letter tile for a
tool without one — and its name, then its standing chip and one more chip
(the time left to answer while it waits on you, else the pull request chip). Pointing at the name shows its tool, branch and the tokens
its provider reported over the last 24 hours when collection is on
(`1.23M tokens in 24h`, `~` where some samples did not say, nothing where
none did); the row's spoken name adds what it is doing. Sessions needing
input appear first within each project. An ended Claude Code session that can
come back carries **Reconnect here** on its row itself (the same action as in
Details, which says why a session cannot be reconnected yet); while its
resume is on its way the row says **Reconnecting…**. Select a row for terminal access, reply, or stop. On a narrow
window, the session replaces the list; **Back to sessions** returns to it. On a
wide window, it opens beside the list. A session working on its own branch (in a
worktree) shows its changes above Details, with no label: its branch, the branch it would be
merged into (the one checked out in the project's main folder) and the lines
added and removed, its files, each opening its own coloured diff (300 lines at
most, a faint `+n` for the rest; a diff too large to show is the alert mark,
and a file with no text changes shows nothing),
one line beside the alert mark for each thing that stops the merge — work
not committed, uncommitted files in the main folder, conflicts, somebody
holding files there — and nothing for what is fine, and **Merge into main**, enabled only
when nothing stops it, with the command it runs on its tooltip (`git merge
--no-ff <branch>`, in the main folder). What only the agent can fix goes back
to it as a message, one click each: **Ask &lt;agent&gt; to commit** for its own
uncommitted work, **Ask &lt;agent&gt; to resolve** for conflicts (naming the files),
and **Ask &lt;agent&gt; to fix checks** when its pull request's checks failed. The
main folder's own uncommitted files and other agents' leases stay yours to sort
out. A merge, rebase, cherry-pick, revert or bisect you have under way in the
main folder is the one blocker shown (*the main folder is in the
middle of a rebase; finish or abort it there first*); nothing is merged, and
your operation is left in place. Merging makes a merge commit in the main folder
and shows a green tick whose tooltip and spoken name say so (*Merged into main
as &lt;commit&gt;.*); a refusal is one line beside the alert mark. A refusal before execution changes
nothing. If Git fails during execution, inspect the main folder: a rejected
merge-commit hook can leave a pending merge to finish or abort. The app never
automatically aborts or resets, since a concurrent operation may belong to you.
An unreadable final operation check refuses execution. A branch that moved
since it was read is never merged unseen. Nothing is pushed. Changes are read
again when the session commits or its branch moves, and with **Refresh**. Once
nothing is left to merge the section says **Up to date**; once GitHub reports
a merged pull request whose head matches both the reviewed and the current
branch head it says **Merged #&lt;n&gt;** and offers no Merge, since a squash leaves
the branch's own commits uncounted by the main branch. Either way the next step
is right there: while the session runs there is no button, since its worktree
stays until it is done; once it has ended, **Remove worktree (may delete its
branch)** removes the clean folder, then lets Git attempt non-force branch
deletion. Git checks the configured upstream, or HEAD when none is configured;
a squash merge and the displayed commit list do not predict whether the branch
stays. The git commands it runs are its tooltip, as Merge's is. The outcome is
Git's actual deletion or refusal report, including the full commit ID when Git
reports a deletion, never a recovery command built from an earlier branch
review; the reported commit recreates a deleted branch while Git retains the
object. **Details** reveals the session ID, process,
checkout, commit and last-seen time.

Diff previews stay within 512 KiB, ending on a complete line when possible and
always on a complete character. Large Unicode patches remain readable without
interrupting review; merge guards are unchanged.
**Board** is the project's work, lane by lane. A board is data: lanes
the person names, each with a category (Backlog, Ready, In progress, Review,
Blocked, Done, Cancelled), whose turn it is (anyone, the person, an agent, a
role) and an optional limit; a project's main board starts from the classic
five, and **New agent workflow board** adds one from the `agents` template
(Backlog, Approved, Analyst, In progress, Testing, UAT, Done, Blocked,
Cancelled). The toolbar switches boards, searches (a key, or words in a
title, description or tag, a typing slip forgiven), keeps six filters as
named icons — high priority, waiting on something, waiting for you (a
Review lane that is the person's), nobody holds it, an agent holds it, and
yours and your agents' (held by the person or by an agent whose record says
the person started it — launched from the app, their own terminal, or on a
card by hand or by a rule, or a session they started themselves in a
terminal app or IDE (root's install, or one whose code signature verifies),
or over SSH, with no agent's session anywhere above it — as `list_tasks`
counts `mine`; each card still shows its holder, and it is for showing,
never permission) — and
**Tags**, a row of the project's tags to show only cards that carry them all.
**Table** lays every card of the project out as a row (key, its lane as a
chip, title, then *High priority*, its due day and its tags as chips, and its
holder; a row opens its card under it), and **Compact** drops a card's chips
for its key and title. Pressed with no
field in use, `n` opens the first lane's title line, `/` the search, `v`
switches lanes and table, `d` roomy and compact, and Esc closes, innermost
first, a menu, the title line, the open card, the board's settings, then
clears the search. **Lanes** edits the board on view: rename,
reorder, add or remove a lane, its category, owner and limit, and which lane
a pull takes from (a removed lane's cards sit in the first lane of its
category). Under it are the project's tags — each its colour (pressed, the
next one), a delete, and a field for a new one; the person's new words on a
card become tags too — and the board's rules: each a bolt (its summary on
the pointer), its name, **On**/**Off** and a delete, and a new one from when
(**Comes in** to a lane, or **Sits** there some days), what (**Comment**,
**Move** to a lane, **Archive**, **Start agent** of an installed runtime)
and a name. A lane's head is its shape, name and count (named *3 of 4* with a
limit, red past it), **Archive all** in a Done or Cancelled lane that holds
cards, and **Add a card**, which opens a title line in that lane: Enter files
it there, and the line stays open for the next. Its words are the project's
card draft: they wait while another project is on view, survive closing and
reopening the window (shown in the board's first lane), and only the reply to
their own filing clears them; a refused filing keeps them and says why under
the line. A card's face is its key, title and
chips, wrapping in a narrow lane — its due day (*Due today* amber, *Overdue*
red, else *Due* and the day), *Waiting* when it waits on something (what, from off
the board, in its detail), its first four tags by name with each tag's colour
beside it (*+n* for the rest) — and who holds it, their mark and standing
chip as every agent row draws them; a high-priority card has a red edge, a
free card in the pull source a *Ready to pull* chip, and a card another
session just changed flashes once.
Drag a card to a lane, or onto a card to land beside it; a press released
where it started opens it in place: title, description, *Done when…*,
priority and size segments, due, what it waits on off the board, tags,
**Save**, its moves as named icons (**To** the lane before or after),
**Approve** and **Send back** (with what is typed in the comment line as the
reason) in a Review lane that is the person's, **Hand to** an agent running
here or nobody, **Archive**, then its tools — **Plan** (the project's plans
to choose from, or none), **Attach a file** (the project's files), **Add a
file from disk**, **Add a link** (URL, PR, path, commit or card, and the
target; a path to something that would run when opened — an app, a script, an
installer, a shortcut or an executable file — is shown in its folder instead) and, on a card nobody holds that waits on nothing, **Start an agent
on it** (installed Codex and Claude Code, after full dependency details
load) — the plan it carries out
(opened on Plans), its files (opened with the system's app from a private
copy, or unlinked), its links (a URL or a pull request in the browser, a
path in its app, a card in place, anything else copied; each with copy and
remove), the cards it waits on and those part of it, its comments with
replies and a composer, and its history. A card an agent is started on
moves into the pull target, held by that agent, and the agent is told to
work on it.
Start checks the full card details, including prerequisites on later board
pages, and checks eligibility again when a runtime is selected. Manual and rule
starts configure the provider's idle input adapter before launching; an unavailable
adapter or incompatible launch arguments leaves the card free. Provider trust,
channel consent and tool permissions still apply in the new terminal.

**To-do**, in the sidebar under Projects, is every project's open cards in
three groups, each named by its mark: yours, your agents' (every card an
agent holds, whoever started it), and those you filed that nobody holds; within a group, by project (named above its rows),
then by due day and priority. A row is a **Done** button (a tick and the
word) that moves the card to its board's Done, its key, title, its *High
priority* and due chips, and its holder; pressed, it selects the card's
project as a click on that project does and opens the card on its board,
where it stays open; a project no longer on the list opens nothing and the
status line says so.

**More → Plans** is the project's plans, files and memory. The list is each plan's
name with a chip for where it stands (*Draft*, *Active*, *Done*, *Archived*) and a
**New plan**; the plan open is its title, its status as segments, its
Markdown in an editor, **Save** while it has changes, and the cards that
carry it out, each opening on its board. Under it are the project's files,
each opening with the system's app (a private copy) or deleted, and **Add a
file**. Under those is the project's **Memory**: a **New memory** line to keep
a fact for its agents (**Keep**, kept as the person's; the words stay until the daemon
has kept them, and stay to fix when it refuses them), a line to search it,
then each memory, newest first or the best answers to the search (at most 100;
the search finds older ones), with who kept it and when, and a cross to
forget it. Each project has its
own words to keep and search; a saved search is asked again whenever its
project comes back into view or a memory changes, and one that could not
be sent is sent on the next tick. A memory an agent keeps appears at once; a
read that fails says so over what was shown; an AgentDocker from before
memory shows a *No project memory* chip, updating AgentDocker its detail. An agent pulls the
next ready card with `pull_task` (or names one), and the board shows it in
progress under that agent at once; two agents never get one card, and a
card that waits on an open card is skipped. When the person moves, hands or
comments on a card an agent holds, that agent is told with the card linked;
what the person does is never told back to them. The Board tab counts the
cards waiting for the person. A held card shows its holder's standing chip
and pull request chip, so whether anyone is on it reads off the card. While
the board is on screen, a card follows its holder's pull request forward:
from In progress to Review once the pull request is open, and from In
progress or Review to Done once it is merged. Each move is made once, so an
agent that moves the card back is not fought. The pull is a `task:<id>`
lease: a card whose holder's lease has lapsed — expired, released, or the
agent gone — carries a *Not being worked on* chip; nobody takes it by a
plain pull, only by naming that holder (`pull_task` with `take_over_from`)
or by **Hand to**, which ends the old hold and gives a running agent the
card's lease in one step. A move back to a waiting lane, or to Done or
Cancelled, ends the hold. The board reads again on every board, card or
lease event; when it could not be read the last board stays and the status
says why. The board is read a page at a time (100 cards, by lane, within a
byte budget); when it goes on, **Show more** appends the next page where the
board ends — every ask is numbered and only its own reply moves the board,
so a late or unsolicited page is ignored; a refresh asks for as many cards
as are on view and supersedes every ask still on its way, and Show more
reads *Loading…* and takes no click while any ask is out; choosing another
project forgets the old asks. Narrow, the lanes stack.

**Pause…** beside it asks for a reason and tells every agent in the project
to hold: they read the reason as a `pause` message, the daemon refuses their
new leases until **Resume**, and while it holds the header shows a chip
beside **Resume** that starts the reason (*Paused · reason*), the whole
reason in its detail when the chip cuts it. What an agent already holds, it keeps; your own actions are not
held; only you can pause or resume, an agent asks with a message.
**Launch agent…** opens the form, led by its **Task** field. For Claude and Codex it offers **Model** and **Effort**, each a chip that opens its list: the provider's default first, named (*Default (&lt;model&gt;)*), then the models the provider shows on this machine (Codex's `models_cache.json`; Claude's aliases) and the efforts the chosen model takes. The defaults are read from the provider's own settings (`~/.codex/config.toml`, `~/.claude/settings.json`) off the UI thread; nothing is fetched. A choice becomes the tool's own options (`--model`, `--effort`, `-c model_reasoning_effort=…`) before **More options**' arguments, which win where they name one (in any spelling the tool takes, the last in force): the chip then shows the argument's value and gives way to it, and an effort the model in force does not take is dropped. Changing the tool starts from its defaults, and a read made for an earlier tool never applies. The session's panel says the model and effort it was launched with (`<model> · xhigh`). In a git project it offers **Own worktree**, on by default: the session works on a branch of its own (`agent/<name>`) in its own folder under AgentDocker's worktree directory, with the files the project's `.worktreeinclude` lists copied in; off, it works in the project folder on its current branch. Pressing the button again leaves the form open; **Cancel**
or a press outside the dialog closes it) to choose an installed CLI and start it at the project root
(the folder on the header title's tooltip). With Codex chosen and no saved trusted entry found for the project root, the form shows *Trust unconfirmed* in amber, whose tooltip directs the person to open Codex in that folder to review trust before launching. A missing trust entry alone does not establish effective sandbox or approval policy; explicit provider configuration can override those defaults. Saved trust is read off the UI thread only for an open Codex form; results for an older form, project or tool are ignored, and unreadable paths remain unknown. Every project-selection path, including notifications while the form stays open, clears the previous notice and checks the newly selected project. AgentDocker never trusts a folder on the person's behalf. Claude and Codex launches default to **Idle messages: On**;
each switch says what it means on its tooltip, and turned off this one says
that messages wait until you interact with the session. Claude still requires its
channel consent. Other tools have no automatic idle delivery and show no such switch.
This launch choice does not connect or restart an existing session. A launched
agent opens in its terminal as soon as it runs: what it asks before it starts —
Claude's *Do you trust this folder?* for a new worktree and its channel consent —
is answered there, and nothing else would show it.
**Connect** under **Not connected** adopts a discovered process for
coordination (it reads **Connecting…** and cannot be pressed again until the
daemon answers; a connected session joins the list, and a refusal says why in a toast); the row names the tool and, when known, the folder it runs in, not a
process number. Known Codex Node launchers with a native Codex child are
omitted from discovery, as is Codex's `app-server` sidecar (an API helper a
receiver or reviewer speaks to, never a session) and anything a bound
receiver started. Claude Chrome native-host helpers are also omitted, for native
and interpreter entry points; enabling Chrome in a real session keeps the agent
visible. A discovery row overlapping a live registration is hidden only
when its PID and process birth time both match. Separate registrations are never
merged by display name. Two live records for one provider session — the same
process (PID and birth time), tool, project and session id — are one row and one count everywhere (the project's live count, the
sessions list, a project's agents, Needs you), under the record that stays,
which is also the one the session's registrations reach. An agent that came in through
the connector is named for it — its tool reads **Claude (connector)** or
**ChatGPT (connector)**, not the browser extension, since any client of the
account may be calling — and has no process here; it counts as live while it is heard from
and, after an hour without a tool call, sits under **Earlier**, *not
connected* in its row's spoken name and its panel's standing chip's detail. A conversation or session that only loads the connector is not
using it. It is still registered, messages still wait for it, and its next
tool call makes it current again.
Installation or configuration alone does not prove that an agent is working.

**Stop session…** turns solid red as **Confirm stop** for five seconds, with
**Keep running** (a square ×) beside it. Confirm sends the stop request. A managed live PTY offers **Open terminal**; **Detach** closes the
view while the process continues. Finished sessions remain available under Earlier.

**Message** opens a small composer for the selected agent. **Send message** uses
the same inbox queue as messages from other agents. Its queued receipt — a
chip, *Message saved to queue*, then *Received by agent* once the agent's
receiver takes it — confirms
local acceptance; it does not mean the agent has read or completed the request.
Direct-message and thread composers show the recipient's current input readiness
as a chip in the readiness words (*Receiving messages*, *Messages may wait for
its next prompt*, *Not receiving messages*), beside a **Connection** shortcut to Tools. The shortcut preserves both drafts.
MCP/hook contact alone cannot verify idle wake; disconnected views show unavailable
readiness. The enabled Claude channel can wake an idle session. Codex and other connections
still require their supported input integration. Drafts survive navigation and
failed sends while this window remains open. A late receipt keeps any new text
you have typed, and uncertain requests are never retried automatically.

Project tabs provide:

- **Chat:** the shared project conversation with current agents and terminal actions.
- **Agents:** current sessions, connection details and reconnect actions.
- **Board:** the project's cards, lane by lane.
- **More → History:** the recent durable journal.
- **More → Files in use:** current leases, one row each: the holder's mark
  (who holds it, in what mode and why, on its tooltip), the path from the
  project's root, and a thin meter of the time left (*4m left* on its
  tooltip) that turns amber under one fifth.
- **More → AgentDocker commands:** the real bundled `agentdocker` CLI in the selected project folder:
  a **Command** field and **Run command** (with *Running* beside it while a
  command runs), and the output under them once a command has said
  something; no heading, since the More tab names the screen. It keeps
  command history and output with a bounded execution deadline.
- **More → Usage:** the tokens the providers reported for this project's
  sessions over the last 24 hours, 7 or 30 days, one row per agent, model,
  provider or hour (largest first; hours in time order), under four totals —
  input, cache read, cache write and output — and, on a wide window, a share
  bar per row: input, cache read, cache write, output and reasoning
  tokens, each shown only where samples said (`~` where some did not, `—`
  where none did; the mark's meaning is the count's tooltip and, for a
  screen reader, its value, with no legend)
  and never as an invented zero. There is no heading (the More tab names
  the screen): the window and the grouping are the two tracks, which are
  the range, and beside them a chip says where collection stands
  (*Collection caught up*, *Collection scanning*, *Collection starting*,
  *Collection off*), with how far it has got, over how many roots, and the
  overhead AgentDocker injected — *not measured yet* until it is — as its
  detail. What the totals do not cover — the current hour still filling,
  history retained only since a day, gaps in the sources (lower bounds), the
  tracking storage limit — is each total's tooltip and accessible value,
  after what its mark means. Nothing sits under the table. A window with nothing reported reads
  *No usage*. Collection is off until `agentd.toml` enables it; the screen
  then shows the empty mark and the *Collection off* chip rather than an
  empty table. A failed read keeps the last report with the failure beside
  it. The screen follows the project on view: choosing another project (or
  forgetting the current one, which selects another) reads that project's
  usage at once, and a screen opened while the daemon was away asks the
  moment it is back; a read that was on its way when the daemon went is not
  waited for.

## Messages, Inbox and tools

A Codex MCP request to continue on a website shows the requesting server, its
message, the destination (with the alert mark when it is an encoded
international name) and the full URL. **Copy link** leaves the decision
pending. Open that link in your own browser if you consent, and choose **Accept**,
**Decline** or **Cancel** in the card; each says what it means on its tooltip. Accept records consent; it does not confirm
that sign-in or another website action finished. The card has no text field:
private information belongs on the website. Expired requests disable the controls;
a failed decision send keeps its error visible. The app never opens or fetches
the website automatically.

Sessions are shown by name. Default app launches and adapter-generated names
carry the same generated-name marker. A name generated from a runtime and
an identifier (the record says so, or it is exactly that adapter's form for the
record's own pid or session) reads as the tool and a word the session's id
picks from a fixed list of animals — `Claude Code · Otter`, `Codex · Heron` —
with the shortest distinguishing start of the id after the word only when
another session of that tool in the project drew the same one (`Codex · Wren
0180`). The id is the session's for good, so nothing renames it: not a peer
ending, not a branch switch — the branch is shown with the name, never in it
(`on main` on a direct conversation's title tooltip, the branch on a session
row's name tooltip) — and
the word is the same alone as in company. **Rename…** in the session panel
gives a live session a name of your choosing (up to 64 characters, unique
among live sessions; the daemon keeps it, so every screen, the CLI and other
agents see it), and the Launch form's name field names a session as it
starts; left empty, a name is made up. A former id after a
reconciliation reads under the id it stands for now and is not listed as an
agent of its own; two records that merely share a generated name stay two
records. A name somebody chose is shown as chosen, whatever it looks like,
and is the way to give a session a word for a name. An author the window has no
record of reads as an unknown session. The session id stays under Details. In a
narrow window Messages and Inbox show either the conversation list or one
conversation; choosing one, a notification, or the next question opens that
conversation, and **Conversations** returns to the list.

**Messages** is what the rail item (named Messages then, Inbox otherwise)
opens against a daemon that keeps conversations (schema 21 and later); an
older daemon still gets the inbox below. It is shaped like a chat workspace.
The sidebar lists the project chats — the chat (`#everyone`) of each project in your
sidebar, on the project's tile, named for the project and opening that project's **Chat** — then,
under a rule, the direct messages: your conversations with agents that have something in
them, one row per agent by its stable name (with its project's name while every
project is on view), a *Live* or *Ended* chip after the name, and one row for an
identity however many ids it has had. A conversation with a session that has
ended stays listed only while it holds something unread; a search box filters
by name, and **+** starts a conversation. Every row is the name alone, on one
line, with its unread counts, and neither group has a label. What two agents said to each other is theirs, and is
not listed.
Nothing else is listed: `#all`, channels, the rooms AgentDocker opens when
two checkouts change the same files, and AgentDocker's notices to an agent
are the CLI's (`agentdocker conversations`, `agentdocker history
<conversation>`, `agentdocker channels`, `agentdocker channel …`). Unread
counts cover what is yours to answer: each project's chat and your own direct
messages, never what two agents said to each other or what AgentDocker told
them. The rail's **Messages** badge is a number only for your direct messages
(questions arrive in them) and an *Unread* chip when only a project chat has
something unread; **Mark all read**, above the rows while
anything is unread, reads all of it at once. What reaches you in `#all`, a channel or a contested room is
read through as each list arrives, so it never waits in your queue or under
a count nobody can clear here. The
sidebar, the conversation and the thread are columns with a divider between
each that drags, like the rail's beside the workspace: a name the sidebar
clips gets its room by dragging, and the widths are kept in pixels and
remembered with the workspace preferences, so a wider window gives the
conversation the room. In a smaller window or with larger text, side columns
shrink to keep the conversation usable; when the columns cannot fit, Messages
shows one pane with a way back. Expanding restores saved widths (rail 180–440
points, sidebar 200–560, thread 240–640).
The pane's header is the name alone, with what the
conversation is about (a project chat's participants, a pair's branches) on
its tooltip, and a direct conversation's *Live* or *Ended* chip after it; in a
project's Chat the page header names it and the pane has none. The pane shows the newest 200 archived messages, newest
last, with **Show earlier messages** at the top until the first is on view, a
bare rule where the day turns and an accent rule before the unread part, each
named for a reader (the day, *New*); a question keeps
its card (Answer, Allow, Deny) in place. Messages from one sender run together
under one mark and one header — the name and the time it was sent, in the
computer's own timezone and in the time format its system uses (12- or
24-hour, as the operating system's settings say), with the day on the name's
tooltip, and a *Question* or *Handoff* chip —
until the sender or kind changes, five minutes pass, or a message names you;
AgentDocker's own notices are single quiet lines that start with its name,
their time on the pointer. Long messages fold behind
**Show more**. The window keeps as much of
one conversation as the daemon does (5,000 messages), so paging back reaches
the earliest it has; when the daemon prunes, the window drops every archive
and the open thread, reads the open conversation again, and ignores replies
from before the prune; a thread whose root was pruned closes. Opening a conversation
marks it read, which acknowledges those rows for you and nothing an agent
still owns; in a narrow window only the conversation on view is read, never
the list or a thread shown instead of it. **Reply** (at a message's top right under the
pointer or keyboard focus) or *n replies* under it opens
its thread beside the conversation, or in place of it when narrow with
**‹ Conversation** to return; the thread is read whole. The thread has a
composer of its own with its own draft, and only it sends with `reply_to`;
the conversation's composer stays under the conversation and never becomes a
reply. It sends to the project (`#everyone`) or to that agent; its field
reads **Session ended** for a direct conversation whose agent is gone and
**Read only** for one between two agents, and the thread's field reads
**Reply**. Drafts survive navigation, a failed
request and disconnection. Notifications: none is posted about what the focused window already shows (the
agent whose session or terminal is open, or the project whose chat is open;
the window keeps a short-lived `viewing.json` in the AgentDocker home for the
poster), and an agent's newer notification replaces its last one in
Notification Center rather than stacking. When an agent finishes while the window is
not in front, the app posts a quiet notice itself (*Badger finished · In &lt;project&gt;.
Click to see what it did.*), one per agent, replacing its last — but not when
your next message to it is already queued, since it goes straight on to that.
An agent that stops for you in its own terminal is told the same way (*Badger
wants permission*, *has a question* or *is waiting for you · In &lt;project&gt;. It is
waiting in its terminal.*) once the stop has lasted two seconds, so a prompt you
answer at once, at the terminal, never notifies. Returning to the app or turning
these notifications off during that delay cancels the pending notice. If an agent
is already waiting when you leave the app, its two-second delay starts when the
window loses focus. Repeated focus events do not reset or duplicate the notice. A notice is
taken back from Notification Center
once it is no longer true: the question was answered, the agent moved on from
its prompt, or you looked at what it finished. Each question's notice is its own,
so taking back an answered one never removes a newer question from the same
agent, and the window's notices about an agent never share one with the
daemon's. Settings → Appearance → **Notifications** turns these off; the
switch’s accessible label names both outcomes. A notification opens the message's conversation
even after it has been read, and even when the sender's record is gone: the
archive outlives it. A notification about a message in a channel or to every
agent (`#all`) opens the direct conversation with its sender when that is an
agent, otherwise Messages, and a toast gives the `agentdocker history
…` command that reads the message; a question put to every agent waits as a
card in the direct conversation with whoever asked it. A notification for a
message has a **Reply** field (macOS): what is typed there goes from you to
where that message went — the project's everyone, or back to the agent who
wrote to you — as a reply to it, so a typed answer closes the question it
answers, without the window opening; a reply to a channel or to every agent
is refused with the `agentdocker send --to …` command that reaches it. Only the daemon's `sent` counts as sent. A reply that did
not go is said as a notification (**Open the app to recover your reply**)
and comes back to the app: the conversation opens with your words in its
composer, after anything already drafted there, and a toast says why. The
notification reads **Reply not sent** when the
daemon refused it and **Reply may not have been sent** when the connection went
before an answer, in which case read the history before sending again.
When the draft cannot take the words (draft storage is full) they wait beside
the composer with **Copy** and **Dismiss**; when the conversation itself cannot
be opened (its message or project is gone, or the message is in a channel or
`#all`) they wait at the top of the
Messages list instead, with the same two controls, whatever is on view; the
window keeps up to eight such replies and says so when a further one cannot be
kept. A notification from
another workspace's daemon hands the words to that workspace's window when
it is running; otherwise the notification carries what fits and says the app
could not keep them. A reply to a question goes to whoever asked it, wherever
it was asked, since that is the reply that closes it. Nothing typed is sent
twice on its own; what can be lost is said each time: a reply cut to a
draft's 16,000 characters, a ninth kept reply, or one another workspace's
window could not take. **Enter sends; Shift+Enter adds a line** in every composer — a
conversation's, a thread's, the inbox reply, the session message and an
answer — the same action as the button beside it, and nothing while the
draft is empty or already sending. Pasted text keeps its line breaks. Longer
messages scroll inside the composer, and unfinished multiline drafts survive
switching conversations and reopening the window. Enter during an active IME
composition does not send; held Enter does not repeat a send. **+** beside the search starts a
direct message the way Slack's New message does: one pick from the agents
running here (in the selected project when one is) opens the conversation
with it. `@` in a composer offers who is here and a pick finishes
the name (`@claude-code-<pid>`, the record's own name, which is what a mention
reaches); a row whose unread rows name the person shows an **@n** count beside
its count, and such a message is tinted amber with an
amber edge. The composer is one frame: the text over a footer that holds only
the send arrow; the keys are in the field's spoken name.
The selected project also scopes archived direct conversations. Project message search retains finished sessions' direct messages and AgentDocker notices after restart.

Conversation, thread, session and unfinished answer text is saved locally for
its daemon and restored after a normal window close; channel and `#all` drafts
found in the file are let go when it loads. Text that was in flight
returns as an editable draft and is never sent automatically; check the history
before retrying an uncertain submission. Newer edits survive older send replies.
A failed save keeps the window open with **Retry saving** and an explicit
**Close without saving** choice. An unreadable saved file is preserved. Storage
limits refuse new text visibly while keeping existing nonempty drafts. Answers
return under their original question ID without restoring approval/review or
sending state; a confirmed answer or fresh completed-question snapshot removes
the saved text. Command input and other forms remain window-local.

Inbox reads like a messenger. The left column lists one conversation per agent
with its mark, the latest line and how many items wait; **Everyone** shows all
of them. The conversation on the right starts with that agent's open questions
as cards, because they carry Answer, Allow, Deny and review controls, followed
by its messages as bubbles, newest last; long messages fold after eight lines
behind **Show more**. Under an open conversation the composer sends to that
agent (**Send**) or to every agent in its project (**Send to everyone**); a
failed send shows *Not sent* and why under the box, as does the delivery
notice when recipients need attention, and drafts survive navigation, a failed
request and disconnection. A pending answer cannot be sent twice. Delivery
does not establish that the recipient consumed it. **Clear** removes one
message from your queue and **Clear shown** the visible ones; direct messages
otherwise remain queued until a consumer explicitly takes them. The rail badge
counts open questions and waiting messages together. Notification navigation
opens the requested message, including an older one outside the recent window.
A message that carries typed links shows them under its text, each as a glyph
for its kind (named on the pointer) and its target, with the link's note on
the target's tooltip. On the Messages screen the message is a row of the archive: the row is marked
and scrolled to once its page is here, and if the conversation was already open
at its newest the pages before are read back for it, one at a time, up to five
(one in flight at a time, so refreshes of the newest page do not spend the
bound), before the person is told how many messages were searched and that
**Show earlier messages** reads further back. When the archive's start is
reached first the message is said to be no longer in the archive, and a
conversation already holding all the window keeps (5,000 messages) is not
paged further and is told that limit instead. Navigating away — another
screen, project, session or conversation, or another notification — or
losing the daemon ends the search, and a page that arrives late moves
nothing.

Each Tools row is the tool's mark and name with a state chip; the part of its
words that is not the state (*Connected*, *Configured*, what is missing) and
the version are the chip's detail. The chip says **Receiving messages** only with a fresh report from the message
delivery bound to a live session. **Messages wait for its next prompt**
(*Connected*) requires recent MCP or hook contact and no input route at all — the session
is in touch, but nothing reaches it while it is idle — and activity and
configuration alone cannot establish it; a session whose receiver is paused,
or whose provider is blocked, reads **Messages paused** and one
whose delivery has stopped reporting **Not heard from recently**, both
*Connected*, since a prompt does not release what such a route holds. Other
states distinguish **Needs setup** (whose detail says what is missing: the MCP
entry, the hooks, or the one or two hook events setup requires, for example
*StopFailure hook*), **Hooks outdated** (hooks an earlier version installed;
its detail names the events they lack, for example *CwdChanged hook*; said
even while a session is connected through them, with **Set up** offered, and
Details reads Hooks *Outdated* — a paused or silent delivery keeps its own chip
and carries the outdated hooks in its detail, still with **Set up**), **Setup
needs review**, **Waiting for contact** (*Configured*), **Integration
unavailable** and **Not installed**.
Peer input needs the opt-in adapters in [CODEX-INPUT.md](CODEX-INPUT.md) and
[CLAUDE-CHANNEL-INPUT.md](CLAUDE-CHANNEL-INPUT.md). Missing supported setup offers
**Set up**. **Details** holds the facts the tool has — MCP, Hooks, Shell,
Version, Command, Browser, URL, Code — and each live session as its name with
its readiness chip and its contact chips (*MCP · recent*, *Hooks · none*), plus **Review setup**,
**Check connections** and **Setup history**; an incomplete inventory is the
alert mark; **Other tools (n)** expands
the inventory. Delivery, a session's readiness chip, says the same words as `ps`
and a send's warning: **Ready for messages** (nothing taken
yet), **Receiving messages**, **Not receiving messages**, **Not heard from
recently**, **Messages may wait for its next prompt**, **Waits until it checks
in** (a browser agent, or a session away for hours), **Waiting for the person
to stop typing** (a live Codex whose terminal you are typing a line in holds
another sender's message until you have stopped for three seconds or pressed
Enter; nothing is typed over your line), **Can't receive
messages**, **Session ended** or the provider's limit, in a tone that follows
the state (positive while it takes them, the accent while words or the
session wait, amber when something stands in the way, quiet once ended); with words queued behind current delivery that no receipt names it
reads **Sent · waiting for the agent to take it**, since an earlier receipt says nothing about them
(a message still in the queue with a receipt from the current process counts
as delivered). The session
inspector shows the same readiness as a chip, the latest receipt in its
detail, beside a chip with the queued count (*2 queued*); an ended session's
latest receipt is *Last message* under its **Details**.
A setup review is named for its tool; its files show their MCP, Hooks or Shell
lines as a diff, and its state is the footer's chip (*Waiting for you*,
*Applying…*, *Nothing to change*). Applying a plan turns that chip to *Saved*,
whose detail says what loads it (*Start a fresh session to load it*, or a new
terminal for the shell), with **Undo** available afterwards. Fresh
sessions load saved setup; only the provider can request its required approval.
The connection check is one line per installed tool, a *Configuration
checked* chip when configuration passes or the alert mark with the first
problem printed and all of them its detail, and closes with **Close**. Busy is a *Checking…*
chip, and a failure one line beside the alert mark. Setup history lists each
saved setup with its phase as a chip (*Applied*, *Prepared*, *Undone*). A connection
failure retains the last snapshot and clearly pauses daemon actions.

## Settings and installation

Settings is a column of names beside their controls: Appearance (Theme, Text
size, Notifications, Roomy rows), Terminal (Terminal text, Terminal colours)
and Installation (Update checks, Installation), with no line of explanation
under any of them. Preferences are stored privately in the AgentDocker state directory.
A malformed preference file is preserved and reported rather than silently reset.
**Update checks** (its switch read aloud *Daily update checks*) is off by default. Enable it to check once per day while
the app is open; a known update appears at the foot of the sidebar. Download and installation
remain explicit actions. A check reads the stable channel and, on a preview
(beta) installation or once **Allow preview builds** is on, the preview channel
too, and offers the newer: a beta user is offered the final release. A beta
installation moves from one preview build to the next without asking again
(an ad-hoc-signed preview is not checked by Gatekeeper, as it could not pass;
a signed one still is);
on a stable installation a newer preview build is shown with its version and
downloads only after **Allow preview builds**. The installation screen's update
card is the state's icon on a disc, the version, and the state as a chip —
**Update available**, **You have the newest release**, or **No update
published yet** when nothing has been — with what to do about it in the chip's
detail; a feed that could not be reached is a *Couldn’t reach the release
feed* chip beside it; a check that fails is one line beside the alert mark,
*Could not check for updates* and why (no connection to GitHub, say), never
*Installation failed*, and a failed daily check is the alert mark in Settings.

The foot of the sidebar, above **Add project…**, says something about the
background service only when there is something to say, and then one thing: it
is not reachable yet (a *Reconnecting…* chip); or it is older than the app —
an older version, or another installed release of the same version, which is
what an update installed underneath a running daemon looks like — a *Service
is older* chip whose detail gives both versions, and **Restart…** beside it;
or an update is ready to install (a download icon and *Update
&lt;version&gt;*). A healthy service says nothing and takes no room. The restart's
confirmation, an amber strip across the bottom with **Restart** and **Cancel**,
asks *Restart the background service?* with what a restart costs as counts:
how many sessions AgentDocker itself launched stop (and how many start again).
Sessions started in a terminal keep running and reconnect, and the strip says
nothing of them. A
request the older service has never heard of reads *The background service is
older than this app…* instead of the service's parser error, and no screen
shows raw error codes or Rust debug output.

Settings → Installation → Versions → **Manage** (read aloud *Manage installation and
retained versions*) opens the screen that previews installation, rollback,
cleanup and launcher removal. Apply is tied to the reviewed payload or cleanup
plan and refuses stale inputs. Running versions and agents remain protected by
the existing installer rules. Public macOS downloads require Developer ID signing
and notarization; locally signed builds are explicit previews.

## Keyboard and terminal

- Tab and Shift-Tab move between controls; Enter/Space activate a focused
  button, once, and not when a field before it already took the key. One
  control has the focus at a time.
- The focus ring is the keyboard's. A click focuses a control without one,
  wherever the click moves it (a row that opens a session, a list that a new
  session reorders under the click), and without scrolling it. Only a key
  that moves or uses the focus — Tab, F6, an arrow, Enter or Space — shows it
  on whatever is focused; a modifier, a Command, Control or Option shortcut,
  Escape, a letter (the board's keys are letters) or a key that only scrolls
  does not. A press on a label inside a row, a status chip say, is the row's;
  a chip never takes the focus. A focused button that becomes unavailable
  lets the focus go.
- A click anywhere else takes the focus away: from a button, from a field
  (whose draft stays) and from the terminal, which then takes no typing until
  it is pressed again. A click in a field's words puts the caret there.
- Escape or a press outside closes a menu that floats over the page. Opened
  from the keyboard, a menu (a project's **⋯**, **More**, **Launch agent**'s
  tools, **Open in…**, **Roles**, **Model** and **Effort**) takes the focus to
  its first entry; Escape, or an entry chosen with Enter or Space that closes
  the menu, gives the focus back, ringed, to the control that opened it.
  Opened or closed by a click, a menu leaves no ring anywhere.
- **Launch agent…** takes the focus into its task field as it opens; nothing
  under the dialog's scrim keeps the focus or answers keys.
- A message's **Reply** shows under the pointer or the keyboard's focus; a
  click on it does not keep it showing once the pointer has left.
- Command/Ctrl+1–4 switch Projects, Inbox, Tools and Settings.
- Command/Ctrl+[ goes back and Command/Ctrl+] forward, from a text field too;
  so do a keyboard's Back and Forward keys and the mouse's side buttons, and
  the rail's arrows, which like any click leave no ring behind. On
  Linux and Windows, Ctrl+[ and Ctrl+] in an agent's terminal stay the
  terminal's (Escape and detach); the keyboard's Back and Forward keys still
  go back and forward there.
- Command/Ctrl+N opens **Launch agent…** for the project on screen.
- Command/Ctrl+Shift+U goes to the next agent that needs you (questions, waits
  and failures first, then finished work you have not seen), round again at
  the end; **Next agent that needs you** in Jump to does the same.
- Escape closes session details or an add/launch form.
- F6 moves focus out of terminal input. Control+] detaches.
- Drag across terminal cells to select text. Command+C on macOS, or Ctrl+Shift+C
  elsewhere, copies the selection (the visible screen when no range is selected).
  Selected text stays stable while new output drains; copy or resumed input
  releases it. Command+V/Ctrl+Shift+V pastes using the agent's bracketed-paste mode.

On macOS the dock icon shows how many questions and prompts are waiting on you,
in every project: the rows Needs you holds. It shows nothing when none are waiting.

Text inputs and the terminal support native input methods. Focus indicators,
and the words every state chip prints and carries as its accessible name,
complement color. Native accessibility adapters expose control
labels, values and actions. Platform screen-reader and input-method validation is described in
[the design and validation contracts](ICED-DESIGN.md).

MCP form questions name the requesting server and show typed fields,
descriptions and editable defaults; a required field's name ends with an
asterisk, and what a field takes (its format and bounds) is the name's
tooltip. Optional fields can be omitted with **Omit**.
Submit validates the whole form before sending; Decline and Cancel share no values.
Editing alone never submits. Form edits stay in window memory until submission,
expiry or closing the window; ordinary saved drafts remain separate. Submit's
tooltip says that submitted answers are kept in the conversation and that
passwords, API keys, access tokens and payment credentials do not belong in it.

Temporary managed-Codex questions use a separate masked form above the current
page, naming the session and project, with the seconds left. Every field in a mixed secret/ordinary
bundle is masked; its edits stay only in memory and never join ordinary drafts.
The person must acknowledge that Codex/model may retain or repeat the values
(the notice is printed on the form, since it is what is acknowledged)
before Submit once is enabled. Submission clears the edits and cannot be repeated
by this window, even if its acknowledgement is lost: the form then shows an
*Awaiting confirmation* chip in place of its fields. Cancel, expiry, closure and
disconnection also clear them. Accessibility marks these controls as password
inputs without publishing their values.
The provider producer is behind its development switch (`secret_input` in the `[experimental]` table of `agentd.toml`).
