use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_STORAGE_STATUS: &str = "storage.status";
pub const METHOD_STORAGE_UPDATE: &str = "storage.update";
pub const METHOD_STORAGE_CLEAR_CACHE: &str = "storage.clearCache";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageUpdateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_gb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatusResult {
    pub cache_bytes: u64,
    pub limit_gb: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
}
