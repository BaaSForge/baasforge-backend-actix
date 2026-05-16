declare module "@sdk" {
  // ---------------------------------------------------------------------------
  // Internal markers — DO NOT export, DO NOT use from unit code.
  // ---------------------------------------------------------------------------

  interface _Internal_RequireExplicit { readonly _: never }
  type _Internal_Resolve<T> = T extends _Internal_RequireExplicit ? never : T;

  export type ApiUnitPathOrId = `${string}.${string}.api.${string}`;
  export type ActorUnitPathOrId = `${string}.${string}.actor.${string}`;
  export type QueueWorkerUnitPathOrId = `${string}.${string}.queue_worker.${string}`;
  export type WorkflowUnitPathOrId = `${string}.${string}.workflow.${string}`;

  declare const __baalBrandWorkflowInstanceId: unique symbol;
  export type WorkflowInstanceId = string & { readonly [__baalBrandWorkflowInstanceId]: true };

  declare const __baalBrandQueueWorkerRunId: unique symbol;
  export type QueueWorkerRunId = string & { readonly [__baalBrandQueueWorkerRunId]: true };

  declare const __baalBrandActorInstanceId: unique symbol;
  export type ActorInstanceId = string & { readonly [__baalBrandActorInstanceId]: true };

  export const sdk: {
    /**
     * Returns the full trigger URL for an API unit.
     *
     * Throws if the target is not an `api` unit.
     *
     * @param args.unitPathOrId — 4-segment path or 5-segment full unit id (`ApiUnitPathOrId`).
     */
    getApiUnitTriggerUrl(args: { unitPathOrId: ApiUnitPathOrId }): string;

    /**
     * Returns a runtime env var value.
     * Encrypted values are decrypted before return.
     */
    getSecureEnv(key: string): string;

    // -------------------------------------------------------------------------
    // Actor primitives
    // -------------------------------------------------------------------------

    /**
     * Invoke an actor handler and BLOCK until it returns its result.
     *
     * Callable from inside a workflow `run` only when wrapped in
     * `await ctx.run("step-name", () => sdk.callActorAndWaitForResultSync({ ... }))`.
     * Outside a workflow (API, Actor, QueueWorker) call it directly.
     */
    callActorAndWaitForResultSync<TInput, TResult = _Internal_RequireExplicit>(args: {
      unitPath: ActorUnitPathOrId;
      actorId: ActorInstanceId;
      handler: string;
      payload?: TInput;
      opts?: CallActorSyncOptions;
    }): _Internal_Resolve<TResult>;

    /**
     * Fire-and-forget actor message: the message is persisted/queued on the
     * actor's mailbox and this call returns immediately.
     *
     * There is no way to retrieve the handler's result via this handle — if
     * you need the result later, either call
     * `callActorAndWaitForResultSync({ ... })`, or have the callee write its result
     * back into an actor / workflow you own.
     */
    sendActorMessageSync<TInput>(args: {
      unitPath: ActorUnitPathOrId;
      actorId: ActorInstanceId;
      handler: string;
      payload?: TInput;
    }): { delivered: true };

    // -------------------------------------------------------------------------
    // Queue Worker primitive
    // -------------------------------------------------------------------------

    /**
     * Enqueue a job on a queue worker (ALWAYS fire-and-forget).
     *
     * Returns `{ runId }` as soon as the job is persisted to the queue. You
     * cannot `await` the job's completion via this handle — if you need the
     * result, have the worker call an actor / workflow you own, or use a
     * workflow instead of a queue worker.
     */
    enqueueWorkerSync<TData>(args: {
      unitPath: QueueWorkerUnitPathOrId;
      data: TData;
      opts?: EnqueueWorkerOptions;
    }): { runId: QueueWorkerRunId };

    // -------------------------------------------------------------------------
    // Workflow primitives
    // -------------------------------------------------------------------------

    /**
     * Fire-and-forget workflow start. Creates a new instance, kicks off its
     * drive in the background, and returns `{ instanceId }` immediately.
     *
     * Use this whenever the workflow:
     *   • waits for signals/promises (calling the blocking variant would deadlock),
     *   • is long-running, or
     *   • the caller only needs to kick it off.
     *
     * Use the returned `instanceId` later with
     * `callWorkflowHandlerAndWaitForResultSync({ unitPath, instanceId, handler, input?, opts? })`
     * to signal / query the workflow, or to look it up via the management HTTP API.
     *
     * @param args — `unitPath` (`WorkflowUnitPathOrId`), `input`. `instanceId` in the result
     *   is typed as `WorkflowInstanceId`.
     *
     * Inside a workflow `run`, this call is NOT durable — wrap it in
     * `ctx.run("start-child", () => sdk.startWorkflowSync({ unitPath, input }))` for replay
     * safety when the caller crashes between the start and the next step.
     */
    startWorkflowSync<TInput>(args: {
      unitPath: WorkflowUnitPathOrId;
      input: TInput;
    }): { instanceId: WorkflowInstanceId };

    /**
     * Start a workflow AND BLOCK until it reaches a terminal state
     * (completed / failed / cancelled); return the workflow's `Output`.
     *
     * Do NOT use this for workflows that wait on external signals — the
     * caller will be blocked waiting for a signal that cannot arrive.
     * Prefer `startWorkflowSync` for signal-driven / long-running workflows.
     *
     * Inside a workflow `run`, this becomes a durable child-workflow
     * suspension (like Temporal's executeChild) — safe to call directly.
     */
    callWorkflowAndWaitForOutputSync<TInput, TOutput = _Internal_RequireExplicit>(args: {
      unitPath: WorkflowUnitPathOrId;
      input: TInput;
      opts?: CallWorkflowSyncOptions;
    }): _Internal_Resolve<TOutput>;

    /**
     * Invoke a shared `handlers` entry on a RUNNING workflow instance (e.g.
     * to deliver a signal, approval, cancel, or status query).
     *
     * BLOCKS until the handler returns its value. The workflow MUST already
     * exist — you usually get the `instanceId` from a previous call to
     * `startWorkflowSync({ unitPath, input })`.
     *
     * This is the ONLY way to reach a workflow's `handlers` from another
     * unit. `callWorkflowAndWaitForOutputSync({ ... })` / `startWorkflowSync({ ... })` can NOT
     * signal an existing instance; they only start new ones.
     */
    callWorkflowHandlerAndWaitForResultSync<TInput, TResult = _Internal_RequireExplicit>(args: {
      unitPath: WorkflowUnitPathOrId;
      instanceId: WorkflowInstanceId;
      handler: string;
      input?: TInput;
      opts?: CallWorkflowHandlerSyncOptions;
    }): _Internal_Resolve<TResult>;
  };

  // ---------------------------------------------------------------------------
  // Shared value types
  // ---------------------------------------------------------------------------

  export interface Duration {
    milliseconds?: number;
    seconds?: number;
    minutes?: number;
    hours?: number;
    days?: number;
  }

  export interface RetryPolicy {
    maxAttempts?: number;
    initialInterval?: Duration;
    maxInterval?: Duration;
    backoffFactor?: number;
  }

  /** Non-retriable error — throw to skip retries and trigger onDeadLetter. */
  export class TerminalError extends Error {
    constructor(message: string, options?: { cause?: unknown });
  }

  // ---------------------------------------------------------------------------
  // Option objects for sdk.* methods
  // ---------------------------------------------------------------------------

  export interface CallActorSyncOptions {
    /** Blocking timeout in milliseconds; default 30_000. */
    timeoutMs?: number;
  }

  export interface CallWorkflowSyncOptions {
    /** Blocking timeout in milliseconds; default 300_000. */
    timeoutMs?: number;
  }

  export interface CallWorkflowHandlerSyncOptions {
    /** Blocking timeout in milliseconds; default 30_000. */
    timeoutMs?: number;
  }

  export interface EnqueueWorkerOptions {
    /** Run the job no sooner than `delay` from now. */
    delay?: Duration;
    /** Same key within the current dedup window enqueues only once. */
    deduplicationKey?: string;
  }

  // ---------------------------------------------------------------------------
  // Queue Worker — definition + context + metadata
  // ---------------------------------------------------------------------------

  export function queueWorker<TData, TResult>(
    handler: (ctx: QueueWorkerContext, data: TData) => Promise<TResult>,
  ): QueueWorkerDefinition<TData, TResult>;

  export function queueWorker<TData, TResult>(
    config: QueueWorkerConfig<TData, TResult>,
  ): QueueWorkerDefinition<TData, TResult>;

  export interface QueueWorkerConfig<TData, TResult> {
    process: (ctx: QueueWorkerContext, data: TData) => Promise<TResult>;
    onDeadLetter?: (ctx: QueueWorkerContext, data: TData, error: Error) => Promise<void>;
    metadata?: QueueWorkerMetadata;
  }

  export interface QueueWorkerDefinition<TData, TResult> {}

  export interface QueueWorkerContext {
    readonly id: QueueWorkerRunId;
    readonly attempt: number;
    readonly enqueuedAt: number;
    readonly TerminalError: typeof TerminalError;
  }

  export interface QueueWorkerRateLimitPolicy {
    max: number;
    window: string;
  }

  export interface QueueWorkerBatchPolicy {
    size: number;
    timeout: string;
  }

  export interface QueueWorkerMetadata {
    retry?: RetryPolicy;
    concurrency?: number;
    rateLimit?: QueueWorkerRateLimitPolicy;
    batch?: QueueWorkerBatchPolicy;
    timeout?: number;
    deadLetter?: { enabled: boolean };
  }

  // ---------------------------------------------------------------------------
  // Workflow — definition + contexts + ctx-helpers
  // ---------------------------------------------------------------------------

  export function workflow<TInput, TOutput>(
    handler: (ctx: WorkflowContext, input: TInput) => Promise<TOutput>,
  ): WorkflowDefinition<TInput, TOutput>;

  export function workflow<TInput, TOutput>(
    config: WorkflowConfig<TInput, TOutput>,
  ): WorkflowDefinition<TInput, TOutput>;

  export interface WorkflowConfig<TInput, TOutput> {
    run: (ctx: WorkflowContext, input: TInput) => Promise<TOutput>;
    handlers?: Record<string, (ctx: WorkflowSharedContext, input?: any) => Promise<any>>;
  }

  export interface WorkflowDefinition<TInput, TOutput> {}

  export interface WorkflowStepOptions {
    retryPolicy?: RetryPolicy;
  }

  /**
   * Workflow `run` context — available inside the `run` function.
   *
   * `ctx.run` returns a Promise and requires
   * `await`. Other members are synchronous (they may throw an internal
   * suspension sentinel; never catch exceptions from `ctx.sleep`, `ctx.get`,
   * or `ctx.promise(...).get` — let them propagate).
   */
  export interface WorkflowContext {
    readonly key: WorkflowInstanceId;

    /**
     * Execute a durable step. The result is persisted; on replay (after
     * crash / sleep / promise), returns the cached result without re-running
     * `fn`. This returns a Promise — use `await ctx.run(...)`.
     */
    run<T>(name: string, fn: () => T | Promise<T>, opts?: WorkflowStepOptions): Promise<T>;

    /**
     * Durable timer. Suspends the workflow and resumes after the given
     * duration. Synchronous (throws a suspension sentinel internally)
     */
    sleep(name: string, duration: Duration | number): void;

    /**
     * Obtain a durable promise handle. Call `.get()` to suspend the
     * workflow until resolved/rejected/timed out. Call `.resolve()` /
     * `.reject()` from a shared handler to unblock the workflow.
     */
    promise<T = _Internal_RequireExplicit>(name: string): WorkflowDurablePromise<_Internal_Resolve<T>>;

    /** Persist a key/value pair on this workflow instance. Synchronous. */
    set(key: string, value: any): void;

    /** Read a previously persisted key. Synchronous. */
    get<T = _Internal_RequireExplicit>(key: string): _Internal_Resolve<T> | null;

    /**
     * Deterministic random helpers — seeded by instance ID + step position
     * so they return the same values on replay. Use instead of
     * `Math.random()` / `crypto.randomUUID()`.
     */
    readonly rand: WorkflowDeterministicRand;

    /**
     * Deterministic date helper. `ctx.date.now()` returns the timestamp
     * captured at the step boundary; safe for replay.
     */
    readonly date: WorkflowDeterministicDate;

    readonly TerminalError: typeof TerminalError;
  }

  /**
   * Workflow shared-handler context — available inside each function in the
   * optional `handlers: { ... }` block. Handlers are invoked externally
   * (from another unit via `sdk.callWorkflowHandlerAndWaitForResultSync({ ... })`).
   * They can read state and resolve/reject a durable promise to steer the
   * `run` function; they cannot run durable steps or sleep.
   */
  export interface WorkflowSharedContext {
    readonly key: WorkflowInstanceId;
    promise<T = _Internal_RequireExplicit>(name: string): WorkflowDurablePromiseHandle<_Internal_Resolve<T>>;
    get<T = _Internal_RequireExplicit>(key: string): _Internal_Resolve<T> | null;
  }

  /**
   * Durable promise used in the workflow `run` context. `.get()` suspends the
   * workflow until `.resolve()` / `.reject()` is called (from a shared
   * handler) or the optional timeout elapses.
   */
  export interface WorkflowDurablePromise<T> {
    /** Suspends the workflow until resolved/rejected; returns null on timeout. */
    get(opts?: { timeout?: Duration }): T | null;
    resolve(value: T): void;
    reject(error: string): void;
  }

  /** Durable promise handle exposed in shared-handler context — only resolve/reject. */
  export interface WorkflowDurablePromiseHandle<T> {
    resolve(value: T): void;
    reject(error: string): void;
  }

  export interface WorkflowDeterministicRand {
    uuidv4(): string;
    random(): number;
  }

  export interface WorkflowDeterministicDate {
    now(): number;
  }

  // ---------------------------------------------------------------------------
  // Actor — definition + context + metadata
  // ---------------------------------------------------------------------------

  export function actor<TState>(config: ActorConfig<TState>): ActorDefinition<TState>;

  export interface ActorConfig<TState> {
    state: TState;
    handlers: ActorHandlers<TState>;
    metadata?: ActorMetadata;
    onAlarm?: (ctx: ActorContext<TState>, name: string) => void | Promise<void>;
    onDeadLetter?: (ctx: ActorContext<TState>, handler: string, payload: any, error: Error) => void | Promise<void>;
  }

  export interface ActorDefinition<TState> {}

  export interface ActorContext<TState> {
    readonly id: ActorInstanceId;
    readonly attempt: number;
    state: TState;
    /**
     * Schedule a named alarm. `at` can be a Date, absolute Unix ms, `Duration`, or `{ fireAfterMs }`.
     * Prefer `{ fireAfterMs }` for delays from “now”: the host applies the delay at commit time (same idea as timer-by-duration in systems like Temporal).
     */
    setAlarm(name: string, at: Date | number | Duration | { fireAfterMs: number }): void;
    cancelAlarm(name: string): void;
    readonly TerminalError: typeof TerminalError;
  }

  type ActorHandlers<TState> = {
    [handler: string]: (ctx: ActorContext<TState>, payload?: any) => any;
  };

  export interface ActorMetadata {
    handlers?: string[];
    maxIdleTime?: string | number;
    alarms?: string[];
    stateLimit?: string;
    retry?: RetryPolicy;
    timeout?: number;
    mailboxBacklog?: number;
  }
}

// -----------------------------------------------------------------------------
// Cross-unit type imports
//
// Units can import types (and only types) from other units via:
//
//   import type { IncrementInput, IncrementOutput }
//     from "@units/counter.core.actor.counter-actor";
//   import type { Input, Output }
//     from "@units/counter.core.workflow.increment-workflow";
//
// This is compile-time only (stripped at runtime). `import type` is the only
// form allowed — value imports are rejected by the platform's lint rules.
// Never put a runtime `import` (without `type`) from `@units/...`.
// -----------------------------------------------------------------------------

declare module "@units/*" {
}
