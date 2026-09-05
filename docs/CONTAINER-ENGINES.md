# Docker and Podman delivery plan

Added September 5, 2026 at the user's request. This extends the current worktree and authenticated-container workstream. Correct stale-context detection and verified session handoff remain the acceptance criteria across engines.

| Step | Deliverable | Acceptance |
|---|---|---|
| 1 (implemented for builds) | Separate container-engine configuration from agent runtime; shared engine interface with Docker and Podman adapters | Explicit engine choice, capability errors, persisted identity; no silent fallback |
| 2 (implemented) | Common image build input with engine-specific options | Both engines build the same test Containerfile; immutable image identity, source/recipe identity, engine version and platform recorded |
| 3 | Managed container lifecycle and crash recovery | Stop/kill acts on the recorded container; lost client or daemon restart never means a still-running container has exited |
| 4 | Authenticated endpoint and checkout mapping | No token, wrong identity, expiry, revocation, traversal and host-admin requests fail; aliases of one physical file conflict |
| 5 | Rootless Linux and macOS VM transport | UID/mount checks and scoped bridge work without mounting the host control or engine socket |
| 6 | Engine-aware validation and handoff | A reads, B changes, A is warned and rereads; handoff survives restart; changed source or image invalidates prior validation evidence |
| 7 | GitHub CI and CodeRabbit review | Shared contract tests plus real Docker and Podman Linux jobs; macOS VM checks documented separately; findings resolved before integration |

Current branch: scoped authentication, physical path mapping, worktree integration and shared Docker/Podman image build adapters are implemented. Managed launch/stop/restart supervision, macOS scoped runtime bridges, and image-bound validation remain planned. Podman has been installed on the development host for real integration testing. Engine-specific results must be reported individually; Docker compatibility must not be inferred solely from a Podman pass.

Podman-specific capabilities include rootless user mappings and `podman machine` connectivity. Docker-specific Buildx features and Podman-only options remain capability-gated extensions rather than assumptions in the common interface. We consume engine commands and OCI-compatible images; embedding engine internals or maintaining a fork is outside this workstream.

References: [Podman build](https://docs.podman.io/en/latest/markdown/podman-build.1.html), [Podman run](https://docs.podman.io/en/latest/markdown/podman-run.1.html), [Docker run](https://docs.docker.com/reference/cli/docker/container/run/).


## Captured image builds

`agentdocker image-build --engine docker CONTEXT -f Containerfile` (or `--engine podman`) builds through the host daemon and prints only the build ID. Add `--json` for the complete `image_build` response. `--connection NAME` selects a Docker context or Podman connection; omitting it uses that explicit engine's configured default. An unavailable engine returns `engine_unavailable`; there is no fallback to another engine. `agentdocker images` lists retained records after restart. Build operations are forbidden on the restricted container socket.

Each build captures a private input tree before invoking the engine: at most 20,000 entries and 256 MiB, excluding `.git`, including Git-ignored files. Engine-specific ignore files remain in the captured tree and are interpreted by that engine. Choose a small context when ignored build outputs would exceed these conservative capture limits. Special files, escaping symlinks and recipes outside the context are rejected. Directory permissions are normalized to 0755 and modification times to the Unix epoch; file rwx permissions and relative symlink targets are preserved; special permission bits are stripped. The source tree remains untouched.

Evidence includes the canonical context path, recipe path and hash, complete captured-context hash, engine selection, client/server versions when reported, inspected OS/architecture/variant, timestamps and immutable image configuration ID. Docker requires an available Buildx builder (checked before building) and explicitly loads the build result into its image store; both engines write an image ID file which must match image inspection. Builds consume the captured tree even if the working checkout changes during execution. This records the inputs actually built; it does not claim the live checkout still matches them or that network-dependent builds are reproducible.

The record and `image_built` replay event commit together before success or publication. If persistence fails, the resulting engine image may remain unrecorded, and coordination fails closed; no image is silently removed. A build timeout terminates the local engine client process group and records no successful evidence. Remote engine build cancellation is engine-dependent; this is not yet managed agent lifecycle supervision.

Invalid context/recipe/size inputs return `invalid`. Host temporary-storage and filesystem faults return `storage_unavailable`; missing or inconsistent engine image evidence returns `build_failed`. Timeout and output-limit failures have distinct messages.
