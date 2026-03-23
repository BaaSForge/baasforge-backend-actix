pub const TITLE: &str = "Flow";

pub const BRIEF: &str = r#"Flow implements long-running processes: sagas that listen for events, remember process state, schedule follow-up work, call compensations on failure, and sometimes trigger side effects via other units such as Action."#;

pub const DETAIL: &str = r#"What it is for: Flow coordinates multi-step business processes that unfold over time—reservations that expire, payments that may fail, notifications that should fire after certain facts, and cleanup that spans several events.

How others use it: Commands and other producers emit events; Flow subscribes and advances orchestration. Timer units may emit ticks that Flow handles. Action units perform external side effects when Flow requests them indirectly via bus subjects.

How it uses others: Flow uses flow context (including stream and storage helpers as designed), reads and writes saga or index state distinct from canonical entity tables, consumes schema event types, and may publish commands or action triggers.

Rules and restrictions: Flow holds process state, not the authoritative lifecycle of a single aggregate—that remains event plus projection territory. Compensations and timeouts should be explicit to avoid stuck processes. Keep orchestration readable; avoid hiding business decisions that belong in Command.

What it exports: Event- and timer-driven handlers that advance or roll back coordinated workflows.

Real-world examples: Reserve inventory then capture payment within a timeout, releasing stock if payment fails; notify dashboards when orders change; nightly purge of expired sessions driven by timer ticks; trip booking saga across flights, hotels, and insurance in a travel backend."#;
