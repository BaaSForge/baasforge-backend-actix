pub const TITLE: &str = "Runtime";

pub const BRIEF: &str = r#"Runtime holds cross-cutting application state for HTTP and request-reply: the stream client, read-only database handle, dynamic query runner, and a generic correlation map for waiting on event responses. It intentionally avoids domain-specific types beyond what handlers need for wiring."#;

pub const DETAIL: &str = r#"What it is for: Runtime centralizes infrastructure handles that many API handlers share, without pulling HTTP routing into core, so main can construct one Arc of state and inject it into Axum.

How others use it: The API unit re-exports the constructor and state struct for ergonomic imports from main. Handlers borrow the stream, query runner, optional read access, and reply channels from this state.

How it uses others: Runtime depends on generic core abstractions for the message fabric, read-only persistence, and the query runner trait. Business rules and schema-specific reply typing stay at the edges of this module as much as possible.

Rules and restrictions: Keep this module free of route definitions, dispatch tables, and domain decisions—those belong to API, Query, Command, and siblings. It is glue, not product logic.

What it exports: The app state struct, small helpers such as header parsing for bearer tokens, and a factory to build the shared Arc used at serve time.

Real-world examples: Shared Axum state in a microservice; a single place to hold NATS connection and DB pool clones; attaching a tracing-aware query executor; storing pending HTTP reply senders keyed by correlation id for event-driven login flows."#;
