mod action;
mod api;
mod bootstrap;
mod command;
mod flow;
mod gateway;
mod projection;
mod query;
mod runtime;
mod schema;
mod timer;

pub fn all_briefs() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::BRIEF),
        (action::TITLE, action::BRIEF),
        (api::TITLE, api::BRIEF),
        (command::TITLE, command::BRIEF),
        (flow::TITLE, flow::BRIEF),
        (gateway::TITLE, gateway::BRIEF),
        (projection::TITLE, projection::BRIEF),
        (query::TITLE, query::BRIEF),
        (runtime::TITLE, runtime::BRIEF),
        (schema::TITLE, schema::BRIEF),
        (timer::TITLE, timer::BRIEF),
    ];
    ROWS
        .iter()
        .map(|(title, brief)| format!("### {title}\n\n{brief}\n"))
        .collect()
}

pub fn all_details() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::DETAIL),
        (action::TITLE, action::DETAIL),
        (api::TITLE, api::DETAIL),
        (command::TITLE, command::DETAIL),
        (flow::TITLE, flow::DETAIL),
        (gateway::TITLE, gateway::DETAIL),
        (projection::TITLE, projection::DETAIL),
        (query::TITLE, query::DETAIL),
        (runtime::TITLE, runtime::DETAIL),
        (schema::TITLE, schema::DETAIL),
        (timer::TITLE, timer::DETAIL),
    ];
    ROWS
        .iter()
        .map(|(title, detail)| format!("### {title}\n\n{detail}\n"))
        .collect()
}
