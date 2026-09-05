//! The frozen capability report exposed as one static MCP resource.

use rmcp::ErrorData;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
    ResourceContents,
};

/// The only public resource URI.
pub const CAPABILITIES_URI: &str = "tmux://capabilities";

const JSON: &str = "application/json";

/// List the static capability report.
#[must_use]
pub fn listed() -> ListResourcesResult {
    ListResourcesResult::with_all_items(vec![
        Resource::new(CAPABILITIES_URI, "capabilities")
            .with_description(
                "The startup-frozen effective tool surface and each tool's direct authority.",
            )
            .with_mime_type(JSON),
    ])
}

/// Dynamic resources are outside the shared cross-port contract.
#[must_use]
pub fn templates() -> ListResourceTemplatesResult {
    ListResourceTemplatesResult::with_all_items(Vec::new())
}

/// Serialize the effective capability report.
///
/// # Errors
///
/// Returns an error if the typed report cannot be serialized.
pub fn capabilities(value: &impl serde::Serialize) -> Result<ReadResourceResult, ErrorData> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(body, CAPABILITIES_URI).with_mime_type(JSON),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_the_only_resource() {
        let resources = listed().resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, CAPABILITIES_URI);
        assert!(templates().resource_templates.is_empty());
    }
}
