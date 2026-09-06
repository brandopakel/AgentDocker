# AgentDocker documentation

AgentDocker is a local orchestration system moving toward an installed desktop app for macOS, Linux and Windows. The daemon and CLI currently work on macOS and Linux. Native agents are the default; engine adapters are optional.

| Read this | For |
|---|---|
| [Product direction](PRODUCT-DIRECTION.md) | Desktop experience, platform targets, adapter capabilities and current delivery order |
| [Getting started](GETTING-STARTED.md) | Source installation, native agents, on-demand discovery, hooks/MCP and teams |
| [Coordination and recovery](COORDINATION.md) | Resource leases, stale reads, journal, verified checkpoints, handoffs and worktrees |
| [Architecture and protocol](ARCHITECTURE.md) | Current wire operations, persistence, process supervision and delivery guarantees |
| [Optional container engines](CONTAINER-ENGINES.md) | Image builds, Docker/Podman lifecycle, scoped mounts, VM transport and Docker Desktop relay |
| [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) | Required verification, coverage, performance provenance and reporting |
| [Real-engine fixtures](../tests/containers/README.md) | Separate Docker, Podman VM and Desktop test procedures |

The [September 4 audit](AUDIT-2026-09-04.md) and [historical phased design](ARCHITECTURE.md#roadmap) describe earlier snapshots and proposals. They are references, not the current delivery order or release status. `agentdocker --help` and command-specific `--help` describe the installed binary.
