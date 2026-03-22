# Chat session queue (RabbitMQ)

The API publishes work to `chat_session_exchange`; the chat worker consumes from `chat_session_work_queue`. See `queue_setup_http.sh` for declarations.

## What one queue message represents

Each message is **at most**:

- one LLM “thinking”,
- plus content retrieval for that step,
- plus **tool result retrieval** needed to build the model context (history) for that step.

**Tool invocations themselves are not bounded by the invoker message.** When the model requests tools, those runs are handled as **separate queue messages** (continuations and tool-message handlers). The worker **acks** after each of those steps completes.

So you do **not** need `CHAT_CONSUMER_TIMEOUT_SECS` to cover an entire multi-tool conversation end-to-end—only the **longest single consumer pass** for one message (one LLM round + its immediate retrievals for that round). Work is split across messages and consumed **separately**.

## `CHAT_CONSUMER_TIMEOUT_SECS` and `x-consumer-timeout`

RabbitMQ’s queue argument `x-consumer-timeout` (milliseconds) is derived from `CHAT_CONSUMER_TIMEOUT_SECS` in `.env` when you run `queue_setup_http.sh`. If a consumer does not **ack** within that window, the broker closes the channel and the message can be redelivered.

Set `CHAT_CONSUMER_TIMEOUT_SECS` high enough for the **worst-case single message** above (heavy models, extended thinking, large context), not for the whole agent loop.

Changing this value requires the work queue to be recreated (or otherwise updated) so the queue argument matches; see comments in `queue_setup_http.sh`.

## Environment

- **`CHAT_CONSUMER_TIMEOUT_SECS`**: required in `.env` for the app and for `queue_setup_http.sh`.
- The setup script loads **`.env` only**, not `.env.local`.

## Messages flow
When a message is received by the user, we save that message into db, then publish a message into rabbitMQ with a uuid

Then a worker consumes that rabbitMQ, and receives that message, it creates that message into the DB and send a websocket event to the user, then it sends it to LLM and waits for the response

When the response is completed (as its streaming, we wait for the end of responses, if it has no tool calls, then everything is done and nothing is done after, so user can send another messages again

But if it has any tool call requested by the LLM, we create a message per tool calls requested and publish those to the rabbitMQ and then we are done, next time the consumer picks those tool calls messages asynchronously, each processing one of those tool calls, first sends a message to the front-end in websocket, then after completing the tool call, sends another message to tell front-end its completed in websocket, after each tool call completed, we may have all tool calls for the caller message completed, so if all completed, we publish another message to the rabbitMQ so the LLM will be called again and loads the history, then the tool calls results are ready to be passed to the LLM and it can continue the loop by calling other tool calls or stop it by just sending string content and stop the loop
