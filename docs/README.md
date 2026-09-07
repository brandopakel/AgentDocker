# Documentation

AgentDocker runs native agents directly on the user's computer. Docker and Podman are optional execution adapters. The desktop product targets macOS, Linux and Windows; the current host implementation supports macOS and Linux.

- [Getting started, adapters and working sets](../README.md)
- [Product direction and current delivery order](PRODUCT-DIRECTION.md)
- [Engineering audit, feature coverage and known blockers](AUDIT-2026-09-06.md)
- [Native delivery progress and regression coverage](NATIVE-DELIVERY.md)
- [Local native trial and acceptance plan](LOCAL-TRIAL.md)
- [Architecture and wire protocol](ARCHITECTURE.md)
- [Implementation and recovery contracts](IMPLEMENTATION-NOTES.md)
- [Optional Docker and Podman execution](CONTAINER-ENGINES.md)
- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md)
- [Real-engine verification](../tests/containers/README.md)

Historical phase numbers are dependency sequence numbers, not GitHub PR numbers. The product-direction page defines upcoming priorities; command-specific `--help` describes the installed binary.

Native delivery follow-ups: [desktop packaging](DESKTOP-DISTRIBUTION.md), [guided setup and health checks](GUIDED-SETUP.md), and the [implementation/acceptance tracker](NATIVE-DELIVERY.md).
