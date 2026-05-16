# Agent missions

We use several specialized agents in chat, each with a distinct role:

| Agent | Role |
| --- | --- |
| **Planner** | The main coordinator: keeps overall direction and integrates outputs from the other agents. |
| **Current Architecture Analyzer** | Inspects and explains the existing system (structure, dependencies, behavior). |
| **Solution Analyzer** | Evaluates approaches, trade-offs, and fit with constraints before implementation. |
| **Programmer** | Implements or edits code according to the agreed plan. |
| **Observability Analyzer** | Reads traces/logs/symptoms, pulls live unit definitions via the unit loader, and returns diagnostics (ranked hypotheses, evidence, next checks). |

## Why we split work across agents

The purpose of using agents in our chat is to delegate complex decisions to **separate LLM calls** with **separate conversation history**. That way we **keep the main Planner’s context smaller**: each sub-agent receives only what it needs for its task.

Each agent can use **its own tools** in **its own context** (its own history) without polluting or interrupting the Planner’s context and history. Sub-agents can call multiple tools, revise their reasoning, try alternatives, and iterate until they are satisfied with a result. When they finish, they return a **single consolidated response** to the main Planner, which then decides how to proceed.

This separation lets specialized work stay deep and iterative where it helps, while the Planner stays focused on orchestration and the big picture.

## **IMPORTANT** — Environment variables

- Unit code reads user-provided secrets and shared config only via **`sdk.getSecureEnv(key)`** — never hardcoded in source.
- Agents use **`list_project_env_keys`** to see which keys exist (names only, never values).
- **Planner:** Before **implement solution**, if the plan needs user-set variables, verify keys exist; if any are missing, stop and ask the user to add them in the dashboard under **Environment variables** (exact key names), then continue after confirmation.
