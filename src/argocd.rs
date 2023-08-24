use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApp {
    pub api_version: String,
    pub metadata: AppMetadata,
    pub spec: AppSpec,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppMetadata {
    pub name: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub annotations: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finalizers: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppSpec {
    pub destination: AppDestination,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppDestination {
    pub server: String,
    pub namespace: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppSource {
    helm: AppHelmConfig,
}

#[derive(Serialize, Deserialize, Debug)]
enum AppHelmConfig {
    ValueFiles(Vec<String>),
    Values(String),
}
