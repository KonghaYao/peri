use serde_json::Value;

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;

#[path = "resources/resource_configuration.rs"]
mod resource_configuration;
pub use resource_configuration::resolve;
pub use resource_configuration::ResourceConfiguration;
