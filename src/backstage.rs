use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
#[serde(rename_all = "camelCase")]
pub struct ComponentSpec {
    #[serde(rename = "type")]
    pub _type: String,
    pub lifecycle: String,
    pub owner: String,
    #[serde(default,skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default,skip_serializing_if = "BTreeSet::is_empty")]
    pub depends_on: BTreeSet<String>,
    #[serde(default,skip_serializing_if = "BTreeSet::is_empty")]
    pub consumes_apis: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EntiyMetadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default,skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<String>
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
                system: None,
                depends_on: BTreeSet::new(),
                consumes_apis: BTreeSet::new(),
            },
            metadata: EntiyMetadata {
                name: name.into(),
                description: descripton.into(),
                annotations: BTreeMap::new(),
                tags: BTreeSet::new()
            },
        }
    }
}
