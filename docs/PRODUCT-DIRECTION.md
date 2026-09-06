# Product direction

AgentDocker is a native application for discovering and orchestrating AI agents on a user's computer. The user opens an installed desktop app and sees active agents, their projects, current work, shared context and coordination state. Docker and Podman supply familiar ideas for lifecycle, inspection and organization; they are not required infrastructure for local agents.

The target platforms are macOS, Linux and Windows. The current implementation supports macOS and Linux. Windows belongs in the product scope, with its own process inspection, local transport, service lifecycle, packaging and test coverage. An application window is the primary GUI experience; users should not need to start a web server, open a localhost URL or use a terminal. The GUI communicates with the local daemon through operating-system IPC. The UI toolkit is still to be selected against these requirements.

## Discovery and integration

- Inventory installed agent CLIs and desktop applications across vendors, including installation location, available version evidence and configuration status.
- Scan for active sessions in the daemon and publish additions, changes and exits as events so the GUI stays current without CLI commands.
- Distinguish an installed application, a running application process and an actual agent session. Show model/provider details only when an integration supplies that evidence.
- Expose each adapter's supported capabilities: discovery, activity reporting, hooks/MCP configuration, messages, launch/stop and handoff. Detecting a process alone does not provide control over it or its context.
- Provide guided setup that previews configuration changes, preserves existing settings and credentials, and can undo changes made by AgentDocker.
- Support native agents directly. Keep existing container execution available as an optional adapter for users who choose it.

## Delivery order

1. Finish the existing handoff/socket-path and optional Docker Desktop integration, resolve review findings, and verify the combined branch before merging.
2. Restructure documentation around the desktop product and native agent workflow; preserve precise implementation and protocol references.
3. Add installed-tool inventory, capability-aware setup and daemon discovery events. Extend the existing runtime table through adapters rather than claiming unsupported tools are integrated.
4. Build the installed desktop GUI with automatic discovery on launch, live agent/project views, journal and handoff visibility, and supported orchestration actions. Add platform tray/notification integration where available.
5. Package a native single-machine trial for the second MacBook and publish a verified release. Test configured Claude Code hooks and Codex MCP integrations as explicit adapters.
6. Deliver Windows host support and packaging with the same local behavior, then expand the tested vendor/application matrix. The GUI architecture must account for Windows from the beginning.

Two computers running AgentDocker remain independent until an explicit multi-host feature is implemented and configured. A first release should make the native single-machine experience useful and accurately report which capabilities each integration supports.

## Current boundary

There is no desktop GUI or automatic installed-app inventory yet. Current process discovery is a heuristic for known runtime command lines; hooks and MCP supply richer coordination data for configured agents. Completing the container work closes an existing workstream and does not change the native desktop priority.
