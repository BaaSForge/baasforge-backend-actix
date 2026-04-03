pub const TITLE: &str = "Runtime";

pub const BRIEF: &str = r#"Runtime holds cross-cutting application state for HTTP and request-reply: the stream client, read-only database handle, dynamic query runner, and a generic correlation map for waiting on event responses. It intentionally avoids domain-specific types beyond what handlers need for wiring."#;

pub const DETAIL: &str = r#"What it is for: Runtime centralizes infrastructure handles that many API handlers share, without pulling HTTP routing into core, so main can construct one Arc of state and inject it into Axum.

How others use it: The API unit re-exports the constructor and state struct for ergonomic imports from main. Handlers borrow the stream, query runner, optional read access, and reply channels from this state.

How it uses others: Runtime depends on generic core abstractions for the message fabric, read-only persistence, and the query runner trait. Business rules and schema-specific reply typing stay at the edges of this module as much as possible.

Rules and restrictions: Keep this module free of route definitions, dispatch tables, and domain decisions—those belong to API, Query, Command, and siblings. It is glue, not product logic.

What it exports: The app state struct, small helpers such as header parsing for bearer tokens, and a factory to build the shared Arc used at serve time.

Real-world examples: Shared Axum state in a microservice; a single place to hold NATS connection and DB pool clones; attaching a tracing-aware query executor; storing pending HTTP reply senders keyed by correlation id for event-driven login flows."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Runtime does NOT publish or subscribe to NATS subjects directly. It is infrastructure glue that holds the handles other units need to communicate.

Runtime is a LOCAL dependency, not distributed through NATS. It is injected into every unit's process as shared state (Arc). Schema types are similarly available as compiled-in imports.

### What Runtime Provides Locally to Units

1. Stream Client Handle (NATS JetStream context)
   - Used by: API (publish commands), Command (publish events), Flow (publish commands/action triggers),
     Action (publish result events), Timer (publish ticks), Bootstrap (publish seed commands),
     Gateway (publish commands)
   - All publishes through this handle are JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked.
   - Runtime also provides handles to the global shared durable consumers:
     command-dispatch, projection-events, flow-events, action-triggers, flow-action-results,
     projection-action-results, flow-timer-ticks, api-correlation.
     Each consumer delivers at-least-once with explicit ack after processing, redelivered on ack timeout or nack.
     The query-dispatch queue group uses Core NATS request-reply (ephemeral, NO persistence, NO automatic retry).
   - Runtime owns the connection; units borrow it.

2. Correlation Reply Map (HashMap<correlation_id, oneshot::Sender>)
   - Used by: API exclusively
   - Purpose: When API publishes a command and needs to wait for the resulting event to build an HTTP
     response, it inserts a oneshot sender keyed by correlation_id. When the matching event arrives
     (via a subscription wired at startup), the event-to-reply hook resolves the sender, unblocking
     the HTTP handler.
   - Payload flowing through the channel: EventEnvelope (framework type, not project-defined) — the matched domain event

3. Read-Only Database Handle
   - Used by: API (lightweight auth checks), Command (business rule validation), Query (read-model reads),
     Projection (read during idempotent upserts)
   - NOT used by: Action, Timer, Bootstrap, Gateway (these are stream-only)

### Communication Rules

- Runtime MUST NOT contain domain logic, dispatch tables, or subject routing.
- Runtime MUST NOT publish or subscribe to any NATS subject on its own initiative.
- Any unit that needs a stream handle, DB pool, or reply channel MUST obtain it from Runtime's shared state (Arc).
- The correlation reply map MUST be cleaned up on timeout to prevent memory leaks from orphaned correlation entries."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Runtime unit instances export a `metadata` object describing which infrastructure handles
are provided and their configuration. Other units reference this to know what capabilities
are available at runtime.

### Metadata Shape

{
  "unit_type": "runtime",
  "provides": {
    "stream_client":          <bool — NATS JetStream context available>,
    "read_db":                <bool — read-only database pool available>,
    "correlation_reply_map":  <bool — correlated event reply resolution available>
  },
  "config": {
    "correlation_timeout_ms":    <number — default timeout for correlated waits>,
    "max_pending_correlations":  <number — max concurrent pending reply channels>,
    "db_pool_max_connections":   <number — max DB pool size>
  }
}

### Example

{
  "unit_type": "runtime",
  "provides": {
    "stream_client": true,
    "read_db": true,
    "correlation_reply_map": true
  },
  "config": {
    "correlation_timeout_ms": 15000,
    "max_pending_correlations": 5000,
    "db_pool_max_connections": 20
  }
}

### How Other Units Use This Metadata

- API unit:     checks "provides.correlation_reply_map" and "config.correlation_timeout_ms" to
                configure correlated wait behavior.
- Query unit:   checks "provides.read_db" to confirm read-only database access is available.
- Command unit: checks "provides.read_db" and "provides.stream_client" for validation reads and event publishing.
- All units:    check "provides.stream_client" to confirm NATS publishing is available.

### Rules

- Runtime metadata is mostly informational for the LLM to understand infrastructure capabilities.
- "config" values are defaults; individual endpoints may override timeout_ms.
- Runtime does NOT declare bus subjects — it only provides the handles for other units to use."#;
