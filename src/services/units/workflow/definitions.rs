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

`ctx` inside `run`:
- ctx.key: string — the workflow instance ID
- ctx.TerminalError — throw to permanently fail the workflow without retry

ctx.run(name, fn, opts?) — durable step:
  - fn executes exactly once; on replay, returns the cached result without calling fn again
  - name must be unique across all steps in this workflow instance
  - opts.retryPolicy: RetryPolicy — step-level retry on fn failure
  - All side effects (HTTP calls, actor calls, enqueue) must be inside ctx.run()

ctx.sleep(name, duration) — durable timer:
  - Suspends the workflow until the duration elapses
  - Survives crashes, restarts, and deployments
  - duration: Duration object { milliseconds?, seconds?, minutes?, hours?, days? } or number (ms)
  - name must be unique

ctx.promise(name) — durable promise (DurablePromise<T>):
  - .get(opts?) — suspends the workflow until the promise is resolved or rejected; returns
    null on timeout. opts.timeout: Duration
  - .resolve(value) — resolve the promise (from inside a ctx.run step or a shared handler)
  - .reject(error: string) — reject the promise
  - name must be unique

ctx.set(key, value) — persist arbitrary key-value state for this workflow instance
ctx.get(key) — read persisted state (returns null if not set)

ctx.rand.uuidv4() / ctx.rand.random() — deterministic random seeded by instance ID + step
  position; produces the same values on replay. Use instead of Math.random().
ctx.date.now() — deterministic current timestamp captured at step boundary; use instead of
  Date.now() inside workflow logic.

`handlers` — optional named functions called externally while the workflow is running.
They receive WorkflowSharedContext:
- ctx.key: string — instance ID
- ctx.promise(name) — returns DurablePromiseHandle: { resolve(value), reject(error) }
- ctx.get(key) — read workflow state

CRITICAL rules:
- ALL side effects must be inside ctx.run() — anything outside re-runs on every replay
- Step names (run, sleep, promise) must be unique within the workflow instance
- Never use Date.now() or Math.random() directly — use ctx.date.now() and ctx.rand.*
- Do not catch or swallow exceptions thrown by ctx.sleep() or ctx.promise().get() in a
  try/catch — they use a special suspension mechanism that must propagate normally
- TerminalError thrown inside ctx.run() permanently fails that step (and the workflow)

RetryPolicy: { maxAttempts?, initialInterval?: Duration, backoffFactor?, maxInterval?: Duration }
Duration: { milliseconds?, seconds?, minutes?, hours?, days? }

Exported types (MANDATORY — used for automatic documentation generation and cross-unit type safety):

- export type Input = { ... }               — the workflow start payload (REQUIRED)
- export type Output = { ... }              — the workflow completion result (REQUIRED)
- export type {HandlerName}Output = { ... } — return type of each shared handler that returns a value (REQUIRED per handler)

Naming convention: PascalCase handler name + "Output" suffix for shared handler types.
Example: handler "status" → StatusOutput, handler "approve" → ApproveOutput.
"#;

pub const CODE_EXAMPLES: &str = r#"
import { workflow, sdk } from "@sdk";
import type { ChargeInput, ChargeOutput, RefundInput, RefundOutput } from "@units/my-ctx.my-module.actor.billing";
import type { Input as FulfillInput } from "@units/my-ctx.my-module.queue_worker.fulfill-order";

export type Input = { orderId: string; userId: string };
export type Output = { success: boolean; orderId: string };
export type StatusOutput = { phase: string } | null;
export type ApproveOutput = { ok: boolean };
export type RejectOutput = { ok: boolean };

export default workflow<Input, Output>({
  run: async (ctx, input): Promise<Output> => {
    await ctx.run("charge-payment", async (): Promise<ChargeOutput> => {
      return sdk.callActorDurable<ChargeInput, ChargeOutput>(
        "my-ctx.my-module.actor.billing",
        input.userId,
        "charge",
        { orderId: input.orderId },
      );
    });

    ctx.set("phase", "awaiting-approval");

    const approval = await ctx.promise<{ approved: boolean }>("approval").get({
      timeout: { hours: 24 },
    });

    if (!approval?.approved) {
      await ctx.run("refund", async (): Promise<RefundOutput> => {
        return sdk.callActorDurable<RefundInput, RefundOutput>(
          "my-ctx.my-module.actor.billing",
          input.userId,
          "refund",
          { orderId: input.orderId },
        );
      });
      return { success: false, orderId: input.orderId };
    }

    ctx.set("phase", "fulfilling");

    await ctx.sleep("fulfillment-delay", { seconds: 5 });

    await ctx.run("fulfill-order", async (): Promise<void> => {
      await sdk.enqueueWorker<FulfillInput>(
        "my-ctx.my-module.queue_worker.fulfill-order",
        { orderId: input.orderId, idempotencyKey: ctx.rand.uuidv4() },
      );
    });

    return { success: true, orderId: input.orderId };
  },

  handlers: {
    status: async (ctx): Promise<StatusOutput> => {
      return (await ctx.get<{ phase: string }>("phase")) ?? null;
    },

    approve: async (ctx): Promise<ApproveOutput> => {
      await ctx.promise("approval").resolve({ approved: true });
      return { ok: true };
    },

    reject: async (ctx): Promise<RejectOutput> => {
      await ctx.promise("approval").resolve({ approved: false });
      return { ok: true };
    },
  },
});
"#;

pub const COMMUNICATION: &str = r#"
Start a workflow from any unit type using sdk.callWorkflow. Blocks until the workflow
completes and returns its output. From inside another workflow's run, it automatically
becomes a durable suspension instead of a blocking call:

  import type { Input, Output } from "@units/ctx.mod.workflow.name";

  const result = await sdk.callWorkflow<Output>("ctx.mod.workflow.name", inputData as Input);
"#;

pub const METADATA: &str = r#"
"#;
