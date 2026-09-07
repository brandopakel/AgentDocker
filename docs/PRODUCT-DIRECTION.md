# Product direction

agentdocker is a native desktop application for discovering and orchestrating AI agents on a user's computer. Opening the app should show the agents already working, their projects, supported actions, shared context and coordination state. Docker and Podman inspired the lifecycle and organization model; they are optional execution adapters, not required infrastructure.

The target platforms are macOS, Linux and Windows. The selected GUI is Rust with egui/eframe, communicating with the local daemon through operating-system IPC. The current transport is a Unix socket. A browser, localhost HTTP server, container engine or cloud account is not required for native use. Windows needs its own transport, process, terminal, service, path and packaging adapters; a portable GUI toolkit alone does not supply those.

The display name is **agentdocker**. macOS application bundles retain their underlying `.app` format with the extension hidden in normal Finder display. Packaging suffixes are not product names.

## Implemented foundation

Main contains a per-user daemon, CLI, native GUI, persistent registry/inboxes/events, project grouping, native process supervision, expiring physical-path leases, FIFO waiting/deadlock detection, activity derived from observed coordination, and messaging. Working-state features include content observations, stale-context checks, a change ledger, journal/digests, checkpoints, validation evidence, linked worktrees, integration previews and addressed/exported handoffs. Channels and contests coordinate reviews and competing attempts.

Discovery runs in the daemon every five seconds. Installed-tool inventory includes CLI paths/versions, selected macOS application bundles and known configuration wiring. Setup supports selected MCP hosts and Claude Code hooks with dry-run output and backups. The GUI includes agents, runtimes, journal, leases, events, human questions, a terminal and a CLI console. Notifications are best effort through installed OS tools. PTY attach/detach and opt-in command relaunch exist; seamless daemon restart does not. Multiplexer adapters recognize reported/observed sessions and can launch a tmux-owned agent.

These are implementation statements, not a claim that every path is hardened or shipped in the latest release. The [September 6 audit](AUDIT-2026-09-06.md) records exact source identities, fresh tests, confirmed restore defects, and privacy/readiness gaps. The [delivery record](NATIVE-DELIVERY.md) tracks subsequent fixes; the [trial plan](LOCAL-TRIAL.md) defines acceptance before normal use.

## Discovery and integration contract

- Keep installed tools, running processes, registered sessions and verified integration health distinct. Finding an executable or configuration entry does not prove control, message consumption or model context access.
- The inventory is a curated table (12 rows at the audit), and running-process recognition covers nine CLI families. It does not discover every company's agent or every session inside a desktop app. Linux desktop application inventory remains missing.
- Preserve separate identities for desktop applications and CLIs. Claude Desktop and Claude Code are separate; VS Code does not prove an agent extension is installed. The current Codex row also lists ChatGPT/Codex bundles; this must not be interpreted as shared integration health and needs clearer per-application capability reporting.
- Report model/provider details only when a launch specification or integration supplies them. An agent's ability to speak the protocol makes the design vendor-neutral; it does not create an adapter for an unsupported tool.
- Show what each adapter supports: inventory, discovery, messages, observation, hooks/MCP setup, launch/stop, terminal access and handoff. Generic adoption adds a registry record; it does not install hooks or cause an agent to read its inbox.
- Guided setup should preview exact changes, retain private backups, verify the connection and offer scoped undo. The current CLI has `--dry-run` and backups; the GUI Set up action applies changes directly, and there is no guided health check or automatic undo yet.
- Native coordination is cooperative. Claude hooks cover selected tools and fail open if coordination is unavailable; arbitrary shell writes and unrelated applications are not guarded by leases. Optional containers supply a stronger execution boundary when chosen explicitly.

## Platforms and distribution

| Platform/channel | Current availability | Remaining delivery |
|---|---|---|
| macOS arm64/Intel | v0.1.0 archives contain CLI, daemon and GUI executables; local universal app preview has been built and launched on Apple Silicon | Repository-owned bundle/DMG packaging, signing/notarization, upgrade/rollback and Intel runtime trials |
| Linux x86-64/ARM64 | Released CLI/daemon archives; GUI builds from source and is included in Ubuntu build/test CI | Desktop packages, desktop inventory, graphical runtime/notification/service trials across target distributions |
| Windows | Product scope only; current binaries depend on Unix APIs | Named pipes/access controls, process identity and termination, ConPTY, service/session lifecycle, paths, installer and Windows CI |
| GitHub and shell installer | [v0.1.0](https://github.com/brandopakel/AgentDocker/releases/tag/v0.1.0), four archives and checksums; `install.sh` selects a target | Publish a newer verified release after trial blockers are fixed |
| Homebrew | Generated formula is a v0.1.0 release asset | Maintained tap/formula and a GUI cask; do not advertise a default `brew install agentdocker` yet |
| Cargo | Source installation from a pinned Git tag/commit or checkout | Registry publication has not been established as a supported install route |

The published release is source `52fd88d`, before sessions, human questions/notifications, fair waiting/activity, contests and multiplexer work. A source build or local preview must name its exact revision; package version `0.1.0` alone cannot distinguish these builds.

## Delivery order

1. Fix the confirmed restore readiness/persistence defects and harden private state/log creation. Turn the fault probes into regression tests. Keep automatic restore off in the initial trial.
2. Run a pinned native trial on this Mac in a disposable repository/private state directory. Exercise the GUI and protocol before configuring one test Claude Code session and one test Codex session. Preview configuration changes and verify message/observation round trips.
3. Complete desktop onboarding, per-tool capability/health reporting, undo and packaging. Publish the tested Mac candidate and run the independent second-Mac trial. Keep Bencher credentials outside the repository and GitHub; upload verified benchmark artifacts using private configuration.
4. Deliver Linux desktop packaging/inventory and real GUI/service trials, then Windows host support with equivalent behavior and tests. Cross-platform desktop delivery takes priority over additional optional engine features or competitive benchmark features.
5. Add admission policy/quotas, restart/backoff/dependencies, retention controls and planned daemon descriptor handoff as real usage identifies requirements. A daemon-attributed worktree commit command and optional compressed log views remain backlog items.
6. Add authenticated federation only after the single-host product is dependable. Two installations currently have independent registries and leases; exported handoff files do not create a shared cluster.

Historical phase numbers in the architecture are implementation dependency labels, not GitHub PR numbers or an override of this delivery order. Additional terminal managers, a cloud control plane and a required web dashboard are not prerequisites for the native desktop product.
