pub const TITLE: &str = "Command";

pub const BRIEF: &str = r#"Command is the decider: it accepts intents, checks authorization and business rules using read-only access to current state, and emits events that describe facts. Normal handlers never mutate stored entity tables directly; facts go on the bus for projections and flows to react."#;

pub const DETAIL: &str = r#"What it is for: Command turns "someone wants to do X" into either a rejected outcome or an immutable fact expressed as an event, preserving a clear audit trail and a single place for business validation.

How others use it: Gateway and API publish commands. Projection and Flow consume the events commands emit. Query reads models that projections built from those events, not the command handlers themselves.

How it uses others: Commands deserialize schema-defined command payloads, use read-only data access where permitted to validate against current views, and publish events through the stream. A narrowly scoped bootstrap-related path may perform idempotent seeding when the framework allows it.

Rules and restrictions: By design, handlers avoid direct writes to application tables; persistence of entity state is event-driven. Build-time checks restrict use of mutable database handles except in explicitly allowed files for seeding. Commands speak in events, not silent database updates.

What it exports: Dispatch and handlers keyed by command subject that encode domain decisions and emit corresponding events.

Real-world examples: Placing an order after stock and payment checks; logging in and issuing a session token event; admin-only configuration reload; enforcing per-customer rate limits before accepting a trade order in a brokerage backend."#;
