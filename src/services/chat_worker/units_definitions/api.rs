pub const TITLE: &str = "API";

pub const BRIEF: &str = r#"API is the HTTP surface: routes, authentication headers, request-reply correlation over the bus, and composition of reads via the query runner plus writes via command emission. It never becomes the place that mutates database tables for domain entities."#;

pub const DETAIL: &str = r#"What it is for: API exposes the product to browsers and mobile clients, translates HTTP into internal commands and queries, and wires correlation so a handler can wait for a matching event when the UX needs a synchronous response.

How others use it: Clients call HTTP; main mounts the router with shared state. Gateway remains separate for non-HTTP ingress. Query handlers answer through the runner; command paths emit events consumed elsewhere.

How it uses others: API uses schema types for JSON bodies, read-only database access for lightweight checks like token presence, the stream for command publish and event reply, and the runtime-provided app state container re-exported from this module for convenience.

Rules and restrictions: No direct persistence of domain rows or saga documents here—those belong to Command, Projection, or Flow. Avoid introducing mutable database writers or ad-hoc state persistence in handlers so the compile-time unit rules stay honest.

What it exports: The HTTP router, event-to-reply hook, and re-exported constructors for shared handler state used when serving Axum.

Real-world examples: REST JSON API for a SaaS dashboard; OAuth-style login returning tokens; placing an order and returning the created id after the matching event; admin-only CSV export endpoints backed by query read models."#;
