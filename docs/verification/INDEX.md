# Verification history

Evidence from a trial on real binaries (an installed release, a real provider
account, a physical machine) is kept in the pull request that ran the trial and
in the CI runs it links, not in the repository.

Until October 2026 this page was a log with one line per trial, and before
September 21, 2026 the full JSON records were kept beside it. Both are in git
history: `git log -p -- docs/verification` shows them. Where another document
says a result is recorded in the verification index, that history is where it
is.
