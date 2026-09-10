# Bounded real-provider acceptance

## Packaged identity lifecycle follow-up

The [September 7 lifecycle report](verification/2026-09-07-identity-lifecycle.json)
separately exercises actual packaged MCP and Claude hook adapters against an
owned synthetic Python host. It does not invoke a model or change provider
configuration. At `8611292`, closing MCP marked the identity exited while that
host remained alive. At the verified `242cd3b` package, both MCP shutdowns and
a third connection preserved one live identity, its lease and a queued nonce.
The actual prompt hook output the nonce and acknowledged it; SessionEnd released
the lease and ended the joined identity. All owned fixture processes exited.

`scripts/identity_smoke.py` runs the same lifecycle on packaged Mac/Linux desktop
CI, verifies executable hashes against the package manifest and records source,
driver hash and cleanup. Registration additionally requires a supplied workdir
to resolve to an existing directory; invalid directories must leave no registry,
database or event state. The alias fixture stays entirely within its owned
temporary directory. These later fixes need their own final CI; the historical
package result alone does not certify them or migrate existing duplicates.

## September 10 Codex lifecycle delivery

The new adapter passed a bounded actual Codex CLI 0.153.4 trial at
`UserPromptSubmit`, `PostToolUse` and `Stop`. Each boundary received a fresh queued
nonce, the model replied with its exact `reply_to` ID and payload, the sender was
the expected provider identity, and the inbox ID was acknowledged. Hooks and MCP
retained one identity. Only the `send_message` MCP tool was exposed; no inbox
tool was available. A repeated Stop completed without another continuation.

The tracked [driver](../scripts/codex_delivery_smoke.py) uses a private daemon,
inline invocation hooks and invocation-only approval of that fixture's reply
tool. It checks the provider configuration, hooks and authentication file hashes
before/after without copying or publishing their contents. The passing run took
25.7 seconds and left those files unchanged. Owned children were stopped.

Retained failures matter: the first execution was sandbox-blocked at socket bind;
the first provider trial received context but used an incorrect reply approval
setting. The second corrected approval but exposed Codex's filtered MCP child
environment: MCP registered against the default daemon and could not find the
private peer. Both owned, finished default-daemon test records were explicitly
removed. The corrected driver pins all three AgentDocker endpoint variables in
the MCP child configuration, and the third provider trial passed. User sessions
were neither stopped nor upgraded. Raw provider/hook evidence stays private.

The [Codex hook contract](https://learn.chatgpt.com/docs/hooks) defines the context
and continuation behavior. These results establish the tested version and source
binaries, not every provider version, an idle wake mechanism, or an overnight soak.

## Earlier real-provider trials

Fresh provider sessions were tested on the development Apple Silicon Mac with a private daemon home/socket, disposable repository, new identities and a fixture peer. Existing sessions were neither adopted nor stopped. The harness checked that the user's Codex TOML and Claude Code settings hashes were unchanged. Provider authentication remained with each vendor CLI; no authentication files were copied into the fixture or repository.

| Adapter | Runtime tested | Actual round trip | Result |
| --- | --- | --- | --- |
| Claude Code hooks | 2.1.263 | Six configured hook events; a hidden nonce injected after registration was consumed by the model and written to its fixture output file | Passed: observed reads, lease claim/release, inbox acknowledgement and exit evidence; three turns |
| Codex stdio MCP | 0.153.4 | Fresh MCP registration, identity, inbox consumption, message echo to the fixture peer and journal note containing the injected nonce | Passed after enabling the five explicitly allowed fixture tools for that invocation |

The first Codex invocation failed acceptance despite provider exit code zero: tool approval policy prevented inbox/identity calls, and no nonce was consumed or echoed. That failure is retained. The successful invocation ignored user configuration and rules, used an ephemeral session and a read-only shell sandbox, and explicitly allowed only `whoami`, `read_inbox`, `wait_for_messages`, `send_message` and `journal_note` through the fixture MCP server. These were invocation-specific settings, not persistent changes to the user's Codex configuration.

The Claude invocation loaded only fixture hook settings and an empty MCP configuration, allowed only Read/Write tools, disabled session persistence and used a bounded $2 budget. Actual reported usage was $0.045488. Passing acceptance required output-file nonce equality plus coordination evidence; an empty successful provider exit was insufficient. Both drivers cleaned up their owned daemon/processes before removing fixture state.

The binaries came from source `a61a14c4cce498b99707f37ab10d5289b40d364b`, packaged as a local universal Mac preview. These trials prove bounded behavior of those adapter versions and source inputs. They do not validate every provider release, desktop-host integration, conversation resumption, all MCP tools, or the later guided-setup GUI. Discovery/adoption alone still does not prove model context access or message consumption.

Detailed driver arguments, transcripts, event captures, configuration hashes, original failures and result manifests remain in the private session handoff. Do not publish those raw captures: local discovery can include other sessions. Repeat with a fresh disposable project and one provider at a time after material adapter changes, then run the longer stages in [LOCAL-TRIAL.md](LOCAL-TRIAL.md).
