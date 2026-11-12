//! CycloneDX inventory of the locked CLI graph for one target, including build inputs.
use crate::{process::require, read_toml, Release, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    license: Option<String>,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Dependency>,
}
#[derive(Deserialize)]
struct Dependency {
    pkg: String,
    dep_kinds: Vec<Kind>,
}
#[derive(Deserialize)]
struct Kind {
    kind: Option<String>,
}
impl Dependency {
    fn included(&self) -> bool {
        self.dep_kinds
            .iter()
            .any(|kind| kind.kind.is_none() || kind.kind.as_deref() == Some("build"))
    }
}
impl Package {
    fn reference(&self) -> String {
        format!(
            "pkg:cargo/{}@{}",
            self.name,
            self.version.replace('+', "%2B")
        )
    }
}

impl Release {
    pub(crate) fn cli_sbom(&self, target: &str) -> Result<Value> {
        let metadata: Metadata = serde_json::from_slice(&self.runner.run(
            &self.root,
            "cargo",
            args![
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                target
            ],
        )?)?;
        let lock = read_toml(&self.root.join("Cargo.lock"))?;
        inventory(metadata, &lock, target)
    }
}

fn inventory(metadata: Metadata, lock: &toml::Value, target: &str) -> Result<Value> {
    let packages: BTreeMap<_, _> = metadata
        .packages
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let nodes: BTreeMap<_, _> = metadata
        .resolve
        .nodes
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();
    let root = packages
        .values()
        .find(|p| p.name == "groove-cli" && p.source.is_none())
        .ok_or_else(|| crate::Error::Invalid("Missing CLI in Cargo metadata".into()))?;
    let mut included = BTreeSet::new();
    let mut pending = vec![root.id.clone()];
    while let Some(id) = pending.pop() {
        if included.insert(id.clone()) {
            let node = nodes
                .get(&id)
                .ok_or_else(|| crate::Error::Invalid("Missing dependency node".into()))?;
            pending.extend(
                node.deps
                    .iter()
                    .filter(|dep| dep.included())
                    .map(|dep| dep.pkg.clone()),
            );
        }
    }
    let mut components = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    for id in included {
        let package = packages
            .get(&id)
            .ok_or_else(|| crate::Error::Invalid("Missing dependency package".into()))?;
        let reference = package.reference();
        let mut component = json!({
            "type": if id == root.id { "application" } else { "library" },
            "bom-ref": reference, "purl": reference,
            "name": package.name, "version": package.version,
        });
        if let Some(license) = &package.license {
            // Cargo historically accepted '/' as the dual-license separator.
            component["licenses"] = json!([{"expression": license.replace('/', " OR ")}]);
        }
        if let Some(source) = &package.source {
            require(
                source == "registry+https://github.com/rust-lang/crates.io-index",
                "SBOM requires an approved crates.io source",
            )?;
            let checksum = lock
                .get("package")
                .and_then(toml::Value::as_array)
                .and_then(|entries| {
                    entries.iter().find(|entry| {
                        entry.get("name").and_then(toml::Value::as_str) == Some(&package.name)
                            && entry.get("version").and_then(toml::Value::as_str)
                                == Some(&package.version)
                            && entry.get("source").and_then(toml::Value::as_str) == Some(source)
                    })
                })
                .and_then(|entry| entry.get("checksum"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| {
                    crate::Error::Invalid("Missing registry checksum in lockfile".into())
                })?;
            require(
                checksum.len() == 64 && checksum.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid registry checksum",
            )?;
            component["hashes"] = json!([{"alg": "SHA-256", "content": checksum}]);
        }
        let node = &nodes[&id]; // Every included node was validated during traversal.
        let children: BTreeSet<_> = node
            .deps
            .iter()
            .filter(|dep| dep.included())
            .map(|dep| packages[&dep.pkg].reference())
            .collect();
        dependencies.insert(
            reference.clone(),
            json!({"ref": reference, "dependsOn": children}),
        );
        components.insert(reference, component);
    }
    let root_component = components
        .remove(&root.reference())
        .ok_or_else(|| crate::Error::Invalid("Missing root SBOM component".into()))?;
    Ok(json!({
        "bomFormat": "CycloneDX", "specVersion": "1.6", "version": 1,
        "metadata": {"component": root_component, "properties": [
            {"name": "groove:target", "value": target},
            {"name": "groove:scope", "value": "locked normal and build dependencies; excludes dev dependencies"}
        ]},
        "components": components.into_values().collect::<Vec<_>>(),
        "dependencies": dependencies.into_values().collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_includes_build_inputs_and_excludes_test_only_dependencies() {
        let metadata: Metadata = serde_json::from_value(json!({
            "packages": [
                {"id":"cli", "name":"groove-cli", "version":"1.0.0", "source":null, "license":"MIT"},
                {"id":"lib", "name":"library", "version":"2.0.0+build", "source":"registry+https://github.com/rust-lang/crates.io-index", "license":"MIT/Apache-2.0"},
                {"id":"build", "name":"generator", "version":"3.0.0", "source":null, "license":null},
                {"id":"test", "name":"test-only", "version":"4.0.0", "source":null, "license":null}
            ],
            "resolve":{"nodes":[
                {"id":"cli", "deps":[{"pkg":"lib", "dep_kinds":[{"kind":null}]}, {"pkg":"build", "dep_kinds":[{"kind":"build"}]}, {"pkg":"test", "dep_kinds":[{"kind":"dev"}]}]},
                {"id":"lib", "deps":[]}, {"id":"build", "deps":[]}, {"id":"test", "deps":[]}
            ]}
        })).unwrap();
        let checksum = "a".repeat(64);
        let lock = toml::from_str(&format!("[[package]]\nname='library'\nversion='2.0.0+build'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{checksum}'\n")).unwrap();
        let bom = inventory(metadata, &lock, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(bom["metadata"]["component"]["name"], "groove-cli");
        let components = bom["components"].as_array().unwrap();
        assert_eq!(components.len(), 2);
        assert!(components.iter().any(|p| p["name"] == "generator"));
        let lib = components.iter().find(|p| p["name"] == "library").unwrap();
        assert_eq!(lib["purl"], "pkg:cargo/library@2.0.0%2Bbuild");
        assert_eq!(lib["hashes"][0]["content"], checksum);
        assert_eq!(lib["licenses"][0]["expression"], "MIT OR Apache-2.0");
        let root = bom["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["ref"] == "pkg:cargo/groove-cli@1.0.0")
            .unwrap();
        assert_eq!(root["dependsOn"].as_array().unwrap().len(), 2);
        assert!(!bom.to_string().contains("test-only"));
    }
}
