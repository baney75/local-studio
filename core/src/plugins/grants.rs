use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::install::live_hash;
use super::{Capability, Plugin};

pub const GRANTS: &str = "grants.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    #[serde(default)]
    plugins: BTreeMap<String, Grant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    version: String,
    capabilities: BTreeSet<String>,
    #[serde(default)]
    tree: String,
    #[serde(default)]
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consent {
    Ready,
    NeedsConsent { missing: Vec<Capability> },
    Refused { reason: String },
}

impl Grants {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(GRANTS);
        let object = crate::workspace::providers::read_object(&path)?;
        serde_json::from_value(Value::Object(object)).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        crate::workspace::providers::write_private(&root.join(GRANTS), &value)
    }

    #[must_use]
    pub fn consent(&self, plugin: &Plugin) -> Consent {
        if plugin.capabilities.is_empty() {
            return Consent::Ready;
        }
        let tree = match live_hash(&plugin.dir) {
            Ok(tree) => tree,
            Err(reason) => return Consent::Refused { reason },
        };
        let granted = self
            .plugins
            .get(&plugin.id)
            .filter(|grant| !grant.tree.is_empty() && grant.tree == tree)
            .map(|grant| &grant.capabilities);
        let missing: Vec<Capability> = plugin
            .capabilities
            .iter()
            .copied()
            .filter(|capability| !granted.is_some_and(|granted| granted.contains(capability.name())))
            .collect();
        if missing.is_empty() {
            Consent::Ready
        } else {
            Consent::NeedsConsent { missing }
        }
    }

    #[must_use]
    pub fn granted(&self, id: &str) -> Option<(&str, &str, Vec<&str>)> {
        self.plugins.get(id).map(|grant| {
            (grant.version.as_str(), grant.source.as_str(), grant.capabilities.iter().map(String::as_str).collect())
        })
    }

    pub fn installed(&mut self, plugin: &Plugin, tree: &str, source: &str) -> Option<String> {
        let asked = |name: &String| Capability::parse(name).is_some_and(|capability| plugin.asks(capability));
        // A tree with no source is a slice 2 grant renewed after the upgrade: its origin is unknown, so only a catalog update keeps it.
        let unknown = |grant: &Grant| grant.source.is_empty() && !grant.tree.is_empty();
        let catalog = source == format!("catalog {}", plugin.id);
        let (capabilities, kept_source, note) = match self.plugins.get(&plugin.id) {
            Some(grant) if grant.tree == tree => (grant.capabilities.clone(), grant.source.clone(), None),
            Some(grant) if grant.source == source || (catalog && unknown(grant)) => {
                let kept: BTreeSet<String> = grant.capabilities.iter().filter(|name| asked(name)).cloned().collect();
                let dropped: Vec<&str> =
                    grant.capabilities.iter().filter(|name| !asked(name)).map(String::as_str).collect();
                let note = (!dropped.is_empty()).then(|| {
                    format!(
                        "{} {} no longer asks for {}, so that grant is gone",
                        plugin.id,
                        plugin.version,
                        dropped.join(", ")
                    )
                });
                (kept, source.to_string(), note)
            }
            Some(grant) if !grant.capabilities.is_empty() => {
                let names: Vec<&str> = grant.capabilities.iter().map(String::as_str).collect();
                let note = format!(
                    "dropped the grants for {} ({}): they were given to code from {}, and this copy came from {source}",
                    plugin.id,
                    names.join(", "),
                    if grant.source.is_empty() { "an earlier Local Studio" } else { grant.source.as_str() }
                );
                (BTreeSet::new(), source.to_string(), Some(note))
            }
            _ => (BTreeSet::new(), source.to_string(), None),
        };
        self.plugins.insert(
            plugin.id.clone(),
            Grant { version: plugin.version.clone(), capabilities, tree: tree.to_string(), source: kept_source },
        );
        note
    }

    pub fn grant(&mut self, plugin: &Plugin, capabilities: &[Capability]) -> Result<(), String> {
        if let Some(extra) = capabilities.iter().find(|capability| !plugin.asks(**capability)) {
            return Err(format!("{} {} does not ask for {}", plugin.id, plugin.version, extra.name()));
        }
        let tree = live_hash(&plugin.dir)?;
        let record = self.plugins.get(&plugin.id);
        let current = record.filter(|grant| grant.tree == tree);
        // A slice 2 record has no tree: keep its source (empty, unknown origin) rather than invent one.
        let source = match record.filter(|grant| grant.tree == tree || grant.tree.is_empty()) {
            Some(grant) => grant.source.clone(),
            None => format!("folder {}", super::absolute(&plugin.dir).display()),
        };
        let kept: BTreeSet<String> = current
            .map(|grant| grant.capabilities.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|name| Capability::parse(name).is_some_and(|capability| plugin.asks(capability)))
            .chain(capabilities.iter().map(|capability| capability.name().to_string()))
            .collect();
        self.plugins
            .insert(plugin.id.clone(), Grant { version: plugin.version.clone(), capabilities: kept, tree, source });
        Ok(())
    }

    pub fn revoke(&mut self, id: &str, capabilities: &[Capability]) -> bool {
        let Some(grant) = self.plugins.get_mut(id) else { return false };
        let before = grant.capabilities.len();
        grant.capabilities.retain(|name| {
            !capabilities.is_empty() && !capabilities.iter().any(|capability| capability.name() == name)
        });
        grant.capabilities.len() != before
    }
}
