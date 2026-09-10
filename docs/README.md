# Documentation

AgentDocker runs native agents directly on the user's computer. Docker and Podman are optional execution adapters. The desktop product targets macOS, Linux and Windows; the current host implementation supports macOS and Linux.

- [Current remaining engineering and manual release steps](REMAINING-WORK.md)
- [Getting started, adapters and working sets](../README.md)
- [Build and install the current desktop locally](LOCAL-BUILD.md)
- [Using AgentDocker: the app, the console, every command, tutorials](GUIDE.md)
- [The desktop app, screen by screen: what every control does and what it will not claim](DESKTOP-UX.md)
- [Setting up distribution: the Homebrew tap, and what a Developer ID is actually for](DISTRIBUTION-SETUP.md)
- [Product direction and current delivery order](PRODUCT-DIRECTION.md)
- [Active delivery plan, commit/PR review and complete testing crosswalk](DELIVERY-PLAN.md)
- [September 7 review scope, evidence and gap ledger](REVIEW-2026-09-07.md)
- [Engineering audit, feature coverage and known blockers](AUDIT-2026-09-06.md)
- [Native delivery progress and regression coverage](NATIVE-DELIVERY.md)
- [Local native trial and acceptance plan](LOCAL-TRIAL.md)
- [Architecture and wire protocol](ARCHITECTURE.md)
- [Implementation and recovery contracts](IMPLEMENTATION-NOTES.md)
- [Optional Docker and Podman execution](CONTAINER-ENGINES.md)
- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md)
- [Real-engine verification](../tests/containers/README.md)

Historical phase numbers are dependency sequence numbers, not GitHub PR numbers. The product-direction page defines upcoming priorities; command-specific `--help` describes the installed binary.

Native delivery follow-ups: [desktop packaging and installation](DESKTOP-DISTRIBUTION.md), [guided setup and health checks](GUIDED-SETUP.md), [bounded real-provider acceptance](INTEGRATION-ACCEPTANCE.md), and the [implementation/acceptance tracker](NATIVE-DELIVERY.md). The [macOS runner workaround](TEST-RUNNER-MACOS.md) preserves strict leak detection while avoiding captured cross-test descriptor inheritance.
