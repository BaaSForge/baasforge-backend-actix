pub const TITLE: &str = "Bootstrap";

pub const BRIEF: &str = r#"Bootstrap runs once when the process and bus are ready: it publishes initial commands such as demo data seeding so startup policy lives in a unit instead of scattered through main."#;

pub const DETAIL: &str = r#"What it is for: Bootstrap encodes one-time or first-run setup as ordinary commands on the message fabric, aligning startup with the same event-sourced patterns as the rest of the app.

How others use it: Main calls bootstrap run with the stream wrapper. Command handlers perform the actual work idempotently; Projection builds views from resulting events. No other unit type should duplicate startup subject strings in generic wiring.

How it uses others: Bootstrap depends on schema command types and the stream emitter only. It does not touch databases or business handlers directly.

Rules and restrictions: Stream-only, same purity expectations as timer and action edges regarding database access—publish commands and let Command decide. Keep payloads and subject choices inside this unit so process entry stays dumb and reusable.

What it exports: A single run routine invoked at startup after infrastructure is live.

Real-world examples: Seeding reference data for a new tenant; registering default feature flags; enqueueing a migration-style command that replays idempotently; publishing a demo user creation command in sample deployments."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Bootstrap is a one-shot async publisher. It runs once at startup, publishes commands, and exits. It never subscribes to anything.

### Publishes (Outbound)

1. Seed Commands — ASYNC fire-and-forget
   - Subject:    {project_uuid}.cmd.{context}.{module}.{unit_name_slug}  (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.cmd.core.users.create-user, f47ac10b-58cc-4372-a567-0e02b2c3d479.cmd.core.config.seed-flags)
   - Payload:    CommandEnvelope
     - meta.source_unit: "bootstrap"
     - meta.correlation_id: generated at bootstrap run (one per seed batch or per command)
     - meta.causation_id: same as correlation_id (bootstrap is always the root cause)
     - actor_id: None or "system"
     - payload: project-specific command payload (from Schema unit) for the specific seed operation
   - Mode:       ASYNC — JetStream publish, no reply expected, no acknowledgment waited beyond delivery confirm
   - Consumed by: Command unit (via its JetStream consumer on {project_uuid}.cmd.>)
   - Delivery:   JetStream publish with ack — durable, at-least-once, message persisted in COMMANDS stream

### Subscribes (Inbound)

   None. Bootstrap does not subscribe to any NATS subjects.

### Auto-Broadcast Events

   None directly. However, every command Bootstrap publishes will cause the Command unit to emit
   corresponding domain events. Bootstrap is the indirect root cause of those events.

   Typical chain: Bootstrap → publishes CommandEnvelope → Command processes → emits EventEnvelope
   → Projection updates read models → Query can serve the seeded data

### Listens To

   Nothing. Bootstrap is invoked by the host process once, not by bus messages.

### Communication Rules

- Bootstrap MUST only use the stream handle (from Runtime) to publish commands. No DB access.
- All published commands MUST be idempotent in design — Bootstrap may run on every restart but
  the Command handler must tolerate duplicate seed commands gracefully.
- Bootstrap MUST NOT wait for event replies; it publishes and returns.
- Subject strings and payload shapes MUST be co-located inside Bootstrap files, not leaked into generic wiring.
- Bootstrap commands use the same CommandEnvelope as any other command source (API, Gateway, Flow).
  The Command unit does not distinguish the origin."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Bootstrap unit instance MUST export a `metadata` object so that the LLM can see
which seed commands are published at startup, their payloads, and which Command handlers
need to exist to process them.

### Metadata Shape

{
  "unit_type": "bootstrap",
  "seed_commands": [
    {
      "id":              "<unique seed slug, e.g. seed_default_user>",
      "command_subject":  "<NATS command subject, e.g. cmd.core.users.create-user>",
      "payload_type":     "<Schema type name for the command body>",
      "description":      "<why this seed exists>",
      "idempotent":       <bool — MUST be true; handler must tolerate re-execution>,
      "run_condition":    "<when to publish: 'always' | 'if_empty' | 'once_per_tenant'>"
    }
  ]
}

### Example

{
  "unit_type": "bootstrap",
  "seed_commands": [
    {
      "id": "seed_default_admin",
      "command_subject": "cmd.core.users.create-user",
      "payload_type": "CreateUserPayload",
      "description": "Creates the default admin user on first startup",
      "idempotent": true,
      "run_condition": "always"
    },
    {
      "id": "seed_feature_flags",
      "command_subject": "cmd.core.config.seed-flags",
      "payload_type": "SeedFeatureFlagsPayload",
      "description": "Registers default feature flags if they do not exist",
      "idempotent": true,
      "run_condition": "always"
    },
    {
      "id": "seed_demo_data",
      "command_subject": "cmd.core.demo.populate-data",
      "payload_type": "PopulateDemoDataPayload",
      "description": "Populates demo orders, products, and customers for sample deployments",
      "idempotent": true,
      "run_condition": "if_empty"
    }
  ]
}

### How Other Units Use This Metadata

- Command unit:  reads "command_subject" + "payload_type" to confirm it has handlers for every seed command.
- Schema unit:   reads "payload_type" to ensure seed command body types are defined.
- Projection:    (indirect) seed commands produce events that Projection materializes.
- No other unit needs to read Bootstrap metadata directly.

### Rules

- Every seed command MUST have "idempotent": true. Bootstrap may run on every startup.
- "command_subject" MUST match a handler declared in a Command unit.
- "payload_type" MUST reference a type defined in Schema.
- "run_condition" describes intent; the actual idempotency is enforced by the Command handler."#;
