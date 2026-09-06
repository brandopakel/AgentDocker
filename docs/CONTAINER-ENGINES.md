# Docker and Podman delivery plan

Added September 5, 2026 at the user's request. This extends the current worktree and authenticated-container workstream. Correct stale-context detection and verified session handoff remain the acceptance criteria across engines.

| Step | Deliverable | Acceptance |
|---|---|---|
| 1 (implemented for builds) | Separate container-engine configuration from agent runtime; shared engine interface with Docker and Podman adapters | Explicit engine choice, capability errors, persisted identity; no silent fallback |
| 2 (implemented) | Common image build input with engine-specific options | Both engines build the same test Containerfile; immutable image identity, source/recipe identity, engine version and platform recorded |
| 3 (implemented) | Managed container lifecycle and crash recovery | Stop/kill acts on the recorded container; lost client or daemon restart never means a still-running container has exited |
| 4 (implemented) | Authenticated endpoint and checkout mapping | No token, wrong identity, expiry, revocation, traversal and host-admin requests fail; aliases of one physical file conflict |
| 5 (Linux and Podman VM implemented) | Rootless Linux and macOS VM transport | UID/mount checks and scoped bridge work without mounting the host control or engine socket |
| 6 (implemented) | Engine-aware validation and handoff | A reads, B changes, A is warned and rereads; handoff survives restart; changed source or image invalidates prior validation evidence |
| 7 | GitHub CI and CodeRabbit review | Shared contract tests plus real Docker and Podman Linux jobs; macOS VM checks documented separately; findings resolved before integration |

Current branch: scoped authentication for externally managed containers, physical path mapping, worktree integration, image build adapters and managed launch/stop/restart recovery are implemented. Opt-in checkout/endpoint mounts, macOS Podman scoped bridges, and image-bound validation are implemented. Docker Desktop VM transport is unsupported; Docker mount validation runs on Linux. Podman has been installed on the development host for real integration testing. Engine-specific results must be reported individually; Docker compatibility must not be inferred solely from a Podman pass.

Podman-specific capabilities include rootless user mappings and `podman machine` connectivity. Docker-specific Buildx features and Podman-only options remain capability-gated extensions rather than assumptions in the common interface. We consume engine commands and OCI-compatible images; embedding engine internals or maintaining a fork is outside this workstream.

References: [Podman build](https://docs.podman.io/en/latest/markdown/podman-build.1.html), [Podman run](https://docs.podman.io/en/latest/markdown/podman-run.1.html), [Docker run](https://docs.docker.com/reference/cli/docker/container/run/).


## Captured image builds

`agentdocker image-build --engine docker CONTEXT -f Containerfile` (or `--engine podman`) builds through the host daemon and prints only the build ID. Add `--json` for the complete `image_build` response. `--connection NAME` selects a Docker context or Podman connection; omitting it uses that explicit engine's configured default. An unavailable engine returns `engine_unavailable`; there is no fallback to another engine. `agentdocker images` lists retained records after restart. Build operations are forbidden on the restricted container socket.

Each build captures a private input tree before invoking the engine: at most 20,000 entries and 256 MiB, excluding `.git`, including Git-ignored files. Engine-specific ignore files remain in the captured tree and are interpreted by that engine. Choose a small context when ignored build outputs would exceed these conservative capture limits. Special files, escaping symlinks and recipes outside the context are rejected. Directory permissions are normalized to 0755 and modification times to the Unix epoch; file rwx permissions and relative symlink targets are preserved; special permission bits are stripped. The source tree remains untouched.

Evidence includes the canonical context path, recipe path and hash, complete captured-context hash, engine selection, client/server versions when reported, inspected OS/architecture/variant, timestamps and immutable image configuration ID. Docker requires an available Buildx builder (checked before building) and explicitly loads the build result into its image store; both engines write an image ID file which must match image inspection. Builds consume the captured tree even if the working checkout changes during execution. This records the inputs actually built; it does not claim the live checkout still matches them or that network-dependent builds are reproducible.

The record and `image_built` replay event commit together before success or publication. If persistence fails, the resulting engine image may remain unrecorded, and coordination fails closed; no image is silently removed. A build timeout terminates the local engine client process group and records no successful evidence. Remote engine build cancellation is engine-dependent; this is not yet managed agent lifecycle supervision.

Invalid context/recipe/size inputs return `invalid`. Host temporary-storage and filesystem faults return `storage_unavailable`; missing or inconsistent engine image evidence returns `build_failed`. Timeout and output-limit failures have distinct messages.

## Managed container commands

```sh
build=$(agentdocker image-build --engine podman tests/containers -f Containerfile)
agentdocker run --image-build "$build" --name worker -- python3 -u -c 'import time; print("ready"); time.sleep(300)'
agentdocker inspect worker
agentdocker logs worker
agentdocker stop worker              # engine TERM, then KILL after two seconds
agentdocker restart worker           # new agent/container identity from the same build
agentdocker stop worker --force      # engine KILL
```

The build ID selects the engine, connection option and immutable image. The command overrides the image entrypoint. By default it uses the image's working directory, has no network access or host mounts, and `-w/--workdir` only associates the host project. Add `--mount-checkout` for the authenticated workspace described below, and `--network bridge` to opt into engine bridge networking. Host networking is unavailable.

Before create, the daemon persists a unique engine name, random ownership label, build/image identity and run intent. It commits the confirmed container ID before start, and records a start attempt before issuing it. Recovery looks up a lost create response by that owned name; it never creates a duplicate container. A start whose outcome is uncertain is not blindly retried: if the engine still reports an unstarted container, stop it before retrying with a new agent. A missing container or unavailable engine is not positive evidence of exit and leaves the agent live with `container.last_error` for diagnosis. Existing leases remain until confirmed exit or their normal TTL expiration.

Start, stop, kill, cleanup and log reads verify the ownership labels, full container ID, immutable image and disabled engine restart/removal policy. Stop intent commits before signaling and survives daemon crashes. An owned, never-started container is removed without force when stopped, so a delayed start cannot resurrect it; finished containers otherwise remain available for inspection/logs. `rm` forgets an AgentDocker record and does not delete its engine container. `restart` waits for confirmed exit, then creates a new agent/container identity; it returns `conflict` if exit is still uncertain. It never transfers old leases or validation evidence.

Container state changes emit `container_updated`. Confirmed exit, lease deletion, journal entries and replay events commit atomically before publication. Host PID liveness never applies to container agents. Schema 6 prevents older daemons from interpreting a container with no host PID as exited. Graceful daemon shutdown requests stop and waits up to eight seconds; uncertain container protection remains durable. Engine commands are bounded to 15 seconds and eight concurrent workers; terminating a client does not establish remote exit.

`logs` reads a verified engine snapshot (at most 10,000 lines and 4 MiB); `--follow` is currently unsupported for managed containers. Engine-side logs and stopped containers remain under the selected engine's retention policy. Do not externally restart or change the restart policy of AgentDocker-owned containers after exit has been recorded.

`tests/containers/lifecycle.py` exercises real engines separately: a deliberately lost create response, crashes with a live container, engine outage, durable kill intent, lease retention and release, replacement identity, logs, stop escalation, and natural exit through the CLI. It owns and cleans up only its fixture daemons/containers. Linux CI runs it for Docker and Podman; macOS Podman uses the same fixture without a socket bridge.

## Authenticated managed workspaces

```sh
# Linux: local Docker or rootless Podman.
agentdocker run --image-build "$build" --mount-checkout -w "$PWD" --name worker -- python3 worker.py
# macOS: select a running rootless Podman machine matching the build connection.
agentdocker run --image-build "$build" --mount-checkout --podman-machine agentdocker-e2e --network bridge --name worker -- python3 worker.py
```

The canonical host checkout is mounted read/write at `/workspace`, which becomes the command's working directory. The daemon refuses a checkout containing its state or control socket. Only a private credential directory and a scoped endpoint directory are also mounted, both read-only. The process receives `AGENTDOCKER_SOCKET`, `AGENTDOCKER_TOKEN_FILE`, and its agent identity; the token is a 0600 file, never part of registry records, events, or engine arguments. Its grant lasts 24 hours and accepts only that live agent and mapped checkout. Revocation immediately denies subsequent requests without releasing a writer's leases. Start a replacement for fresh credentials after expiry. Clients should retry connection/authentication while the daemon is recovering or confirming startup.

Directories, rather than individual socket inodes, are mounted so a restarted daemon can replace the endpoint. Linux proxies connect only to the restricted socket. Podman VM transport verifies machine state, rootless SSH user, engine connection and shared-directory probes; it then forwards only the restricted Unix socket over SSH. The engine connection is pinned in the run record and reused for restart. A private SSH control socket allows recovery to replace a forward orphaned by a daemon crash. SSH keys, known-host records and control sockets stay outside the container mounts. Known hosts use a private, persistent `accept-new` file for the loopback VM. VM configuration changes require a new launch; no automatic machine creation or engine fallback occurs.

Local rootless Podman uses `keep-id`; rootful Docker uses the daemon user's UID/GID, and rootless Docker uses container UID 0, which maps to its host user. Remote Docker endpoints, Docker `userns-remap`, and Docker Desktop workspace transport are rejected. Shared workspace/socket containers disable SELinux labeling because host and SSH socket processes have different labels; they retain UID isolation, dropped capabilities and `no-new-privileges`. Inspection verifies the exact bind sources, destinations, read/write policy, user, workdir and network mode before lifecycle operations. Paths must fit Unix socket limits and cannot contain mount-option delimiters.

## Image-aware checks and recovery

`validate --as worker -- COMMAND` uses a fresh managed container from the worker's recorded immutable image and build, with the same environment, UID and network mode. It mounts the checkout **read-only**, has a private writable image layer, and receives no coordination credential. Direct build/test outputs to a writable location such as `/tmp` (for Cargo, set `CARGO_TARGET_DIR=/tmp/target` in the worker's environment). This checks a fresh image, not modifications made to the running worker's container layer. A container agent launched without checkout mounts must be relaunched with `--mount-checkout` before validation.

An incomplete validation commits before launch. Its runner has a durable kill deadline; reconciliation enforces that deadline even after the validating client or daemon disappears. Only positively inspected container exit and matching before/after checkout fingerprints can produce passing evidence. Lost engine responses or interrupted daemons leave non-passing evidence and a retained runner for reconciliation. Engine logs remain bounded; validation containers remain inspectable under the engine's retention policy.

Validation and checkpoint records bind immutable image and build input evidence, engine, network, UID and explicit environment; local build IDs and engine connection names remain audit metadata. Recovery reports `environment_matches`, refuses acknowledgement after an environment change, and shows passing validation only while both checkout and environment match. Integration requires validation from the target's environment as well. Native evidence remains distinct from container evidence. Schema 6 stops older daemons from ignoring these bindings or validation deadlines. The existing ignore-aware content scope and point-in-time limitations still apply.

`tests/containers/workspace.py` exercises real scoped requests from inside the managed image, physical alias conflicts, stale rereads, daemon-crash socket recovery, read-only image validation, timeout exit, source/image invalidation and credential revocation. Linux CI reports Docker and Podman independently; macOS uses the same fixture with `--machine`.

`run --isolate --image-build BUILD --mount-checkout` creates a persistent linked checkout beside daemon state (`<home>.worktrees`). Its shared Git metadata is mounted separately, with the linked directory layout preserved so `HEAD`, refs and the index remain usable. Validation makes both mounts read-only. Repository metadata is shared between worktrees; coordination leases still govern concurrent repository changes.

Handoff schema 2 preserves image environment evidence across export/import. Compatibility uses the immutable image, build input fingerprints, platform, engine, UID, network and explicit environment. Local build IDs and connection names can differ when portable input evidence matches; legacy evidence still requires exact equality. Changed source or environment blocks acknowledgement before any lease transfer. Cross-host imports carry notes/evidence but do not transfer host-local leases or import passing validation records.

Mounted runs currently reject image-declared volumes as well as any unexpected engine mount. Supporting image volumes requires retaining and checking their destinations and lifetime semantics; filtering out all volume mounts would hide unauthorized extras.
