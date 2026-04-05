# Unit Technical Reviewer

The unit technical reviewer is a second worker binary (`unit_technical_reviewer`) that runs asynchronous sub-task checks on unit tool calls (`create_unit`, `modify_unit`, `remove_unit`). It consumes from its own RabbitMQ queue (`unit_reviewer_work_queue`) independently from the chat worker.

## Why a separate binary

Unit code checks (compiler syntax validation, rule/convention checking) are expected to be non-trivial operations. Offloading them to a dedicated binary means:

- This binary might use v8 engine which is heavy, so it's better to have separated binary with v8 included to have flexibility to scale depending on the overal throughput of these code review messages
- The chat worker is never blocked waiting for a check to finish.
- Multiple reviewer instances can run in parallel and scale independently from the chat worker.
- Each reviewer instance picks up one check message at a time (prefetch = 1), so the same check is never processed twice by two instances simultaneously.

## Queue setup

Run `queue_setup_http.sh` once per environment. It declares:

| Resource | Name | Type |
|---|---|---|
| Exchange | `unit_reviewer_exchange` | topic, durable |
| Work queue | `unit_reviewer_work_queue` | durable |
| Binding | `unit_reviewer.#` → queue | topic routing |

All messages are published with routing key `unit_reviewer.check`, which matches the `unit_reviewer.#` binding.

## Message types

The queue carries `UnitReviewerQueueMessage` (defined in `src/services/unit_technical_reviewer/queue.rs`). The `message_type` field determines which check to run:

| Message type | Triggered by | Description |
|---|---|---|
| `create_unit_compiler_check` | `create_unit` handler after pre_check success | Syntax/compiler check on the unit's TS code |
| `create_unit_rule_check` | Reviewer after compiler_check succeeds | Naming-convention and project-level rules check for `create_unit` |
| `modify_unit_compiler_check` | `modify_unit` handler after pre_check success | Compiler check for a revised unit |
| `modify_unit_rule_check` | Reviewer after compiler_check succeeds | Rule check for `modify_unit` |
| `remove_unit_rule_check` | `remove_unit` handler after pre_check success | Rule check for a removal (no code to compile) |

Each message carries the context needed for the check and for the callback:

- `tool_message_id` — ID of the original `ToolCreateUnit` / `ToolModifyUnit` / `ToolRemoveUnit` row in `chat_messages`
- `user_id`, `project_id`, `session_id` — for WebSocket broadcasting and RabbitMQ routing
- `code` — JavaScript source to check (empty for remove checks)
- `unit_type`, `context_slug`, `module_slug`, `unit_name_slug` — unit identity for rule checks

## End-to-end flow for `create_unit`

```
LLM requests create_unit
        │
        ▼
chat worker: payload_from_args → publishes ToolCreateUnit message
        │
        ▼
chat worker runner: inserts chat_messages row (status=Generating), spawns handler
        │
        ▼
create_unit::handler
  ├─ pre_check fails (bad slug / duplicate)
  │     └─ updates row: status=Completed, pre_check_sub_task=Failed, error set
  │        broadcasts ChatSessionMessageUpdated via WS
  │        returns Ok(()) → on_tool_message_finished → maybe_complete_invoking_message
  │        → LLM continuation published → LLM continues (sees failure in history)
  │
  └─ pre_check succeeds
        inserts project_units row (the real unit)
        updates chat_messages row: status=Generating (unchanged),
            pre_check_sub_task=Succeeded, compiler/rule=Waiting
        broadcasts ChatSessionMessageUpdated via WS
        broadcasts ProjectUnitCreated via WS
        publishes create_unit_compiler_check → unit_reviewer_work_queue
        returns Ok(())
            → on_tool_message_finished → maybe_complete_invoking_message
            → tool message is still Generating → done != total → LLM waits
        │
        ▼
unit_technical_reviewer: receives create_unit_compiler_check
  ├─ check fails
  │     loads tool message row, deserializes ToolCreateUnitPayload
  │     updates row: compiler_check_sub_task=Failed, error set, status=Completed
  │     broadcasts ChatSessionMessageUpdated via WS
  │     calls maybe_complete_invoking_message
  │         → all tool siblings done → AI Text invoker set to Failed/Completed
  │         → LLM continuation published → LLM continues (sees check failure in history)
  │
  └─ check succeeds
        updates row: compiler_check_sub_task=Succeeded, status=Generating (still waiting)
        broadcasts ChatSessionMessageUpdated via WS
        publishes create_unit_rule_check → unit_reviewer_work_queue
        │
        ▼
unit_technical_reviewer: receives create_unit_rule_check
  ├─ check fails
  │     updates row: rule_check_sub_task=Failed, error set, status=Completed
  │     broadcasts ChatSessionMessageUpdated via WS
  │     calls maybe_complete_invoking_message → LLM continues (sees failure)
  │
  └─ check succeeds
        updates row: rule_check_sub_task=Succeeded, status=Completed
        broadcasts ChatSessionMessageUpdated via WS
        calls maybe_complete_invoking_message → LLM continues (all checks passed)
```

## Key invariant: when the LLM continues

`maybe_complete_invoking_message` (in `src/services/chat_worker/llm/tools/mod.rs`) counts child tool rows for the AI Text invoker. It only triggers the LLM continuation when **all** child rows are either `Completed` or `Failed`. Since the reviewer holds the `create_unit` row in `Generating` during checks, the LLM cannot continue prematurely.

When the final check finishes (success **or** failure), the reviewer sets the row to `Completed` and calls `maybe_complete_invoking_message`. At that point the invariant is satisfied and the LLM gets a new queue message to continue its turn.

## WebSocket events

The reviewer publishes `ChatSessionMessageUpdated` after every sub-task transition so the client UI can show real-time progress (e.g. a spinner on each sub-task step). Each event carries the full updated `ChatMessageResponse`, which includes the current `pre_check_sub_task`, `compiler_check_sub_task`, and `rule_check_sub_task` values.

When the unit is first inserted (`create_unit` pre-check succeeds), the handler also publishes `ProjectUnitCreated` so the client can display the new unit immediately, before checks complete.

## Running locally

```bash
# One-time queue setup (adds unit_reviewer_exchange + unit_reviewer_work_queue)
./queue_setup_http.sh

# Start everything (2 API instances, 2 chat workers, 2 reviewers)
./dev-scaled.sh

# Or start a single reviewer instance manually
./dev-unit-technical-reviewer.sh

# Stop everything
./dev-stop.sh
```

## Scaling

Because all reviewer instances consume from the same `unit_reviewer_work_queue` with `basic_qos(1)` (prefetch = 1), RabbitMQ load-balances messages across instances. Each check message is delivered to exactly one reviewer instance. The number of instances is set in `dev-scaled.sh` (currently 2); adjust as needed.