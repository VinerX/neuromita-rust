//! Filesystem transport for durable memory maintenance archives.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};
use uuid::Uuid;

/// Stage a complete backup, sync its contents, and publish it without overwriting
/// any existing file. A failed write never advances Timeline.
pub(crate) fn persist_exclusive_backup(path: &Path, payload: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "backup path needs a filename")
    })?;
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temporary)?;
        output.write_all(payload)?;
        output.sync_all()?;
        drop(output);
        fs::hard_link(&temporary, path)?;
        #[cfg(unix)]
        {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}
