use std::path::{Path, PathBuf};

use super::{LauncherError, paths_equal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub executable: PathBuf,
    pub created: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessScan {
    pub matching: Vec<ProcessIdentity>,
    pub same_name_foreign: Vec<ProcessIdentity>,
    pub inaccessible: Vec<u32>,
}

impl ProcessScan {
    pub fn is_running(&self) -> bool {
        !self.matching.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProbe {
    Running,
    Stale,
    Gone,
    Inaccessible,
}

pub fn scan_install(executable: &Path) -> Result<ProcessScan, LauncherError> {
    let executable = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    platform::scan(&executable)
}

pub fn identity_for_pid(pid: u32) -> Result<Option<ProcessIdentity>, LauncherError> {
    platform::identity(pid)
}

pub(crate) fn inspect_identity(identity: &ProcessIdentity) -> Result<IdentityProbe, LauncherError> {
    platform::inspect(identity)
}

fn classify(executable: &Path, identity: ProcessIdentity) -> (Option<ProcessIdentity>, bool) {
    if paths_equal(&identity.executable, executable) {
        (Some(identity), false)
    } else if identity
        .executable
        .file_name()
        .zip(executable.file_name())
        .is_some_and(|(left, right)| {
            left.to_string_lossy()
                .eq_ignore_ascii_case(&right.to_string_lossy())
        })
    {
        (None, true)
    } else {
        (None, false)
    }
}

#[cfg(windows)]
mod platform {
    use std::{ffi::c_void, mem::size_of, path::PathBuf};

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_NO_MORE_FILES, FILETIME, GetLastError,
            INVALID_HANDLE_VALUE,
        },
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
    };

    use super::{IdentityProbe, ProcessIdentity, ProcessScan, classify};
    use crate::launcher::{LauncherError, classify_io, paths_equal};

    enum Query {
        Found(ProcessIdentity),
        Gone,
        Denied,
    }

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) fn scan(executable: &std::path::Path) -> Result<ProcessScan, LauncherError> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            let error = std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
            return Err(classify_io(
                "enumerating processes",
                std::path::Path::new("."),
                error,
            ));
        }
        let snapshot = Handle(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..PROCESSENTRY32W::default()
        };
        let mut first = unsafe { Process32FirstW(snapshot.0, &mut entry) };
        if first == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_MORE_FILES {
                let error = std::io::Error::from_raw_os_error(code as i32);
                return Err(classify_io(
                    "enumerating processes",
                    std::path::Path::new("."),
                    error,
                ));
            }
        }
        let mut result = ProcessScan::default();
        while first != 0 {
            let pid = entry.th32ProcessID;
            match query(pid) {
                Query::Found(identity) => {
                    let (matching, foreign) = classify(executable, identity.clone());
                    if let Some(identity) = matching {
                        result.matching.push(identity);
                    } else if foreign {
                        result.same_name_foreign.push(identity);
                    }
                }
                Query::Denied => result.inaccessible.push(pid),
                Query::Gone => {}
            }
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            first = unsafe { Process32NextW(snapshot.0, &mut entry) };
        }
        Ok(result)
    }

    pub(super) fn identity(pid: u32) -> Result<Option<ProcessIdentity>, LauncherError> {
        match query(pid) {
            Query::Found(identity) => Ok(Some(identity)),
            Query::Gone => Ok(None),
            Query::Denied => Err(LauncherError::AccessDenied {
                operation: "reading process identity",
                path: PathBuf::from(format!("pid {pid}")),
                source: std::io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32),
            }),
        }
    }

    pub(super) fn inspect(identity: &ProcessIdentity) -> Result<IdentityProbe, LauncherError> {
        match query(identity.pid) {
            Query::Gone => Ok(IdentityProbe::Gone),
            Query::Denied => Ok(IdentityProbe::Inaccessible),
            Query::Found(current) => {
                if current.pid == identity.pid
                    && current.created == identity.created
                    && paths_equal(&current.executable, &identity.executable)
                {
                    Ok(IdentityProbe::Running)
                } else {
                    Ok(IdentityProbe::Stale)
                }
            }
        }
    }

    fn query(pid: u32) -> Query {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            let code = unsafe { GetLastError() };
            return if code == ERROR_ACCESS_DENIED {
                Query::Denied
            } else {
                Query::Gone
            };
        }
        let handle = Handle(handle);
        let mut buffer = [0_u16; 32_768];
        let mut length = buffer.len() as u32;
        if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0
        {
            let code = unsafe { GetLastError() };
            return if code == ERROR_ACCESS_DENIED {
                Query::Denied
            } else {
                Query::Gone
            };
        }
        let executable = String::from_utf16_lossy(&buffer[..length as usize]);
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            == 0
        {
            return Query::Denied;
        }
        let created =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let path = PathBuf::from(executable);
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        Query::Found(ProcessIdentity {
            pid,
            executable: path,
            created,
        })
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{IdentityProbe, ProcessIdentity, ProcessScan, classify};
    use crate::launcher::{LauncherError, classify_io};

    pub(super) fn scan(executable: &Path) -> Result<ProcessScan, LauncherError> {
        let mut result = ProcessScan::default();
        let entries = std::fs::read_dir("/proc")
            .map_err(|error| classify_io("scanning processes", Path::new("/proc"), error))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| classify_io("reading process entry", Path::new("/proc"), error))?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            match query(pid) {
                Ok(Some(identity)) => {
                    let (matching, foreign) = classify(executable, identity.clone());
                    if let Some(identity) = matching {
                        result.matching.push(identity);
                    } else if foreign {
                        result.same_name_foreign.push(identity);
                    }
                }
                Ok(None) => {}
                Err(LauncherError::AccessDenied { .. }) => result.inaccessible.push(pid),
                Err(error) => return Err(error),
            }
        }
        Ok(result)
    }

    pub(super) fn identity(pid: u32) -> Result<Option<ProcessIdentity>, LauncherError> {
        query(pid)
    }

    pub(super) fn query(pid: u32) -> Result<Option<ProcessIdentity>, LauncherError> {
        let executable_path = PathBuf::from(format!("/proc/{pid}/exe"));
        let executable = match std::fs::read_link(&executable_path) {
            Ok(executable) => executable,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(classify_io(
                    "reading process identity",
                    &executable_path,
                    error,
                ));
            }
        };
        let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(classify_io("reading process start time", &stat_path, error)),
        };
        let parsed = stat.rfind(')').and_then(|end| {
            stat[end + 1..]
                .split_whitespace()
                .nth(19)
                .and_then(|value| value.parse::<u64>().ok())
        });
        let Some(created) = parsed else {
            return Err(classify_io(
                "reading process start time",
                &stat_path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "process stat does not contain a start time",
                ),
            ));
        };
        Ok(Some(ProcessIdentity {
            pid,
            executable,
            created,
        }))
    }

    pub(super) fn inspect(expected: &ProcessIdentity) -> Result<IdentityProbe, LauncherError> {
        match query(expected.pid)? {
            None => Ok(IdentityProbe::Gone),
            Some(current)
                if current.executable == expected.executable
                    && current.created == expected.created =>
            {
                Ok(IdentityProbe::Running)
            }
            Some(_) => Ok(IdentityProbe::Stale),
        }
    }
}
