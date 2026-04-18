declare module "@sdk" {
  // ---------------------------------------------------------------------------
  // SDK singleton — platform primitives
  // ---------------------------------------------------------------------------

  export const sdk: {
    callWorkflow<T = any>(unitPath: string, data?: any): Promise<T>;

    enqueueWorker<T = any>(
      unitPath: string,
      data?: T,
      opts?: EnqueueWorkerOptions,
    ): Promise<{ runId: string }>;

    callActor<TInput = any, TResult = any>(
      unitPath: string,
      actorId: string,
      handler: string,
      payload?: TInput,
      opts?: CallActorOptions,
    ): Promise<TResult>;

    /**
     * Durable actor call (workflow-only). Suspends the workflow via a durable
     * promise until the actor handler completes, then resumes with the result.
     * Only available inside a workflow `run` handler; patched by workflow-runtime.js.
     */
    callActorDurable<TInput = any, TResult = any>(
      unitPath: string,
      actorId: string,
      handler: string,
      payload?: TInput,
    ): Promise<TResult>;
  };

  // ---------------------------------------------------------------------------
  // Duration
  // ---------------------------------------------------------------------------

  export interface Duration {
    milliseconds?: number;
    seconds?: number;
    minutes?: number;
    hours?: number;
    days?: number;
  }

  // ---------------------------------------------------------------------------
  // Shared options
  // ---------------------------------------------------------------------------

  export interface EnqueueWorkerOptions {
    delay?: Duration;
    deduplicationKey?: string;
  }

  export interface CallActorOptions {
    timeoutMs?: number;
    awaitResult?: boolean;
  }

  export interface RunOptions {
    retryPolicy?: RetryPolicy;
  }

  export interface RetryPolicy {
    maxAttempts?: number;
    initialInterval?: Duration;
    maxInterval?: Duration;
    backoffFactor?: number;
  }

  // ---------------------------------------------------------------------------
  // Queue Worker — metadata (static config, read by runtime at registration)
  // ---------------------------------------------------------------------------

  export interface RateLimitPolicy {
    max: number;
    window: string;
  }

  export interface BatchPolicy {
    size: number;
    timeout: string;
  }

  export interface QueueWorkerMetadata {
    retry?: RetryPolicy;
    concurrency?: number;
    rateLimit?: RateLimitPolicy;
    batch?: BatchPolicy;
    timeout?: number;
    deadLetter?: { enabled: boolean };
  }

  // ---------------------------------------------------------------------------
  // Queue Worker — definition
  // ---------------------------------------------------------------------------

  export function queueWorker<TData = any, TResult = any>(
    handler: (ctx: QueueWorkerContext, data: TData) => Promise<TResult>,
  ): QueueWorkerDefinition<TData, TResult>;

  export function queueWorker<TData = any, TResult = any>(
    config: QueueWorkerConfig<TData, TResult>,
  ): QueueWorkerDefinition<TData, TResult>;

  export interface QueueWorkerConfig<TData, TResult> {
    process: (ctx: QueueWorkerContext, data: TData) => Promise<TResult>;
    onDeadLetter?: (ctx: QueueWorkerContext, data: TData, error: Error) => Promise<void>;
    metadata?: QueueWorkerMetadata;
  }

  export interface QueueWorkerDefinition<TData = any, TResult = any> {}

  // ---------------------------------------------------------------------------
  // Queue Worker — execution context
  // ---------------------------------------------------------------------------

  export interface QueueWorkerContext {
    readonly id: string;
    readonly attempt: number;
    readonly enqueuedAt: number;
    readonly TerminalError: typeof TerminalError;
  }

  // ---------------------------------------------------------------------------
  // Workflow — definition
  // ---------------------------------------------------------------------------

  export function workflow<TInput = any, TOutput = any>(
    handler: (ctx: WorkflowContext, input: TInput) => Promise<TOutput>,
  ): WorkflowDefinition<TInput, TOutput>;

  export function workflow<TInput = any, TOutput = any>(
    config: WorkflowConfig<TInput, TOutput>,
  ): WorkflowDefinition<TInput, TOutput>;

  export interface WorkflowConfig<TInput, TOutput> {
    run: (ctx: WorkflowContext, input: TInput) => Promise<TOutput>;
    handlers?: Record<string, (ctx: WorkflowSharedContext, input?: any) => Promise<any>>;
  }

  export interface WorkflowDefinition<TInput = any, TOutput = any> {}

  // ---------------------------------------------------------------------------
  // Workflow — run context (available inside the `run` handler)
  // ---------------------------------------------------------------------------

  export interface WorkflowContext {
    readonly key: string;

    /**
     * Execute a durable step. The result is persisted in Redis.
     * On replay (after crash / sleep / promise), returns the cached result
     * without re-executing `fn`.
     */
    run<T>(name: string, fn: () => Promise<T>, opts?: RunOptions): Promise<T>;

    /**
     * Durable timer. Suspends the workflow and resumes after the given
     * duration. Survives crashes, restarts, and deployments.
     */
    sleep(name: string, duration: Duration | number): Promise<void>;

    /**
     * Obtain a durable promise. Use `.get()` to suspend until the promise
     * is resolved or rejected (or a timeout elapses). Use `.resolve()` /
     * `.reject()` from a shared handler to unblock the workflow.
     */
    promise<T = any>(name: string): DurablePromise<T>;

    set(key: string, value: any): void;
    get<T = any>(key: string): Promise<T | null>;

    /**
     * Deterministic random helpers. Values are seeded by the instance ID
     * and step position so they produce the same result on replay.
     */
    readonly rand: DeterministicRand;

    /**
     * Deterministic date helper. `ctx.date.now()` returns the timestamp
     * captured at the step boundary, safe for replay.
     */
    readonly date: DeterministicDate;

    readonly TerminalError: typeof TerminalError;
  }

  // ---------------------------------------------------------------------------
  // Workflow — shared context (available inside shared handlers)
  // ---------------------------------------------------------------------------

  export interface WorkflowSharedContext {
    readonly key: string;
    promise<T = any>(name: string): DurablePromiseHandle<T>;
    get<T = any>(key: string): Promise<T | null>;
  }

  // ---------------------------------------------------------------------------
  // Durable promises
  // ---------------------------------------------------------------------------

  export interface DurablePromise<T> {
    get(opts?: { timeout?: Duration }): Promise<T | null>;
    resolve(value: T): Promise<void>;
    reject(error: string): Promise<void>;
  }

  export interface DurablePromiseHandle<T> {
    resolve(value: T): Promise<void>;
    reject(error: string): Promise<void>;
  }

  // ---------------------------------------------------------------------------
  // Deterministic helpers
  // ---------------------------------------------------------------------------

  export interface DeterministicRand {
    uuidv4(): string;
    random(): number;
  }

  export interface DeterministicDate {
    now(): number;
  }

  // ---------------------------------------------------------------------------
  // Actor — metadata
  // ---------------------------------------------------------------------------

  export interface ActorMetadata {
    handlers?: string[];
    maxIdleTime?: string;
    alarms?: string[];
    stateLimit?: string;
    retry?: RetryPolicy;
    timeout?: number;
    mailboxBacklog?: number;
  }

  // ---------------------------------------------------------------------------
  // Actor — execution context
  // ---------------------------------------------------------------------------

  export interface ActorContext<TState> {
    readonly id: string;
    readonly attempt: number;
    state: TState;
    setAlarm(name: string, at: Date | number | Duration): void;
    cancelAlarm(name: string): void;
    readonly TerminalError: typeof TerminalError;
  }

  // ---------------------------------------------------------------------------
  // Actor — definition
  // ---------------------------------------------------------------------------

  type ActorHandlers<TState> = {
    [handler: string]: (ctx: ActorContext<TState>, payload?: any) => any;
  };

  export interface ActorConfig<TState> {
    state: TState;
    handlers: ActorHandlers<TState>;
    metadata?: ActorMetadata;
    onAlarm?: (ctx: ActorContext<TState>, name: string) => void | Promise<void>;
    onDeadLetter?: (ctx: ActorContext<TState>, handler: string, payload: any, error: Error) => void | Promise<void>;
  }

  export interface ActorDefinition<TState = any> {}

  export function actor<TState = any>(config: ActorConfig<TState>): ActorDefinition<TState>;

  // ---------------------------------------------------------------------------
  // Terminal error
  // ---------------------------------------------------------------------------

  export class TerminalError extends Error {
    constructor(message: string, options?: { cause?: unknown });
  }
}

// -----------------------------------------------------------------------------
// Cross-unit type imports
//
// Units can import types from other units via:
//   import type { Input, Output } from "@units/context.module.workflow.name";
//
// This is compile-time only (stripped at runtime).
// Only `import type` is allowed — value imports are rejected by the dashboard
// platform's lint rules.
// -----------------------------------------------------------------------------

declare module "@units/*" {
  export type Input = any;
  export type Output = any;
  export type BatchOutput = any;
}
