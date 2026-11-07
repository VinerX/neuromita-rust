use std::{
    fs::{File, Metadata},
    io::Read,
    path::{Path, PathBuf},
};

use super::{LauncherConfig, LauncherError, classify_io, path_is_within};

const KNOWN_EXECUTABLES: [&str; 2] = ["NeuroMita.exe", "NeuroMita-Unity.exe"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSelection {
    pub root: PathBuf,
    pub executable: PathBuf,
}

pub(crate) fn resolve_install(config: &LauncherConfig) -> Result<InstallSelection, LauncherError> {
    let root = existing_root(&config.install_root)?;
    let candidate = match &config.candidate {
        Some(candidate) => {
            let candidate = if candidate.is_absolute() {
                candidate.clone()
            } else {
                root.join(candidate)
            };
            verify_candidate(&root, &candidate)?
        }
        None => discover_candidate(&root)?,
    };
    Ok(InstallSelection {
        root,
        executable: candidate,
    })
}

pub(crate) fn existing_root(root: &Path) -> Result<PathBuf, LauncherError> {
    let metadata = std::fs::metadata(root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LauncherError::MissingInstall(root.to_path_buf())
        } else {
            classify_io("reading install root", root, error)
        }
    })?;
    if !metadata.is_dir() {
        return Err(LauncherError::InstallRootNotDirectory(root.to_path_buf()));
    }
    std::fs::canonicalize(root).map_err(|error| classify_io("resolving install root", root, error))
}

fn discover_candidate(root: &Path) -> Result<PathBuf, LauncherError> {
    let mut found: Vec<PathBuf> = Vec::new();
    collect_known_executables(root, &mut found)?;
    let entries = std::fs::read_dir(root)
        .map_err(|error| classify_io("scanning install root", root, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| classify_io("reading install entry", root, error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| classify_io("reading install entry type", &entry.path(), error))?;
        if file_type.is_dir() {
            collect_known_executables(&entry.path(), &mut found)?;
        }
    }

    for name in KNOWN_EXECUTABLES {
        let matches: Vec<PathBuf> = found
            .iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        if matches.len() == 1 {
            return verify_candidate(root, &matches[0]);
        }
        if matches.len() > 1 {
            return Err(LauncherError::AmbiguousCandidates {
                root: root.to_path_buf(),
                candidates: matches,
            });
        }
    }
    Err(LauncherError::NoCandidate(root.to_path_buf()))
}

fn collect_known_executables(
    directory: &Path,
    found: &mut Vec<PathBuf>,
) -> Result<(), LauncherError> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| classify_io("scanning install directory", directory, error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| classify_io("reading install entry", directory, error))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|error| classify_io("reading install entry metadata", &path, error))?;
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if KNOWN_EXECUTABLES
            .iter()
            .any(|known| name.to_string_lossy().eq_ignore_ascii_case(known))
        {
            found.push(path);
        }
    }
    Ok(())
}

fn verify_candidate(root: &Path, candidate: &Path) -> Result<PathBuf, LauncherError> {
    let link_metadata = std::fs::symlink_metadata(candidate)
        .map_err(|error| classify_io("reading game executable", candidate, error))?;
    if is_reparse_point(&link_metadata) {
        return Err(LauncherError::UnsafeCandidate(candidate.to_path_buf()));
    }
    let candidate = std::fs::canonicalize(candidate)
        .map_err(|error| classify_io("resolving game executable", candidate, error))?;
    if !path_is_within(&candidate, root) {
        return Err(LauncherError::CandidateOutsideRoot {
            root: root.to_path_buf(),
            candidate,
        });
    }
    let metadata = std::fs::metadata(&candidate)
        .map_err(|error| classify_io("reading game executable", &candidate, error))?;
    if !metadata.is_file() {
        return Err(LauncherError::CandidateNotFile(candidate));
    }
    if cfg!(windows)
        && !candidate
            .extension()
            .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("exe"))
    {
        return Err(LauncherError::CandidateNotExecutable(candidate));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(LauncherError::CandidateNotExecutable(candidate));
        }
    }
    if cfg!(windows) {
        let mut magic = [0_u8; 2];
        File::open(&candidate)
            .and_then(|mut file| file.read_exact(&mut magic))
            .map_err(|error| classify_io("reading game executable header", &candidate, error))?;
        if &magic != b"MZ" {
            return Err(LauncherError::CandidateNotExecutable(candidate));
        }
    }
    Ok(candidate)
}

fn is_reparse_point(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
