pub const TITLE: &str = "Flow";

pub const BRIEF: &str = r#"Flow implements long-running processes: sagas that listen for events, remember process state, schedule follow-up work, call compensations on failure, and sometimes trigger side effects via other units such as Action."#;

pub const DETAIL: &str = r#"What it is for: Flow coordinates multi-step business processes that unfold over time—reservations that expire, payments that may fail, notifications that should fire after certain facts, and cleanup that spans several events.

How others use it: Commands and other producers emit events; Flow subscribes and advances orchestration. Timer units may emit ticks that Flow handles. Action units perform external side effects when Flow requests them indirectly via bus subjects.

How it uses others: Flow uses flow context (including stream and storage helpers as designed), reads and writes saga or index state distinct from canonical entity tables, consumes schema event types, and may publish commands or action triggers.

Rules and restrictions: Flow holds process state, not the authoritative lifecycle of a single aggregate—that remains event plus projection territory. Compensations and timeouts should be explicit to avoid stuck processes. Keep orchestration readable; avoid hiding business decisions that belong in Command.

What it exports: Event- and timer-driven handlers that advance or roll back coordinated workflows.

Real-world examples: Reserve inventory then capture payment within a timeout, releasing stock if payment fails; notify dashboards when orders change; nightly purge of expired sessions driven by timer ticks; trip booking saga across flights, hotels, and insurance in a travel backend."#;

pub const COMMUNICATION: &str = r#"## Inter-Unit Communication

Flow is the most complex communicator. It subscribes to domain events, action results, and timer ticks, then publishes commands and action triggers to orchestrate multi-step processes. It maintains its own saga state separate from projection tables.

### Subscribes (Inbound)

1. Domain Events — ASYNC durable consumer
   - Subject:    {project_uuid}.evt.>  or specific subjects like {project_uuid}.evt.shop.orders.place-order
   - Payload:    EventEnvelope
   - Consumer:   JetStream durable consumer, consumer name "flow-events"
                 Global shared consumer across ALL projects and all sagas.
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     Command unit (domain events)
   - Purpose:    Trigger new saga instances, advance existing saga steps, or start compensations
   - Handling:   Flow handler deserializes the event, looks up saga state by aggregate_id or
                 correlation_id, and decides the next step.

2. Action Result Events — ASYNC durable consumer
   - Subject:    {project_uuid}.act.{context}.{module}.{unit_name_slug}.result
   - Payload:    ActionResultEvent
   - Consumer:   JetStream durable consumer, consumer name "flow-action-results"
                 Global shared consumer across ALL projects.
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     Action unit
   - Purpose:    Advance saga after a side effect completes (success path) or trigger compensation
                 (failure path). The ActionResultEvent.trigger_id links back to the original trigger.
   - Handling:
     On success: advance saga to next step, publish next command or action trigger
     On failure (retryable): re-publish the ActionTrigger with incremented retry count
     On failure (non-retryable): publish compensation commands to roll back prior steps

3. Timer Ticks — ASYNC durable consumer
   - Subject:    {project_uuid}.tmr.{context}.{module}.{unit_name_slug}.tick
   - Payload:    TimerTick
   - Consumer:   JetStream durable consumer, consumer name "flow-timer-ticks"
                 Global shared consumer across ALL projects.
   - Delivery:   at-least-once delivery, explicit ack after processing, redelivered on ack timeout or nack
   - Source:     Timer unit
   - Purpose:    Handle timeouts, deadlines, and scheduled saga steps
   - Handling:   Check saga state for instances that have exceeded their deadline.
                 Publish timeout commands or compensation actions for stuck sagas.

4. Rejection Events — ASYNC (same consumer as domain events)
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}.rejected
   - Payload:    EventEnvelope (with rejection payload)
   - Source:     Command unit
   - Purpose:    Handle command rejections within saga flows (e.g. payment command rejected
                 → release inventory reservation)

### Publishes (Outbound)

1. Follow-Up Commands — ASYNC publish
   - Subject:    {project_uuid}.cmd.{context}.{module}.{unit_name_slug}
   - Payload:    CommandEnvelope
     - meta.source_unit: "flow"
     - meta.correlation_id: propagated from the saga's originating correlation_id
     - meta.causation_id: message_id of the event that triggered this saga step
     - actor_id: "system" or propagated from the original actor
     - payload: project-specific command payload (from Schema unit) for the next step
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: Command unit
   - Examples:
     After evt.shop.orders.place-order → publish cmd.billing.payments.capture
     After evt.billing.payments.capture → publish cmd.shop.orders.confirm-order
     After evt.billing.payments.capture.rejected → publish cmd.shop.inventory.release (compensation)

2. Action Triggers — ASYNC publish
   - Subject:    {project_uuid}.act.{context}.{module}.{unit_name_slug}
   - Payload:    ActionTrigger
     - meta.source_unit: "flow"
     - meta.correlation_id: propagated from saga
     - meta.causation_id: message_id of the event that triggered this step
     - idempotency_key: derived from saga_id + step_number + attempt to prevent duplicate side effects
     - payload: project-specific action trigger payload (from Schema unit)
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: Action unit
   - Examples:
     After evt.shop.orders.confirm-order → publish act.notifications.email.send-confirmation
     After evt.auth.users.create-user → publish act.notifications.email.send-welcome
     After timeout tick → publish act.notifications.alerts.send-reminder

3. Saga-State Events (optional, for observability) — ASYNC publish
   - Subject:    {project_uuid}.evt.{context}.{module}.{unit_name_slug}.{transition}
                 (e.g. f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.fulfillment.order-flow.step-completed,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.fulfillment.order-flow.compensating,
                       f47ac10b-58cc-4372-a567-0e02b2c3d479.evt.shop.fulfillment.order-flow.completed)
   - Payload:    EventEnvelope
     - payload: {
         saga_id:      String,
         saga_name:    String,
         from_step:    String,
         to_step:      String,
         trigger_event: String,    // subject of the event that caused the transition
       }
   - Mode:       ASYNC — JetStream publish — durable, at-least-once delivery, persisted in stream until consumed and acked
   - Consumed by: Projection (for saga monitoring dashboards), other Flows (for chained orchestration)
   - Note:       These are optional observability events. Not all saga transitions need to emit them.

### Reads/Writes (Saga State)

   Flow maintains its own saga state store, SEPARATE from Projection read-model tables:
   - Saga State Record:
     {
         saga_id:         String,        // unique id for this saga instance
         saga_name:       String,        // type of saga (e.g. "order_fulfillment")
         correlation_id:  String,        // links to the originating request chain
         current_step:    String,        // current step name in the saga
         status:          String,        // "running", "compensating", "completed", "failed"
         step_data:       Value,         // accumulated data from prior steps
         started_at:      i64,
         updated_at:      i64,
         deadline_at:     Option<i64>,   // when this saga should timeout
         retry_count:     u32,
     }
   - Flow reads saga state to determine the current step and decide what to do next.
   - Flow writes saga state after publishing the next message, recording the transition.

### Auto-Broadcast Events

   Flow does NOT auto-broadcast on every invocation. However:
   - On saga completion (final step reached): MAY emit {project_uuid}.evt.{context}.{module}.{unit_name_slug}.completed
   - On saga failure (non-retryable or max retries exceeded): MAY emit {project_uuid}.evt.{context}.{module}.{unit_name_slug}.failed
   - On compensation start: MAY emit {project_uuid}.evt.{context}.{module}.{unit_name_slug}.compensating
   These are opt-in per saga design.

### Listens To (summarized)

   - {project_uuid}.evt.{context}.{module}.{unit_name_slug}          — domain events from Command
   - {project_uuid}.evt.{context}.{module}.{unit_name_slug}.rejected  — rejection events from Command
   - {project_uuid}.act.{context}.{module}.{unit_name_slug}.result    — action results from Action
   - {project_uuid}.tmr.{context}.{module}.{unit_name_slug}.tick      — timer ticks from Timer

### Interactions With Each Unit

   Flow ← Command:      receives EventEnvelope / rejection events (triggers and advances sagas)
   Flow ← Action:       receives ActionResultEvent (side-effect outcomes for saga continuation)
   Flow ← Timer:        receives TimerTick (timeout checks and scheduled saga steps)
   Flow → Command:      publishes CommandEnvelope (follow-up commands in saga steps)
   Flow → Action:       publishes ActionTrigger (requests side effects)
   Flow → Projection:   (indirect) emits saga-state events that Projection may materialize
   Flow → Schema:       [LOCAL] compiled-in type definitions — event body types, command body types, action trigger types

### Communication Rules

- Flow MUST propagate correlation_id through the entire saga chain. Every command or action trigger
  published by Flow carries the correlation_id from the saga's originating event.
- Flow MUST NOT call Query directly. If saga logic needs current state, it should maintain its own
  step_data or subscribe to the relevant events that carry the needed information.
- Flow MUST NOT write to Projection tables. Saga state is stored in Flow-owned tables/documents.
- Flow MUST implement compensation logic for every step that has external side effects.
  If step 3 fails, steps 1 and 2 must have defined rollback commands.
- Flow MUST handle timeouts explicitly. Use Timer ticks + deadline_at to detect stuck sagas.
- Action triggers from Flow MUST include an idempotency_key derived from saga_id + step + attempt
  so that JetStream redelivery does not cause duplicate side effects.
- Flow MUST NOT create infinite loops. A saga step should not publish a command that causes an event
  that re-triggers the same saga step without an explicit termination condition.
- CommandEnvelope, EventEnvelope, ActionTrigger, ActionResultEvent, and TimerTick are framework types
  (not project-defined). The inner payload field carries project-specific types from Schema."#;

pub const METADATA: &str = r#"## Unit Instance Metadata

Every Flow unit instance MUST export a `metadata` object so that the LLM and other units can discover
which sagas exist, what events they react to, what commands and action triggers they publish,
and how their state machines are wired. This is the most structurally complex metadata.

### Metadata Shape

{
  "unit_type": "flow",
  "sagas": [
    {
      "saga_name":   "<unique saga identifier, e.g. order_fulfillment>",
      "description": "<what this saga orchestrates>",

      "state_shape": {
        "<field_name>": { "type": "<type>", "description": "<purpose>" }
      },

      "steps": [
        {
          "name":    "<step identifier, e.g. pending_payment>",
          "status":  "<running | compensating | terminal>",
          "description": "<what this step represents in the process>"
        }
      ],

      "subscribes_to_events": [
        {
          "event_subject": "<NATS event subject, e.g. evt.shop.orders.place-order>",
          "payload_type":  "<Schema type name>",
          "triggers":      "<what this event does: 'start' | 'advance:{from_step}→{to_step}' | 'compensate:{step}'>"
        }
      ],

      "subscribes_to_action_results": [
        {
          "result_subject": "<NATS action result subject, e.g. act.notifications.email.send-confirmation.result>",
          "advances":       "<what happens: 'advance:{from_step}→{to_step}' | 'retry_or_compensate:{step}'>"
        }
      ],

      "subscribes_to_timer_ticks": [
        {
          "tick_subject": "<NATS timer tick subject, e.g. tmr.shop.timeouts.order-expiry.tick>",
          "handles":      "<what the tick does: 'timeout_check:{step}' | 'scheduled_action:{description}'>"
        }
      ],

      "publishes_commands": [
        {
          "command_subject": "<NATS command subject, e.g. cmd.billing.payments.capture>",
          "payload_type":    "<Schema type name>",
          "from_step":       "<which saga step publishes this>",
          "description":     "<why this command is published>"
        }
      ],

      "publishes_action_triggers": [
        {
          "trigger_subject": "<NATS action subject, e.g. act.notifications.email.send-confirmation>",
          "payload_type":    "<Schema type name>",
          "from_step":       "<which saga step triggers this>",
          "description":     "<why this action is triggered>"
        }
      ],

      "compensation_map": [
        {
          "failed_step":          "<step that failed>",
          "compensation_action":  "<what to do: 'publish_command:{subject}' | 'publish_action:{subject}' | 'mark_failed'>",
          "description":          "<business reason for this compensation>"
        }
      ]
    }
  ]
}

### Example

{
  "unit_type": "flow",
  "sagas": [
    {
      "saga_name": "order_fulfillment",
      "description": "Orchestrates order placement → payment → confirmation email → completion",

      "state_shape": {
        "order_id":     { "type": "string",        "description": "UUID of the order being fulfilled" },
        "customer_id":  { "type": "string",        "description": "UUID of the customer" },
        "payment_id":   { "type": "string | null",  "description": "Payment charge ID once captured" },
        "email_sent":   { "type": "boolean",        "description": "Whether confirmation email was delivered" },
        "items":        { "type": "OrderItem[]",    "description": "Line items from the original order" }
      },

      "steps": [
        { "name": "started",            "status": "running",      "description": "Order placed, awaiting payment" },
        { "name": "payment_pending",    "status": "running",      "description": "Payment capture command sent" },
        { "name": "confirmed",          "status": "running",      "description": "Payment captured, sending email" },
        { "name": "notification_pending", "status": "running",    "description": "Confirmation email triggered" },
        { "name": "completed",          "status": "terminal",     "description": "All steps done" },
        { "name": "compensation",       "status": "compensating", "description": "Rolling back due to failure" },
        { "name": "failed",             "status": "terminal",     "description": "Saga failed after compensation" }
      ],

      "subscribes_to_events": [
        {
          "event_subject": "evt.shop.orders.place-order",
          "payload_type": "OrderPlacedEvent",
          "triggers": "start"
        },
        {
          "event_subject": "evt.billing.payments.capture",
          "payload_type": "PaymentCapturedEvent",
          "triggers": "advance:payment_pending→confirmed"
        },
        {
          "event_subject": "evt.billing.payments.capture.rejected",
          "payload_type": "PaymentCaptureRejectedEvent",
          "triggers": "compensate:payment_pending"
        }
      ],

      "subscribes_to_action_results": [
        {
          "result_subject": "act.notifications.email.send-confirmation.result",
          "advances": "advance:notification_pending→completed (on success) | retry_or_compensate:notification_pending (on failure)"
        }
      ],

      "subscribes_to_timer_ticks": [
        {
          "tick_subject": "tmr.shop.timeouts.order-expiry.tick",
          "handles": "timeout_check:payment_pending — if payment_pending for >30min, trigger compensation"
        }
      ],

      "publishes_commands": [
        {
          "command_subject": "cmd.billing.payments.capture",
          "payload_type": "CapturePaymentPayload",
          "from_step": "started",
          "description": "Capture payment for the placed order"
        },
        {
          "command_subject": "cmd.shop.inventory.release",
          "payload_type": "ReleaseInventoryPayload",
          "from_step": "compensation",
          "description": "Release reserved inventory on payment failure"
        }
      ],

      "publishes_action_triggers": [
        {
          "trigger_subject": "act.notifications.email.send-confirmation",
          "payload_type": "SendConfirmationEmailPayload",
          "from_step": "confirmed",
          "description": "Send order confirmation email to customer"
        }
      ],

      "compensation_map": [
        {
          "failed_step": "payment_pending",
          "compensation_action": "publish_command:cmd.shop.inventory.release",
          "description": "Payment failed or timed out — release inventory reservation"
        },
        {
          "failed_step": "notification_pending",
          "compensation_action": "mark_failed",
          "description": "Email delivery failed permanently — mark saga as failed (order still valid)"
        }
      ]
    }
  ]
}

### How Other Units Use This Metadata

- Command unit:  reads "publishes_commands[].command_subject" to confirm it has handlers for all
                 commands Flow will publish. Reads "subscribes_to_events[].event_subject" to confirm
                 the events it emits will be consumed.
- Action unit:   reads "publishes_action_triggers[].trigger_subject" to confirm it has handlers for
                 all action triggers Flow will publish.
- Timer unit:    reads "subscribes_to_timer_ticks[].tick_subject" to confirm it emits the ticks
                 Flow expects.
- Projection:    reads "subscribes_to_events" to understand which events are also consumed by Flow
                 (events are fan-out, so both Projection and Flow receive them).
- Schema unit:   reads all type names to ensure payload types are defined.

### Rules

- Each saga MUST have a unique "saga_name".
- "steps" MUST include at least one "terminal" step (completed or failed).
- Every event in "subscribes_to_events" MUST correspond to a declared event in a Command or Action unit.
- Every subject in "publishes_commands" MUST correspond to a declared handler in a Command unit.
- Every subject in "publishes_action_triggers" MUST correspond to a declared handler in an Action unit.
- "compensation_map" MUST cover every non-terminal step that has destructive side effects.
- "state_shape" MUST list all fields the saga accumulates across steps."#;
