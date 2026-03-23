pub const TITLE: &str = "Action";

pub const BRIEF: &str = r#"Action performs side effects in the outside world—email, payments, push notifications, websocket fan-out—triggered by bus messages. It reports success or failure back as events so the rest of the system can stay deterministic."#;

pub const DETAIL: &str = r#"What it is for: Action isolates I/O that must not run inside Command or Query: network calls, third-party APIs, and delivery channels where failure, retries, and idempotency keys are operational concerns.

How others use it: Flow, Command (indirectly via events), or other publishers place work on action subjects. Projections and Flow often listen for the success or failure events Actions emit to update state or continue sagas.

How it uses others: Actions deserialize schema-defined job payloads, use action context and the stream to emit result events, and call external services. They do not perform direct application database reads or writes for domain tables.

Rules and restrictions: No embedding of read-model queries or database handles for app data; keep side effects here so pure handlers stay testable. Design messages so duplicate delivery can be detected or tolerated. Prefer emitting explicit outcome events over silent failure.

What it exports: Per-subject handlers that execute a side effect and finish by publishing success or failure facts.

Real-world examples: Charging a card through a payment provider; sending order confirmation email; pushing realtime updates to connected browsers; uploading a generated invoice to object storage; posting to Slack when an on-call alert fires."#;
