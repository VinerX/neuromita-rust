//! Verify installed wheel metadata before a package layer is activated or reused.

use std::{collections::HashMap, path::Path};

use super::overlay::LockedPackage;

fn normalized(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '.'], "-")
}

pub fn inspect(path: &Path, expected: &[LockedPackage]) -> Result<(), String> {
    let mut distributions = HashMap::new();
    let entries = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".dist-info") {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_dir() || entry.path().is_symlink() {
            return Err(format!("invalid distribution directory {name}"));
        }
        let metadata_path = entry.path().join("METADATA");
        let info = std::fs::symlink_metadata(&metadata_path).map_err(|error| error.to_string())?;
        if !info.is_file() || info.len() > 256 * 1024 {
            return Err(format!("invalid wheel metadata {name}"));
        }
        let text = std::fs::read_to_string(metadata_path).map_err(|error| error.to_string())?;
        let mut package_name = None;
        let mut version = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("Name: ") {
                package_name = Some(value.trim());
            }
            if let Some(value) = line.strip_prefix("Version: ") {
                version = Some(value.trim());
            }
            if package_name.is_some() && version.is_some() {
                break;
            }
        }
        let (Some(package_name), Some(version)) = (package_name, version) else {
            return Err(format!("missing wheel identity in {name}"));
        };
        if distributions
            .insert(normalized(package_name), version.to_owned())
            .is_some()
        {
            return Err(format!("duplicate wheel metadata for {package_name}"));
        }
    }
    if distributions.len() != expected.len() {
        return Err(format!(
            "installed distribution count {} differs from lock {}",
            distributions.len(),
            expected.len()
        ));
    }
    for package in expected {
        match distributions.get(&normalized(&package.name)) {
            Some(actual) if actual == &package.version => (),
            Some(actual) => {
                return Err(format!(
                    "{} installed {actual}, locked {}",
                    package.name, package.version
                ));
            }
            None => return Err(format!("missing installed distribution {}", package.name)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn catches_missing_mismatched_and_unexpected_distributions() {
        let root = std::env::temp_dir().join(format!("nm-distributions-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let lock = [LockedPackage {
            name: "some_pkg".into(),
            version: "1.2.3".into(),
        }];
        assert!(inspect(&root, &lock).is_err());
        let wheel = root.join("some_pkg-1.2.3.dist-info");
        std::fs::create_dir_all(&wheel).unwrap();
        std::fs::write(
            wheel.join("METADATA"),
            "Metadata-Version: 2.1\nName: some-pkg\nVersion: 1.2.3\n",
        )
        .unwrap();
        assert!(inspect(&root, &lock).is_ok());
        assert!(
            inspect(
                &root,
                &[LockedPackage {
                    name: "some_pkg".into(),
                    version: "9.0".into()
                }]
            )
            .is_err()
        );
        let extra = root.join("extra-1.dist-info");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("METADATA"), "Name: extra\nVersion: 1\n").unwrap();
        assert!(inspect(&root, &lock).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
