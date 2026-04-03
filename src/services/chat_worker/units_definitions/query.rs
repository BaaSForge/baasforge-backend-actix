pub const TITLE: &str = "Query";

pub const BRIEF: &str = r#"Query answers read requests: it loads from read models and returns bytes for the API layer. It never publishes messages and never writes application data; it is the librarian over projection-built state."#;

pub const DETAIL: &str = r#"What it is for: Query provides fast, purpose-built reads—single record fetches, lists, filters, pagination, and token resolution—without entangling HTTP or message parsing.

How others use it: The API unit (via a runner trait) and sometimes internal wiring call query dispatch with a subject and payload. End users and gateways interact with HTTP or messages, not with query handlers directly.

How it uses others: Query uses schema input and output types for each subject, read-only database access to views that projections maintain, and its own query context. It does not emit commands or events.

Rules and restrictions: No writes to application stores, no publishing to the bus, and no borrowing other unit contexts. Keeping queries pure reads preserves caching assumptions and avoids dual-write bugs.

What it exports: A dispatch implementation plus per-subject handlers returning serialized response bodies, and optionally a runner type that implements the core trait so HTTP stays decoupled from concrete query modules.

Real-world examples: Fetching current user profile after Bearer resolution; paginated order history for a storefront; hourly statistics for an operations dashboard; resolving an opaque token to a user id before authorization checks in middleware-free stacks."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Query is the only unit that participates in SYNC request-reply. It receives query requests, reads from Projection-maintained tables, and returns serialized responses. It has zero outbound bus communication.

### Responds To (Inbound — SYNC)

1. Query Dispatch — SYNC request-reply
   - Subject:    {project_uuid}.qry.{context}.{module}.{unit_name_slug}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.qry.shop.orders.get-by-id, f47ac10b-58cc-4372-a567-0e02b2c3d479.qry.core.users.list, f47ac10b-58cc-4372-a567-0e02b2c3d479.qry.core.auth.resolve-token)
   - Request:    QueryRequest (framework type, not project-defined)
     - meta.source_unit: "api" (or any caller)
     - actor_id: authenticated user id (for access-scoped reads)
     - payload: project-specific query input (type defined in Schema unit), specific per operation:
       Examples:
         qry.shop.orders.get-by-id    → { "order_id": "uuid" }
         qry.shop.orders.list         → { "page": 1, "per_page": 20, "status_filter": "active" }
         qry.core.users.get-profile   → { "user_id": "uuid" }
         qry.core.auth.resolve-token  → { "token": "opaque_string" }
   - Response:   QueryResponse (framework type, not project-defined)
     - correlation_id: echoed from request
     - success: true/false
     - data: project-specific query output (type defined in Schema unit) on success:
       Examples:
         qry.shop.orders.get-by-id    → { "id": "uuid", "status": "placed", "total": 99.50, ... }
         qry.shop.orders.list         → { "items": [...], "total_count": 42, "page": 1 }
         qry.core.users.get-profile   → { "id": "uuid", "name": "...", "email": "..." }
         qry.core.auth.resolve-token  → { "user_id": "uuid", "roles": ["admin"] }
     - error: QueryError (framework type, not project-defined) on failure:
         { "code": "not_found", "message": "Order not found" }
         { "code": "forbidden", "message": "Access denied to this resource" }
   - Mode:       SYNC — Core NATS request-reply (ephemeral, NO persistence, NO automatic retry, times out if responder unavailable).
                 API sends a NATS Request to the query subject,
                 Query unit has a subscription on {project_uuid}.qry.> with queue group "query-dispatch" (shared across all projects),
                 processes the query, and sends the NATS Reply with QueryResponse.
   - Called by:  API (primary), potentially internal wiring or other services

### Publishes (Outbound)

   NONE. Query never publishes to any NATS subject. It is a pure read responder.

### Subscribes (Inbound)

   - Subject:    {project_uuid}.qry.>  (wildcard on all query subjects)
   - Consumer:   Core NATS subscription with queue group "query-dispatch" (global, shared across all projects,
                 load-balanced across Query instances).
                 Core NATS request-reply — ephemeral, NO persistence, NO automatic retry, times out if responder unavailable.
   - Purpose:    Receives QueryRequest (framework type, not project-defined), dispatches to per-subject handler, sends reply

### Reads (Data Source)

   - Read-only database access to Projection-maintained tables
   - Query MUST NOT write to any database table
   - Query MUST NOT access tables owned by Flow (saga state) or other non-projection stores

### Auto-Broadcast Events

   NONE. Query is completely silent on the bus. Pure function: input → output.

### Listens To

   - {project_uuid}.qry.{context}.{module}.{unit_name_slug} requests (from API or other callers)

### Interactions With Each Unit

   Query ← API:         receives QueryRequest, returns QueryResponse (SYNC)
   Query ← Projection:  (indirect) reads the tables that Projection writes
   Query → Schema:      [LOCAL] compiled-in type definitions for query input/output payloads

### Communication Rules

- Query MUST NOT publish any messages (commands, events, or anything else) to the bus.
- Query MUST NOT write to any database table. Read-only access only.
- Query MUST NOT import or call Command, Action, Flow, Timer, Gateway, or Bootstrap.
- Each query subject MUST have a paired input type and output type defined in Schema.
- Query handlers MUST be safe to call concurrently. No shared mutable state.
- Query responses MUST be deterministic given the same database state — no side effects.
- Query MUST propagate the correlation_id from request to response for traceability."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Query unit instance MUST export a `metadata` object so that API endpoints and the LLM
can discover which read operations exist, their input/output types, and which projection tables they access.

### Metadata Shape

{
  "unit_type": "query",
  "handlers": [
    {
      "id":            "<unique handler slug, e.g. get_order_by_id>",
      "query_subject": "<NATS query subject, e.g. qry.shop.orders.get-by-id>",
      "description":   "<what this query returns>",
      "input_type":    "<Schema type name for query input, e.g. GetOrderByIdInput>",
      "output_type":   "<Schema type name for query output, e.g. GetOrderByIdOutput>",

      "reads_from": [
        { "table": "<projection table name>", "record_type": "<Schema view type name>" }
      ],

      "error_codes": [
        { "code": "<machine-readable code>", "description": "<when this error is returned>" }
      ]
    }
  ]
}

### Example

{
  "unit_type": "query",
  "handlers": [
    {
      "id": "get_order_by_id",
      "query_subject": "qry.shop.orders.get-by-id",
      "description": "Fetch a single order by its UUID",
      "input_type": "GetOrderByIdInput",
      "output_type": "GetOrderByIdOutput",
      "reads_from": [
        { "table": "orders_view", "record_type": "OrderView" }
      ],
      "error_codes": [
        { "code": "not_found", "description": "No order with the given ID exists" },
        { "code": "forbidden", "description": "Caller does not have access to this order" }
      ]
    },
    {
      "id": "list_orders",
      "query_subject": "qry.shop.orders.list",
      "description": "Paginated list of orders for a customer, with optional status filter",
      "input_type": "ListOrdersInput",
      "output_type": "ListOrdersOutput",
      "reads_from": [
        { "table": "orders_view", "record_type": "OrderView" }
      ],
      "error_codes": [
        { "code": "invalid_page", "description": "Page number is out of range" }
      ]
    },
    {
      "id": "resolve_token",
      "query_subject": "qry.core.auth.resolve-token",
      "description": "Resolve an opaque bearer token to a user identity and roles",
      "input_type": "ResolveTokenInput",
      "output_type": "ResolveTokenOutput",
      "reads_from": [
        { "table": "tokens_view", "record_type": "TokenView" }
      ],
      "error_codes": [
        { "code": "token_expired", "description": "Token has expired" },
        { "code": "token_invalid", "description": "Token is malformed or not found" }
      ]
    }
  ]
}

### How Other Units Use This Metadata

- API unit:        reads "query_subject" + "input_type" + "output_type" to know how to dispatch
                   reads and what types to use for HTTP response bodies.
- Projection unit: reads "reads_from[].table" to confirm the tables it writes are consumed by queries.
- Schema unit:     reads "input_type" + "output_type" to know which query types to define.
                   Reads "reads_from[].record_type" to confirm view types exist.
- Command unit:    reads "reads_from[].table" to confirm it validates against the same tables.

### Rules

- Each handler MUST have a unique "id" and unique "query_subject".
- "query_subject" MUST follow the pattern: qry.{context}.{module}.{unit_name_slug}
- "input_type" and "output_type" MUST reference types defined in Schema's query_inputs/query_outputs.
- "reads_from[].table" MUST match tables declared in a Projection unit's "writes_to".
- "error_codes" MUST cover at least the common failure cases (not_found, forbidden, etc.)."#;
