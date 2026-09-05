# Shared Docker and Podman protocol tests

Build the fixture with either engine:

```sh
podman build -t agentdocker-protocol-test:local -f tests/containers/Containerfile tests/containers
# The same command works with docker in place of podman.
```

Run an isolated native daemon, then `python3 tests/containers/smoke.py --engine podman --host-socket <host.sock> --engine-socket <container.sock-as-seen-by-engine> --root <shared-test-directory> --image agentdocker-protocol-test:local`. The test registers its own agents and grants, and revokes/deregisters them on completion. It retains only nonsecret fixtures. It checks authentication, impersonation, traversal, host-admin denial, read → change → stale → reread, physical alias conflict and revocation without early lease loss. Output records the selected engine and built image ID.

On native Linux the engine socket path is the daemon home's `container.sock`. The fixture disables SELinux labeling for its test-owned mounts; production adapters must choose an explicit labeling policy. No host control or Docker/Podman engine socket is mounted.

On macOS, bind mounting a host Unix socket through the VM filesystem is not assumed to work. The development check used a dedicated Podman VM and an SSH reverse Unix-socket forward of **only** `container.sock` to `/tmp/ad-container-e2e.sock` in that VM. The checkout/token paths were under Podman's shared `/private` directory. Use `podman machine inspect` for that machine's SSH port/key and `ssh -N -R <vm-socket>:<host-container.sock> ...` with a verified host key and `ExitOnForwardFailure=yes`. Keep the bridge alive for the test and close it afterward. Never forward `host.sock`.

September 5, 2026 local result: Podman 6.1.1, rootless Linux arm64 in the macOS VM, all scenarios passed. Fixture image ID: `54a2fa1cd8bdd568099ef6ecc826826a4a15690e95aabca5801045dd7534da4c`. This is protocol interoperability evidence; it does not claim managed container lifecycle or immutable container validation support. Docker has a separate Linux CI matrix entry and is reported independently.
