use crate::AllowDenyRequirementToml;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ComputerUseConfigToml {
    pub default_app_access: Option<AllowDenyRequirementToml>,
    pub macos: Option<ComputerUseMacosConfigToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ComputerUseMacosConfigToml {
    pub bundle_ids: Option<BTreeMap<String, AllowDenyRequirementToml>>,
}

#[cfg(test)]
#[path = "computer_use_tests.rs"]
mod tests;
