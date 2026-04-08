use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TypeScriptJsonSchemaPair {
    pub typescript: Option<String>,
    pub json_schema: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiUnitMetadata {
    pub method: Option<HttpMethod>,
    pub request_body: TypeScriptJsonSchemaPair,
    pub request_params: TypeScriptJsonSchemaPair,
    pub response_body: TypeScriptJsonSchemaPair,
}
