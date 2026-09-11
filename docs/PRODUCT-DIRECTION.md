# Product direction

agentdocker is a native desktop application for discovering and orchestrating AI agents on a user's computer. Opening the app should show the agents already working, their projects, supported actions, shared context and coordination state. Docker and Podman inspired the lifecycle and organization model; they are optional execution adapters, not required infrastructure.

The target platforms are macOS, Linux and Windows. The desktop uses Rust with Iced and a software renderer; the [Iced design and migration](ICED-DESIGN.md) describe its project model, workflows, and validation. The app communicates with the local daemon through operating-system IPC. The current transport is a Unix socket. A browser, localhost HTTP server, container engine or cloud account is not required for native use. Windows needs its own transport, process, terminal, service, path and packaging adapters; a portable GUI toolkit alone does not supply those.

The display name is **agentdocker**. macOS application bundles retain their underlying `.app` format; Finder preferences control extension display. Packaging suffixes are not product names. Packaging must not mutate the signed bundle to force an extension-display preference.

## Implemented foundation

Main contains a per-user daemon, CLI, native GUI, persistent registry/inboxes/events, project grouping, native process supervision, expiring physical-path leases, FIFO waiting/deadlock detection, activity derived from observed coordination, and messaging. Working-state features include content observations, stale-context checks, a change ledger, journal/digests, checkpoints, validation evidence, linked worktrees, integration previews and addressed/exported handoffs. Channels and contests coordinate reviews and competing attempts.

Discovery runs in the daemon every five seconds. Installed-tool inventory includes CLI paths/versions, selected macOS application bundles and known configuration wiring. Setup supports selected MCP hosts and Claude Code hooks with dry-run output and backups. The GUI includes agents, runtimes, journal, leases, events, human questions, a terminal and a CLI console. Notifications are best effort through installed OS tools. PTY attach/detach and opt-in command relaunch exist; seamless daemon restart does not. Multiplexer adapters recognize reported/observed sessions and can launch a tmux-owned agent.

These are implementation statements, not a claim that every path is hardened or shipped in the latest release. The [September 6 audit](AUDIT-2026-09-06.md) records exact source identities, fresh tests, confirmed restore defects, and privacy/readiness gaps. The [delivery record](NATIVE-DELIVERY.md) tracks subsequent fixes; the [trial plan](LOCAL-TRIAL.md) defines acceptance before normal use.

## Discovery and integration contract

- Keep installed tools, running processes, registered sessions and verified integration health distinct. Finding an executable or configuration entry does not prove control, message consumption or model context access.
- The inventory is a curated table of 14 runtime/application identities, and running-process recognition covers nine CLI families. macOS bundles and known Linux desktop-entry IDs are inventoried. This does not discover every company's agent, authenticate a publisher, or enumerate sessions inside desktop apps. Windows desktop installation inventory remains unfinished.
- Preserve separate identities for desktop applications and CLIs. Claude Desktop and Claude Code are separate; Codex CLI, Codex desktop and ChatGPT are separate too. No MCP adapter is claimed for the latter two desktop identities. VS Code installation does not prove an agent extension is installed.
- Report model/provider details only when a launch specification or integration supplies them. An agent's ability to speak the protocol makes the design vendor-neutral; it does not create an adapter for an unsupported tool.
- Show what each adapter supports: inventory, discovery, messages, observation, hooks/MCP setup, launch/stop, terminal access and handoff. Generic adoption adds a registry record; it does not install hooks or cause an agent to read its inbox.
- Guided setup must preview exact changes, retain private backups, check the connection and offer scoped undo. The review stack implements saved preview/apply/undo plans and native controls; #53 adds bounded provider-configuration/executable diagnostics. See [GUIDED-SETUP.md](GUIDED-SETUP.md). These checks do not prove provider consumption; older main/release candidates may have only direct setup and backups.
- Native coordination is cooperative. Claude hooks cover selected tools and fail open if coordination is unavailable; arbitrary shell writes and unrelated applications are not guarded by leases. Optional containers supply a stronger execution boundary when chosen explicitly.

## Platforms and distribution

| Platform/channel | Current availability | Remaining delivery |
|---|---|---|
| macOS arm64/Intel | Native bundle/DMG packaging and explicit per-user installation/activation/rollback implemented in the review stack; Apple Silicon and Rosetta graphical trials recorded | Public Developer ID signing/notarization, release publication, Intel hardware and longer upgrade trials |
| Linux | Native desktop archive/launcher and x86-64 Xvfb/Mesa graphical CI; per-user installer implemented with CI acceptance in the review stack | Distribution packages, ARM64 graphical trials, and target-distribution acceptance |
| Windows | Partial core/host foundations and native CI in draft #54; shared named-pipe IPC under development. The full product still depends on Unix APIs | Complete process/IPC acceptance, ConPTY, service/session lifecycle, physical path semantics, installer and full native daemon/GUI CI |
| GitHub and shell installer | [v0.1.0](https://github.com/brandopakel/AgentDocker/releases/tag/v0.1.0), four archives and checksums; `install.sh` selects a target | Publish a newer verified release after trial blockers are fixed |
| Homebrew | `packaging/homebrew/generate.py` writes a formula from the release's real per-target checksums, CI validates it with `ruby -c` and uploads it as a release asset. The [tap](https://github.com/brandopakel/homebrew-tap) contains the v0.1.0 formula; repository publishing configuration is present (verified September 9) | Verify the next release updates the formula and publish a cask with the notarized app; the tap currently has no app cask |
| Cargo | Source installation from a pinned Git tag/commit or checkout | Registry publication has not been established as a supported install route |

The published release is source `52fd88d`, before sessions, human questions/notifications, fair waiting/activity, contests and multiplexer work. A source build or local preview must name its exact revision; package version `0.1.0` alone cannot distinguish these builds.

## Delivery order

The [active delivery plan](DELIVERY-PLAN.md) adds a required review of recent commits, all open PRs and documentation, plus the complete testing-standard/local-trial crosswalk. Its [review ledger](REVIEW-2026-09-07.md) distinguishes implemented fixes, open work and evidence still needed.

1. Fix the confirmed restore readiness/persistence defects and harden private state/log creation. Turn the fault probes into regression tests. Keep automatic restore off in the initial trial.
2. Run a pinned native trial on this Mac in a disposable repository/private state directory. Exercise the GUI and protocol before configuring one test Claude Code session and one test Codex session. Preview configuration changes and verify message/observation round trips.
3. Complete desktop onboarding, per-tool capability/health reporting, undo and packaging. Public macOS desktop distribution needs Developer ID signing and notarization of the native `agentdocker` bundle. Complete the download/update feed, uninstall/retention and maintained distribution channels, including an optional Homebrew tap. Source builds and explicit local previews remain separate from a supported signed release. Publish the tested candidate and run the independent second-Mac trial. Keep Bencher credentials outside the repository and GitHub; upload verified benchmark artifacts using private configuration.
4. Deliver Linux desktop packaging/inventory and real GUI/service trials, then Windows host support with equivalent behavior and tests. Cross-platform desktop delivery takes priority over additional optional engine features or competitive benchmark features.
5. Audit and harden the merged admission/quotas, restart/backoff/dependencies, retention, daemon-attributed commit and bounded log implementations. The delivery audit reproduced policy-loading and run-admission defects; their fixes require final verification and review. Live daemon replacement remains unfinished: #59 deliberately makes `daemon reload` return unavailable after actual batch/PTY termination and log-loss failures. Implement safe process/I/O ownership transfer and successor readiness, then verify restore, installation and upgrade together. Merged code alone does not satisfy that acceptance.
6. Add authenticated federation only after the single-host product is dependable. Two installations currently have independent registries and leases; exported handoff files do not create a shared cluster.

Historical phase numbers in the architecture are implementation dependency labels, not GitHub PR numbers or an override of this delivery order. Additional terminal managers, a cloud control plane and a required web dashboard are not prerequisites for the native desktop product.


## Lightweight operation

Native operation must have a small installed footprint and bounded disk/memory
growth during sustained use. Container engines and development toolchains are
optional development/execution choices, not application dependencies. Measure
package size, idle/loaded RSS and CPU, persistent state and cleanup recovery on
each platform. The delivery plan now treats those budgets and the full local
storage audit as release gates. Source and sanitized verification reports live
in GitHub; local build outputs and disposable trial data are pruned after their
reports are published. Private user state and credentials remain local/private,
and native operation does not require a GitHub connection.
