pub const TITLE: &str = "Projection";

pub const BRIEF: &str = r#"Projection turns the event stream into query-friendly storage: tables, indexes, and materialized views. Each handler should be idempotent so retries or duplicate deliveries do not corrupt read models."#;

pub const DETAIL: &str = r#"What it is for: Projection maintains derived state that makes reads fast and simple—flattening events into rows, maintaining lists and lookups, and preparing admin or user dashboards without joining the raw log at request time.

How others use it: Command and Flow (and sometimes Action or Timer-driven flows) produce events; Query units read what Projection writes. API handlers call Query, not Projection directly, in the usual layering.

How it uses others: Handlers consume event payloads defined in schema, use projection context to read and write the appropriate stores, and may coordinate with stream usage where the design requires follow-up messages.

Rules and restrictions: Processing the same logical event twice must yield the same stored outcome—use natural keys, upserts, or deduplication as needed. Projections should not become a second place for business authorization; they react to facts, they do not decide them.

What it exports: Per-event-subject projectors that update read models and supporting indexes used by queries, timers, and sagas.

Real-world examples: Building a customer order list from order lifecycle events; maintaining a token index for cleanup jobs; updating an enterprise order aggregate view for reporting; appending an audit table for every domain event in a compliance-heavy system."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Projection is primarily a listener. It subscribes to domain events and action result events, then updates read-model tables so Query can serve them. In rare cases it may publish follow-up events.

### Subscribes (Inbound)

1. Domain Events — ASYNC durable consumer
   - Subject:    {project_uuid}.evt.>  (wildcard on all domain events)
                 or specific subjects like {project_uuid}.evt.shop.orders.place-order, {project_uuid}.evt.auth.users.create-user
   - Payload:    EventEnvelope
   - Consumer:   JetStream durable consumer, consumer name "projection-events"
                 Global shared consumer across ALL projects and all projectors.
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     Command unit (domain events), Action unit (result events via ACTIONS stream)
   - Dispatch:   Projection routes by event subject to the appropriate projector handler.
                 Each handler deserializes payload into the project-specific event payload type (defined in Schema unit).

2. Action Result Events — ASYNC durable consumer
   - Subject:    {project_uuid}.act.{context}.{module}.{unit_name_slug}.result
   - Payload:    ActionResultEvent
   - Consumer:   JetStream durable consumer, consumer name "projection-action-results"
                 Global shared consumer across ALL projects.
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     Action unit
   - Purpose:    Update read models to reflect side-effect outcomes
                 (e.g. mark email as "sent" or "failed" in the notification view)

### Publishes (Outbound) — RARE, use sparingly

1. Derived/Composite Events — ASYNC publish (only when the design requires it)
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.analytics.rollup.daily-summary-ready)
   - Payload:    EventEnvelope
     - meta.source_unit: "projection"
     - meta.causation_id: message_id of the event that triggered the derived event
     - payload: project-specific derived event payload (defined in Schema unit)
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: Flow (to trigger downstream workflows), other Projections (chained views)
   - CAUTION:    This pattern should be rare. Prefer having Flow or Timer trigger derived work.
                 Projection publishing events risks creating cascading projection chains.

### Writes (Side Effect)

   Database writes to read-model tables:
   - INSERT/UPSERT rows in projection-specific tables (e.g. orders_view, users_view, token_index)
   - These tables are owned by Projection and read by Query. No other unit writes to them.
   - Write operations MUST be idempotent: processing the same event twice produces the same result.
     Use upsert with natural keys, or dedup by event message_id.

### Auto-Broadcast Events

   Normally none. Projection is a silent reactor — it consumes events and writes to DB.

   Exception: If a projector detects a significant derived condition (e.g. "order count for customer
   reached 100"), it MAY publish a derived event. This must be explicitly designed and documented
   per projector, not done implicitly.

### Listens To

   - {project_uuid}.evt.> (domain events from Command)
   - {project_uuid}.act.{context}.{module}.{unit_name_slug}.result (action results from Action)

### Interactions With Each Unit

   Projection ← Command:    receives EventEnvelope (domain facts to materialize into read models)
   Projection ← Action:     receives ActionResultEvent (side-effect outcomes to reflect in views)
   Projection ← Flow:       receives EventEnvelope if Flow publishes saga-state events (rare)
   Projection ← Timer:      receives TimerTick if Projection needs to handle time-based updates
                            (uncommon — prefer Flow for time-driven logic)
   Projection → Query:      (indirect) Query reads the tables Projection writes
   Projection → Schema:     [LOCAL] compiled-in type definitions — event body types for deserialization

### Communication Rules

- Projection MUST be idempotent. Reprocessing the same event MUST NOT corrupt read models.
  Use upsert operations, dedup by event message_id, or check aggregate_version.
- Projection MUST NOT publish commands. It reacts to facts; it does not initiate intent.
- Projection MUST NOT call Query, Action, or API. It is a unidirectional consumer.
- EventEnvelope and ActionResultEvent are framework types (not project-defined). The inner payload
  carries project-specific types from Schema.
- Projection tables are append/update-only from Projection's perspective. Query reads them.
  No other unit should write to Projection-owned tables.
- If Projection needs to publish a follow-up event, it MUST be a derived/composite event
  with clear documentation. Avoid creating infinite event loops."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Projection unit instance MUST export a `metadata` object so that other units can discover
which events it reacts to, which read-model tables it writes, and how it transforms events into rows.
Query units depend on this metadata to know which tables and record shapes are available.

### Metadata Shape

{
  "unit_type": "projection",
  "projectors": [
    {
      "id":            "<unique projector slug, e.g. order_list>",
      "description":   "<what this projector builds / maintains>",

      "subscribes_to": [
        {
          "event_subject":    "<NATS event subject or wildcard, e.g. evt.shop.orders.place-order>",
          "payload_type":     "<Schema type name for the event body>",
          "source_unit_type": "<which unit type produces this: command | action | flow | timer>"
        }
      ],

      "writes_to": [
        {
          "table":       "<read-model table name, e.g. orders_view>",
          "record_type": "<Schema view type name, e.g. OrderView>",
          "operations":  ["<insert | upsert | update | delete — which SQL operations this projector uses>"],
          "key_fields":  ["<primary key or natural key field names used for idempotent upsert>"]
        }
      ],

      "publishes_derived_events": [
        {
          "event_subject": "<derived event subject, or omit if none>",
          "payload_type":  "<Schema type name>",
          "trigger":       "<condition that causes this derived event>"
        }
      ]
    }
  ]
}

### Example

{
  "unit_type": "projection",
  "projectors": [
    {
      "id": "order_list",
      "description": "Maintains the orders_view table from order lifecycle events",
      "subscribes_to": [
        {
          "event_subject": "evt.shop.orders.place-order",
          "payload_type": "OrderPlacedEvent",
          "source_unit_type": "command"
        },
        {
          "event_subject": "evt.shop.orders.confirm-order",
          "payload_type": "OrderConfirmedEvent",
          "source_unit_type": "command"
        },
        {
          "event_subject": "evt.shop.orders.cancel-order",
          "payload_type": "OrderCancelledEvent",
          "source_unit_type": "command"
        },
        {
          "event_subject": "evt.shop.orders.ship-order",
          "payload_type": "OrderShippedEvent",
          "source_unit_type": "command"
        }
      ],
      "writes_to": [
        {
          "table": "orders_view",
          "record_type": "OrderView",
          "operations": ["insert", "update"],
          "key_fields": ["order_id"]
        }
      ],
      "publishes_derived_events": []
    },
    {
      "id": "notification_status",
      "description": "Tracks email/push delivery status from action results",
      "subscribes_to": [
        {
          "event_subject": "act.notifications.email.send-confirmation.result",
          "payload_type": "SendConfirmationEmailResult",
          "source_unit_type": "action"
        }
      ],
      "writes_to": [
        {
          "table": "notifications_view",
          "record_type": "NotificationView",
          "operations": ["insert", "upsert"],
          "key_fields": ["trigger_id"]
        }
      ],
      "publishes_derived_events": []
    }
  ]
}

### How Other Units Use This Metadata

- Query unit:    reads "writes_to[].table" + "writes_to[].record_type" to know which tables
                 to read from and which Schema types represent the rows.
- Command unit:  reads "subscribes_to[].event_subject" to confirm the events it emits have
                 a corresponding projector handling them.
- Flow unit:     reads "publishes_derived_events" to discover derived events it may subscribe to.
- Schema unit:   reads "writes_to[].record_type" to know which view types to define.
                 Reads "subscribes_to[].payload_type" to confirm event types exist.
- API unit:      does NOT read Projection metadata directly (goes through Query).

### Rules

- Each projector MUST have a unique "id".
- "subscribes_to" MUST list every event subject this projector handles — no implicit wildcards.
- "writes_to[].key_fields" MUST be specified so idempotent upsert logic is clear.
- "record_type" names MUST match types defined in Schema's "views" section.
- If "publishes_derived_events" is non-empty, the derived event MUST also be listed in Schema."#;
