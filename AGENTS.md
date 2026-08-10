# nullnet — agent instructions

Instructions for any AI coding agent working in this repo (Claude Code, Codex,
Cursor, Gemini CLI, …). Read this before starting work, and follow the gates in
"Submitting a PR" before proposing that a change is ready.

## Ground rules

- **Keep in-code comments short** — 5 lines max. Deeper rationale belongs in the
  PR description, not in the source.
- **No speculative guards.** Do not add defensive code for failure modes that
  cannot realistically occur. Prefer the simpler path.
- **Verify before implementing.** On design tasks, check feasibility against the
  real API source (not memory, not docs summaries) and confirm the approach
  before writing code.
- **No code duplicates.** Re-use existing code and logic where applicable.

## Submitting a PR

Four gates, in order. **A clean build and passing unit tests are not sufficient
evidence to open a PR** — they are the floor, not the bar.

### Gate 1 — prove the problem is real

Do not write a fix before the failure has been reproduced.

- State the reproduction, the observed behavior, and the expected behavior.
- If the issue came from a report, confirm the reported cause is the actual
  cause. Several past investigations found the stated theory was wrong.
- If you cannot reproduce it, stop and say so rather than fixing speculatively.

### Gate 2 — static soundness and hidden-regression hunt

Before touching the lab with Linux boxes for end-to-end verification:

- Re-read the changed code as a reviewer would. Does it hold under concurrency,
  restart, and partial failure — not just the happy path?
- Hunt for regressions the change could cause elsewhere as side effect: ordering assumptions,
  cleanup/teardown paths, anything that shares state with what you touched.
- Run the full CI command set.

### Gate 3 — full end-to-end on a network with multiple Linux-based hosts

Every change ships only after it has run end-to-end on real hosts.

- The gRPC control channel is TLS. Each host needs `JWT_SIGNING_KEY`,
  `MFA_ENCRYPTION_KEY`, `CONTROL_SERVICE_TLS_SAN`, and `ca-cert.pem` present in
  `/root/nullnet/`, plus a rebuilt proxy. See README.md for the full setup.
- Containers need a restart after nullnet restarts.
- To restart a Swarm stack, use `docker service update --force`.
- The default-deny eBPF firewall blocks Swarm ports. `2377`, `7946`, and `4789`
  must be in the `.env` allowlists on strict nodes, or the worker goes Down.

Report the actual observed result under load with a complex service topology — the before/after numbers, the log lines, the
`/api/graph/{stack}` state. "It works" is not a verification.

### Gate 4 — diagnostics and documentation

A change that is invisible when it misbehaves is not finished.

- **Emit an event for the cases an operator would need to see.** New failure
  modes, and state transitions that are otherwise silent, belong in the `Event`
  enum in `members/nullnet-server/src/events.rs`, with a `Severity` that matches
  how much attention the case actually deserves. Reuse an existing variant when
  one fits.
- Use judgment, not coverage. Events serve someone debugging a live network from
  the Events tab — not routine per-request or per-packet activity, and not
  general telemetry. Anything that is not Events-tab material gets a dedicated
  RPC instead.
- The proto-to-server half of a new event fails to compile if you miss a step;
  the UI half does not. Finish it in `nullnet-server/ui/src/`: the member in
  `types.ts`, and both a filter entry and a render arm in `pages/Events.tsx`.
  Otherwise, the event is emitted and silently never shown.
- Update `README.md` when the change alters set up and configuration,
  but only include the essential info without being verbose.

## Definition of done

A change is done when all four gates have passed and the end-to-end evidence is
written down. Anything short of that is reported as in-progress, with the
specific gate it is blocked on.
