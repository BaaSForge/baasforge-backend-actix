use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActorUnitMetadata {
    pub state: TypeScriptJsonSchemaPair,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub handler_inputs: HashMap<String, TypeScriptJsonSchemaPair>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub handler_outputs: HashMap<String, TypeScriptJsonSchemaPair>,
}
