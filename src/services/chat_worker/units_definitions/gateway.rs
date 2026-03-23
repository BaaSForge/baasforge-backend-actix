pub const TITLE: &str = "Gateway";

pub const BRIEF: &str = r#"Gateway is the message-edge adapter: it turns inbound broker messages into validated internal commands. It does not serve HTTP and does not open databases; it only checks payloads and publishes onward so the core domain stays behind a consistent command interface."#;

pub const DETAIL: &str = r#"What it is for: Gateway protects the system from raw external messages by validating structure and policy at the perimeter, then translating to the command vocabulary the Command unit understands.

How others use it: The process entry that consumes external subjects invokes gateway dispatch. Command units receive the resulting command messages; API and Query units do not call Gateway directly in the typical flow, though the overall application wires both edges.

How it uses others: Gateway uses the schema types for inbound payloads and command bodies, and uses the shared stream abstraction to publish commands. It does not read or write application data stores and does not embed business outcomes—only forwarding and validation.

Rules and restrictions: No database access and no persistence helpers for application state. No mixing in HTTP concerns; those belong to the API unit. It should stay a thin translate-and-publish layer so enforcement rules remain obvious at build time.

What it exports: Per-subject handlers that map from external message types to command emission, keeping all subject and payload knowledge in gateway files rather than in generic wiring code.

Real-world examples: A NATS consumer that turns partner inventory feed messages into internal restock commands; a message queue worker that validates signed mobile-app events before they become commands; a versioned envelope parser that rejects stale API clients before any domain logic runs."#;
