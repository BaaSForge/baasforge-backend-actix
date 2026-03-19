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
