pub const TITLE: &str = "Schema";

pub const BRIEF: &str = r#"Schema is the shared contract layer: it holds the shapes of commands, events, HTTP payloads, gateway messages, action payloads, query inputs and outputs, and data that projections store or queries return. It has no runtime behavior and never talks to the bus or storage; every other unit imports these types so messages stay consistent across the system."#;

pub const DETAIL: &str = r#"What it is for: Schema defines what travels between parts of the application—intents to change state, facts that already happened, read requests and responses, and stored view shapes—so every unit agrees on field names and meaning without sharing implementation.

How others use it: Gateway, Command, Projection, Flow, Action, Timer, API, and Query all depend on schema types to serialize and deserialize payloads. New behavior usually starts by adding or extending types here, then implementing handlers that use those types.

How it uses others: It does not call or import other units. It may depend only on ordinary language primitives and shared libraries, not on message infrastructure or persistence handles from the framework core.

Rules and restrictions: There is no dispatch, no handler context, and no access to the message fabric or databases. It is types-only documentation-by-compilation; keeping it free of runtime hooks avoids circular dependencies and keeps the contract stable.

What it exports: Named data structures and enums that represent commands, events, API bodies, gateway payloads, action jobs, projection-backed records and views, and query request and response pairs with predictable naming so callers know the contract without reading handler code.

Real-world examples: OpenAPI-generated DTOs in a BFF; protobuf or Avro schemas in an event-driven shop; shared JSON types for payment webhooks and internal commands in a subscription billing system; a single package of structs used by both HTTP handlers and background workers in a logistics platform."#;
