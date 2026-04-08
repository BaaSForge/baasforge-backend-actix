pub const TITLE: &str = "Action";

pub const BRIEF: &str = r#"Action performs side effects in the outside world—email, payments, push notifications, websocket fan-out—triggered by bus messages. It reports success or failure back as events so the rest of the system can stay deterministic."#;

pub const DETAIL: &str = r#"What it is for: Action isolates I/O that must not run inside Command or Query: network calls, third-party APIs, and delivery channels where failure, retries, and idempotency keys are operational concerns.

How others use it: Flow, Command (indirectly via events), or other publishers place work on action subjects. Projections and Flow often listen for the success or failure events Actions emit to update state or continue sagas.

How it uses others: Actions deserialize schema-defined job payloads, use action context and the stream to emit result events, and call external services. They do not perform direct application database reads or writes for domain tables.

Rules and restrictions: No embedding of read-model queries or database handles for app data; keep side effects here so pure handlers stay testable. Design messages so duplicate delivery can be detected or tolerated. Prefer emitting explicit outcome events over silent failure.

What it exports: Per-subject handlers that execute a side effect and finish by publishing success or failure facts.

Real-world examples: Charging a card through a payment provider; sending order confirmation email; pushing realtime updates to connected browsers; uploading a generated invoice to object storage; posting to Slack when an on-call alert fires."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Action subscribes to action trigger messages, executes external side effects, and ALWAYS publishes a result event (success or failure). It is the I/O boundary of the system.

### Subscribes (Inbound)

1. Action Triggers — ASYNC durable consumer
   - Subject:    {project_uuid}.act.{context}.{module}.{unit_name_slug}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.act.notifications.email.send-confirmation, f47ac10b-58cc-4372-a567-0e02b2c3d479.act.billing.stripe.charge-card,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.act.notifications.push.send, f47ac10b-58cc-4372-a567-0e02b2c3d479.act.documents.storage.upload-invoice)
   - Payload:    ActionTrigger
     - meta: MessageMeta (correlation_id, causation_id trace back to the saga/command chain)
     - idempotency_key: used to detect duplicate triggers and skip re-execution
     - payload: project-specific action job payload (type defined in the project's Schema unit), specific per action subject:
       Examples:
         act.notifications.email.send-confirmation → { "to": "user@example.com", "order_id": "uuid", "template": "order_confirm" }
         act.billing.stripe.charge-card          → { "customer_id": "uuid", "amount_cents": 9950, "currency": "USD", "payment_method_id": "pm_..." }
         act.notifications.push.send             → { "user_id": "uuid", "title": "Order shipped", "body": "Your order #123 is on the way" }
         act.documents.storage.upload-invoice  → { "order_id": "uuid", "pdf_bytes_b64": "..." }
   - Consumer:   Global shared JetStream durable consumer `action-triggers`, queue group
                 (at-least-once delivery, explicit ack, exactly-one processing per trigger,
                 shared across ALL projects). Dispatches to the correct action handler based
                 on {project_uuid} and unit identity in the subject.
   - Source:     Flow (primary — saga steps), or direct event subscriptions (less common)

2. Domain Events (alternative trigger mode) — ASYNC durable consumer
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}
   - Payload:    EventEnvelope
   - Consumer:   Global shared JetStream durable consumer `action-events-standalone`
                 (at-least-once delivery, explicit ack, shared across ALL projects).
                 Dispatches to the correct action handler based on {project_uuid} and event subject.
   - Source:     Command unit
   - Purpose:    Some actions may subscribe directly to domain events instead of requiring
                 Flow to explicitly trigger them. Use this for simple "always do X when Y happens"
                 patterns that don't need saga orchestration.
   - Example:    Whenever evt.core.users.create-user fires, Action directly sends a welcome email
                 without Flow involvement.
   - Note:       Prefer the Flow → ActionTrigger path for any action that is part of a multi-step
                 process or needs compensation. Direct event subscription is for standalone effects.

### Publishes (Outbound)

1. Action Result Events — ASYNC publish (MANDATORY after every execution)
   - Subject:    {project_uuid}.act.{context}.{module}.{unit_name_slug}.result
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.act.notifications.email.send-confirmation.result,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.act.billing.stripe.charge-card.result)
   - Payload:    ActionResultEvent
     - meta.source_unit: "action"
     - meta.correlation_id: propagated from the ActionTrigger
     - meta.causation_id: message_id of the ActionTrigger that caused this
     - trigger_id: message_id of the ActionTrigger (explicit link for saga lookup)
     - subject: echoed subject for easy routing
     - success: bool

     On success:
     - result_payload: project-specific success payload (type defined in the project's Schema unit), specific per action:
       Examples:
         act.notifications.email.send-confirmation.result → { "message_id": "ses_abc123", "delivered_at": 1700000000 }
         act.billing.stripe.charge-card.result          → { "charge_id": "ch_xyz", "amount_cents": 9950, "receipt_url": "https://..." }
         act.notifications.push.send.result             → { "push_id": "pn_456", "platform": "apns" }
         act.documents.storage.upload-invoice.result  → { "object_key": "invoices/order_uuid.pdf", "url": "https://..." }
     - error: None

     On failure:
     - result_payload: None or partial data
     - error: ActionError
       {
           code: "provider_timeout" | "invalid_recipient" | "insufficient_funds" | "rate_limited" | ...,
           message: "Human-readable description of what went wrong",
           retryable: bool   // true → Flow can safely retry; false → needs compensation
       }

   Note: ActionResultEvent, ActionTrigger, ActionError, and MessageMeta are FRAMEWORK types
   provided by the platform. They are NOT defined in the project's Schema unit. Only the inner
   payload/result_payload fields carry project-specific types from Schema.

   - Mode:       ASYNC — JetStream publish to ACTIONS stream (durable, at-least-once delivery, persisted in stream until consumed and acked)
   - Consumed by: Flow (to advance/compensate saga), Projection (to update side-effect status views)

### External Calls (Sync, Outside the Bus)

   Action handlers make synchronous HTTP/gRPC calls to external services:
   - Payment providers (Stripe, PayPal, etc.)
   - Email services (SES, SendGrid, etc.)
   - Push notification services (APNs, FCM)
   - Object storage (S3, GCS)
   - Messaging platforms (Slack, Discord webhooks)
   - Any third-party API

   These calls are SYNC within the action handler (await the HTTP response) but the overall
   action processing is ASYNC from the system's perspective (triggered by bus message, result
   published as bus message).

### Auto-Broadcast Events

   MANDATORY: Every action handler invocation MUST emit exactly one ActionResultEvent:
   - On success: ActionResultEvent with success=true and result_payload
   - On failure: ActionResultEvent with success=false and error details
   - On idempotency_key duplicate detection: ActionResultEvent with success=true and the
     previously stored result (or a "duplicate" indicator), NOT a re-execution

   This is critical because:
   - Flow depends on receiving the result to advance the saga
   - Projection depends on it to show side-effect status in read models
   - Silent failures cause sagas to hang indefinitely

### Listens To

   - {project_uuid}.act.{context}.{module}.{unit_name_slug}  — action triggers from Flow
   - {project_uuid}.evt.{context}.{module}.{unit_name_slug}  — domain events (for standalone direct-trigger actions)

### Interactions With Each Unit

   Action ← Flow:        receives ActionTrigger (saga-orchestrated side effects)
   Action ← Command:     (indirect) receives EventEnvelope if using direct event subscription mode
   Action → Flow:        publishes ActionResultEvent (saga continuation/compensation signal)
   Action → Projection:  publishes ActionResultEvent (for side-effect status views)
   Action → Schema:      [LOCAL] compiled-in type definitions — action job body types, result payload types, error types

### Communication Rules

- Action MUST emit an ActionResultEvent for EVERY trigger processed. No silent completions.
- Action MUST NOT access application databases. It calls external services only.
- Action MUST NOT publish commands or domain events. It only publishes ActionResultEvents.
  If the action outcome needs to trigger further domain logic, Flow handles that by
  listening to the result event and publishing the appropriate command.
- Action MUST use idempotency_key to prevent duplicate side effects on JetStream redelivery.
  Before executing the external call, check if this idempotency_key was already processed.
- Action MUST NOT import or call Query, Projection, Command, Timer, or Gateway.
- External call timeouts and retries are Action's responsibility. Flow sees only the final result.
- Action handlers should be stateless (aside from idempotency dedup). All context comes from
  the trigger payload."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Action unit instance MUST export a `metadata` object so that Flow sagas and Projection
can discover which side effects are available, how to trigger them, and what result payloads to expect.

### Metadata Shape

{
  "unit_type": "action",
  "handlers": [
    {
      "id":                   "<unique handler slug, e.g. send_confirmation_email>",
      "trigger_subject":      "<NATS action subject, e.g. act.notifications.email.send-confirmation>",
      "input_payload_type":   "<Schema type name for trigger body, e.g. SendConfirmationEmailPayload>",
      "description":          "<what this action does in the outside world>",

      "result_subject":       "<NATS action result subject, e.g. act.notifications.email.send-confirmation.result>",
      "success_result_type":  "<Schema type name for success result, e.g. SendConfirmationEmailResult>",

      "failure_codes": [
        {
          "code":        "<machine-readable error code>",
          "retryable":   <bool>,
          "description": "<when this failure occurs>"
        }
      ],

      "external_service": {
        "name":        "<service name, e.g. SES, Stripe, APNs>",
        "type":        "<service category: email | payment | push_notification | storage | webhook | http_api>",
        "description": "<what external system is called>"
      },

      "idempotency_strategy": "<how duplicate triggers are handled, e.g. 'dedup by idempotency_key in action_dedup table'>",

      "direct_event_subscriptions": [
        {
          "event_subject": "<NATS event subject if this action subscribes directly to domain events>",
          "payload_type":  "<Schema type name>",
          "description":   "<why this action subscribes directly instead of via Flow>"
        }
      ]
    }
  ]
}

### Example

{
  "unit_type": "action",
  "handlers": [
    {
      "id": "send_confirmation_email",
      "trigger_subject": "act.notifications.email.send-confirmation",
      "input_payload_type": "SendConfirmationEmailPayload",
      "description": "Sends an order confirmation email via SES",
      "result_subject": "act.notifications.email.send-confirmation.result",
      "success_result_type": "SendConfirmationEmailResult",
      "failure_codes": [
        { "code": "invalid_recipient",  "retryable": false, "description": "Email address is invalid or bounced" },
        { "code": "provider_timeout",   "retryable": true,  "description": "SES did not respond within timeout" },
        { "code": "rate_limited",       "retryable": true,  "description": "SES rate limit exceeded, try later" },
        { "code": "template_not_found", "retryable": false, "description": "Email template ID does not exist" }
      ],
      "external_service": {
        "name": "Amazon SES",
        "type": "email",
        "description": "AWS Simple Email Service for transactional emails"
      },
      "idempotency_strategy": "dedup by idempotency_key in action_dedup table; if key exists, return stored result",
      "direct_event_subscriptions": []
    },
    {
      "id": "charge_payment",
      "trigger_subject": "act.billing.stripe.charge-card",
      "input_payload_type": "ChargePaymentPayload",
      "description": "Charges a customer's payment method via Stripe",
      "result_subject": "act.billing.stripe.charge-card.result",
      "success_result_type": "ChargePaymentResult",
      "failure_codes": [
        { "code": "insufficient_funds", "retryable": false, "description": "Card has insufficient funds" },
        { "code": "card_declined",      "retryable": false, "description": "Card was declined by the issuer" },
        { "code": "provider_error",     "retryable": true,  "description": "Stripe API returned a transient error" }
      ],
      "external_service": {
        "name": "Stripe",
        "type": "payment",
        "description": "Stripe payment processing"
      },
      "idempotency_strategy": "Stripe idempotency key passed to charge API; dedup in action_dedup table",
      "direct_event_subscriptions": []
    },
    {
      "id": "send_welcome_email",
      "trigger_subject": "act.notifications.email.send-welcome",
      "input_payload_type": "SendWelcomeEmailPayload",
      "description": "Sends a welcome email when a new user registers — subscribed directly to user.created",
      "result_subject": "act.notifications.email.send-welcome.result",
      "success_result_type": "SendWelcomeEmailResult",
      "failure_codes": [
        { "code": "invalid_recipient", "retryable": false, "description": "Email address is invalid" },
        { "code": "provider_timeout",  "retryable": true,  "description": "SES did not respond" }
      ],
      "external_service": {
        "name": "Amazon SES",
        "type": "email",
        "description": "AWS Simple Email Service"
      },
      "idempotency_strategy": "dedup by idempotency_key in action_dedup table",
      "direct_event_subscriptions": [
        {
          "event_subject": "evt.core.users.create-user",
          "payload_type": "UserCreatedEvent",
          "description": "Standalone action: always send welcome email when a user is created, no saga needed"
        }
      ]
    }
  ]
}

### How Other Units Use This Metadata

- Flow unit:       reads "trigger_subject" + "input_payload_type" to know how to publish ActionTriggers.
                   Reads "result_subject" + "success_result_type" to subscribe to results.
                   Reads "failure_codes" to decide retry vs compensate logic.
- Projection unit: reads "result_subject" + "success_result_type" to materialize side-effect status.
- Schema unit:     reads "input_payload_type", "success_result_type" to ensure types are defined in
                   action_triggers and action_results sections.
- Command unit:    generally does NOT read Action metadata (Command emits events; Flow triggers actions).

### Rules

- Each handler MUST have a unique "id" and unique "trigger_subject".
- "trigger_subject" MUST follow: act.{context}.{module}.{unit_name_slug}
- "result_subject" MUST follow: act.{context}.{module}.{unit_name_slug}.result (same prefix + ".result")
- Every handler MUST declare "failure_codes" with "retryable" flags so Flow knows what to do.
- "direct_event_subscriptions" is for standalone fire-and-forget actions only. For saga-coordinated
  actions, the trigger should come from Flow via "trigger_subject".
- "external_service" MUST be declared so developers know what credentials/config are needed."#;
