pub const TITLE: &str = "Timer";

pub const BRIEF: &str = r#"Timer is the clock: on schedule or interval it emits events into the system so Flow, Projection, or other consumers can run time-based logic without blocking request paths."#;

pub const DETAIL: &str = r#"What it is for: Timer drives recurring housekeeping, reminders, expirations, aggregations, and any work that should happen because time passed, not because a user clicked something.

How others use it: The host invokes tick entry points; Timer handlers publish timed events. Flow units often implement the meaningful work—timeouts, cleanups, batch steps—while Timer stays a thin emitter.

How it uses others: Timer uses timer context and the stream to publish schema-backed or conventional event payloads. It does not query or mutate application databases directly.

Rules and restrictions: Like Action at the edge of purity, Timer must not reach into app data stores; emit an event and let Query, Flow, or Projection own data access. Keep ticks cheap and idempotent where possible because schedules can overlap or retry.

What it exports: Tick routines and per-job emitters that translate wall clock or cron semantics into bus messages.

Real-world examples: Hourly rollups for analytics; daily deletion of expired tokens; minute-level checks on unpaid reservations; weekly reconciliation jobs; sending digest emails on a fixed cadence."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Timer is a one-way async publisher. It is invoked by the host scheduler (cron, interval loop) and publishes tick events onto the bus. It never subscribes to application subjects.

### Publishes (Outbound)

1. Timer Tick Events — ASYNC publish
   - Subject:    {project_uuid}.tmr.{context}.{module}.{unit_name_slug}.tick
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.maintenance.cleanup.expire-reservations.tick,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.maintenance.cleanup.purge-tokens.tick,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.analytics.rollup.hourly-summary.tick,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.maintenance.reconciliation.weekly-check.tick,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.notifications.digest.email-cadence.tick)
   - Payload:    TimerTick
     - meta.source_unit: "timer"
     - meta.correlation_id: generated per tick (UUIDv7), unique to this specific tick invocation
     - meta.causation_id: same as correlation_id (Timer is always the root cause for time-driven chains)
     - job_name: the specific timer job identifier (matches the subject segment)
     - schedule_expression: cron or interval spec for observability (e.g. "0 * * * *", "every 5m")
     - scheduled_at: unix millis of when the tick was supposed to fire (per the schedule)
     - fired_at: unix millis of when the tick actually fired (may differ due to scheduling jitter)
   - Mode:       ASYNC — JetStream publish to TIMERS stream (durable, at-least-once delivery, persisted in stream until consumed and acked)
   - Consumed by:
     - Flow (primary): handles timeouts, deadlines, scheduled saga steps, periodic cleanup workflows
     - Projection (occasional): time-based aggregation updates, view refreshes
     - Action (rare, via Flow): Timer tick → Flow decides → publishes ActionTrigger

2. Domain-Semantic Timer Events (optional, richer alternative) — ASYNC publish
   - Subject:    {project_uuid}.evt.timer.{event_name}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.timer.reservation_check_due,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.timer.daily_cleanup_due)
   - Payload:    EventEnvelope
     - meta.source_unit: "timer"
     - aggregate_id: "timer" or the job_name
     - payload: project-specific timer event payload (from Schema unit) with richer context:
       {
           job_name:            String,
           schedule_expression: String,
           scheduled_at:        i64,
           fired_at:            i64,
           parameters:          Option<Value>,  // job-specific config (e.g. "max_age_hours": 24)
       }
   - Mode:       ASYNC — JetStream publish to EVENTS stream (durable, at-least-once delivery, persisted in stream until consumed and acked)
   - Consumed by: Flow, Projection (same as domain events)
   - Note:       This alternative publishes into the EVENTS stream (alongside domain events) rather
                 than the dedicated TIMERS stream. Useful when timer events need to be treated
                 uniformly with domain events in Flow subscriptions.
   - Choose one style per job: either TimerTick on tmr.* OR EventEnvelope on evt.timer.*

### Subscribes (Inbound)

   NONE from the bus. Timer is invoked by the host process scheduler:
   - Cron-based: host runs a cron loop, calls Timer tick handlers on schedule
   - Interval-based: host spawns a tokio::interval task per job, invokes Timer on each tick
   - External scheduler: Kubernetes CronJob, systemd timer, or cloud scheduler hits an internal
     endpoint that triggers the Timer run

### Auto-Broadcast Events

   MANDATORY: Every tick invocation MUST publish exactly one TimerTick (or domain-semantic timer event).
   Timer handlers must not silently execute and return — the bus message IS the output.

   This is critical because:
   - Flow depends on tick events to detect timeouts and advance scheduled saga steps
   - Projection may depend on tick events for periodic aggregation
   - Observability systems track tick events to monitor scheduler health (missed ticks = alertable)

### Listens To

   Nothing on the bus. Timer is triggered by the host clock, not by NATS messages.

### Interactions With Each Unit

   Timer → Flow:         publishes TimerTick (timeout checks, scheduled saga steps)
   Timer → Projection:   publishes TimerTick (periodic aggregation, view refresh triggers)
   Timer → Action:       (indirect) Timer → Flow → ActionTrigger → Action
   Timer → Schema:       [LOCAL] compiled-in type definitions — timer event/tick payload types

### Communication Rules

- Timer MUST NOT access application databases. It is a pure emitter, same as Bootstrap.
- Timer MUST NOT publish commands. Time-driven intent goes through Flow → Command.
  Timer only publishes ticks/timer events; Flow decides what commands to issue.
- Timer MUST NOT call Query, Projection, Action, or Gateway.
- Each timer job MUST have a unique job_name that maps 1:1 to a NATS subject.
- Timer ticks MUST be idempotent in effect: overlapping or retried ticks should not cause
  duplicate work. Flow and Projection handlers must tolerate duplicate ticks gracefully.
- Timer MUST NOT encode business logic. "Check for expired reservations" is not Timer's job;
  Timer only says "it's time to check." Flow decides what qualifies as expired.
- Keep tick handlers cheap and fast — just publish and return. Heavy processing belongs in
  the units that consume the tick events."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Timer unit instance MUST export a `metadata` object so that Flow and Projection can discover
which timer jobs exist, their schedules, and which NATS subjects they publish ticks to.

### Metadata Shape

{
  "unit_type": "timer",
  "jobs": [
    {
      "id":                   "<unique job slug, e.g. expire_unpaid_reservations>",
      "job_name":             "<unit_name_slug — identifies this timer job, e.g. expire-reservations>",
      "tick_subject":         "<NATS subject, e.g. tmr.maintenance.cleanup.expire-reservations.tick>",
      "description":          "<what this timer job is for, in business terms>",
      "schedule_type":        "<cron | interval>",
      "schedule_expression":  "<cron expression or interval spec, e.g. '*/5 * * * *' or 'every 5m'>",

      "tick_payload_type":    "<Schema type name if custom payload beyond TimerTick, or 'TimerTick' for standard>",

      "consumed_by": [
        {
          "unit_type":     "<flow | projection>",
          "consumer_name": "<global shared consumer name: flow-timer-ticks | projection-events>",
          "unit_hint":     "<saga name or projector name that handles this tick>",
          "purpose":       "<what the consumer does with the tick>"
        }
      ]
    }
  ]
}

### Example

{
  "unit_type": "timer",
  "jobs": [
    {
      "id": "expire_unpaid_reservations",
      "job_name": "expire-reservations",
      "tick_subject": "tmr.maintenance.cleanup.expire-reservations.tick",
      "description": "Every 5 minutes, signal Flow to check for orders awaiting payment past deadline",
      "schedule_type": "cron",
      "schedule_expression": "*/5 * * * *",
      "tick_payload_type": "TimerTick",
      "consumed_by": [
        {
          "unit_type": "flow",
          "consumer_name": "flow-timer-ticks",
          "unit_hint": "order_fulfillment saga",
          "purpose": "Check payment_pending sagas that exceeded 30-minute deadline"
        }
      ]
    },
    {
      "id": "daily_token_cleanup",
      "job_name": "purge-tokens",
      "tick_subject": "tmr.maintenance.cleanup.purge-tokens.tick",
      "description": "Daily cleanup of expired authentication tokens",
      "schedule_type": "cron",
      "schedule_expression": "0 3 * * *",
      "tick_payload_type": "TimerTick",
      "consumed_by": [
        {
          "unit_type": "flow",
          "consumer_name": "flow-timer-ticks",
          "unit_hint": "token_lifecycle saga",
          "purpose": "Publish cmd.token.purge_expired for batch deletion"
        }
      ]
    },
    {
      "id": "hourly_analytics_rollup",
      "job_name": "hourly-summary",
      "tick_subject": "tmr.analytics.rollup.hourly-summary.tick",
      "description": "Hourly signal for analytics projection to compute period summaries",
      "schedule_type": "cron",
      "schedule_expression": "0 * * * *",
      "tick_payload_type": "TimerTick",
      "consumed_by": [
        {
          "unit_type": "projection",
          "consumer_name": "projection-events",
          "unit_hint": "analytics_summary projector",
          "purpose": "Compute hourly aggregates from raw event data"
        }
      ]
    }
  ]
}

### How Other Units Use This Metadata

- Flow unit:       reads "tick_subject" to know which timer subjects to subscribe to in saga handlers.
                   Reads "schedule_expression" to understand timing guarantees for timeout logic.
- Projection unit: reads "tick_subject" to subscribe for periodic aggregation triggers.
- Schema unit:     reads "tick_payload_type" — if custom (not "TimerTick"), ensures the type is defined.
- Command unit:    does NOT read Timer metadata (Timer → Flow → Command is the chain).
- Action unit:     does NOT read Timer metadata (Timer → Flow → Action is the chain).

### Rules

- Each job MUST have a unique "id" and unique "job_name".
- "tick_subject" MUST follow: tmr.{context}.{module}.{unit_name_slug}.tick
- "job_name" MUST match the unit_name_slug (last segment before .tick) in "tick_subject".
- "schedule_expression" MUST be a valid cron expression or interval spec.
- "consumed_by" SHOULD list expected consumers so the LLM knows which Flow/Projection units
  need to handle this tick. An empty list is valid but means the tick has no known consumer yet.
- Timer jobs MUST NOT encode business logic in the tick payload. Timer says "it is time";
  Flow/Projection decide what to do."#;
