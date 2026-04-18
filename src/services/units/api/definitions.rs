pub const TITLE: &str = "API";

pub const BRIEF: &str = r#"
An API unit is a single HTTP endpoint that handles one specific request type. Use it to
expose functionality over HTTP — REST endpoints, webhooks, or any request-response
interaction. Each unit serves one purpose and one HTTP method.
"#;

pub const DETAIL: &str = r#"
URL: The trigger URL of an API unit is fixed by the platform and derived entirely from the
unit's path — users cannot define or customize it. Do not write code that depends on a
specific URL shape; use req.url only to read query parameters.

IMPORTANT: One API unit = one endpoint = one HTTP method. Do NOT switch on req.method or
handle multiple methods in a single unit. Each HTTP method (GET, POST, PUT, DELETE, etc.)
is a separate unit. The platform routes all incoming requests to a specific unit by path;
the unit is expected to handle exactly one method.
The platform checks the method before reaching to the unit's code so you don't need to check if the method or url is correct.

Entry point — the following function MUST be exported with this exact signature:

  import type { IncomingMessage, ServerResponse } from "node:http";
  export function handleRequest(req: IncomingMessage, res: ServerResponse) { ... }

Both parameter types MUST be annotated as `IncomingMessage` and `ServerResponse` and MUST be
imported from `node:http`. No other signature is accepted.

`req` and `res` follow the standard Node.js IncomingMessage / ServerResponse interface.
Read the body with: import { text } from "node:stream/consumers"; const body = await text(req);

Exported types (MANDATORY — used for automatic documentation generation and cross-unit type safety):
The following three type exports are ALWAYS required:

- export type RequestBody = { ... } | never  — request body shape; use `never` if no body
- export type ResponseBody = { ... }         — response body shape; MUST be a concrete object type, NEVER `never`
- export type QueryParams = { ... } | never  — query parameter shape; use `never` if no query params

No other exports are allowed. The only permitted exports are:
  - export function handleRequest (the entry point)
  - export type RequestBody
  - export type ResponseBody
  - export type QueryParams

Forbidden: any other type alias, `export const`, `export class`, `export default`,
`export { … }`, `export * from '…'`.

Blocking warning: every `...AndWaitForResultSync` / `...AndWaitForOutputSync` call in an
API unit holds the HTTP response open until it returns. Never use one of the waiting
forms for a workflow that suspends on a signal/promise — use `startWorkflowSync` to
kick it off and `callWorkflowHandlerAndWaitForResultSync` to later signal/query it.
"#;

pub const CODE_EXAMPLES: &str = r#"
import type { IncomingMessage, ServerResponse } from "node:http";
import { text } from "node:stream/consumers";
import { sdk } from "@sdk";
import type { IncrementInput, IncrementOutput } from "@units/my-ctx.my-module.actor.counter";
import type { Input as WfInput, Output as WfOutput } from "@units/my-ctx.my-module.workflow.process-order";

export type RequestBody = { actorId: string; by: number };
export type ResponseBody = { ok: boolean; count: number; workflowId: string };
export type QueryParams = never;

export async function handleRequest(req: IncomingMessage, res: ServerResponse) {
  const url = new URL(req.url ?? "/", "http://x");
  const actorId = url.searchParams.get("id") ?? "default";

  let body: RequestBody;
  try {
    body = JSON.parse(await text(req));
  } catch {
    res.writeHead(400, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "Invalid JSON body" }));
    return;
  }

  // Blocking actor call — waits for the handler result. Sync; no await.
  const result = sdk.callActorAndWaitForResultSync<IncrementInput, IncrementOutput>(
    "my-ctx.my-module.actor.counter",
    actorId,
    "increment",
    { by: body.by },
  );

  // Fire-and-forget workflow start — returns instanceId immediately. Use this
  // form for workflows that suspend on signals/promises. Sync; no await.
  const { instanceId } = sdk.startWorkflowSync<WfInput>(
    "my-ctx.my-module.workflow.process-order",
    { actorId, count: result.count },
  );

  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify({ ok: true, count: result.count, workflowId: instanceId }));
}
"#;

pub const COMMUNICATION: &str = r#"
Inbound — API units are triggered exclusively by external HTTP requests. No SDK function
exists to call an API unit from another unit; callers must use fetch() directly. Other
units can import the exported types for compile-time type safety on fetch calls:

  import type { RequestBody, ResponseBody, QueryParams }
    from "@units/context.module.api.unit-name";

Outbound (what THIS unit can do) — ALL sdk.* calls are synchronous; write NO `await`
in front of any `sdk.*` call. A call with `...AndWaitFor...` in its name BLOCKS the
HTTP response until the callee finishes; plain `sendFooSync` / `enqueueFooSync` /
`startFooSync` return immediately without waiting.

  // Call an actor and wait for the handler result
  const out = sdk.callActorAndWaitForResultSync<In, Out>(
    "ctx.mod.actor.name", id, "handler", payload,
  );

  // Fire-and-forget actor message (no result)
  sdk.sendActorMessageSync<In>("ctx.mod.actor.name", id, "handler", payload);

  // Fire-and-forget background job (returns runId)
  const { runId } = sdk.enqueueWorkerSync<In>("ctx.mod.queue_worker.name", payload);

  // Start a workflow, get back instanceId immediately (recommended default)
  const { instanceId } = sdk.startWorkflowSync<WfInput>(
    "ctx.mod.workflow.name", input,
  );

  // Start a workflow AND wait for its full Output (ONLY for fast, non-signal workflows)
  const out = sdk.callWorkflowAndWaitForOutputSync<WfInput, WfOutput>(
    "ctx.mod.workflow.name", input,
  );

  // Signal / query a RUNNING workflow by instanceId
  const r = sdk.callWorkflowHandlerAndWaitForResultSync<HandlerIn, HandlerOut>(
    "ctx.mod.workflow.name", instanceId, "handlerName", input,
  );

Common mistakes to avoid:

- Using `callWorkflowAndWaitForOutputSync` for a workflow that waits on a signal
  freezes the endpoint forever — use `startWorkflowSync` instead, then signal via
  `callWorkflowHandlerAndWaitForResultSync`.
- Writing `await sdk.xxxSync(...)` — wrong; these functions are NOT async and return
  plain values. `await` on a non-Promise is a no-op but misleads anyone reading the code.
- Using `sdk.callActorDurableAndWaitForResultSync` — that variant is workflow-only.
  API units must use `sdk.callActorAndWaitForResultSync`.
"#;

pub const METADATA: &str = r#"
"#;
