pub const TITLE: &str = "Bootstrap";

pub const BRIEF: &str = r#"Bootstrap runs once when the process and bus are ready: it publishes initial commands such as demo data seeding so startup policy lives in a unit instead of scattered through main."#;

pub const DETAIL: &str = r#"What it is for: Bootstrap encodes one-time or first-run setup as ordinary commands on the message fabric, aligning startup with the same event-sourced patterns as the rest of the app.

How others use it: Main calls bootstrap run with the stream wrapper. Command handlers perform the actual work idempotently; Projection builds views from resulting events. No other unit type should duplicate startup subject strings in generic wiring.

How it uses others: Bootstrap depends on schema command types and the stream emitter only. It does not touch databases or business handlers directly.

Rules and restrictions: Stream-only, same purity expectations as timer and action edges regarding database access—publish commands and let Command decide. Keep payloads and subject choices inside this unit so process entry stays dumb and reusable.

What it exports: A single run routine invoked at startup after infrastructure is live.

Real-world examples: Seeding reference data for a new tenant; registering default feature flags; enqueueing a migration-style command that replays idempotently; publishing a demo user creation command in sample deployments."#;
