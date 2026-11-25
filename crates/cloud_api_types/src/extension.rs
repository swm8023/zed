use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strum::EnumString;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ExtensionApiManifest {
    pub name: String,
    pub version: Arc<str>,
    pub description: Option<String>,
    pub authors: Vec<String>,
    pub repository: String,
    pub schema_version: Option<i32>,
    pub wasm_api_version: Option<String>,
    #[serde(default)]
    pub provides: BTreeSet<ExtensionProvides>,
}

#[derive(Clone, Deserialize)]
pub struct ExtensionMetadata {
    pub id: Arc<str>,
    #[serde(flatten)]
    pub manifest: ExtensionApiManifest,
}

#[derive(
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    EnumString,
    strum::Display,
    strum::EnumIter,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ExtensionProvides {
    Languages,
    Themes,
    IconThemes,
    Grammars,
    LanguageServers,
    DebugAdapters,
    /// Deprecated
    ContextServers,
    /// Deprecated
    AgentServers,
    /// Deprecated
    SlashCommands,
    /// Deprecated
    IndexedDocsProviders,
    Snippets,
}

impl ExtensionProvides {
    pub fn is_deprecated(&self) -> bool {
        matches!(
            self,
            ExtensionProvides::AgentServers
                | ExtensionProvides::ContextServers
                | ExtensionProvides::SlashCommands
                | ExtensionProvides::IndexedDocsProviders
        )
    }
}
