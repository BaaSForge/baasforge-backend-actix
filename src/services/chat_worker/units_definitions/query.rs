pub const TITLE: &str = "Query";

pub const BRIEF: &str = r#"Query answers read requests: it loads from read models and returns bytes for the API layer. It never publishes messages and never writes application data; it is the librarian over projection-built state."#;

pub const DETAIL: &str = r#"What it is for: Query provides fast, purpose-built reads—single record fetches, lists, filters, pagination, and token resolution—without entangling HTTP or message parsing.

How others use it: The API unit (via a runner trait) and sometimes internal wiring call query dispatch with a subject and payload. End users and gateways interact with HTTP or messages, not with query handlers directly.

How it uses others: Query uses schema input and output types for each subject, read-only database access to views that projections maintain, and its own query context. It does not emit commands or events.

Rules and restrictions: No writes to application stores, no publishing to the bus, and no borrowing other unit contexts. Keeping queries pure reads preserves caching assumptions and avoids dual-write bugs.

What it exports: A dispatch implementation plus per-subject handlers returning serialized response bodies, and optionally a runner type that implements the core trait so HTTP stays decoupled from concrete query modules.

Real-world examples: Fetching current user profile after Bearer resolution; paginated order history for a storefront; hourly statistics for an operations dashboard; resolving an opaque token to a user id before authorization checks in middleware-free stacks."#;
