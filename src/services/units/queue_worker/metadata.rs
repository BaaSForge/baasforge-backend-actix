use serde::{Deserialize, Serialize};

use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueWorkerUnitMetadata {
    pub input: TypeScriptJsonSchemaPair,
    pub output: TypeScriptJsonSchemaPair,
    pub batch_output: TypeScriptJsonSchemaPair,
}
