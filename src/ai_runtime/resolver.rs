//! Resolve a plugin's direct package pins into a complete transitive uv lock.
//! Never import plugin code or mutate a live Python environment during resolution.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

use super::{layer::validate_lock, overlay::LockedPackage};

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("invalid resolution request: {0}")]
    Invalid(String),
    #[error("dependency conflict: {0}")]
    Conflict(String),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv resolution timed out")]
    Timeout,
    #[error("uv dependency resolution failed: {0}")]
    Uv(String),
    #[error("unsupported uv lock syntax: {0}")]
    Unsupported(String),
}

#[derive(Clone)]
pub struct DependencyResolver {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl DependencyResolver {
    pub async fn resolve_requirements(
        &self,
        requirements: &[String],
        constraints: &[LockedPackage],
        python_version: &str,
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        for requirement in requirements {
            validate_requirement(requirement)?;
        }
        validate_lock(constraints, python_version)
            .map_err(|e| ResolveError::Invalid(e.to_string()))?;
        if requirements.is_empty() {
            return Ok(Vec::new());
        }
        if !self.uv_executable.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute()
            || self.timeout.is_zero()
            || !self.uv_executable.is_file()
            || !self.python_executable.is_file()
        {
            return Err(ResolveError::Invalid(
                "managed Python, uv, and absolute paths required".into(),
            ));
        }
        let staging = self
            .cache_directory
            .join("resolution")
            .join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&staging).await?;
        let outcome = self
            .resolve_in_requirements(&staging, requirements, constraints)
            .await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let packages = outcome?;
        let backend_names: HashMap<_, _> = constraints
            .iter()
            .map(|p| (normalize(&p.name), p.version.as_str()))
            .collect();
        for package in &packages {
            if let Some(expected) = backend_names.get(&normalize(&package.name))
                && package.version != *expected
            {
                return Err(ResolveError::Conflict(format!(
                    "{}: backend {} vs resolved {}",
                    package.name, expected, package.version
                )));
            }
        }
        Ok(packages
            .into_iter()
            .filter(|p| !backend_names.contains_key(&normalize(&p.name)))
            .collect())
    }

    async fn resolve_in_requirements(
        &self,
        staging: &Path,
        requirements: &[String],
        constraints: &[LockedPackage],
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        let input = staging.join("requirements.in");
        let output = staging.join("requirements.lock");
        let constraint_file = staging.join("backend-constraints.txt");
        tokio::fs::write(&input, requirements.join("\n") + "\n").await?;
        tokio::fs::write(&constraint_file, serialize_pins(constraints)).await?;
        let mut cmd = Command::new(&self.uv_executable);
        cmd.args([
            "pip",
            "compile",
            "--no-config",
            "--no-python-downloads",
            "--python",
        ])
        .arg(&self.python_executable)
        .arg("--output-file")
        .arg(&output)
        .arg("--constraint")
        .arg(&constraint_file)
        .arg(&input)
        .env("UV_CACHE_DIR", &self.cache_directory)
        .env_remove("VIRTUAL_ENV")
        .env_remove("UV_PROJECT_ENVIRONMENT")
        .current_dir(staging)
        .kill_on_drop(true);
        let result = timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| ResolveError::Timeout)??;
        if !result.status.success() {
            return Err(ResolveError::Uv(
                String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .take(1600)
                    .collect(),
            ));
        }
        parse_uv_lock(&tokio::fs::read_to_string(output).await?)
    }

    pub async fn resolve(
        &self,
        direct: &[LockedPackage],
        constraints: &[LockedPackage],
        python_version: &str,
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        if !self.uv_executable.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute()
            || self.timeout.is_zero()
        {
            return Err(ResolveError::Invalid(
                "absolute paths and a nonzero timeout are required".into(),
            ));
        }
        validate_lock(direct, python_version).map_err(|e| ResolveError::Invalid(e.to_string()))?;
        validate_lock(constraints, python_version)
            .map_err(|e| ResolveError::Invalid(e.to_string()))?;
        if direct.is_empty() {
            return Ok(Vec::new());
        }
        let mut backend_names = HashMap::new();
        for pin in constraints {
            backend_names.insert(normalize(&pin.name), pin.version.as_str());
        }
        for pin in direct {
            if let Some(expected) = backend_names.get(&normalize(&pin.name))
                && *expected != pin.version
            {
                return Err(ResolveError::Conflict(format!(
                    "{}: backend pins {}, plugin requests {}",
                    pin.name, expected, pin.version
                )));
            }
        }
        if !self.uv_executable.is_file() || !self.python_executable.is_file() {
            return Err(ResolveError::Invalid(
                "managed uv or Python is missing".into(),
            ));
        }
        let root = self.cache_directory.join("resolution");
        tokio::fs::create_dir_all(&root).await?;
        let staging = root.join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&staging).await?;
        let outcome = self.resolve_in(&staging, direct, constraints).await;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let packages = outcome?;
        for package in &packages {
            if let Some(expected) = backend_names.get(&normalize(&package.name))
                && package.version != *expected
            {
                return Err(ResolveError::Conflict(format!(
                    "{}: backend {} vs resolved {}",
                    package.name, expected, package.version
                )));
            }
        }
        Ok(packages
            .into_iter()
            .filter(|package| !backend_names.contains_key(&normalize(&package.name)))
            .collect())
    }

    async fn resolve_in(
        &self,
        staging: &Path,
        direct: &[LockedPackage],
        constraints: &[LockedPackage],
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        let input = staging.join("requirements.in");
        let output_path = staging.join("requirements.lock");
        let constraint_path = staging.join("backend-constraints.txt");
        tokio::fs::write(&input, serialize_pins(direct)).await?;
        tokio::fs::write(&constraint_path, serialize_pins(constraints)).await?;
        let mut cmd = Command::new(&self.uv_executable);
        cmd.args([
            "pip",
            "compile",
            "--no-config",
            "--no-python-downloads",
            "--python",
        ])
        .arg(&self.python_executable)
        .arg("--output-file")
        .arg(&output_path)
        .arg("--constraint")
        .arg(&constraint_path)
        .arg(&input)
        .env("UV_CACHE_DIR", &self.cache_directory)
        .env_remove("VIRTUAL_ENV")
        .env_remove("UV_PROJECT_ENVIRONMENT")
        .current_dir(staging)
        .kill_on_drop(true);
        let result = timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| ResolveError::Timeout)??;
        if !result.status.success() {
            return Err(ResolveError::Uv(
                String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .take(1600)
                    .collect(),
            ));
        }
        let lock = tokio::fs::read_to_string(&output_path).await?;
        let packages = parse_uv_lock(&lock)?;
        let resolved: HashMap<_, _> = packages
            .iter()
            .map(|p| (normalize(&p.name), p.version.as_str()))
            .collect();
        for direct_pin in direct {
            if resolved.get(&normalize(&direct_pin.name)).copied()
                != Some(direct_pin.version.as_str())
            {
                return Err(ResolveError::Unsupported(format!(
                    "missing direct pin {}=={}",
                    direct_pin.name, direct_pin.version
                )));
            }
        }
        Ok(packages)
    }
}

pub fn validate_requirement(requirement: &str) -> Result<(), ResolveError> {
    let text = requirement.trim();
    if text.is_empty()
        || text.len() > 160
        || text != requirement
        || !text.is_ascii()
        || text.bytes().any(|b| {
            !(b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'_' | b'.' | b'[' | b']' | b'<' | b'>' | b'=' | b'!' | b',' | b'+'
                ))
        })
        || !text.bytes().next().unwrap().is_ascii_alphanumeric()
    {
        return Err(ResolveError::Invalid(format!(
            "unsafe requirement: {requirement}"
        )));
    }
    let name_end = text
        .find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
        .unwrap_or(text.len());
    if name_end == 0 || text[name_end..].contains('[') && !text[name_end..].contains(']') {
        return Err(ResolveError::Invalid("invalid package requirement".into()));
    }
    Ok(())
}

fn normalize(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', '_', '.'], "-")
}

fn serialize_pins(pins: &[LockedPackage]) -> String {
    pins.iter()
        .map(|p| format!("{}=={}\n", p.name, p.version))
        .collect()
}

fn parse_uv_lock(input: &str) -> Result<Vec<LockedPackage>, ResolveError> {
    let mut result = Vec::new();
    let mut known = HashSet::new();
    for line in input.lines() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if text.starts_with('-')
            || text.starts_with('\\')
            || text.contains(';')
            || text.contains('@')
        {
            return Err(ResolveError::Unsupported(text.into()));
        }
        let (name, version) = text
            .split_once("==")
            .ok_or_else(|| ResolveError::Unsupported(text.into()))?;
        if name.is_empty()
            || version.is_empty()
            || version.contains('=')
            || version.contains(char::is_whitespace)
        {
            return Err(ResolveError::Unsupported(text.into()));
        }
        let pin = LockedPackage {
            name: name.into(),
            version: version.into(),
        };
        validate_lock(std::slice::from_ref(&pin), "3.13.5")
            .map_err(|_| ResolveError::Unsupported(text.into()))?;
        if !known.insert(normalize(&pin.name)) {
            return Err(ResolveError::Unsupported(format!("duplicate {name}")));
        }
        result.push(pin);
    }
    if result.len() > 256 {
        return Err(ResolveError::Unsupported("too many packages".into()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_pins_and_rejects_unsupported_markers() {
        let pins = parse_uv_lock("# compiled by uv\nhttpx==0.28.1\n    # via test\nanyio==4.9.0\n")
            .unwrap();
        assert_eq!(pins.len(), 2);
        assert!(parse_uv_lock("some-pkg==1.0 ; python_version > '3.11'").is_err());
        assert!(parse_uv_lock("a @ https://example.test/a.whl").is_err());
        assert!(parse_uv_lock("--index-url https://evil.test").is_err());
        assert!(parse_uv_lock("httpx==1.0\nhttpx==2.0\n").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_preserves_backend_and_extracts_transitive_pins() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("nm-resolve-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(&uv, "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do if [ \"$1\" = --output-file ]; then shift; out=$1; fi; shift; done\nprintf 'torch==2.8.0\\ntransformers==4.45.0\\ntokenizers==0.21.0\\n' > \"$out\"\n").await.unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let python = root.join("python");
        tokio::fs::write(&python, "fake").await.unwrap();
        let resolver = DependencyResolver {
            uv_executable: uv,
            python_executable: python,
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(4),
        };
        let pins = resolver
            .resolve(
                &[LockedPackage {
                    name: "transformers".into(),
                    version: "4.45.0".into(),
                }],
                &[LockedPackage {
                    name: "torch".into(),
                    version: "2.8.0".into(),
                }],
                "3.13.5",
            )
            .await
            .unwrap();
        assert_eq!(pins.len(), 2);
        assert!(pins.iter().any(|x| x.name == "tokenizers"));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
