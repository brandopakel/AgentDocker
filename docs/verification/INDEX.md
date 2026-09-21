# Verification records

One line per trial on real binaries, newest last. The full JSON records these lines came from were kept in the repository until September 21, 2026 and remain in its history (`git log -- docs/verification`); a new trial adds a line here, not a file.

| Date | Trial | Source | Result |
| --- | --- | --- | --- |
| 2026-09-07 | claude profile setup | `a65d956` | Portable coordination skill merged in #149 and is included in the recorded installed d14610b7 candidate built from source 3785e810; fresh-session acceptance remains open. |
| 2026-09-07 | desktop maintenance | `—` | owned native desktop fixtures; private paths and captures excluded |
| 2026-09-07 | identity lifecycle | `—` | Packaged adapter lifecycle with a synthetic host; separate from actual model-provider trials |
| 2026-09-07 | integration benchmark failure | `e008831` | Original failed integrated benchmark; not a completed performance acceptance campaign |
| 2026-09-07 | local | `—` | Selected exact-source local campaigns; this is not final release acceptance. |
| 2026-09-07 | macos capture failure | `d06a117` | failed |
| 2026-09-07 | native resources | `509f746` | sanitized exact-source native desktop integration and short resource observations; raw logs, process identities and captures remain private |
| 2026-09-07 | provider activity | `—` | Source-pinned native integration, actual provider callbacks and synthetic message trials; not complete delivery or release acceptance |
| 2026-09-07 | state timing diagnostic | `ef3fd7b` | Separate opt-in state timing diagnostic campaign, not a diagnosis of the original failed campaign |
| 2026-09-07 | terminal resources | `—` | Terminal admission/lifecycle verification and short native Mac observation at the stated sources; not release acceptance |
| 2026-09-08 | final candidate compile | `2b76e46` | failed |
| 2026-09-08 | hook input baseline | `f8ec0f6` | reproduced Claude stdin wait with no timeout within the 1.5 s observation window |
| 2026-09-08 | integrated provider trials | `—` | setup_trial: passed |
| 2026-09-09 | desktop simplification | `—` | passed |
| 2026-09-10 | bulk receipts | `—` | Source-bound bulk receipts and removed-checkout regression checkpoint; not provider idle-wake or public release acceptance. |
| 2026-09-10 | button interaction | `5772736` | Primary button text contrast during pointer interaction; follow-up to desktop delivery verification |
| 2026-09-10 | claude channel probe | `—` | Owned Claude provider capability probes; no production AgentDocker outbox implementation yet |
| 2026-09-10 | codex delivery | `—` | passed |
| 2026-09-10 | desktop delivery | `bf39280` | Iced usability, provider lifecycle delivery, durable coordination, package acceptance and bounded resource trials |
| 2026-09-10 | desktop design | `—` | Iced desktop design rounds of 2026-09-10: visual, interaction and idle-cost verification for commits 4f5b921, 17ca37a, 00c1c98, 4f0f579 on codex/desktop-delivery. |
| 2026-09-10 | desktop load | `—` | UI load benchmark and memory bisect for the Iced desktop on 2026-09-10, packaged baseline binaries and a release UI rebuild for the fix; see per-campaign contention limits. |
| 2026-09-10 | durable queue | `—` | standard_gate: passed |
| 2026-09-10 | message receipts | `—` | standard_gate: passed |
| 2026-09-10 | notification routing | `1be976c` | release_workflow: passed |
| 2026-09-11 | claude channel input | `—` | implemented_and_partial_acceptance |
| 2026-09-11 | codex appserver input | `—` | passed |
| 2026-09-11 | codex hook discovery | `—` | passed |
| 2026-09-11 | codex input bridge | `b1ce9d0` | Experimental managed native Codex input, queue ownership and bounded crash recovery |
| 2026-09-11 | codex input review | `—` | network_review_2026_09_15: Full release gate passed; actual managed-network callback acceptance remains open after a private configuration denied the connection before emitting a callback. |
| 2026-09-11 | codex queue recovery | `—` | passed |
| 2026-09-11 | daily updates | `—` | passed_for_listed_scope |
| 2026-09-11 | desktop identity | `—` | Iced desktop identity round of 2026-09-11 on codex/desktop-delivery: per-project monogram tiles, the window-local unviewed-done badge, the Connections hooks copy and the per-launch Claude channel checkbox. |
| 2026-09-11 | desktop release | `—` | implemented_and_local_acceptance_passed |
| 2026-09-11 | desktop update | `—` | Update consumer ('agentdocker desktop update') verification on 2026-09-11: unit tests, lint and the offline update smoke against a locally packaged release under a disposable prefix. |
| 2026-09-11 | followup integration | `a8b1dbf` | PRs #99–#102 merged; their original local acceptance records and failures remain source-specific. |
| 2026-09-11 | hour sustained use | `—` | passed_for_listed_scope |
| 2026-09-11 | identity repair | `—` | Explicit offline reconciliation of proven local external Claude/Codex duplicate records; durable exact aliases and preserved history. |
| 2026-09-11 | macos watcher recovery | `4d516d0` | passed |
| 2026-09-11 | managed claude input | `—` | passed_bounded_managed_launch_acceptance |
| 2026-09-11 | mcp answer receipts | `f42a8b0` | Codex MCP ask_human answer receipts and restart recovery |
| 2026-09-11 | provider configuration | `—` | passed_for_listed_scope |
| 2026-09-11 | provider question receipts | `—` | separate_mcp_question_gap: failed |
| 2026-09-11 | recovery fixtures | `3616bba` | passed_for_listed_scope |
| 2026-09-11 | retyped drafts | `a3f87b6` | passed_for_listed_scope |
| 2026-09-11 | session messages | `—` | passed_for_listed_scope |
| 2026-09-11 | structured questions | `0850c0e` | Structured Iced command and choice questions; rendered callbacks and owned actual Codex trials |
| 2026-09-11 | terminal selection | `—` | passed_for_listed_scope |
| 2026-09-12 | claude question queue | `5819975` | One Claude channel question-answer path with durable shared-queue receipts and reply metadata |
| 2026-09-12 | cli sender identity | `—` | CLI sender identity is integrated through #119; the original sender trials remain source-specific. |
| 2026-09-12 | compact question history | `—` | Compact retained Inbox questions, explicit complete-text details, and notification navigation without draft submission or message dismissal. |
| 2026-09-12 | event continuation | `463f5ae` | Checked event continuation and its provider-worker integration are merged; live replacement remains experimentally gated. |
| 2026-09-12 | file change review | `—` | Bounded Codex file-change approval through the shared human answer queue and compact native full-diff review. |
| 2026-09-12 | input delivery status | `33d52a3` | PR #108 merged as 1e2914270b59fcb6721d463999eed80229e15e5d after final 5778212 CI and actual source inspection. |
| 2026-09-12 | integrated desktop | `462c1b3` | Combined PR119: messenger Inbox, minimal home and Tools, permission validation, sender identity, launcher compatibility and persisted Applications destination |
| 2026-09-12 | launcher hook repair | `5610c91` | Historical hook repair and September 15 intact launcher: private install/rollback/routes, production activation and native Codex peer wake passed. |
| 2026-09-12 | output drain | `—` | Managed output ownership through pipe/terminal EOF and final log flush before publishing exit, releasing protection or restarting; not cross-process daemon handover. |
| 2026-09-12 | permission review | `—` | Concrete permission review merged through #115/#119; broader provider review surfaces remain open. |
| 2026-09-12 | provider event reconnect | `687e57f` | Local implementation and actual Codex event-only reconnect acceptance passed at 687e57f. |
| 2026-09-12 | queue read reconnect | `—` | Bounded retry of retained Codex inbox reads with empty acknowledgements; uncertain writes retain existing pause behavior. |
| 2026-09-12 | thirty minute codex queue | `3ceaa4a` | passed |
| 2026-09-12 | ux home | `—` | Home simplification is integrated through #119; the recorded CPU investigation remains historical evidence. |
| 2026-09-14 | overnight sustained use | `—` | passed_for_listed_scope |
| 2026-09-15 | native codex queue | `—` | Existing Codex input is merged and has installed idle/active receipt evidence; universal-provider and startup acceptance remain open. |
| 2026-09-15 | reload acceptance | `bffc599` | Passed at bffc599 on release binaries built from the committed source (state schema 18): 20 successive gated reloads of one private daemon, each predecessor retired within 14.0 s of the reload being asked for, while t... |
| 2026-09-15 | retention sustained use | `—` | passed: 20-minute retention trial rerun with every claimed assertion (source 33f8117 of the retention branch on main 51a1a9f, hashed private daemon copy): ten registered agents, journal retention 120s applied by the d... |
| 2026-09-15 | successor readiness | `fb92879` | Passed at fb92879 on release binaries built from the committed source (state schema 18): two successive gated reloads kept a batch and a PTY agent's processes, logs and exact exits 7/3 under the third daemon with the... |
| 2026-09-16 | reload controller episode | `—` | passed |
| 2026-09-19 | windows slice one | `—` | passed: the first native Windows daemon/CLI slice on a real windows-latest runner (Windows Server 2025, AMD64): agentd and agentdocker built from 7b3fd108 answered all 17 smoke steps over the named pipe, including thr... |
