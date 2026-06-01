import type { IncomingMessage, ServerResponse } from "node:http";
import { text } from "node:stream/consumers";
import { sdk, type ActorInstanceId } from "@sdk";
import { z } from "zod";

const SignupSchema = z.object({
  email: z.string().email(),
  password: z.string().min(8),
});

export type RequestBody = z.infer<typeof SignupSchema>;
export type ResponseBody = { success: boolean };
export type QueryParams = never;

export async function handleRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const raw = await text(req);
  let body: unknown;
  try {
    body = JSON.parse(raw);
  } catch {
    res.writeHead(400, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "Invalid JSON" }));
    return;
  }

  const result = SignupSchema.safeParse(body);
  if (!result.success) {
    res.writeHead(400, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: result.error.format() }));
    return;
  }

  const { email, password } = result.data;

  try {
    const actorId = email as ActorInstanceId;
    sdk.callActorAndWaitForResultSync<{ email: string; password: string }, { success: boolean }>({
      unitPath: "auth.user.actor.account",
      actorId,
      handler: "create",
      payload: { email, password },
    });
    res.writeHead(201, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ success: true } satisfies ResponseBody }));
  } catch (err) {
    res.writeHead(400, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: err instanceof Error ? err.message : "Registration failed" }));
  }
}
