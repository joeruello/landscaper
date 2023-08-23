use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct  
    Component {
        api_version: String,
        kind: String,
        pub metadata: EntiyMetadata,
        pub spec: ComponentSpec,
}

#[derive(Serialize, Deserialize)]
pub struct ComponentSpec {
    #[serde(rename = "type")]
    pub _type: String,
    pub lifecycle: String,
    pub owner: String,
}

#[derive(Serialize, Deserialize)]
pub struct EntiyMetadata {
    pub name: String,
    pub description: String,
    pub annotations: HashMap<String, String>,
}

impl Component {
    pub fn new(name: impl Into<String>, descripton: impl Into<String>) -> Self {
        Self {
            api_version: "backstage.io/v1alpha1".to_owned(),
            kind: "Component".to_owned(),
            spec: ComponentSpec {
                _type: "service".to_owned(),
                lifecycle: "experimental".to_owned(),
                owner: "hipages".to_owned(),
            },
            metadata: EntiyMetadata {
                name: name.into(),
                description: descripton.into(),
                annotations: HashMap::new(),
            },
        }
    }
}
