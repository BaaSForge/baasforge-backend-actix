pub const TITLE: &str = "Projection";

pub const BRIEF: &str = r#"Projection turns the event stream into query-friendly storage: tables, indexes, and materialized views. Each handler should be idempotent so retries or duplicate deliveries do not corrupt read models."#;

pub const DETAIL: &str = r#"What it is for: Projection maintains derived state that makes reads fast and simple—flattening events into rows, maintaining lists and lookups, and preparing admin or user dashboards without joining the raw log at request time.

How others use it: Command and Flow (and sometimes Action or Timer-driven flows) produce events; Query units read what Projection writes. API handlers call Query, not Projection directly, in the usual layering.

How it uses others: Handlers consume event payloads defined in schema, use projection context to read and write the appropriate stores, and may coordinate with stream usage where the design requires follow-up messages.

Rules and restrictions: Processing the same logical event twice must yield the same stored outcome—use natural keys, upserts, or deduplication as needed. Projections should not become a second place for business authorization; they react to facts, they do not decide them.

What it exports: Per-event-subject projectors that update read models and supporting indexes used by queries, timers, and sagas.

Real-world examples: Building a customer order list from order lifecycle events; maintaining a token index for cleanup jobs; updating an enterprise order aggregate view for reporting; appending an audit table for every domain event in a compliance-heavy system."#;
