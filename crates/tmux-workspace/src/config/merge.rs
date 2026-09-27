//! Resolving YAML merge keys before the model reads a document. `yaml-rust2`
//! does not expand `<<: *anchor` on its own, so every merge is folded here
//! first, apart from the parsing that reads a mapping's own keys.

use yaml_rust2::Yaml;

/// Resolve `<<: *anchor` and `<<: [*a, *b]` merge keys at every level, as
/// `PyYAML`'s safe loader does. Merged keys are folded in first-source-wins
/// order, then keys the mapping wrote itself override whatever the merge
/// produced, regardless of where `<<` appeared among the document's own keys.
/// A source that is not itself a mapping contributes nothing.
pub(super) fn resolve_merges(value: &Yaml) -> Yaml {
    match value {
        Yaml::Hash(entries) => {
            let mut own = Vec::new();
            let mut sources = Vec::new();
            for (key, value) in entries {
                if key.as_str() == Some("<<") {
                    match value {
                        Yaml::Array(items) => sources.extend(items.iter().map(resolve_merges)),
                        other => sources.push(resolve_merges(other)),
                    }
                    continue;
                }
                own.push((key.clone(), resolve_merges(value)));
            }
            let mut merged = yaml_rust2::yaml::Hash::new();
            for source in sources {
                if let Yaml::Hash(fields) = source {
                    for (key, value) in fields {
                        merged.entry(key).or_insert(value);
                    }
                }
            }
            for (key, value) in own {
                merged.insert(key, value);
            }
            Yaml::Hash(merged)
        }
        Yaml::Array(items) => Yaml::Array(items.iter().map(resolve_merges).collect()),
        other => other.clone(),
    }
}
