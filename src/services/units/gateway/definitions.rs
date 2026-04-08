pub const TITLE: &str = "Gateway";

pub const BRIEF: &str = r#"Gateway is the message-edge adapter: it turns inbound broker messages into validated internal commands. It does not serve HTTP and does not open databases; it only checks payloads and publishes onward so the core domain stays behind a consistent command interface."#;

pub const DETAIL: &str = r#"What it is for: Gateway protects the system from raw external messages by validating structure and policy at the perimeter, then translating to the command vocabulary the Command unit understands.

How others use it: The process entry that consumes external subjects invokes gateway dispatch. Command units receive the resulting command messages; API and Query units do not call Gateway directly in the typical flow, though the overall application wires both edges.

How it uses others: Gateway uses the schema types for inbound payloads and command bodies, and uses the shared stream abstraction to publish commands. It does not read or write application data stores and does not embed business outcomes—only forwarding and validation.

Rules and restrictions: No database access and no persistence helpers for application state. No mixing in HTTP concerns; those belong to the API unit. It should stay a thin translate-and-publish layer so enforcement rules remain obvious at build time.

What it exports: Per-subject handlers that map from external message types to command emission, keeping all subject and payload knowledge in gateway files rather than in generic wiring code.

Real-world examples: A NATS consumer that turns partner inventory feed messages into internal restock commands; a message queue worker that validates signed mobile-app events before they become commands; a versioned envelope parser that rejects stale API clients before any domain logic runs."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Gateway is an async adapter: it subscribes to external/partner broker subjects, validates inbound messages, translates them into internal commands, and publishes those commands. It is the non-HTTP ingress counterpart to API.

### Subscribes (Inbound)

1. External Broker Subjects — ASYNC durable consumer
   - Subject:    {project_uuid}.gw.{partner_or_source}.{message_type}
                 or external subjects outside the project namespace depending on integration
   - Payload:    Raw external message (partner-defined schema, not our envelopes)
     - Gateway validates structure, version, and basic policy (e.g. signature verification,
       required fields, schema version compatibility)
   - Consumer:   Global shared JetStream durable consumer `gateway-inbound`, queue group
                 (at-least-once delivery, explicit ack, exactly-one processing per external message,
                 shared across ALL projects). Dispatches to the correct gateway handler based
                 on the external subject.
   - Source:     External systems, partner feeds, mobile app event bridges, webhook relays

### Publishes (Outbound)

1. Internal Commands — ASYNC fire-and-forget
   - Subject:    {project_uuid}.cmd.{context}.{module}.{unit_name_slug}
   - Payload:    CommandEnvelope
     - meta.source_unit: "gateway"
     - meta.correlation_id: derived from external message id or generated fresh
     - meta.causation_id: external message identifier for traceability
     - actor_id: extracted from external message (partner id, device id, etc.) or None
     - payload: project-specific command payload (from Schema unit), translated from the external format
   - Mode:       ASYNC — JetStream publish to COMMANDS stream (durable, at-least-once delivery, persisted in stream until consumed and acked)
   - Consumed by: Command unit

### Auto-Broadcast Events

   None directly. Gateway only translates and forwards; the Command unit produces domain events.

   On validation failure, Gateway has two options (project-specific decision):
   a) Silently drop + log (no bus message for invalid external input)
   b) Publish a gateway-specific rejection event:
      - Subject:  {project_uuid}.evt.core.gateway.message-rejected
      - Payload:  EventEnvelope where payload contains:
        {
            external_subject:  String,     // the original subject
            rejection_reason:  String,     // validation failure description
            raw_payload_hash:  String,     // hash of the original payload for debugging
            partner_id:        Option<String>
        }
      - Consumed by: Projection (for rejected-message dashboards), Flow (for alerting workflows)

### Listens To

   - External broker subjects only. Gateway MUST NOT subscribe to internal {project_uuid}.evt.> or
     {project_uuid}.cmd.> subjects. It is an edge adapter, not an internal event consumer.

### Communication Rules

- Gateway MUST NOT access application databases. Validation uses only the message content itself
  and optionally a small allow-list/config (not projection read models).
- Gateway MUST NOT produce domain events directly. It always publishes commands, letting the
  Command unit decide whether the intent is valid from a business perspective.
- The translation from external format → CommandEnvelope MUST happen entirely within Gateway
  handler code, keeping external schema knowledge out of Command.
- If the external message requires enrichment from internal state before becoming a command,
  that enrichment belongs in a Flow or Command handler, not Gateway.
- Gateway handlers MUST be safe to retry — external messages may be redelivered by the broker."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Gateway unit instance MUST export a `metadata` object so that external integration
documentation and internal wiring can discover which external subjects are consumed,
how they are validated, and which internal commands they produce.

### Metadata Shape

{
  "unit_type": "gateway",
  "handlers": [
    {
      "id":                "<unique handler slug, e.g. partner_inventory_feed>",
      "description":       "<what this handler adapts from the outside world>",

      "external_subject":          "<NATS or broker subject for inbound messages, e.g. partner.inventory.update>",
      "external_payload_format":   "<description of external message format: JSON, protobuf, Avro, etc.>",
      "external_payload_fields": {
        "<field_name>": { "type": "<type>", "required": <bool>, "description": "<purpose>" }
      },

      "validation_rules": [
        "<human-readable rule, e.g. 'sku must exist in internal catalog'>"
      ],

      "publishes_command": {
        "command_subject": "<NATS command subject, e.g. cmd.shop.inventory.restock>",
        "payload_type":    "<Schema type name for the translated command body>"
      },

      "rejection_behavior": "<'drop_and_log' | 'emit_rejection_event'>",
      "rejection_event_subject": "<if emit_rejection_event: evt.core.gateway.message-rejected, else null>"
    }
  ]
}

### Example

{
  "unit_type": "gateway",
  "handlers": [
    {
      "id": "partner_inventory_feed",
      "description": "Consumes partner inventory updates and translates to internal restock commands",
      "external_subject": "partner.inventory.update",
      "external_payload_format": "JSON",
      "external_payload_fields": {
        "partner_id": { "type": "string", "required": true,  "description": "Partner account identifier" },
        "sku":        { "type": "string", "required": true,  "description": "Stock-keeping unit code" },
        "quantity":   { "type": "number", "required": true,  "description": "New stock quantity" },
        "timestamp":  { "type": "string", "required": true,  "description": "ISO 8601 timestamp of update" },
        "signature":  { "type": "string", "required": true,  "description": "HMAC signature for verification" }
      },
      "validation_rules": [
        "HMAC signature must be valid using the partner's shared secret",
        "quantity must be non-negative",
        "sku must match a known product in the catalog"
      ],
      "publishes_command": {
        "command_subject": "cmd.shop.inventory.restock",
        "payload_type": "RestockInventoryPayload"
      },
      "rejection_behavior": "emit_rejection_event",
      "rejection_event_subject": "evt.core.gateway.message-rejected"
    },
    {
      "id": "mobile_app_events",
      "description": "Consumes mobile app analytics events and translates to internal tracking commands",
      "external_subject": "mobile.app.event",
      "external_payload_format": "JSON",
      "external_payload_fields": {
        "device_id":  { "type": "string", "required": true, "description": "Unique device identifier" },
        "event_type": { "type": "string", "required": true, "description": "Event type code" },
        "payload":    { "type": "object", "required": true, "description": "Event-specific data" },
        "app_version": { "type": "string", "required": true, "description": "Client app version" }
      },
      "validation_rules": [
        "app_version must be >= minimum supported version",
        "event_type must be in the allowed event types list"
      ],
      "publishes_command": {
        "command_subject": "cmd.analytics.tracking.track-event",
        "payload_type": "TrackEventPayload"
      },
      "rejection_behavior": "drop_and_log",
      "rejection_event_subject": null
    }
  ]
}

### How Other Units Use This Metadata

- Command unit:  reads "publishes_command.command_subject" to confirm it has handlers for all
                 commands Gateway will publish.
- Schema unit:   reads "publishes_command.payload_type" to ensure the translated command body type exists.
- Projection:    reads "rejection_event_subject" (if emit_rejection_event) to build rejected-message dashboards.
- Flow unit:     reads "rejection_event_subject" to wire alerting workflows for message rejections.
- API unit:      does NOT read Gateway metadata (they are separate ingress paths).

### Rules

- Each handler MUST have a unique "id" and unique "external_subject".
- "external_payload_fields" documents the EXTERNAL format (may differ from internal Schema types).
- "publishes_command.payload_type" MUST reference a Schema type (the internal representation).
- "validation_rules" MUST be explicit so the LLM knows what checks to implement."#;
