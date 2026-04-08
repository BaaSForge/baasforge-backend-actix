pub const TITLE: &str = "Schema";

pub const BRIEF: &str = r#"Schema is the shared contract layer: it holds the shapes of commands, events, HTTP payloads, gateway messages, action payloads, query inputs and outputs, and data that projections store or queries return. It has no runtime behavior and never talks to the bus or storage; every other unit imports these types so messages stay consistent across the system."#;

pub const DETAIL: &str = r#"What it is for: Schema defines what travels between parts of the application—intents to change state, facts that already happened, read requests and responses, and stored view shapes—so every unit agrees on field names and meaning without sharing implementation.

How others use it: Gateway, Command, Projection, Flow, Action, Timer, API, and Query all depend on schema types to serialize and deserialize payloads. New behavior usually starts by adding or extending types here, then implementing handlers that use those types.

How it uses others: It does not call or import other units. It may depend only on ordinary language primitives and shared libraries, not on message infrastructure or persistence handles from the framework core.

Rules and restrictions: There is no dispatch, no handler context, and no access to the message fabric or databases. It is types-only documentation-by-compilation; keeping it free of runtime hooks avoids circular dependencies and keeps the contract stable.

What it exports: Named data structures and enums that represent commands, events, API bodies, gateway payloads, action jobs, projection-backed records and views, and query request and response pairs with predictable naming so callers know the contract without reading handler code.

Real-world examples: OpenAPI-generated DTOs in a BFF; protobuf or Avro schemas in an event-driven shop; shared JSON types for payment webhooks and internal commands in a subscription billing system; a single package of structs used by both HTTP handlers and background workers in a logistics platform."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Schema has NO runtime communication — it never publishes, subscribes, or touches the bus.
Its role is to define every payload type that travels between units. All other units import from Schema so that serialization and deserialization stay consistent system-wide.

### Canonical Envelope Types (defined here, used everywhere)

Schema MUST export the following envelope structures. Every message on NATS is wrapped in the appropriate envelope.

#### MessageMeta (embedded in every envelope)

    MessageMeta {
        message_id:     String,         // unique id for this specific message (UUIDv7 recommended)
        correlation_id: String,         // traces the full causal chain back to the originating request
        causation_id:   String,         // message_id of the direct parent that caused this message
        timestamp:      i64,            // unix milliseconds when the message was produced
        source_unit:    String,         // unit kind that produced the message (e.g. "command", "action")
    }

#### CommandEnvelope (travels on {project_uuid}.cmd.{context}.{module}.{unit_name_slug})

    CommandEnvelope {
        meta:       MessageMeta,
        subject:    String,             // full NATS subject, e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.cmd.shop.orders.place-order"
        actor_id:   Option<String>,     // authenticated user, service account, or None for system
        payload:    Value,              // schema-defined command body (specific per command subject)
    }

#### EventEnvelope (travels on {project_uuid}.evt.{context}.{module}.{unit_name_slug})

    EventEnvelope {
        meta:               MessageMeta,
        subject:            String,             // full NATS subject, e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.orders.place-order"
        aggregate_id:       String,             // the entity this event belongs to
        aggregate_version:  u64,                // optimistic concurrency version for ordering
        payload:            Value,              // schema-defined event body (specific per event subject)
    }

#### QueryRequest (travels on {project_uuid}.qry.{context}.{module}.{unit_name_slug} via NATS Request)

    QueryRequest {
        meta:       MessageMeta,
        subject:    String,             // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.qry.shop.orders.get-by-id"
        actor_id:   Option<String>,     // caller identity for access-scoped reads
        payload:    Value,              // schema-defined query input
    }

#### QueryResponse (reply to QueryRequest)

    QueryResponse {
        correlation_id: String,         // matches the request's correlation_id
        success:        bool,
        data:           Option<Value>,  // schema-defined query output on success
        error:          Option<QueryError>,
    }

    QueryError {
        code:    String,                // machine-readable error code (e.g. "not_found", "forbidden")
        message: String,                // human-readable description
    }

#### ActionTrigger (travels on {project_uuid}.act.{context}.{module}.{unit_name_slug})

    ActionTrigger {
        meta:             MessageMeta,
        subject:          String,       // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.act.notifications.email.send-confirmation"
        idempotency_key:  String,       // dedup key so retries do not cause duplicate side effects
        payload:          Value,        // schema-defined action job body
    }

#### ActionResultEvent (travels on {project_uuid}.act.{context}.{module}.{unit_name_slug}.result)

    ActionResultEvent {
        meta:             MessageMeta,
        trigger_id:       String,       // message_id of the ActionTrigger that caused this
        subject:          String,       // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.act.notifications.email.send-confirmation.result"
        success:          bool,
        result_payload:   Option<Value>,// output data from the side effect (e.g. external id)
        error:            Option<ActionError>,
    }

    ActionError {
        code:       String,
        message:    String,
        retryable:  bool,               // hint to Flow whether automatic retry is safe
    }

#### TimerTick (travels on {project_uuid}.tmr.{context}.{module}.{unit_name_slug}.tick)

    TimerTick {
        meta:                MessageMeta,
        job_name:            String,     // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479.tmr.maintenance.cleanup.expire-reservations"
        schedule_expression: String,     // cron or interval spec for observability
        scheduled_at:        i64,        // unix millis of the intended fire time
        fired_at:            i64,        // unix millis when the tick actually fired
    }

### Framework Types vs Project Schema Types

The envelope types above (MessageMeta, CommandEnvelope, EventEnvelope, QueryRequest, QueryResponse,
QueryError, ActionTrigger, ActionResultEvent, ActionError, TimerTick) are FRAMEWORK types — defined
once in the platform core, shared by all projects. They are NOT defined in any project's Schema unit.

The `payload`, `data`, and `result_payload` fields inside these envelopes carry PROJECT-SPECIFIC types
that ARE defined in each project's Schema unit instances (e.g. PlaceOrderPayload, OrderPlacedEvent,
SendConfirmationEmailPayload, SendConfirmationEmailResult, OrderView, etc.).

When the LLM creates a Schema unit, it defines only the project-specific payload types. The framework
envelopes are always available and do not need to be created.

### NATS Subject Naming Convention

Every unit has an identity of {context}.{module}.{unit_name_slug} derived from its stored ID
(minus the revision segment). Subjects embed this identity after the message-type prefix.
{project_uuid} is always a UUID (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479), never a human-readable slug.

    {project_uuid}.cmd.{context}.{module}.{unit_name_slug}              — Commands (targeting a Command handler unit)
    {project_uuid}.evt.{context}.{module}.{unit_name_slug}              — Domain events (produced by a Command handler)
    {project_uuid}.evt.{context}.{module}.{unit_name_slug}.rejected     — Rejection events
    {project_uuid}.qry.{context}.{module}.{unit_name_slug}              — Query request-reply
    {project_uuid}.act.{context}.{module}.{unit_name_slug}              — Action triggers
    {project_uuid}.act.{context}.{module}.{unit_name_slug}.result       — Action result events
    {project_uuid}.tmr.{context}.{module}.{unit_name_slug}.tick         — Timer ticks

### Schema and Runtime: Local Dependencies

Schema types and Runtime handles are NOT distributed through NATS. They are compiled-in / injected
dependencies available locally in every unit's runtime. When a unit "uses Schema", it means
importing the type definitions at compile time. When a unit "uses Runtime", it means accessing
injected infrastructure handles (NATS client, DB pool, correlation map) from the unit's local context.
All other inter-unit communication (Command, Event, Query, Action, Timer) is distributed through NATS.

### NATS Stream Topology

    Stream COMMANDS   — subjects: {project_uuid}.cmd.>       — retention: WorkQueue (each command consumed once)
        JetStream, at-least-once delivery, WorkQueue retention (each command consumed exactly once), explicit ack required.
    Stream EVENTS     — subjects: {project_uuid}.evt.>       — retention: Limits   (fan-out to many consumers, replay-friendly)
        JetStream, at-least-once delivery, Limits retention (fan-out to multiple consumers: projection, flow, api-correlation), explicit ack per consumer.
    Stream ACTIONS    — subjects: {project_uuid}.act.>       — retention: Limits   (triggers + results, multiple consumers)
        JetStream, at-least-once delivery, Limits retention (triggers consumed by action-triggers consumer, results fan-out to flow + projection consumers), explicit ack.
    Stream TIMERS     — subjects: {project_uuid}.tmr.>       — retention: Limits   (ticks consumed by multiple subscribers)
        JetStream, at-least-once delivery, Limits retention, explicit ack.
    Queries — Core NATS request-reply, ephemeral, NO persistence, NO automatic retry, subject to caller-defined timeout.
        If the responder is down, the request times out. No JetStream stream.

### Global Shared Consumers

Consumers are NOT per-project. A fixed set of consumers handles messages for ALL projects.
The {project_uuid} in the subject allows routing to the correct project's unit implementation
at dispatch time, but the consumer infrastructure is shared.

    command-dispatch             — JetStream pull consumer on COMMANDS stream, queue group, exactly-one processing
    projection-events            — JetStream push consumer on EVENTS stream, receives all domain events for projection handlers
    flow-events                  — JetStream push consumer on EVENTS stream, receives all domain events for flow/saga handlers
    action-triggers              — JetStream pull consumer on ACTIONS stream, filtered to trigger subjects (no .result), queue group
    flow-action-results          — JetStream push consumer on ACTIONS stream, filtered to *.act.*.*.*.result subjects, for flow saga advancement
    projection-action-results    — JetStream push consumer on ACTIONS stream, filtered to *.act.*.*.*.result subjects, for projection view updates
    flow-timer-ticks             — JetStream push consumer on TIMERS stream, for flow timeout/scheduling handlers
    query-dispatch               — Core NATS queue group subscription on *.qry.>, request-reply, no JetStream
    gateway-inbound              — JetStream pull consumer on external/partner subjects, queue group
    api-correlation              — JetStream push consumer on EVENTS stream, for API correlated reply resolution

### Communication Rules

- Every payload struct for a specific command, event, query, action, or timer MUST be defined in Schema before any handler uses it.
- Envelope types are generic; the inner `payload: Value` is always deserialized into a Schema-defined struct by the receiving unit.
- Schema NEVER imports from other units. Dependency flows one way: other units → Schema.
- Adding a new inter-unit communication path always starts by adding the payload types here first."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Schema unit instance MUST export a `metadata` object so that other units can discover
what payload types exist, which NATS subjects they belong to, and what fields they carry.
The LLM reads this metadata when writing any other unit to know the exact shape of data flowing through the bus.

Schema is the ONLY unit type that declares payload field shapes. All other units reference Schema type names.

Schema defines only project-specific payload types. Framework envelope types (MessageMeta, CommandEnvelope, EventEnvelope, etc.) are provided by the platform and should NOT be redefined in Schema units.

### Metadata Shape

{
  "unit_type": "schema",
  "types": {
    "commands": [
      {
        "name":    "<PascalCase type name, e.g. PlaceOrderPayload>",
        "subject": "<NATS command subject, e.g. cmd.shop.orders.place-order — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": {
          "<field_name>": { "type": "<type>", "required": <bool>, "description": "<purpose>" }
        }
      }
    ],
    "events": [
      {
        "name":    "<PascalCase type name, e.g. OrderPlacedEvent>",
        "subject": "<NATS event subject, e.g. evt.shop.orders.place-order — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": {
          "<field_name>": { "type": "<type>", "required": <bool>, "description": "<purpose>" }
        }
      }
    ],
    "query_inputs": [
      {
        "name":    "<PascalCase type name, e.g. GetOrderByIdInput>",
        "subject": "<NATS query subject, e.g. qry.shop.orders.get-by-id — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": { ... }
      }
    ],
    "query_outputs": [
      {
        "name":    "<PascalCase type name, e.g. GetOrderByIdOutput>",
        "subject": "<NATS query subject, e.g. qry.shop.orders.get-by-id — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": { ... }
      }
    ],
    "action_triggers": [
      {
        "name":    "<PascalCase type name, e.g. SendConfirmationEmailPayload>",
        "subject": "<NATS action subject, e.g. act.notifications.email.send-confirmation — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": { ... }
      }
    ],
    "action_results": [
      {
        "name":    "<PascalCase type name, e.g. SendConfirmationEmailResult>",
        "subject": "<NATS action result subject, e.g. act.notifications.email.send-confirmation.result — uses unit identity {context}.{module}.{unit_name_slug}>",
        "fields": { ... }
      }
    ],
    "views": [
      {
        "name":    "<PascalCase type name, e.g. OrderView>",
        "table":   "<read-model table name, e.g. orders_view>",
        "description": "<what this view represents>",
        "fields": { ... }
      }
    ]
  }
}

### Field Type Vocabulary

Use these type strings in field definitions so all units share one vocabulary:
  "string"            — text / UUID / ISO date string
  "number"            — integer or float (specify in description if integer-only)
  "boolean"           — true/false
  "string[]"          — array of strings
  "number[]"          — array of numbers
  "object"            — nested object (define nested fields inline or reference another type by name)
  "<TypeName>"        — reference to another Schema type by its PascalCase name
  "<TypeName>[]"      — array of another Schema type
  "string | null"     — nullable string (append " | null" to any type for nullable)

### Example

{
  "unit_type": "schema",
  "types": {
    "commands": [
      {
        "name": "PlaceOrderPayload",
        "subject": "cmd.shop.orders.place-order",
        "fields": {
          "customer_id": { "type": "string",        "required": true,  "description": "UUID of the customer placing the order" },
          "items":       { "type": "OrderItem[]",   "required": true,  "description": "Line items with product_id and quantity" },
          "currency":    { "type": "string",        "required": true,  "description": "ISO 4217 currency code" },
          "notes":       { "type": "string | null", "required": false, "description": "Optional order notes" }
        }
      }
    ],
    "events": [
      {
        "name": "OrderPlacedEvent",
        "subject": "evt.shop.orders.place-order",
        "fields": {
          "order_id":    { "type": "string",      "required": true, "description": "Generated UUID of the new order" },
          "customer_id": { "type": "string",      "required": true, "description": "UUID of the customer" },
          "items":       { "type": "OrderItem[]", "required": true, "description": "Confirmed line items" },
          "total_cents": { "type": "number",      "required": true, "description": "Computed total in cents" },
          "currency":    { "type": "string",      "required": true, "description": "ISO 4217 currency code" },
          "placed_at":   { "type": "string",      "required": true, "description": "ISO 8601 timestamp" }
        }
      },
      {
        "name": "OrderPlaceRejectedEvent",
        "subject": "evt.shop.orders.place-order.rejected",
        "fields": {
          "command_subject":  { "type": "string",        "required": true,  "description": "Original command subject" },
          "rejection_code":   { "type": "string",        "required": true,  "description": "Machine-readable code" },
          "rejection_reason": { "type": "string",        "required": true,  "description": "Human-readable explanation" },
          "original_payload": { "type": "object | null", "required": false, "description": "Echo of the command payload" }
        }
      }
    ],
    "query_inputs": [
      {
        "name": "GetOrderByIdInput",
        "subject": "qry.shop.orders.get-by-id",
        "fields": {
          "order_id": { "type": "string", "required": true, "description": "UUID of the order to fetch" }
        }
      }
    ],
    "query_outputs": [
      {
        "name": "GetOrderByIdOutput",
        "subject": "qry.shop.orders.get-by-id",
        "fields": {
          "order_id":    { "type": "string",      "required": true,  "description": "UUID of the order" },
          "customer_id": { "type": "string",      "required": true,  "description": "UUID of the customer" },
          "status":      { "type": "string",      "required": true,  "description": "Current status: placed, confirmed, shipped, delivered, cancelled" },
          "items":       { "type": "OrderItem[]", "required": true,  "description": "Line items" },
          "total_cents": { "type": "number",      "required": true,  "description": "Total in cents" },
          "placed_at":   { "type": "string",      "required": true,  "description": "ISO 8601 timestamp" }
        }
      }
    ],
    "action_triggers": [
      {
        "name": "SendConfirmationEmailPayload",
        "subject": "act.notifications.email.send-confirmation",
        "fields": {
          "to":       { "type": "string", "required": true, "description": "Recipient email address" },
          "order_id": { "type": "string", "required": true, "description": "Order UUID for template rendering" },
          "template": { "type": "string", "required": true, "description": "Email template identifier" }
        }
      }
    ],
    "action_results": [
      {
        "name": "SendConfirmationEmailResult",
        "subject": "act.notifications.email.send-confirmation.result",
        "fields": {
          "message_id":   { "type": "string", "required": true, "description": "Provider message ID (e.g. SES message ID)" },
          "delivered_at": { "type": "string", "required": true, "description": "ISO 8601 delivery timestamp" }
        }
      }
    ],
    "views": [
      {
        "name": "OrderView",
        "table": "orders_view",
        "description": "Denormalized order record for fast reads",
        "fields": {
          "order_id":      { "type": "string",      "required": true, "description": "Primary key UUID" },
          "customer_id":   { "type": "string",      "required": true, "description": "Customer UUID" },
          "status":        { "type": "string",      "required": true, "description": "Current lifecycle status" },
          "items":         { "type": "OrderItem[]", "required": true, "description": "Line items snapshot" },
          "total_cents":   { "type": "number",      "required": true, "description": "Total in cents" },
          "currency":      { "type": "string",      "required": true, "description": "ISO 4217" },
          "placed_at":     { "type": "string",      "required": true, "description": "When the order was placed" },
          "confirmed_at":  { "type": "string | null", "required": false, "description": "When the order was confirmed" }
        }
      }
    ]
  }
}

### Rules

- Every NATS subject that carries a payload MUST have a corresponding Schema type.
- Schema types are the single source of truth. Command, Projection, Query, Action, etc. reference
  them by name — they do NOT re-declare field shapes.
- When adding a new inter-unit communication path, add the Schema types FIRST, then implement handlers.
- Type names MUST be PascalCase. Subject segments after the message-type prefix follow the unit identity pattern {context}.{module}.{unit_name_slug} where slugs use lowercase letters and hyphens.
- Rejection event types follow the convention: {CommandName}RejectedEvent or generic CommandRejection."#;
