# Docker and Podman delivery plan

Added September 5, 2026 at the user's request. This extends the current worktree and authenticated-container workstream. Correct stale-context detection and verified session handoff remain the acceptance criteria across engines.

| Step | Deliverable | Acceptance |
|---|---|---|
| 1 | Separate container-engine configuration from agent runtime; shared engine interface with Docker and Podman adapters | Explicit engine choice, capability errors, persisted identity; no silent fallback |
| 2 | Common image build input with engine-specific options | Both engines build the same test Containerfile; immutable image identity, source/recipe identity, engine version and platform recorded |
| 3 | Managed container lifecycle and crash recovery | Stop/kill acts on the recorded container; lost client or daemon restart never means a still-running container has exited |
| 4 | Authenticated endpoint and checkout mapping | No token, wrong identity, expiry, revocation, traversal and host-admin requests fail; aliases of one physical file conflict |
| 5 | Rootless Linux and macOS VM transport | UID/mount checks and scoped bridge work without mounting the host control or engine socket |
| 6 | Engine-aware validation and handoff | A reads, B changes, A is warned and rereads; handoff survives restart; changed source or image invalidates prior validation evidence |
| 7 | GitHub CI and CodeRabbit review | Shared contract tests plus real Docker and Podman Linux jobs; macOS VM checks documented separately; findings resolved before integration |

Current branch: scoped authentication, physical path mapping and worktree integration are implemented and being tested. Shared engine launch/build adapters and their provenance records are planned, not yet available CLI features. Podman has been installed on the development host for real integration testing. Engine-specific results must be reported individually; Docker compatibility must not be inferred solely from a Podman pass.

Podman-specific capabilities include rootless user mappings and `podman machine` connectivity. Docker-specific Buildx features and Podman-only options remain capability-gated extensions rather than assumptions in the common interface. We consume engine commands and OCI-compatible images; embedding engine internals or maintaining a fork is outside this workstream.

References: [Podman build](https://docs.podman.io/en/latest/markdown/podman-build.1.html), [Podman run](https://docs.podman.io/en/latest/markdown/podman-run.1.html), [Docker run](https://docs.docker.com/reference/cli/docker/container/run/).
