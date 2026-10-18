# Unit Technical Reviewer

The unit technical reviewer is a second worker binary (`unit_technical_reviewer`) that runs asynchronous sub-task checks on unit tool calls (`create_unit`, `modify_unit`, `remove_unit`). It consumes from its own RabbitMQ queue (`unit_reviewer_work_queue`) independently from the chat worker.

## Why a separate binary

Unit code checks can use significant CPU and memory. A dedicated binary means:

- The reviewer can scale for review traffic without scaling the API or chat worker.
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
| `create_unit_compiler_check` | `create_unit` handler after pre_check success | TypeScript type-check (tsgo) |
| `create_unit_lint_check` | Reviewer after compiler_check succeeds | `no-explicit-any`, no `import * as …`, + type-aware `no-unsafe-type-assertion` (tsgolint; `as ActorInstanceId` / `WorkflowInstanceId` / `QueueWorkerRunId` allowed) |
| `create_unit_rule_check` | Reviewer after lint_check succeeds | Naming-convention and project-level rules; **inserts the `project_units` row on success** |
| `modify_unit_compiler_check` | `modify_unit` handler after pre_check success | Type-check for revised code |
| `modify_unit_lint_check` | Reviewer after compiler_check succeeds | Same as create lint check |
| `modify_unit_rule_check` | Reviewer after lint_check succeeds | Rule check; **inserts the new revision row on success** |
| `remove_unit_rule_check` | `remove_unit` handler after pre_check success | Rule check (no compiler check for removals); **inserts the deletion marker row on success** |

Each message carries the minimum context for the check:

- `tool_message_id` — ID of the `ToolCreateUnit` / `ToolModifyUnit` / `ToolRemoveUnit` row in `chat_messages`
- `user_id`, `project_id`, `session_id` — for WebSocket broadcasting
- `code` — NodeJS source to check (empty string for remove checks)
- `unit_type`, `context_slug`, `module_slug`, `unit_name_slug` — unit identity for rule checks

The reviewer loads the full payload (`ToolCreateUnitPayload`, etc.) directly from the DB row identified by `tool_message_id`. This gives it access to all `args` fields (`explanation`, typed `unit_type`, slugs, code) needed for the `project_units` insert, without duplicating them in the queue message.

## Unit rows are written only after all checks pass

**No `project_units` row is ever written by a tool handler.** Handlers only validate (slugs, duplicates, parent existence) and dispatch to the reviewer. The actual insert happens in the reviewer's final check:

| Tool | Where the row is written |
|---|---|
| `create_unit` | Reviewer `create_unit_rule_check` success |
| `modify_unit` | Reviewer `modify_unit_rule_check` success |
| `remove_unit` | Reviewer `remove_unit_rule_check` success (deletion marker: `is_deleted = true`) |

If any check fails, no row is written and the LLM receives the failure details in its history.

## End-to-end flow for `create_unit`

```
LLM requests create_unit
        │
        ▼
chat worker runner: inserts chat_messages row (status=Generating), spawns handler
        │
        ▼
create_unit::handler
  ├─ pre_check fails (bad slug / duplicate)
  │     updates chat_messages row: status=Completed,
  │         pre_check_sub_task=Failed, error set, created_unit=None
  │     broadcasts ChatSessionMessageUpdated via WS
  │     calls maybe_complete_invoking_message directly → LLM continues
  │
  └─ pre_check succeeds (slugs valid, no duplicate found)
        NO project_units insert
        updates chat_messages row: status=Generating,
            pre_check_sub_task=Succeeded, compiler/rule=Waiting, created_unit=None
        broadcasts ChatSessionMessageUpdated via WS
        publishes create_unit_compiler_check → unit_reviewer_work_queue
        returns Ok() — message stays Generating, LLM waits
        │
        ▼
unit_technical_reviewer: receives create_unit_compiler_check
  ├─ check fails
  │     updates row: compiler_check_sub_task=Failed, error set, status=Completed
  │     broadcasts ChatSessionMessageUpdated via WS
  │     calls maybe_complete_invoking_message → LLM continues (no unit in DB)
  │
  └─ check succeeds
        updates row: compiler_check_sub_task=Succeeded, status=Generating
        broadcasts ChatSessionMessageUpdated via WS
        publishes create_unit_lint_check → unit_reviewer_work_queue
        │
        ▼
unit_technical_reviewer: receives create_unit_lint_check
  ├─ check fails → lint_check_sub_task=Failed, status=Completed, LLM continues
  └─ check succeeds → lint_check_sub_task=Succeeded, publishes create_unit_rule_check
        │
        ▼
unit_technical_reviewer: receives create_unit_rule_check
  ├─ check fails
  │     updates row: rule_check_sub_task=Failed, error set, status=Completed
  │     broadcasts ChatSessionMessageUpdated via WS
  │     calls maybe_complete_invoking_message → LLM continues (no unit in DB)
  │
  └─ check succeeds
        *** inserts project_units row (first and only write) ***
        sets payload.created_unit = Some(ProjectUnitResponse)
        updates row: rule_check_sub_task=Succeeded, created_unit set, status=Completed
        broadcasts ChatSessionMessageUpdated via WS
        broadcasts ProjectUnitCreated via WS
        calls maybe_complete_invoking_message → LLM continues
```

The flows for `modify_unit` (compiler_check → lint_check → rule_check → insert new revision) and `remove_unit` (rule_check only → insert deletion marker) follow the same pattern.

## LLM continuation: who calls `maybe_complete_invoking_message`

`ToolCreateUnit`, `ToolModifyUnit`, and `ToolRemoveUnit` are listed in `tool_message_type_skips_generic_invoker_completion`. This means the chat worker's generic `on_tool_message_finished` hook is **not** called after these handlers return — the handlers and the reviewer manage the LLM continuation themselves:

| Situation | Who triggers LLM continuation |
|---|---|
| pre_check fails → message set to `Completed` | Handler calls `maybe_complete_invoking_message` before returning |
| pre_check succeeds → dispatched to reviewer | Nobody — message stays `Generating`, LLM waits |
| Reviewer final check (success or failure) | Reviewer calls `maybe_complete_invoking_message` after setting status to `Completed` |

`maybe_complete_invoking_message` only fires the LLM continuation when **all** tool siblings for the invoking AI Text message are `Completed` or `Failed`. A tool in `Generating` state blocks the trigger.

## WebSocket events

The client receives incremental updates as each sub-task completes:

1. `ChatSessionMessageUpdated` — broadcast by the handler after pre_check, showing `pre_check_sub_task=Succeeded` and checks `Waiting`.
2. `ChatSessionMessageUpdated` — after compiler_check (`compiler_check_sub_task`).
3. `ChatSessionMessageUpdated` — after lint_check (`lint_check_sub_task`).
4. `ChatSessionMessageUpdated` — after rule_check (`rule_check_sub_task`, `status=Completed`, unit fields on success).
5. `ProjectUnitCreated` / `ProjectUnitModified` / `ProjectUnitDeleted` — broadcast by the reviewer **only** on rule_check success, after the row is actually in the database. Never sent on failure.

## Compiler check: real TypeScript type checking

The `compiler_check` sub-task runs a full TypeScript type check against the
LLM-generated code. See [Type checker](type-checker.md) for the complete
breakdown. It uses tsgo with an in-memory virtual file system. The next lint
step uses Oxc and tsgolint. A unit row is written only after all checks pass.


## Running locally

```bash
# One-time queue setup (adds unit_reviewer_exchange + unit_reviewer_work_queue)
./queue_setup_http.sh

# Start 2 API instances, 2 chat workers, 2 reviewers, and 2 cost consumers
./dev-scaled.sh

# Or start a single reviewer instance manually (feature flag is included)
./dev-unit-technical-reviewer.sh

# Stop everything
./dev-stop.sh
```

## Scaling

Because all reviewer instances consume from the same `unit_reviewer_work_queue` with `basic_qos(1)` (prefetch = 1), RabbitMQ load-balances messages across instances. Each check message is delivered to exactly one reviewer instance. The number of instances is set in `dev-scaled.sh` (currently 2); adjust as needed.

See [Local development](local-development.md) for prerequisites and current
restart behavior.