pub const TITLE: &str = "API";

pub const BRIEF: &str = r#"API is the HTTP surface: routes, authentication headers, request-reply correlation over the bus, and composition of reads via the query runner plus writes via command emission. It never becomes the place that mutates database tables for domain entities."#;

pub const DETAIL: &str = r#"What it is for: API exposes the product to browsers and mobile clients, translates HTTP into internal commands and queries, and wires correlation so a handler can wait for a matching event when the UX needs a synchronous response.

How others use it: Clients call HTTP; main mounts the router with shared state. Gateway remains separate for non-HTTP ingress. Query handlers answer through the runner; command paths emit events consumed elsewhere.

How it uses others: API uses schema types for JSON bodies, read-only database access for lightweight checks like token presence, the stream for command publish and event reply, and the runtime-provided app state container re-exported from this module for convenience.

Rules and restrictions: No direct persistence of domain rows or saga documents here—those belong to Command, Projection, or Flow. Avoid introducing mutable database writers or ad-hoc state persistence in handlers so the compile-time unit rules stay honest.

What it exports: The HTTP router, event-to-reply hook, and re-exported constructors for shared handler state used when serving Axum.

Real-world examples: REST JSON API for a SaaS dashboard; OAuth-style login returning tokens; placing an order and returning the created id after the matching event; admin-only CSV export endpoints backed by query read models."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

API is the most connected unit for outbound communication. It publishes commands (async), calls queries (sync), and uses a correlated-event-wait pattern for request-reply when HTTP clients need synchronous responses to write operations.

### Publishes (Outbound)

1. Commands — ASYNC publish (fire-and-forget OR correlated wait)
   - Subject:    {project_uuid}.cmd.{context}.{module}.{unit_name_slug}
   - Payload:    CommandEnvelope (framework type, not project-defined)
     - meta.source_unit: "api"
     - meta.correlation_id: generated per HTTP request (UUIDv7), shared across the entire chain
     - meta.causation_id: same as correlation_id (API is the root cause for user-initiated actions)
     - actor_id: extracted from Bearer token / session (authenticated user id)
     - payload: project-specific command payload (type defined in the project's Schema unit), deserialized from the HTTP request JSON body
   - Mode:       ASYNC — JetStream publish to COMMANDS stream.
                 JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked.
   - Consumed by: Command unit (via durable consumer "command-dispatch", at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack)

   Two sub-patterns:

   a) Fire-and-forget: API publishes command, immediately returns 202 Accepted to HTTP client.
      Used when the client does not need the result synchronously.

   b) Correlated wait (sync-over-async): API publishes command, then blocks the HTTP handler
      waiting for a matching EventEnvelope whose correlation_id matches. Returns 200/201 with
      the event payload as the HTTP response body.
      - Mechanism: Before publishing the command, API inserts a oneshot::Sender into Runtime's
        correlation reply map keyed by correlation_id. A background subscription on {project_uuid}.evt.>
        (the event-to-reply hook) matches incoming events by correlation_id and resolves the sender.
      - Timeout: Configurable (e.g. 5–30 seconds). On timeout, API returns 504 Gateway Timeout.
      - Payload received through the channel: EventEnvelope (framework type, not project-defined) — the domain event produced by Command

### Calls (Sync — NATS Request-Reply)

1. Queries — SYNC via NATS request-reply
   - Subject:    {project_uuid}.qry.{context}.{module}.{unit_name_slug}
   - Request:    QueryRequest (framework type, not project-defined)
     - meta.source_unit: "api"
     - actor_id: from Bearer token
     - payload: project-specific query input (type defined in Schema unit) (e.g. { "order_id": "..." })
   - Response:   QueryResponse (framework type, not project-defined)
     - success: bool
     - data: project-specific query output (type defined in Schema unit) (e.g. the order object)
     - error: Optional QueryError (framework type, not project-defined) with code and message
   - Mode:       SYNC — Core NATS request-reply (ephemeral, NO persistence, NO automatic retry, times out if responder unavailable).
                 The Query unit subscribes and replies. Caller blocks until reply arrives or timeout.
   - Answered by: Query unit (via its NATS subscription on {project_uuid}.qry.> with queue group "query-dispatch", shared across all projects)

### Subscribes (Inbound)

1. Correlated Events — ASYNC subscription (background, wired at startup)
   - Subject:    {project_uuid}.evt.>  (wildcard on all domain events)
   - Payload:    EventEnvelope (framework type, not project-defined)
   - Purpose:    The event-to-reply hook reads correlation_id from each event and checks
                 Runtime's correlation reply map. If a pending sender exists, it resolves
                 the oneshot channel, unblocking the waiting HTTP handler.
   - Consumer:   JetStream durable consumer "api-correlation" (global, shared across all projects).
                 At-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack.
                 Events are persisted so correlation can survive brief handler restarts.
                 If the process restarts, pending HTTP requests are lost (they timeout on client side).
   - This subscription is NOT a business event consumer. It exists solely for the correlated
     request-reply pattern. It does not process events or update state.

### Auto-Broadcast Events

   None. API does not produce events. It publishes commands and lets Command emit events.

### Listens To (summarized)

   - HTTP requests from external clients (browsers, mobile, other services)
   - Correlated events from {project_uuid}.evt.> for request-reply resolution

### Interactions With Each Unit

   API → Command:    ASYNC publish CommandEnvelope on {project_uuid}.cmd.{context}.{module}.{unit_name_slug}
   API → Query:      SYNC NATS request-reply QueryRequest → QueryResponse on {project_uuid}.qry.{context}.{module}.{unit_name_slug}
   API ← Command:    (indirect) receives EventEnvelope via correlation reply map
   API → Runtime:    [LOCAL] injected infrastructure handles — NATS client, correlation map, DB pool. Available in-process, not via NATS.
   API → Schema:     [LOCAL] compiled-in type definitions for command/query/event payloads. Available in-process, not via NATS.

### Communication Rules

- API MUST NOT subscribe to events for business processing — that is Projection's and Flow's job.
  The event subscription is ONLY for correlation-based reply resolution.
- API MUST NOT write to application database tables. Reads are allowed for things like auth checks only.
- API MUST NOT call Action, Timer, or Projection directly. All write-path communication goes
  through commands on the bus.
- For read paths, API MUST go through the Query unit (via NATS request-reply), never reading projection tables directly.
- Every HTTP write endpoint MUST generate a unique correlation_id and include it in the CommandEnvelope.
- Timeout on correlated waits MUST be enforced to prevent resource leaks in the reply map."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every API unit instance MUST export a `metadata` object so that the LLM and other units can discover
which HTTP endpoints exist, what HTTP method and path each uses, what payload goes in and out,
and how each endpoint connects to the bus (command publish or NATS query request-reply).

### Metadata Shape

{
  "unit_type": "api",
  "endpoints": [
    {
      "id":            "<unique endpoint slug, e.g. place_order>",
      "method":        "<HTTP method: GET | POST | PUT | PATCH | DELETE>",
      "path":          "<URL path with colon params, e.g. /api/v1/orders/:order_id>",
      "auth_required": <bool>,
      "description":   "<what this endpoint does>",

      "path_params": [
        { "name": "<param name from path>", "type": "<string | number>", "description": "<purpose>" }
      ],
      "query_params": [
        { "name": "<param name>", "type": "<type>", "required": <bool>, "description": "<purpose>" }
      ],

      "request_body_type":  "<Schema type name for HTTP request body, or null if no body>",
      "response_body_type": "<Schema type name for HTTP response body>",

      // For write endpoints (POST/PUT/PATCH/DELETE that publish commands):
      "command": {
        "subject":         "<NATS command subject, e.g. cmd.shop.orders.place-order>",
        "payload_type":    "<Schema type name for command body, e.g. PlaceOrderPayload>",
        "correlated_wait": <bool — true if handler blocks for event reply>,
        "expected_event":  "<event subject to wait for, e.g. evt.shop.orders.place-order, or null if fire-and-forget>",
        "timeout_ms":      <number — max wait time for correlated reply, e.g. 10000>
      },

      // For read endpoints (GET or any that dispatch queries):
      "query": {
        "subject":     "<NATS query subject, e.g. qry.shop.orders.get-by-id>",
        "input_type":  "<Schema type name for query input>",
        "output_type": "<Schema type name for query output>"
      },

      "success_status":   <HTTP status code on success, e.g. 200, 201, 202>,
      "error_responses": [
        { "status": <number>, "code": "<machine-readable>", "description": "<when this happens>" }
      ]
    }
  ]
}

NOTE: An endpoint has EITHER a "command" block OR a "query" block, not both.
Endpoints that do both (rare: e.g. read-then-write) should be split or use "command" with
correlated_wait=true where the event reply carries the read data.

### Example

{
  "unit_type": "api",
  "endpoints": [
    {
      "id": "place_order",
      "method": "POST",
      "path": "/api/v1/orders",
      "auth_required": true,
      "description": "Place a new order for the authenticated customer",
      "path_params": [],
      "query_params": [],
      "request_body_type": "PlaceOrderRequest",
      "response_body_type": "PlaceOrderResponse",
      "command": {
        "subject": "cmd.shop.orders.place-order",
        "payload_type": "PlaceOrderPayload",
        "correlated_wait": true,
        "expected_event": "evt.shop.orders.place-order",
        "timeout_ms": 10000
      },
      "success_status": 201,
      "error_responses": [
        { "status": 400, "code": "validation_error", "description": "Invalid request body" },
        { "status": 401, "code": "unauthorized",     "description": "Missing or invalid Bearer token" },
        { "status": 504, "code": "timeout",           "description": "Command processing timed out" }
      ]
    },
    {
      "id": "get_order",
      "method": "GET",
      "path": "/api/v1/orders/:order_id",
      "auth_required": true,
      "description": "Fetch a single order by ID",
      "path_params": [
        { "name": "order_id", "type": "string", "description": "UUID of the order" }
      ],
      "query_params": [],
      "request_body_type": null,
      "response_body_type": "GetOrderByIdOutput",
      "query": {
        "subject": "qry.shop.orders.get-by-id",
        "input_type": "GetOrderByIdInput",
        "output_type": "GetOrderByIdOutput"
      },
      "success_status": 200,
      "error_responses": [
        { "status": 404, "code": "not_found", "description": "Order does not exist" },
        { "status": 403, "code": "forbidden", "description": "Order belongs to another customer" }
      ]
    },
    {
      "id": "list_orders",
      "method": "GET",
      "path": "/api/v1/orders",
      "auth_required": true,
      "description": "List orders for the authenticated customer with pagination",
      "path_params": [],
      "query_params": [
        { "name": "page",      "type": "number",  "required": false, "description": "Page number (default 1)" },
        { "name": "per_page",  "type": "number",  "required": false, "description": "Items per page (default 20, max 100)" },
        { "name": "status",    "type": "string",  "required": false, "description": "Filter by status" }
      ],
      "request_body_type": null,
      "response_body_type": "ListOrdersOutput",
      "query": {
        "subject": "qry.shop.orders.list",
        "input_type": "ListOrdersInput",
        "output_type": "ListOrdersOutput"
      },
      "success_status": 200,
      "error_responses": [
        { "status": 401, "code": "unauthorized", "description": "Missing or invalid Bearer token" }
      ]
    }
  ]
}

### How Other Units Use This Metadata

- Command unit:  reads "command.subject" to know which command subjects API will publish, so it knows
                 which handlers to implement.
- Query unit:    reads "query.subject" + "query.input_type" + "query.output_type" to know which
                 query handlers to implement and what types to use.
- Schema unit:   reads all type names referenced here ("request_body_type", "response_body_type",
                 "command.payload_type", "query.input_type", "query.output_type") to know which
                 types it must define.
- Gateway unit:  does NOT read API metadata (Gateway handles non-HTTP ingress).
- Flow unit:     reads "command.expected_event" to understand the event subjects API expects,
                 which confirms the events Command must emit.

### Rules

- Every endpoint MUST have a unique "id" slug within the unit.
- "method" MUST be an uppercase HTTP verb.
- "path" MUST use colon-prefix for path parameters (e.g. :order_id), matching the path_params list.
- Type names in "request_body_type", "response_body_type", "command.payload_type", "query.input_type",
  "query.output_type" MUST match exactly the type names defined in the Schema unit's metadata.
- If "correlated_wait" is true, "expected_event" MUST be specified.
- If "correlated_wait" is false, "expected_event" should be null and "success_status" is typically 202."#;
