# Chat session queue

The API publishes work to `chat_session_exchange`. `chat_worker` consumes
`chat_session_work_queue`. `queue_setup_http.sh` creates these resources.

## One queue message

One message contains at most:

- One LLM generation pass
- Content retrieval for that pass
- Tool-result retrieval needed to build the model context

Tool execution is separate work. The worker publishes each tool call and each
later LLM continuation as another queue message. It acknowledges a message
after that step finishes.

`CHAT_CONSUMER_TIMEOUT_SECS` must cover the slowest single message. It does not
need to cover a complete multi-tool conversation.

## Consumer timeout

`queue_setup_http.sh` converts `CHAT_CONSUMER_TIMEOUT_SECS` to milliseconds and
sets RabbitMQ `x-consumer-timeout` on the work queue. If a worker does not
acknowledge the message in time, RabbitMQ closes the channel and can redeliver
the message.

Allow time for a slow model, extended thinking, and a large context.

RabbitMQ cannot update this argument on an existing queue declaration. If the
value changes, delete and recreate `chat_session_work_queue`, then run
`queue_setup_http.sh` again.

The setup script loads `.env`, then `.env.local` if it exists.

## Message flow

1. The API stores the user message and publishes a serialized chat work
   message to RabbitMQ. The work message identifies the assistant response.
2. A chat worker loads the message and history from PostgreSQL.
3. The worker publishes WebSocket state through Redis.
4. The worker calls the selected LLM and streams the response.
5. If the LLM returns text only, the worker completes the message.
6. If the LLM requests tools, the worker creates and publishes one message for
   each tool call.
7. Workers process the tool calls and publish their progress.
8. After all sibling tool calls finish, the worker publishes an LLM
   continuation.
9. The next LLM pass loads the tool results and either requests more tools or
   completes the response.

See [Architecture](architecture.md) and [Chat agents](chat-agents.md).
