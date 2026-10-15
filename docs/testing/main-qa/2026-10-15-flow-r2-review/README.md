# R2 review corrections

Applies to frozen public 8101e4e and private 84c020b; Jev work is separate.

- Flow gate now returns an operation-correlated `notExecuted` proof for rejection
  before the mutation dispatcher is entered. If an accepted receipt exists, that
  proof is persisted. Once dispatch begins, failures remain `outcomeUnknown` and
  only the original receipt may be queried. Missing receipts are never proof of
  non-execution.
- Private runtime persists pre-invoke cancellation/second authorization failures
  and typed native preflight refusals as terminal `not_sent`, releases its pending
  latch, and can reconcile the durable local proof after reopening. Actual unknown
  writes retain their original IDs and query-only recovery.
- `flow_get_task` now queries the node's original saved request through the existing
  recover API once. It does not generate or resume. Current-account/project checks
  stay in the existing backend; frontend checks request/node identity before
  updating state, actual credits, results and immutable asset/version metadata.
  Flow flushes the recovered output before returning. Running/unknown/prepared
  states return without silently resubmitting.
- Formatted R2 Rust files; `cargo +1.93 fmt --all -- --check` passed.

Focused evidence: 14 gate/recovery tests, 67 assertions; durable router test, 8
assertions; private cancellation/refusal/unknown-reopen tests, 7 passing. The
unknown-reopen runtime test confirms one write and one original receipt query.
The frontend regression also submits once, stops waiting, reopens the saved node,
and recovers a synthetic completed result without another generation request.
No production credentials, deployment or real model calls were used.

The original R2 actual UI configuration/connection/generation/reopen chain remains
incomplete as recorded in ../2026-10-15-flow-r2/README.md. These regressions are not
represented as that full UI acceptance or live-provider quality evidence.
