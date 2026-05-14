pub const TITLE: &str = "Workflow";

pub const BRIEF: &str = r#"
A workflow is a long-running process that survives crashes and restarts. It executes a
sequence of steps durably — if something fails mid-way, it resumes from where it left off
rather than starting over. Use workflows for multi-step business processes, operations that
span minutes to days, sequences that wait for external events or human input, and anything
that must complete reliably regardless of failures.
"#;

pub const DETAIL: &str = r#"
A workflow unit must have a default export produced by the `workflow()` function:

  export default workflow(async (ctx, input) => { ... });

  // or with shared handlers:
  export default workflow({ run, handlers? });

`run(ctx, input)` — the main workflow function. It must be deterministic on replay: all
side effects and I/O must be inside `ctx.run()` steps. Code outside steps re-executes on
every replay.

ctx.run(name, fn, opts?) — durable step. RETURNS A PROMISE; use `await ctx.run(...)`.
  - fn executes exactly once; on replay, returns the cached result without calling fn again
  - name must be unique across all steps in this workflow instance
  - opts.retryPolicy: RetryPolicy — step-level retry on fn failure
  - All side effects (HTTP calls, actor calls, enqueue) must be inside ctx.run()
  - `ctx.run` is the ONLY ctx method that needs `await`.

ctx.sleep(name, duration) — durable timer. SYNCHRONOUS; no await.
  - Suspends the workflow until the duration elapses (via an internal sentinel throw)
  - Survives crashes, restarts, and deployments
  - duration: Duration object { milliseconds?, seconds?, minutes?, hours?, days? } or number (ms)
  - name must be unique

ctx.promise(name) — durable promise. Returns a WorkflowDurablePromise:
  - .get(opts?) — SYNCHRONOUS; suspends the workflow until resolved/rejected; returns
    null on timeout. opts.timeout: Duration. No `await`; let its internal throw propagate.
  - .resolve(value) — SYNCHRONOUS; resolve the promise (from a ctx.run step or a shared handler)
  - .reject(error: string) — SYNCHRONOUS; reject the promise
  - name must be unique

ctx.set(key, value) — SYNCHRONOUS; persist arbitrary key-value state
ctx.get(key) — SYNCHRONOUS; read persisted state (returns T | null)

ctx.rand.uuidv4() / ctx.rand.random() — deterministic random seeded by instance ID + step
  position; produces the same values on replay. Use instead of Math.random().
ctx.date.now() — deterministic current timestamp captured at step boundary; use instead of
  Date.now() inside workflow logic.

`handlers` — optional named functions invoked EXTERNALLY (from another unit via
`sdk.callWorkflowHandlerAndWaitForResultSync`) while the workflow is running. They are
NOT triggered by `sdk.startWorkflowSync` / `callWorkflowAndWaitForOutputSync`, and they
do NOT auto-run when the workflow starts. A handler's only jobs are to read state and
resolve/reject a durable promise that the `run` function is waiting on. Handlers
receive WorkflowSharedContext:
- ctx.key: WorkflowInstanceId (from `@sdk`) — instance ID
- ctx.promise(name) — returns WorkflowDurablePromiseHandle: { resolve(value), reject(error) } — synchronous
- ctx.get(key) — synchronous; read workflow state

CRITICAL rules:
- Two categories of calls inside `run` — never mix them:
  SUSPENSION PRIMITIVES — call bare in the run body, never inside ctx.run():
    ctx.sleep(...)
    ctx.promise(...).get()
    sdk.callWorkflowAndWaitForOutputSync({ unitPath, input, opts? })
  They throw an internal sentinel that must propagate freely. Wrapping in ctx.run()
  swallows the sentinel and breaks the suspension mechanism.
  SIDE EFFECTS — wrap every one inside `await ctx.run("step", () => ...)`:
    sdk.callActorAndWaitForResultSync({ unitPath, actorId, handler, payload?, opts? })
    sdk.enqueueWorkerSync({ unitPath, data, opts? })
    sdk.startWorkflowSync({ unitPath, input })
    sdk.sendActorMessageSync({ unitPath, actorId, handler, payload? })
  Anything outside ctx.run() re-executes on every replay.
- Step names (ctx.run, ctx.sleep, ctx.promise) must be unique within the workflow instance
- Never use Date.now() or Math.random() directly — use ctx.date.now() and ctx.rand.*
- Do not catch or swallow exceptions from suspension primitives — let them propagate
- TerminalError thrown inside ctx.run() permanently fails that step (and the workflow)

RetryPolicy: { maxAttempts?, initialInterval?: Duration, backoffFactor?, maxInterval?: Duration }
Duration: { milliseconds?, seconds?, minutes?, hours?, days? }

Exports (MANDATORY — used for automatic documentation generation and cross-unit type safety):

- export type Input = { ... }               — the workflow start payload (REQUIRED)
- export type Output = { ... }              — the workflow completion result (REQUIRED)
- export type {HandlerName}Input = { ... }  — input type of each shared handler that takes input
- export type {HandlerName}Output = { ... } — return type of each shared handler that returns a value (REQUIRED per handler)
- export default workflow(...)              — the workflow definition (REQUIRED)

Naming convention: PascalCase handler name + "Input" / "Output" suffix.
Example: handler "approve" → ApproveInput, ApproveOutput.
"#;

// IMPORTANT: This variable is auto generated by `gen-code-examples.cjs`, don't modify it manually
pub const CODE_EXAMPLES: &str = r#"
// basic/sequential-steps.ts
/**
 * @covers basic `workflow<Input, Output>(async (ctx, input) => {...})` function
 * form, typed Input/Output exports, sequential `ctx.run` steps where each step consumes the
 * previous result.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = { seed: number };
export type Output = {
  first: number;
  second: number;
  third: number;
  chain: string;
};

/**
 * Chains ctx.run steps where each step consumes the previous step's return value.
 * Final output encodes the full pipeline for assertions.
 */
export default workflow<Input, Output>(async (ctx, input) => {
  void sdk;

  const first = await ctx.run("increment-seed", async () => input.seed + 1);
  const second = await ctx.run("double-first", async () => first * 2);
  const third = await ctx.run("add-seed-to-second", async () => second + input.seed);
  const chain = await ctx.run("format-chain", async () => `${first}:${second}:${third}`);

  return { first, second, third, chain };
});

// compensation/basic-compensation.ts
/**
 * @covers the compensation (saga-undo) pattern — registering undo
 * `ctx.run` closures as forward steps succeed, then executing them in reverse order inside
 * a `ctx.TerminalError` catch block
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = Record<string, never>;

export type Output = {
  /** Undo steps in actual execution order (reverse of successful forward registration) */
  compensationOrder: string[];
};

export default workflow(async (ctx, _input: Input): Promise<Output> => {
  void sdk;

  const compensations: Array<() => Promise<void>> = [];
  const compensationOrder: string[] = [];

  try {
    compensations.push(() =>
      ctx.run("undo-charge", async () => {
        compensationOrder.push("undo-charge");
      }),
    );
    await ctx.run("charge", async () => {
      ctx.set("charged", true);
    });

    compensations.push(() =>
      ctx.run("undo-reserve", async () => {
        compensationOrder.push("undo-reserve");
      }),
    );
    await ctx.run("reserve", async () => {
      ctx.set("reserved", true);
    });

    throw new ctx.TerminalError("abort-after-reserve");
  } catch (e) {
    if (e instanceof ctx.TerminalError) {
      for (const comp of [...compensations].reverse()) {
        await comp();
      }
      return { compensationOrder };
    }
    throw e;
  }
});

// endpoints/suspend-for-signal.ts
/**
 * @covers `workflow({ run, handlers })` object form with multiple typed
 * handlers, `ctx.set`/`ctx.get` for durable state, `ctx.promise<T>("name").get({ timeout })`
 * for suspension, `ctx.promise<T>("name").resolve()` from a shared handler, and reading state
 * from within a handler
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { phase: "done"; signal: unknown };
export type GetStateOutput = { phase: string | null; counter: number | null };
export type SignalInput = unknown;
export type SignalOutput = { accepted: boolean };

export default workflow({
  run: async (ctx, _input) => {
    ctx.set("phase", "waiting");
    ctx.set("counter", 42);
    const signal = await ctx.promise<unknown>("gate").get({ timeout: { hours: 24 } });
    return { phase: "done", signal };
  },
  handlers: {
    getState: async (ctx) => {
      return {
        phase: await ctx.get("phase"),
        counter: await ctx.get("counter"),
      };
    },
    signal: async (ctx, input?: any) => {
      await ctx.promise<unknown>("gate").resolve(input ?? "default");
      return { accepted: true };
    },
  },
});

// integration/sdk-call-basic.ts
/**
 * @covers `sdk.callWorkflowAndWaitForOutputSync<Input, Output>({ unitPath, input })`
 * to durably call another workflow unit and block until it returns a result.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { called: true; doubled: number };

export default workflow<Input, Output>(async (ctx) => {
  void ctx;
  const result = sdk.callWorkflowAndWaitForOutputSync<{ n: number }, { doubled: number }>({
    unitPath: "test.wf.workflow.sdk-call-callee",
    input: { n: 2 },
  });
  return { called: true, doubled: result.doubled };
});

// integration/sdk-start-workflow-fire-forget.ts
/**
 * @covers `sdk.startWorkflowSync<Input>({ unitPath, input })` for
 * fire-and-forget workflow launching (returns `{ instanceId }` immediately without waiting
 * for completion), and the `WorkflowInstanceId` type import.
 *
 */
import { workflow, sdk, type WorkflowInstanceId } from "@sdk";

export type Input = void;
export type Output = { instanceId: WorkflowInstanceId; nonEmpty: boolean };

/**
 * sdk.startWorkflowSync is fire-and-forget:
 * returns { instanceId } immediately without waiting for the callee to finish.
 * The callee workflow (sdk-call-callee) completes asynchronously; this
 * caller does not wait for it.
 */
export default workflow<Input, Output>(async (_ctx) => {
  const started = sdk.startWorkflowSync<{ n: number }>({
    unitPath: "test.wf.workflow.sdk-call-callee",
    input: { n: 9 },
  });
  return {
    instanceId: started.instanceId,
    nonEmpty: typeof started.instanceId === "string" && started.instanceId.length > 0,
  };
});

// lifecycle/workflow-deduplication.ts
/**
 * @covers `ctx.key` (the stable workflow instance identifier used for
 * deduplication), `ctx.rand.uuidv4()` for deterministic UUID generation inside a step, and
 * `ctx.date.now()` for deterministic timestamps inside a step.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = { salt?: string };
export type Output = {
  instanceKey: string;
  once: { nonce: string; at: number };
};

/**
 * Same instance_id (ctx.key) replays the same durable steps: the side-effect
 * step runs once; duplicate triggers attach to the same execution graph.
 */
export default workflow<Input, Output>(async (ctx, input) => {
  void sdk;

  const once = await ctx.run("idempotent-side-effect", async () => ({
    nonce: ctx.rand.uuidv4(),
    at: ctx.date.now(),
    salt: input.salt ?? null,
  }));

  return { instanceKey: ctx.key, once };
});

// parallel/promise-all.ts
/**
 * @covers parallel `ctx.run` steps scheduled via `Promise.all`, showing
 * that multiple durable steps can be fanned out in a single await.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = Record<string, never>;

export type Output = {
  /** Distinct results from two durable steps scheduled via Promise.all */
  a: { label: string; value: number };
  b: { label: string; value: number };
  sum: number;
};

export default workflow(async (ctx, _input: Input): Promise<Output> => {
  void sdk;

  const [a, b] = await Promise.all([
    ctx.run("branch-a", async () => {
      return { label: "a", value: 21 };
    }),
    ctx.run("branch-b", async () => {
      return { label: "b", value: 21 };
    }),
  ]);

  return { a, b, sum: a.value + b.value };
});

// promises/multiple-promises.ts
/**
 * @covers creating multiple independent named `ctx.promise<T>` handles,
 * resolving them with `.resolve()` before calling `.get()` (pre-resolve pattern), and
 * consuming resolved values.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { a: number | null; b: number | null; c: string | null; sum: number };

/**
 * Independent durable promises in one workflow; each is named and resolved
 * before its corresponding `.get()` (deterministic ordering).
 */
export default workflow(async (ctx) => {
  const pa = ctx.promise<number>("a");
  const pb = ctx.promise<number>("b");
  const pc = ctx.promise<string>("c");

  await pa.resolve(10);
  await pb.resolve(32);
  await pc.resolve("done");

  const a = await pa.get();
  const b = await pb.get();
  const c = await pc.get();

  return {
    a,
    b,
    c,
    sum: (a ?? 0) + (b ?? 0),
  };
});

// promises/promise-reject.ts
/**
 * @covers `ctx.promise<T>("name").reject(reason)` to reject a durable
 * promise, and catching the resulting error thrown by `.get()` with try/catch.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { ok: boolean; rejected?: boolean; message?: string; reason?: string };

/**
 * Rejection surfaces to the waiter (error propagates from `.get()`).
 */
export default workflow(async (ctx) => {
  await ctx.promise<number>("will-fail").reject("expected rejection");

  try {
    await ctx.promise<number>("will-fail").get();
    return { ok: false, reason: "get did not throw" };
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return {
      ok: true,
      rejected: true,
      message,
    };
  }
});

// promises/promise-timeout.ts
/**
 * @covers `ctx.promise<T>("name").get({ timeout: { milliseconds } })`
 * returning `null` when no resolver fires within the deadline — the canonical way to model
 * an optional/time-bounded external signal.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { timedOut: boolean; value: string | null };

/**
 * No resolver is invoked; `.get()` yields `null` after the timeout.
 */
export default workflow(async (ctx) => {
  const result = await ctx.promise<string>("never-resolved").get({
    timeout: { milliseconds: 150 },
  });

  return {
    timedOut: result === null,
    value: result,
  };
});

// retry/custom-retry-policy.ts
/**
 * @covers the `retryPolicy` option on `ctx.run` (`maxAttempts`,
 * `initialInterval`, `backoffFactor`, `maxInterval`), and reading/writing `ctx.get`/`ctx.set`
 * state inside a failing step closure to track attempt counts across retries.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = Record<string, never>;

export type Output = {
  ok: boolean;
  attempts: number;
};

// ctx.set inside a throwing step closure still persists across retries.
// The step reads the previous attempt count via ctx.get, increments it, writes it back
// with ctx.set, then throws — and on the next retry the incremented value is visible.
// This means state written inside a failing step is NOT rolled back on error; it is
// durable regardless of whether the step succeeded. The loop therefore reaches attempt 5
// and returns successfully.
export default workflow(async (ctx, _input: Input): Promise<Output> => {
  void sdk;

  const result = await ctx.run(
    "custom-backoff-step",
    async () => {
      const prev = Number((await ctx.get("policy-attempt")) ?? 0);
      const next = prev + 1;
      ctx.set("policy-attempt", next);
      if (next < 5) {
        throw new Error(`not yet: ${next}`);
      }
      return { ok: true, attempts: next };
    },
    {
      retryPolicy: {
        maxAttempts: 12,
        initialInterval: { milliseconds: 50 },
        backoffFactor: 3,
        maxInterval: { minutes: 1 },
      },
    },
  );

  return result;
});

// retry/terminal-error-no-retry.ts
/**
 * @covers `ctx.TerminalError` to throw a non-retriable error from inside
 * a step, catching it with `instanceof ctx.TerminalError` in the workflow body, and verifying
 * that no additional retries occur even when `retryPolicy.maxAttempts` is large.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = Record<string, never>;

export type Output = {
  /** stays 1 as TerminalError prevents step retries */
  terminalInvocations: number;
};

// ctx.set writes inside a step closure that throws TerminalError are still persisted —
// the same durable-write semantics apply regardless of the error type.
// TerminalError signals that the step should not be retried at all; the runtime
// propagates it immediately as a rejected promise without scheduling any further attempts.
// The invocation counter therefore equals exactly 1 after the catch: incremented once,
// never incremented again because no retry runs.
export default workflow(async (ctx, _input: Input): Promise<Output> => {
  void sdk;

  try {
    await ctx.run(
      "dies-once",
      async () => {
        const n = Number((await ctx.get("terminal-invocations")) ?? 0) + 1;
        ctx.set("terminal-invocations", n);
        throw new ctx.TerminalError("non-retriable");
      },
      { retryPolicy: { maxAttempts: 10 } },
    );
    return { terminalInvocations: -1 };
  } catch (e) {
    if (e instanceof ctx.TerminalError) {
      const terminalInvocations = Number((await ctx.get("terminal-invocations")) ?? 0);
      return { terminalInvocations };
    }
    throw e;
  }
});

// sleep/basic-sleep.ts
/**
 * @covers all `ctx.sleep` duration forms (raw millisecond number,
 * `{ milliseconds }`, `{ seconds }`, mixed `{ minutes, seconds, milliseconds }` object),
 * and `ctx.date.now()` inside a durable step for stable timestamps.
 *
 */
import { workflow, sdk } from "@sdk";

export type Input = void;
export type Output = { ok: boolean; startedAt: number; endedAt: number; slept: string[] };

export default workflow(async (ctx) => {
  const startedAt = await ctx.run("mark-start", async () => ctx.date.now());

  await ctx.sleep("sleep-ms-number", 50);
  await ctx.sleep("sleep-ms-obj", { milliseconds: 50 });
  await ctx.sleep("sleep-seconds", { seconds: 1 });
  await ctx.sleep("sleep-mixed", { minutes: 0, seconds: 0, milliseconds: 100 });

  const endedAt = await ctx.run("mark-end", async () => ctx.date.now());

  return {
    ok: true,
    startedAt,
    endedAt,
    slept: ["sleep-ms-number", "sleep-ms-obj", "sleep-seconds", "sleep-mixed"],
  };
});
"#;

pub const COMMUNICATION: &str = r#"
Inbound — another unit can interact with this workflow in two ways. All sdk.* calls are
synchronous; do NOT write `await` in front of them. Each call takes one object argument
(`{ unitPath, input }`, `{ unitPath, instanceId, handler, input?, opts? }`, …) per `@sdk`.
`unitPath` is `WorkflowUnitPathOrId` (four segments, third is `workflow`); `instanceId`
is `WorkflowInstanceId` (use `as WorkflowInstanceId` when the value is a plain `string`).

  1) Start a new instance:

     import type { Input, Output } from "@units/ctx.mod.workflow.name";

     // Fire-and-forget — returns { instanceId } immediately; this is the
     // recommended form whenever the workflow waits on a signal/promise.
     const { instanceId } = sdk.startWorkflowSync<Input>({
       unitPath: "ctx.mod.workflow.name",
       input: inputData,
     });

     // Start and BLOCK until the workflow terminates. Only safe for short,
     // non-signal workflows.
     const out = sdk.callWorkflowAndWaitForOutputSync<Input, Output>({
       unitPath: "ctx.mod.workflow.name",
       input: inputData,
     });

  2) Signal / query a RUNNING instance by calling one of its `handlers`:

     import { type WorkflowInstanceId } from "@sdk";
     import type { ApproveInput, ApproveOutput } from "@units/ctx.mod.workflow.name";

     const result = sdk.callWorkflowHandlerAndWaitForResultSync<ApproveInput, ApproveOutput>({
       unitPath: "ctx.mod.workflow.name",
       instanceId: wfInstanceId as WorkflowInstanceId,
       handler: "approve",
       input: { approvedBy: "admin" },
     });

     It blocks until the handler returns. The handler typically resolves a
     durable promise that the `run` function is waiting on.

Outbound (what THIS workflow can do) — two patterns, never mix them.
`unitPath` for actors / queue workers / workflows must match `ActorUnitPathOrId`,
`QueueWorkerUnitPathOrId`, `WorkflowUnitPathOrId` respectively; `actorId` is `ActorInstanceId`.

  import { type ActorInstanceId } from "@sdk";

SUSPENSION PRIMITIVES — call bare in the run body, never inside ctx.run():

  // Child workflow — suspends durably until child completes
  const childOut = sdk.callWorkflowAndWaitForOutputSync<ChildInput, ChildOutput>({
    unitPath: "ctx.mod.workflow.child",
    input: childInput,
  });

  // Timer and external signal — also bare suspension primitives
  ctx.sleep("wait", { seconds: 30 });
  ctx.promise<T>("signal-name").get();

SIDE EFFECTS — every one wrapped in `await ctx.run("name", () => ...)`:

  // Actor call as a named, journaled step (this is the correct pattern for actor calls in workflows)
  const result = await ctx.run("step-name", (): Out =>
    sdk.callActorAndWaitForResultSync<In, Out>({
      unitPath: "ctx.mod.actor.name",
      actorId: userKey as ActorInstanceId,
      handler: "handler",
      payload,
    }),
  );

  // Fire-and-forget enqueue
  await ctx.run("enqueue-step", (): void => {
    sdk.enqueueWorkerSync<In>({ unitPath: "ctx.mod.queue_worker.name", data: payload });
  });

  // Fire-and-forget workflow start
  await ctx.run("start-child", () =>
    sdk.startWorkflowSync<ChildInput>({
      unitPath: "ctx.mod.workflow.child",
      input: childInput,
    }),
  );
"#;

pub const RUNTIME: &str = r#"
### Automatically generated endpoints for Workflow units

These endpoints are created automatically by the platform, and user can interact with them through the dashboard. They are internal to the dashboard only — not the public HTTP surface for external integrations (unlike API unit trigger URLs).

HTTP trigger prefix `/api/v1/trigger/wf/`. Stage URLs insert `/stage/{runtime_id}/` after `{project_id}/`.

After `{project_id}/` (and optional `stage/{runtime_id}/`), the next three segments are `{context_slug}/{module_slug}/{workflow_name_slug}` — same slugs as in the logical unit path `{context_slug}.{module_slug}.workflow.{workflow_name_slug}`.

- Start a new instance: `POST .../{project_id}/{context}/{module}/{workflow_slug}` — JSON body is the exported `Input` type; response includes `instanceId` and initial status.
- Call a shared handler on a running instance: `POST .../{project_id}/.../{workflow_slug}/{instance_id}/handlers/{handler_name}` — JSON body matches the exported `{HandlerName}Input` for that handler; response data matches `{HandlerName}Output`.
- Status: `GET .../{instance_id}/status`
- Final result (when complete): `GET .../{instance_id}/result`
- Cancel: `POST .../{instance_id}/cancel`

The dashboard lists start with `Input`/`Output` and lists each `handlers` entry with its `{Name}Input`/`{Name}Output` exports. `instance_id` is opaque to the client until returned from start.

Example (production start): `POST /api/v1/trigger/wf/{project_id}/billing/flows/invoice-wf` with body matching `Input`.
"#;

pub const METADATA: &str = r#"
"#;
