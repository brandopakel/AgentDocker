# Guided setup and connection checks

Codex plans include MCP and a separate lifecycle `hooks.json`, respecting
`CODEX_HOME`. Both files use the same private receipt and preflight rules.
Existing hooks are preserved; review and trust new definitions in Codex `/hooks`.
Hooks deliver queued messages at prompt/tool/Stop boundaries with acknowledgement
after output. See [activity and messaging](ACTIVITY-AND-MESSAGING.md).

In the native agentdocker window, open **Connections**, choose **Review setup**, inspect the tool, configuration path and executable, then **Apply changes**. The window also offers **Undo this setup**, **Saved setup plans**, and **Check connections**. Applying a plan does not reconfigure an already-running provider session; start a fresh session to use it.

The equivalent CLI flow is:

```sh
agentdocker setup codex claude-code --preview
agentdocker setup --show PLAN_ID
agentdocker setup --apply PLAN_ID
agentdocker setup --health
agentdocker setup --list
agentdocker setup --undo PLAN_ID
```

Preview prints the new plan ID on stdout and its redacted description on stderr. `--json` prints a machine-readable description instead. The public description includes paths, channels and the AgentDocker executable, never the contents of existing provider configuration. Plain `agentdocker setup` and `--dry-run` retain their existing CLI behavior; the native window uses the saved-plan flow.

Guided Claude Code setup installs the complete six-event hooks adapter in `.claude/settings.json`. When MCP registration is missing and the Claude CLI is available, setup delegates registration to that CLI; it does not restore or rewrite `.claude.json` as a file snapshot. The receipt tracks ownership for undo. Identity reuse and existing duplicate reconciliation still require the delivery plan's lifecycle review. Codex receives both stdio MCP and activity hooks; other supported JSON MCP hosts receive their stdio adapter. Other runtimes remain discoverable without claiming an unsupported integration. Existing verified registrations are preserved. A disabled or unrecognized entry under the reserved `agentdocker` key requires user review rather than replacement. Inventory labels it `unverified`, even when another alias is configured correctly; **Review setup** and **Check connections** remain available. Missing registration and malformed or unreadable configuration are also reported separately.

## Apply, recovery and undo

Saved plans live in `$AGENTDOCKER_HOME/setup` (default `~/.agentdocker/setup`), with directory mode 0700 and receipt mode 0600. Receipts contain before/after configuration snapshots, which may include secrets already in those files: keep that directory private and out of source control, exports and shared diagnostics. Existing configuration files also keep the private backups used by the legacy setup writer.

All planned files and delegated provider entries are checked before the first write. Apply refuses configuration changed since preview, a changed symlink target, or an unavailable previewed executable. Each file is replaced atomically, preserving existing content outside the integration and preserving symlink targets. A durable `applying` receipt precedes writes. After interruption, applying the same plan resumes only if every file still matches its recorded before or after state.

A multi-file plan is **not one filesystem transaction**. A failure can leave a partially applied plan; its ID is retained for inspection, resume or undo. Undo has the same recovery behavior, refuses later user edits, restores original bytes, and removes a newly created configuration file while keeping its directory. A completed/undone plan does not silently reapply after external changes. Saved plans can be reopened after the app restarts. The list shows at most the 100 most recently modified receipts; a known ID can still be opened directly. Unreadable or incompatible receipts are preserved and counted while healthy plans remain visible.

For delegated Claude MCP registration, a new receipt records the complete planned
server entry and a unique ownership marker in its environment. Undo requires
that exact entry, including command, arguments, environment and flags. A
matching runtime name alone does not authorize removal. Existing registrations
are left alone; an interrupted add cannot claim a later registration with a
different marker. Older receipts without this evidence refuse to remove a
present entry. Invalid provider JSON or an invalid `mcpServers` container fails
preflight before hook changes. A provider command succeeds only when the exact
planned entry appears after add, or the reserved entry is absent after remove.

The provider CLI remains the writer of its live application state, using its
[documented MCP registration interface](https://code.claude.com/docs/en/mcp).
AgentDocker writers now lock the canonical configuration targets for the entire
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

## What a connection check proves

**Check connections** separates configuration detection from daemon connectivity. It makes a bounded ping to the selected socket without starting a daemon. Per-adapter diagnostics distinguish missing or malformed configuration, disabled registrations, incomplete hook coverage, unrecognized wrappers and unavailable executables. Configuration reads are limited to regular UTF-8 files up to 8 MiB; a FIFO does not wait for a writer. JSON output contains paths and fixed diagnostics, never configuration snapshots, arguments, environment values or parser excerpts.

MCP diagnostics recognize a direct `agentdocker mcp` launch and the `mcp --runtime <runtime>` arguments generated by setup, with the runtime value matching the tool being inspected. Unknown or different runtime names remain unverified. Other arguments and shell wrappers remain unverified, including commands that would only print help or fail argument parsing. This conservative check does not alter custom registrations.

An available executable is not a successful adapter call. Checks never execute configured commands or contact a model, and do not prove provider message consumption. Bare commands are checked against the inspecting process's PATH; relative paths need an explicit working directory. Claude user settings that disable hooks are reported, while project/managed settings may override them. Claude Code can use its complete hooks adapter without an optional MCP registration. These checks do not resolve every provider setting layer or certify that an available executable will run correctly. MCP approval remains controlled by the provider; setup does not preapprove tools.

The [local trial](LOCAL-TRIAL.md) requires a fresh real-provider round trip. During the native delivery work, a disposable Claude Code session received a token through hooks and wrote it to its fixture file; a fresh Codex session received a different token through MCP, echoed it to its fixture peer and recorded it in the journal. Both used private fixture IPC and left the monitored provider settings unchanged. This is bounded acceptance of those installed CLI versions, not certification of every provider/version or sustained use. The first Codex run correctly failed consumption assertions because tool approval was not configured; the passing run explicitly approved only its five fixture tools for that invocation.

Provider configuration follows the installed CLI capabilities and the official [Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) and [Claude Code hooks reference](https://code.claude.com/docs/en/hooks). Private raw trial records remain outside the repository. Packaging and signing are documented in [DESKTOP-DISTRIBUTION.md](DESKTOP-DISTRIBUTION.md).


CLI inventory also checks standard installation directories when a native app inherits a minimal PATH. Connection checks continue to use the inspecting process's actual PATH; finding a CLI in an inventory fallback does not validate a bare MCP command. Selecting a runtime explicitly inspects only that target, so an unrelated malformed desktop launcher does not block its setup preview. Codex desktop and ChatGPT have independent inventory rows without a supported setup adapter; the Codex CLI configuration is not treated as their connection health.

Codex hooks must belong to a configuration layer the running provider actually
loads. An earlier trial with `--ignore-user-config` invoked no callbacks. The
September 10 trial of 0.153.4 passed with inline `hooks.<event>` invocation
overrides and invocation-only trust for vetted fixture hooks. This does not
grant trust to installed user hooks. Keep configuration health unverified until
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
override the default remains `~/.claude/settings.json` and `~/.claude.json`.
Saved delegated steps pin the selected profile directory at preview. Apply,
resume and undo set that profile only in the provider child process; a plan for
the default profile explicitly removes an inherited override from that child.
Changing the invoking shell's profile after preview does not redirect the saved
plan. Setup does not switch the profiles of existing provider sessions.

The [packaged Claude profile trial](verification/2026-09-07-claude-profile-setup.json)
passed with Claude Code 2.1.263 and candidate `a65d956`: preview in profile A,
apply and undo while invoking from profile B, matching health diagnostics,
preservation of unrelated MCP entries/hooks, and refusal before edits for a
changed server environment or malformed provider JSON. Both disposable profiles
were removed; monitored user configurations stayed unchanged. Run
`scripts/claude_setup_smoke.py --binary PATH --manifest PATH --output NEW_DIRECTORY`
with an installed Claude CLI to repeat this configuration-only trial. It does
not invoke a model or prove automatic inbox consumption.
