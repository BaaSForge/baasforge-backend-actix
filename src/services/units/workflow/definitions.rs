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

`ctx` inside `run` — SYNCHRONOUS unless noted:
- ctx.key: string — the workflow instance ID
- ctx.TerminalError — throw to permanently fail the workflow without retry

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
- ctx.key: string — instance ID
- ctx.promise(name) — returns WorkflowDurablePromiseHandle: { resolve(value), reject(error) } — synchronous
- ctx.get(key) — synchronous; read workflow state

CRITICAL rules:
- Two categories of calls inside `run` — never mix them:
  SUSPENSION PRIMITIVES — call bare in the run body, never inside ctx.run():
    ctx.sleep(...)
    ctx.promise(...).get()
    sdk.callActorDurableAndWaitForResultSync(...)
    sdk.callWorkflowAndWaitForOutputSync(...)
  They throw an internal sentinel that must propagate freely. Wrapping in ctx.run()
  swallows the sentinel and breaks the suspension mechanism.
  SIDE EFFECTS — wrap every one inside `await ctx.run("step", () => ...)`:
    sdk.callActorAndWaitForResultSync(...)
    sdk.enqueueWorkerSync(...)
    sdk.startWorkflowSync(...)
    sdk.sendActorMessageSync(...)
  Anything outside ctx.run() re-executes on every replay.
- Step names (ctx.run, ctx.sleep, ctx.promise) must be unique within the workflow instance
- Never use Date.now() or Math.random() directly — use ctx.date.now() and ctx.rand.*
- Do not catch or swallow exceptions from suspension primitives — let them propagate
- TerminalError thrown inside ctx.run() permanently fails that step (and the workflow)

RetryPolicy: { maxAttempts?, initialInterval?: Duration, backoffFactor?, maxInterval?: Duration }
Duration: { milliseconds?, seconds?, minutes?, hours?, days? }

Exported types (MANDATORY — used for automatic documentation generation and cross-unit type safety):

- export type Input = { ... }               — the workflow start payload (REQUIRED)
- export type Output = { ... }              — the workflow completion result (REQUIRED)
- export type {HandlerName}Input = { ... }  — input type of each shared handler that takes input
- export type {HandlerName}Output = { ... } — return type of each shared handler that returns a value (REQUIRED per handler)

Naming convention: PascalCase handler name + "Input" / "Output" suffix.
Example: handler "approve" → ApproveInput, ApproveOutput.
"#;

pub const CODE_EXAMPLES: &str = r#"
import { workflow, sdk } from "@sdk";
import type { ChargeInput, ChargeOutput, RefundInput, RefundOutput } from "@units/my-ctx.my-module.actor.billing";
import type { Input as FulfillInput } from "@units/my-ctx.my-module.queue_worker.fulfill-order";

export type Input = { orderId: string; userId: string };
export type Output = { success: boolean; orderId: string };
export type StatusOutput = { phase: string } | null;
export type ApproveInput = { approvedBy: string };
export type ApproveOutput = { ok: boolean };
export type RejectOutput = { ok: boolean };

export default workflow<Input, Output>({
  run: async (ctx, input): Promise<Output> => {
    await ctx.run("charge-payment", (): ChargeOutput =>
      sdk.callActorAndWaitForResultSync<ChargeInput, ChargeOutput>(
        "my-ctx.my-module.actor.billing",
        input.userId,
        "charge",
        { orderId: input.orderId },
      ),
    );

    ctx.set("phase", "awaiting-approval");

    const approval = ctx.promise<{ approved: boolean }>("approval").get({
      timeout: { hours: 24 },
    });

    if (!approval?.approved) {
      await ctx.run("refund", (): RefundOutput =>
        sdk.callActorAndWaitForResultSync<RefundInput, RefundOutput>(
          "my-ctx.my-module.actor.billing",
          input.userId,
          "refund",
          { orderId: input.orderId },
        ),
      );
      return { success: false, orderId: input.orderId };
    }

    ctx.set("phase", "fulfilling");

    ctx.sleep("fulfillment-delay", { seconds: 5 });

    await ctx.run("fulfill-order", (): void => {
      sdk.enqueueWorkerSync<FulfillInput>(
        "my-ctx.my-module.queue_worker.fulfill-order",
        { orderId: input.orderId, idempotencyKey: ctx.rand.uuidv4() },
      );
    });

    return { success: true, orderId: input.orderId };
  },

  handlers: {
    status: async (ctx): Promise<StatusOutput> => {
      return ctx.get<{ phase: string }>("phase") ?? null;
    },

    approve: async (ctx, _input: ApproveInput): Promise<ApproveOutput> => {
      ctx.promise<{ approved: boolean }>("approval").resolve({ approved: true });
      return { ok: true };
    },

    reject: async (ctx): Promise<RejectOutput> => {
      ctx.promise<{ approved: boolean }>("approval").resolve({ approved: false });
      return { ok: true };
    },
  },
});
"#;

pub const COMMUNICATION: &str = r#"
Inbound — another unit can interact with this workflow in two ways. All sdk.* calls are
synchronous; do NOT write `await` in front of them.

  1) Start a new instance:

     import type { Input, Output } from "@units/ctx.mod.workflow.name";

     // Fire-and-forget — returns { instanceId } immediately; this is the
     // recommended form whenever the workflow waits on a signal/promise.
     const { instanceId } = sdk.startWorkflowSync<Input>(
       "ctx.mod.workflow.name", inputData,
     );

     // Start and BLOCK until the workflow terminates. Only safe for short,
     // non-signal workflows.
     const out = sdk.callWorkflowAndWaitForOutputSync<Input, Output>(
       "ctx.mod.workflow.name", inputData,
     );

  2) Signal / query a RUNNING instance by calling one of its `handlers`:

     import type { ApproveInput, ApproveOutput } from "@units/ctx.mod.workflow.name";

     const result = sdk.callWorkflowHandlerAndWaitForResultSync<ApproveInput, ApproveOutput>(
       "ctx.mod.workflow.name", instanceId, "approve", { approvedBy: "admin" },
     );

     It blocks until the handler returns. The handler typically resolves a
     durable promise that the `run` function is waiting on.

Outbound (what THIS workflow can do) — two patterns, never mix them:

SUSPENSION PRIMITIVES — call bare in the run body, never inside ctx.run():

  // Actor call via durable suspension — bare, no ctx.run wrapper
  const result = sdk.callActorDurableAndWaitForResultSync<In, Out>(
    "ctx.mod.actor.name", id, "handler", payload,
  );

  // Child workflow — suspends durably until child completes
  const childOut = sdk.callWorkflowAndWaitForOutputSync<ChildInput, ChildOutput>(
    "ctx.mod.workflow.child", childInput,
  );

  // Timer and external signal — also bare suspension primitives
  ctx.sleep("wait", { seconds: 30 });
  ctx.promise<T>("signal-name").get();

SIDE EFFECTS — every one wrapped in `await ctx.run("name", () => ...)`:

  // Actor call as a named, journaled step
  const result = await ctx.run("step-name", (): Out =>
    sdk.callActorAndWaitForResultSync<In, Out>(
      "ctx.mod.actor.name", id, "handler", payload,
    ),
  );

  // Fire-and-forget enqueue
  await ctx.run("enqueue-step", (): void => {
    sdk.enqueueWorkerSync<In>("ctx.mod.queue_worker.name", payload);
  });

  // Fire-and-forget workflow start
  await ctx.run("start-child", (): { instanceId: string } =>
    sdk.startWorkflowSync<ChildInput>("ctx.mod.workflow.child", childInput),
  );
"#;

pub const METADATA: &str = r#"
"#;
