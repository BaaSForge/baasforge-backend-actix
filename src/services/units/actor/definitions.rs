pub const TITLE: &str = "Actor";

pub const BRIEF: &str = r#"
An actor is a stateful entity identified by an ID. Each instance holds its own persistent
state and processes one operation at a time. Use actors to model things in your domain that
have identity, state that changes over time, and multiple operations — user profiles, shopping
carts, game sessions, device state, account balances, and anything similar.
"#;

pub const DETAIL: &str = r#"
An actor unit must have a default export produced by the `actor()` function:

  export default actor<TState>({ state, handlers, metadata?, onAlarm?, onDeadLetter? });

`state` — the initial state value for new actor instances. It is deep-cloned on first
activation. Mutate `ctx.state` directly inside handlers; the updated state is persisted
atomically after each successful handler execution.

`handlers` — a record of named handler functions. Each receives (ctx, payload?):
- ctx.id: string — the actor instance ID (supplied by the caller)
- ctx.attempt: number — current attempt number, 1-based; increments on each retry
- ctx.state: TState — mutable reference to this instance's persisted state
- ctx.setAlarm(name: string, at: Date | number | Duration) — schedule a named alarm
- ctx.cancelAlarm(name: string) — cancel a pending alarm by name
- ctx.TerminalError — throw an instance to skip retries and trigger onDeadLetter

Handler return values are serialized and sent back to the caller.

Error handling:
- Throw any regular Error → handler is retried (ctx.attempt increments); state changes are
  rolled back to the pre-handler snapshot
- Throw `new ctx.TerminalError(msg)` → skips remaining retries, triggers onDeadLetter

`onAlarm(ctx, name)` — fired when a scheduled alarm's time arrives. `name` is the string
passed to `ctx.setAlarm()`. Alarms can read and mutate `ctx.state` like normal handlers.

`onDeadLetter(ctx, handler, payload, error)` — called after max retries are exhausted or
on TerminalError. Must not throw. Use it to log, alert, or compensate.

`metadata` (static config, all fields optional):
- retry.maxAttempts: number — max total attempts including the first; default 3
- retry.initialInterval: Duration — wait before first retry; default { seconds: 1 }
- retry.backoffFactor: number — exponential backoff multiplier; default 2
- retry.maxInterval: Duration — cap on retry interval; default { minutes: 5 }
- timeout: number — handler execution timeout in milliseconds
- maxIdleTime: string — duration string after which an idle actor is passivated (e.g. "1h")
- stateLimit: string — maximum persisted state size (e.g. "512kb", "1mb"); enforced at write
- mailboxBacklog: number — max queued handler calls before back-pressure
- handlers: string[] — declare the list of valid handler names (for validation)
- alarms: string[] — declare the list of valid alarm names (for validation)

Duration object: { milliseconds?, seconds?, minutes?, hours?, days? }

Key guarantees:
- State isolation: ctx.state for instance "alice" is completely independent from "bob"
- Sequential delivery: only one handler runs at a time per instance ID; no interleaving
- Atomic state: state is only updated if the handler succeeds; retries always see the
  pre-handler state
- Durable alarms: alarms survive crashes and restarts

Exported types (MANDATORY — used for automatic documentation generation and cross-unit type safety):

- export type State = { ... }               — the actor state shape (REQUIRED)
- export type {HandlerName}Input = { ... }  — payload type for each handler that takes input (REQUIRED per handler)
- export type {HandlerName}Output = { ... } — return type for each handler that returns a value (REQUIRED per handler)

Naming convention: PascalCase handler name + "Input" / "Output" suffix.
Example: handler "increment" → IncrementInput, IncrementOutput.
"#;

pub const CODE_EXAMPLES: &str = r#"
import { actor, sdk } from "@sdk";
import type { Input as NotificationInput } from "@units/my-ctx.my-module.queue_worker.send-notification";

export type State = {
  count: number;
  lastSeen: number | null;
  alarmFired: boolean;
};

export type IncrementInput = { by?: number };
export type IncrementOutput = { count: number };
export type ResetOutput = { ok: boolean };
export type NotifyInput = { message: string };
export type NotifyOutput = { ok: boolean };

export default actor<State>({
  state: { count: 0, lastSeen: null, alarmFired: false },

  metadata: {
    retry: { maxAttempts: 3, initialInterval: { milliseconds: 500 }, backoffFactor: 2 },
    maxIdleTime: "1h",
    stateLimit: "512kb",
    timeout: 10000,
    handlers: ["increment", "reset", "notify"],
    alarms: ["idle-check"],
  },

  handlers: {
    increment(ctx, payload: IncrementInput): IncrementOutput {
      ctx.state.count += payload?.by ?? 1;
      ctx.state.lastSeen = Date.now();
      ctx.setAlarm("idle-check", { minutes: 30 });
      return { count: ctx.state.count };
    },

    reset(ctx): ResetOutput {
      ctx.state.count = 0;
      ctx.cancelAlarm("idle-check");
      return { ok: true };
    },

    async notify(ctx, payload: NotifyInput): Promise<NotifyOutput> {
      await sdk.enqueueWorker<NotificationInput>(
        "my-ctx.my-module.queue_worker.send-notification",
        { actorId: ctx.id, message: payload.message },
      );
      return { ok: true };
    },
  },

  onAlarm(ctx, name: string): void {
    if (name === "idle-check") {
      ctx.state.alarmFired = true;
    }
  },

  onDeadLetter(ctx, handler: string, payload: unknown, error: Error): void {
    // must not throw
  },
});
"#;

pub const COMMUNICATION: &str = r#"
Actors are called via the SDK. The function to use depends on the caller's unit type.

From API, actor, or queue_worker units — sdk.callActor is available:

  import type { IncrementInput, IncrementOutput } from "@units/ctx.mod.actor.name";

  // Wait for result (default)
  const result = await sdk.callActor<IncrementInput, IncrementOutput>(
    "ctx.mod.actor.name", "actor-instance-id", "increment", { by: 1 },
  );

  // Fire-and-forget
  await sdk.callActor<IncrementInput>(
    "ctx.mod.actor.name", "actor-instance-id", "increment", { by: 1 }, { awaitResult: false },
  );

  // With a custom timeout (ms)
  const result = await sdk.callActor<IncrementInput, IncrementOutput>(
    "ctx.mod.actor.name", "actor-instance-id", "increment", { by: 1 }, { timeoutMs: 5000 },
  );

From workflow units — only sdk.callActorDurable is available (sdk.callActor does not exist
in workflow units). It suspends the workflow durably until the handler completes, and returns
the cached result on replay:

  const result = await sdk.callActorDurable<IncrementInput, IncrementOutput>(
    "ctx.mod.actor.name", "actor-instance-id", "increment", { by: 1 },
  );

The actorId is any string chosen by the caller. Same unit path + actorId always reaches the
same state. Different actorIds are fully isolated instances.
"#;

pub const METADATA: &str = r#"
"#;