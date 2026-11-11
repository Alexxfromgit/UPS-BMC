//! Starting the collector next to the UI executable.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn collector_path() -> std::io::Result<PathBuf> {
    let name = if cfg!(windows) {
        "ups-collector.exe"
    } else {
        "ups-collector"
    };
    Ok(std::env::current_exe()?.with_file_name(name))
}

/// Starts the collector as an independent background process. It keeps
/// running (and recording) after the UI exits.
pub fn start_collector(args: &[String]) -> std::io::Result<()> {
    let path = collector_path()?;
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} not found", path.display()),
        ));
    }
    let mut cmd = Command::new(path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ())
}
