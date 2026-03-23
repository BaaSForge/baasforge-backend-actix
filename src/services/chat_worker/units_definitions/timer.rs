pub const TITLE: &str = "Timer";

pub const BRIEF: &str = r#"Timer is the clock: on schedule or interval it emits events into the system so Flow, Projection, or other consumers can run time-based logic without blocking request paths."#;

pub const DETAIL: &str = r#"What it is for: Timer drives recurring housekeeping, reminders, expirations, aggregations, and any work that should happen because time passed, not because a user clicked something.

How others use it: The host invokes tick entry points; Timer handlers publish timed events. Flow units often implement the meaningful work—timeouts, cleanups, batch steps—while Timer stays a thin emitter.

How it uses others: Timer uses timer context and the stream to publish schema-backed or conventional event payloads. It does not query or mutate application databases directly.

Rules and restrictions: Like Action at the edge of purity, Timer must not reach into app data stores; emit an event and let Query, Flow, or Projection own data access. Keep ticks cheap and idempotent where possible because schedules can overlap or retry.

What it exports: Tick routines and per-job emitters that translate wall clock or cron semantics into bus messages.

Real-world examples: Hourly rollups for analytics; daily deletion of expired tokens; minute-level checks on unpaid reservations; weekly reconciliation jobs; sending digest emails on a fixed cadence."#;
