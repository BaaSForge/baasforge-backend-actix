pub const TITLE: &str = "Command";

pub const BRIEF: &str = r#"Command is the decider: it accepts intents, checks authorization and business rules using read-only access to current state, and emits events that describe facts. Normal handlers never mutate stored entity tables directly; facts go on the bus for projections and flows to react."#;

pub const DETAIL: &str = r#"What it is for: Command turns "someone wants to do X" into either a rejected outcome or an immutable fact expressed as an event, preserving a clear audit trail and a single place for business validation.

How others use it: Gateway and API publish commands. Projection and Flow consume the events commands emit. Query reads models that projections built from those events, not the command handlers themselves.

How it uses others: Commands deserialize schema-defined command payloads, use read-only data access where permitted to validate against current views, and publish events through the stream. A narrowly scoped bootstrap-related path may perform idempotent seeding when the framework allows it.

Rules and restrictions: By design, handlers avoid direct writes to application tables; persistence of entity state is event-driven. Build-time checks restrict use of mutable database handles except in explicitly allowed files for seeding. Commands speak in events, not silent database updates.

What it exports: Dispatch and handlers keyed by command subject that encode domain decisions and emit corresponding events.

Real-world examples: Placing an order after stock and payment checks; logging in and issuing a session token event; admin-only configuration reload; enforcing per-customer rate limits before accepting a trade order in a brokerage backend."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Command is the central hub of the write path. It subscribes to all command subjects, processes business logic, and emits domain events. Every command MUST produce at least one event (success or rejection).

### Subscribes (Inbound)

1. Commands — ASYNC durable consumer
   - Subject:    {project_uuid}.cmd.>  (wildcard matching all command subjects)
   - Payload:    CommandEnvelope
   - Consumer:   JetStream durable consumer, queue group "command-dispatch"
                 Global shared consumer across ALL projects.
                 (exactly-one processing per command message across Command instances)
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     API (user-initiated), Gateway (external-message-initiated),
                 Bootstrap (startup seeds), Flow (saga-driven follow-up commands)
   - Dispatch:   Command unit routes by subject to the appropriate per-subject handler.
                 Each handler deserializes payload into the project-specific command payload type (defined in Schema unit).

   Per-handler processing:
   - Read-only DB access: to validate against current state (e.g. "does this user exist?",
     "is stock available?", "is the token valid?")
   - Business rule checks: authorization, invariant enforcement, rate limits
   - Decision: accept → emit success event, OR reject → emit rejection event

### Publishes (Outbound)

1. Domain Events (success) — ASYNC publish
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.orders.place-order, f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.auth.users.create-user, f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.auth.sessions.start-session)
   - Payload:    EventEnvelope
     - meta.source_unit: "command"
     - meta.correlation_id: propagated from the incoming CommandEnvelope
     - meta.causation_id: message_id of the incoming CommandEnvelope
     - aggregate_id: the entity id affected (e.g. order_id, user_id)
     - aggregate_version: incremented version for the aggregate
     - payload: project-specific event payload (defined in Schema unit) containing the facts of what happened
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: Projection, Flow, Action (via event subscriptions), API (via correlation reply)

2. Rejection Events (failure) — ASYNC publish
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}.rejected
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.orders.place-order.rejected, f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.auth.users.login.rejected)
   - Payload:    EventEnvelope
     - meta fields: same propagation as success events
     - aggregate_id: the entity id that was targeted (if known)
     - aggregate_version: 0 or current version (no state change)
     - payload: project-specific rejection payload (defined in Schema unit):
       {
           command_subject: String,       // the original command subject
           rejection_code:  String,       // machine-readable (e.g. "insufficient_stock", "unauthorized")
           rejection_reason: String,      // human-readable explanation
           original_payload: Option<Value> // optionally echo back the command payload for debugging
       }
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: API (via correlation reply to send error HTTP response),
                  Flow (to handle rejection in saga), Projection (optional, for audit)

### Auto-Broadcast Events

   MANDATORY: Every command handler invocation MUST emit exactly one event:
   - On success: a domain event on {project_uuid}.evt.{context}.{module}.{unit_name_slug}
   - On rejection: a rejection event on {project_uuid}.evt.{context}.{module}.{unit_name_slug}.rejected

   This is critical because:
   - API's correlated wait depends on receiving an event to unblock the HTTP response
   - Flow's saga steps depend on events to advance or compensate
   - Projection needs events to maintain consistent read models
   - Silent failures (no event emitted) cause the system to stall

### Listens To

   - {project_uuid}.cmd.> (all commands from any source)

### Interactions With Each Unit

   Command ← API:        receives CommandEnvelope (user intent from HTTP)
   Command ← Gateway:    receives CommandEnvelope (translated external message)
   Command ← Bootstrap:  receives CommandEnvelope (startup seed commands)
   Command ← Flow:       receives CommandEnvelope (saga-driven follow-up commands)
   Command → Projection: emits EventEnvelope (facts for read-model updates)
   Command → Flow:       emits EventEnvelope (facts that advance/start sagas)
   Command → Action:     emits EventEnvelope (may trigger action via Flow listening, or directly
                         if Action subscribes to domain events)
   Command → API:        emits EventEnvelope (resolved via correlation reply for sync HTTP response)
   Command → Schema:     [LOCAL] compiled-in type definitions — command body types and event body types

### Communication Rules

- Command MUST NOT publish commands (it would create loops). It only publishes events.
- Command MUST NOT call Query, Action, or Timer directly. All downstream effects happen via
  the events it emits, consumed by other units asynchronously.
- Command MUST NOT write to application tables directly. State changes are expressed as events;
  Projection handles the actual DB writes.
- Command MUST propagate correlation_id from the incoming command to the outgoing event so that
  the entire causal chain remains traceable.
- Command handlers MUST be safe to retry — if a command is redelivered by JetStream, the handler
  should either produce the same event idempotently or detect the duplicate and skip.
- Read-only DB access in Command is for validation only. The data Command reads comes from
  Projection-maintained read models.
- CommandEnvelope and EventEnvelope are framework types (not project-defined). The inner payload
  field carries project-specific types from Schema."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Command unit instance MUST export a `metadata` object so that other units can discover
which command subjects are handled, what events each handler produces, and what rejection codes are possible.
This is the most critical metadata for the system: it defines the write-path contract.

### Metadata Shape

{
  "unit_type": "command",
  "handlers": [
    {
      "id":                  "<unique handler slug, e.g. place_order>",
      "command_subject":     "<NATS command subject, e.g. cmd.shop.orders.place-order>",
      "input_payload_type":  "<Schema type name for the command body, e.g. PlaceOrderPayload>",
      "description":         "<what this command does, business intent>",

      "reads_from": [
        { "table": "<projection table name>", "purpose": "<why it reads this table for validation>" }
      ],

      "success_events": [
        {
          "event_subject":  "<NATS event subject, e.g. evt.shop.orders.place-order>",
          "payload_type":   "<Schema type name for event body, e.g. OrderPlacedEvent>",
          "description":    "<what this event represents>",
          "aggregate_id_source": "<where aggregate_id comes from, e.g. 'generated UUID' or 'input.customer_id'>"
        }
      ],

      "rejection_event": {
        "event_subject": "<NATS rejection subject, e.g. evt.shop.orders.place-order.rejected>",
        "payload_type":  "<Schema type name or 'CommandRejection' for generic>",
        "possible_codes": [
          { "code": "<machine-readable code>", "description": "<when this rejection happens>" }
        ]
      }
    }
  ]
}

### Example

{
  "unit_type": "command",
  "handlers": [
    {
      "id": "place_order",
      "command_subject": "cmd.shop.orders.place-order",
      "input_payload_type": "PlaceOrderPayload",
      "description": "Validates stock availability and customer standing, then emits OrderPlacedEvent",
      "reads_from": [
        { "table": "inventory_view", "purpose": "Check stock availability for each line item" },
        { "table": "customers_view", "purpose": "Verify customer is not suspended" }
      ],
      "success_events": [
        {
          "event_subject": "evt.shop.orders.place-order",
          "payload_type": "OrderPlacedEvent",
          "description": "An order has been accepted and assigned an ID",
          "aggregate_id_source": "generated UUID"
        }
      ],
      "rejection_event": {
        "event_subject": "evt.shop.orders.place-order.rejected",
        "payload_type": "OrderPlaceRejectedEvent",
        "possible_codes": [
          { "code": "insufficient_stock",       "description": "One or more items exceed available inventory" },
          { "code": "customer_suspended",       "description": "Customer account is suspended" },
          { "code": "invalid_payment_method",   "description": "Provided payment method is invalid or expired" }
        ]
      }
    },
    {
      "id": "cancel_order",
      "command_subject": "cmd.shop.orders.cancel-order",
      "input_payload_type": "CancelOrderPayload",
      "description": "Cancels an existing order if it has not been shipped yet",
      "reads_from": [
        { "table": "orders_view", "purpose": "Check current order status" }
      ],
      "success_events": [
        {
          "event_subject": "evt.shop.orders.cancel-order",
          "payload_type": "OrderCancelledEvent",
          "description": "An order has been cancelled",
          "aggregate_id_source": "input.order_id"
        }
      ],
      "rejection_event": {
        "event_subject": "evt.shop.orders.cancel-order.rejected",
        "payload_type": "OrderCancelRejectedEvent",
        "possible_codes": [
          { "code": "order_not_found",    "description": "No order with this ID exists" },
          { "code": "already_shipped",    "description": "Order has already been shipped" },
          { "code": "already_cancelled",  "description": "Order is already cancelled" }
        ]
      }
    }
  ]
}

### How Other Units Use This Metadata

- API unit:        reads "command_subject" to know which subjects to publish commands to.
                   Reads "success_events[].event_subject" for correlated_wait expected_event.
                   Reads "rejection_event.possible_codes" to map to HTTP error responses.
- Gateway unit:    reads "command_subject" to know which subjects to publish translated commands to.
- Projection unit: reads "success_events[].event_subject" + "payload_type" to know which events
                   to subscribe to and which Schema types to deserialize.
- Flow unit:       reads "success_events[].event_subject" to know which events start or advance sagas.
                   reads "rejection_event.event_subject" to know which events trigger compensation.
- Query unit:      reads "reads_from[].table" to confirm it reads the same projection tables.
- Schema unit:     reads all type names ("input_payload_type", "success_events[].payload_type",
                   "rejection_event.payload_type") to know which types it must define.
- Action unit:     reads "success_events[].event_subject" if using direct event subscription mode.

### Rules

- Every handler MUST declare at least one success event.
- Every handler MUST declare a rejection event with at least one possible code.
- "command_subject" MUST match the subject pattern: cmd.{context}.{module}.{unit_name_slug}
- "event_subject" MUST match the subject pattern: evt.{context}.{module}.{unit_name_slug}
- Rejection subjects MUST follow: evt.{context}.{module}.{unit_name_slug}.rejected
- All type names MUST reference types defined in the Schema unit."#;
