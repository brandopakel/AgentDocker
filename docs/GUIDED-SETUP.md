# Guided setup and connection checks

Codex plans include MCP and a separate lifecycle `hooks.json`, respecting
`CODEX_HOME`. Both files use the same private receipt and preflight rules.
Existing hooks are preserved; review and trust new definitions in Codex `/hooks`.
Hooks deliver queued messages at prompt/tool/Stop boundaries with acknowledgement
after output. See [messaging](ARCHITECTURE.md#messaging).

In the native agentdocker window, open **Tools**, expand **Details**, choose **Review setup**, inspect the tool, configuration path and executable, then **Apply changes**. The window also offers **Undo this setup**, **Setup history**, and **Check connections**. Applying a plan does not reconfigure an already-running provider session; start a fresh session to use it.

The equivalent CLI flow is:

```sh
agentdocker setup codex claude-code --preview
agentdocker setup --show PLAN_ID
agentdocker setup --apply PLAN_ID
agentdocker setup --health
agentdocker setup --list
agentdocker setup --undo PLAN_ID
```

Preview prints the new plan ID on stdout and its redacted description on stderr. `--json` prints a machine-readable description instead. The public description includes paths, channels and the AgentDocker executable, never the contents of existing provider configuration. Ordinary `agentdocker setup [runtime...]` creates and applies a saved plan, prints its ID and an undo command, and supports `--json`. `--dry-run` is read-only; `--shell` saves a preview that needs an explicit apply. The native window uses the same saved-plan apply/undo safeguards.

Guided Claude Code setup installs the complete seven-event hooks adapter in `.claude/settings.json`. When MCP registration is missing and the Claude CLI is available, setup delegates registration to that CLI; it does not restore or rewrite `.claude.json` as a file snapshot. The receipt tracks ownership for undo. Identity reuse is verified; existing duplicates are repaired offline with `identity-repair`. Codex receives both stdio MCP and activity hooks; other supported JSON MCP hosts receive their stdio adapter. Other runtimes remain discoverable without claiming an unsupported integration. Existing verified registrations are preserved. A disabled or unrecognized entry under the reserved `agentdocker` key requires user review rather than replacement. Inventory labels it `unverified`, even when another alias is configured correctly; **Review setup** and **Check connections** remain available. Missing registration and malformed or unreadable configuration are also reported separately.

## Shared coordination skill

Setup also installs the bundled [AgentDocker skill](../crates/cli/skills/agentdocker/SKILL.md)
for Codex, Claude Code and Gemini CLI. OpenCode reads skills from `~/.claude/skills`
too, so it sees the Claude Code copy; for OpenCode, setup instead writes its MCP entry
(`mcp` in `~/.config/opencode/opencode.json`) and the AgentDocker plugin
(`~/.config/opencode/plugins/agentdocker.js`, stamped and undone like a skill, an
edited copy preserved), which gives OpenCode the lease refusals, messages and idle
wake-up Claude Code gets from its hooks. MCP onboarding uses the same instruction
source, with receipt guidance selected for the active delivery adapter. The
skill includes CLI equivalents, so its workflow is independent of a model vendor.
`agentdocker skill` prints the installable file without contacting the daemon.

The destination is `skills/agentdocker/SKILL.md` beneath the selected Codex or
Claude profile (`CODEX_HOME` / `CLAUDE_CONFIG_DIR`, defaulting to `~/.codex` /
`~/.claude`), or `~/.gemini` for Gemini CLI. Saved plans show the skill alongside
MCP/hooks and support the same apply, recovery and undo. An unchanged generated
copy can be upgraded using its complete-content SHA-256 stamp; a custom or edited
copy is preserved and reported for comparison. Undo refuses later edits.

The [Codex skill loader](https://learn.chatgpt.com/docs/build-skills),
[Claude skill loader](https://code.claude.com/docs/en/skills) and
[Gemini skill loader](https://geminicli.com/docs/cli/skills/) support the common
SKILL.md format. An installed skill is discovered, not automatically activated
for every prompt. Other tools can use the exported file through their own
documented loader; desktop inventory alone does not imply skill support.
Provider trust, skill policy and tool permissions still apply. No AGENTS.md,
CLAUDE.md or global approval settings are rewritten.

## Apply, recovery and undo

Saved plans live in `$AGENTDOCKER_HOME/setup` (default `~/.agentdocker/setup`), with directory mode 0700 and receipt mode 0600. Receipts contain before/after configuration snapshots, which may include secrets already in those files: keep that directory private and out of source control, exports and shared diagnostics. Existing configuration files also keep the private backups used by the legacy setup writer.

All planned files and delegated provider entries are checked before the first write. Apply refuses configuration changed since preview, a changed symlink target, or an unavailable previewed executable. Each file is replaced atomically, preserving existing content outside the integration and preserving symlink targets. A durable `applying` receipt precedes writes. After interruption, applying the same plan resumes only if every file still matches its recorded before or after state.

A multi-file plan is **not one filesystem transaction**. A failure can leave a partially applied plan; its ID is retained for inspection, resume or undo. Undo has the same recovery behavior, refuses later user edits, restores original bytes, and removes a newly created configuration file while keeping its directory. A completed/undone plan does not silently reapply after external changes. Saved plans can be reopened after the app restarts. The list shows at most the 100 most recently modified receipts; a known ID can still be opened directly. Unreadable or incompatible receipts are preserved and counted while healthy plans remain visible.

For delegated Claude MCP registration, a new receipt records the complete planned
server entry and a unique ownership marker in its environment. Undo requires
that exact entry, including command, arguments, environment and flags. A
matching runtime name alone does not authorize removal. Existing registrations
are left alone; an interrupted add cannot claim a later registration with a
different marker. Receipts without this evidence refuse to remove a
present entry. Invalid provider JSON or an invalid `mcpServers` container fails
preflight before hook changes. A provider command succeeds only when the exact
planned entry appears after add, or the reserved entry is absent after remove.

The provider CLI remains the writer of its live application state, using its
[documented MCP registration interface](https://code.claude.com/docs/en/mcp).
AgentDocker writers lock the canonical configuration targets for the entire
operation. Guided apply/undo, legacy setup and hook installation coordinate even
when they use different AgentDocker homes; symlink aliases share a lock. A busy
target fails before configuration or receipt changes. Independent profiles can
still be configured concurrently. Locks live in a private per-user namespace
under `/tmp`, independent of endpoint and profile overrides; do not delete lock
files while setup operations are running.

These locks and ownership checks are not a compare-and-swap transaction with an
independent provider CLI or editor. Such writers can still race between validation
and the provider command, so edits to the same MCP entry need coordination.
Ordinary unrelated provider application-state updates do not invalidate a receipt.

## Removal

`agentdocker setup --remove [runtime...]` plans taking AgentDocker's wiring out of every tool it knows (or those named), installed or not, without the receipt that wired it, and applies it at once; `--remove --preview` saves it for review and `setup --apply`. Only what runs AgentDocker goes, recognised as setup and health recognise it: a Claude Code, Codex or JSON-host hook object or MCP entry whose command runs `agentdocker` with its own arguments (an entry under the reserved name that runs anything else stays, and is said); Codex's `[mcp_servers.agentdocker]` table and the tables under it, taken out by lines and kept only when the rest parses to exactly the document without it (a dotted-key entry is left to remove by hand); OpenCode's `mcp.agentdocker`; skills and the OpenCode plugin only while their stamp verifies (an edited one stays, and is said); the shell block between its markers. Hook groups and events left empty go with them; other tools' hooks, theirs beside ours in one group included, stay. Claude Code's MCP entry is removed through `claude mcp remove`, never by rewriting `.claude.json`, and only when it has exactly the shape `claude mcp add` writes (`type: "stdio"`, the command, string arguments and an `env` of strings, nothing else), so the undo puts it back exactly; otherwise the plan says to remove it by hand. An `--env` value it saved is masked wherever the plan or an error shows it. Codex's table is cut from its header to its last key, with the one blank line setup put before it; a comment or blank line that belongs to the next table, or ends the file, stays. The plan is an ordinary receipt (format 2: a change may delete a file; a format 1 receipt never does, and is refused if it says so): apply refuses a file changed since the preview, and `setup --undo` restores each file to the byte, recreates a deleted skill or plugin, and re-adds the exact Claude Code entry it removed. `desktop uninstall` does not touch provider configuration; run removal first.

## What a connection check proves

Tools separates saved configuration, recent adapter contact and input delivery.
Applying setup says **Setup saved**; checking the configuration says
**Configuration checked**. Neither operation establishes a provider connection.
A fresh MCP request or verified hook callback records its adapter and process
birth, with contact expiring after five minutes. General activity and leases do
not establish contact. Details lists the evidence for each live session, so a
different session's callback cannot establish that session's readiness.

The managed Codex receiver and Claude channel refresh input readiness every
30 seconds while their input loop is working. Periodic refresh attempts are bounded to 250 ms; a refusal or timeout does not end the input transport. Receipts still must persist before ACK. Reports expire after 90 seconds;
an older process's report or receipt cannot make a replacement ready. Before
the first exact receipt the session says **Ready for messages**. **Receiving
messages** requires that receipt and current receiver evidence. Paused delivery, missing recent evidence and unavailable daemon state
have distinct labels. Ordinary MCP/hooks contact says **Connected · messages
wait for its next prompt**. For idle delivery, create a session with **Receive
messages while idle** enabled; setup cannot retrofit an existing provider
process. Start a fresh session after applying setup and follow only integration
prompts actually required by that provider.

**Check connections** separates configuration detection from daemon connectivity. It makes a bounded ping to the selected socket without starting a daemon. Per-adapter diagnostics distinguish missing or malformed configuration, disabled registrations, incomplete hook coverage, unrecognized wrappers and unavailable executables. Configuration reads are limited to regular UTF-8 files up to 8 MiB; a FIFO does not wait for a writer. JSON output contains paths and fixed diagnostics, never configuration snapshots, arguments, environment values or parser excerpts.

MCP diagnostics recognize a direct `agentdocker mcp` launch and the `mcp --runtime <runtime>` arguments generated by setup, with the runtime value matching the tool being inspected. Unknown or different runtime names remain unverified. Other arguments and shell wrappers remain unverified, including commands that would only print help or fail argument parsing. This conservative check does not alter custom registrations.

An available executable is not a successful adapter call. Checks never execute configured commands or contact a model, and do not prove provider message consumption. Bare commands are checked against the inspecting process's PATH; relative paths need an explicit working directory. Claude user settings that disable hooks are reported, while project/managed settings may override them. Claude Code can use its complete hooks adapter without an optional MCP registration. These checks do not resolve every provider setting layer or certify that an available executable will run correctly. MCP approval remains controlled by the provider; setup does not preapprove tools.

The [local trial](LOCAL-TRIAL.md) requires a fresh real-provider round trip.

Provider configuration follows the installed CLI capabilities and the official [Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) and [Claude Code hooks reference](https://code.claude.com/docs/en/hooks). Packaging and signing are documented in [DISTRIBUTION-SETUP.md](DISTRIBUTION-SETUP.md).


CLI inventory also checks standard installation directories when a native app inherits a minimal PATH. Connection checks continue to use the inspecting process's actual PATH; finding a CLI in an inventory fallback does not validate a bare MCP command. Selecting a runtime explicitly inspects only that target, so an unrelated malformed desktop launcher does not block its setup preview. Codex desktop and ChatGPT have independent inventory rows without a supported setup adapter; the Codex CLI configuration is not treated as their connection health.

Codex hooks must belong to a configuration layer the running provider actually
loads. The delivery smoke keeps the hook configuration layer active and does not use
`--ignore-user-config`, which also suppresses inline hook overrides. Inline
`hooks.<event>` invocation overrides with invocation-only trust can run vetted
fixture hooks; this does not grant trust to installed user hooks. Keep configuration health unverified until
an actual callback is observed; this is separate from a passing MCP round trip.
For isolated trials, explicitly configure the MCP child's `AGENTDOCKER_HOME`,
`AGENTDOCKER_SOCKET` and `AGENTDOCKER_NO_AUTOSTART`: Codex filters the inherited
MCP environment. A provider's shell environment alone does not pin its MCP endpoint.
The activity adapter resolves physical checkout aliases, so macOS `/tmp` and
`/private/tmp` do not reject a report from the same directory. A different
checkout remains a mismatch. Codex interrupt hooks use its documented maximum
three-second timeout; other activity hooks retain a fifteen-second outer limit.

Claude Code inventory, connection checks, user hooks and guided setup respect
[`CLAUDE_CONFIG_DIR`](https://code.claude.com/docs/en/env-vars). A selected profile
uses `settings.json` and `.claude.json` inside that directory; without an
override the default is `~/.claude/settings.json` and `~/.claude.json`.
Saved delegated steps pin the selected profile directory at preview. Apply,
resume and undo set that profile only in the provider child process; a plan for
the default profile explicitly removes an inherited override from that child.
Changing the invoking shell's profile after preview does not redirect the saved
plan. Setup does not switch the profiles of existing provider sessions.

## Ordinary setup

Ordinary `setup <runtime>` persists a new plan before applying through the same
guarded flow as the app. A failed apply retains its receipt for review, resume or
undo. Repeated setup owns only its own changes; undo refuses later user edits.
Ordinary Claude setup refuses unverified MCP configuration or a missing CLI
needed for registration before writing hooks or skills; an explicit preview can
still offer hooks-only changes for review. Preview/apply remains available when
review before writing is preferred.
