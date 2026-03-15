use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub stage_runtime_connected: bool,
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogToolParams {
    pub message: String,
}

fn get_message_from_args(args: &Value) -> &str {
    for key in ["message", "description", "text"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return s;
        }
    }
    "(no message)"
}

pub fn tool_log_message(args: &Value) -> Result<String, String> {
    let msg = get_message_from_args(args);
    eprintln!("[tool log_message] {}", msg);
    tracing::info!("[tool log_message] {}", msg);
    Ok(format!("Logged message: {}", msg))
}

pub fn tool_log_debug(args: &Value) -> Result<String, String> {
    let msg = get_message_from_args(args);
    eprintln!("[tool log_debug] {}", msg);
    tracing::debug!("[tool log_debug] {}", msg);
    Ok(format!("Logged debug: {}", msg))
}

pub fn tool_is_any_stage_connected(context: Option<&ToolContext>) -> Result<String, String> {
    let ctx = context.ok_or_else(|| "Session context not available for is_any_stage_connected".to_string())?;
    let result = json!({ "connected": ctx.stage_runtime_connected });
    Ok(result.to_string())
}

pub fn all_tool_specs() -> Vec<ToolSpec> {
    let params_schema = json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "The message to log"
            }
        },
        "required": ["message"]
    });
    let no_params_schema = json!({
        "type": "object",
        "properties": {},
        "required": []
    });
    vec![
        ToolSpec {
            name: "log_message",
            description: "Log a message at INFO level. Use for notable user or system events.",
            parameters: params_schema.clone(),
        },
        ToolSpec {
            name: "log_debug",
            description: "Log a message at DEBUG level. Use for debugging or verbose information.",
            parameters: params_schema,
        },
        ToolSpec {
            name: "is_any_stage_connected",
            description: "Check whether the current chat session has any stage runtime connected (attached). Use this before suggesting the user to connect a runtime or to create a draft release. Returns connected: true/false.",
            parameters: no_params_schema,
        },
    ]
}

pub fn execute_tool(name: &str, args: &Value, context: Option<&ToolContext>) -> Result<String, String> {
    eprintln!("[tool-debug] execute_tool called: name={:?} args={}", name, args);
    tracing::info!(tool = %name, args = ?args, "Executing tool");
    match name {
        "log_message" => tool_log_message(args),
        "log_debug" => tool_log_debug(args),
        "is_any_stage_connected" => tool_is_any_stage_connected(context),
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

pub fn to_openai_functions() -> Vec<openai::chat::ChatCompletionFunctionDefinition> {
    all_tool_specs()
        .into_iter()
        .map(|t| openai::chat::ChatCompletionFunctionDefinition {
            name: t.name.to_string(),
            description: Some(t.description.to_string()),
            parameters: Some(t.parameters),
        })
        .collect()
}

pub fn to_gemini_declarations() -> Vec<gemini_rust::tools::model::FunctionDeclaration> {
    use gemini_rust::tools::model::FunctionDeclaration;
    all_tool_specs()
        .into_iter()
        .map(|t| FunctionDeclaration::new(t.name, t.description, None))
        .collect()
}
