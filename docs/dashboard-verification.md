# Dashboard follow-up verification

The current dashboard and refresh changes are separate from the earlier
observation-mode lab verification.

## Completed locally

- Server CI passes on macOS with Rust 1.98: formatting, build, Clippy with
  warnings denied, and all 258 server tests.
- TypeScript and production UI build pass.
- Five polling tests pass (`npm --prefix members/nullnet-server/ui test`).
  They cover responses slower than the polling interval, cancellation during a
  request, cancellation between requests, non-polling pages, and awaited manual
  refresh. The tests also run in the server's CI job.
- The review reproduced four successful responses producing zero state updates
  with the previous overlapping polling implementation. Polling now waits for
  completion before scheduling the next request.
- Targeted lint passes for the polling hook, scheduler and tests. The wider UI
  still has the previously reported lint failures.

## Backend changes awaiting Linux verification

Retained policy totals now use a seeded summary table maintained by insert,
policy-update and delete triggers in the same SQLite transaction as each session
change. Dashboard refreshes read at most four summary rows for a stack;
retention deletes decrement the counters. Backend sessions are excluded.
The active-session and busiest-service queries use a partial index over live
rows and do not scan ended history.

Rust tests cover direction totals, top-three ordering, policy totals, migration
backfill, policy changes, unchanged updates, rollback and retention deletion.
These tests pass locally. A synthetic SQLite check with one million retained
sessions also verified backfill, rollback, policy changes and retention deletes.
The summary query returned four rows with a median duration of 0.0145 ms across
100 runs and used the summary table's stack index.

Migration rollout and multi-host end-to-end verification remain pending: SSH to
both lab hosts timed out. The local checks and earlier observation-mode evidence
do not establish those results for this follow-up.
