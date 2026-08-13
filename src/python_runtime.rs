use std::{
    env,
    ffi::OsString,
    fs,
    fs::OpenOptions,
    io::Cursor,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use fs2::FileExt;

use crate::{Result, config::create_private_directory};

const RUNTIME_ID: &str = env!("ME_EMBEDDED_PYTHON_ID");
const ARCHIVE_SHA256: &str = env!("ME_EMBEDDED_PYTHON_SHA256");
const ARCHIVE: &[u8] = include_bytes!(env!("ME_EMBEDDED_PYTHON_ARCHIVE"));
const MARKER_FILE: &str = ".me-python-runtime";
static INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct EmbeddedPython {
    pub executable: PathBuf,
    pub path_directory: PathBuf,
}

pub(crate) fn ensure(config_home: &Path) -> Result<EmbeddedPython> {
    let _guard = INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "embedded Python install lock is poisoned")?;
    let parent = config_home.join("runtime/python");
    create_private_directory(&parent)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(parent.join(".install.lock"))?;
    FileExt::lock_exclusive(&lock)?;
    let result = ensure_locked(&parent);
    let unlock = FileExt::unlock(&lock);
    result.and_then(|runtime| {
        unlock?;
        Ok(runtime)
    })
}

fn ensure_locked(parent: &Path) -> Result<EmbeddedPython> {
    let destination = parent.join(RUNTIME_ID);
    if runtime_is_valid(&destination) {
        return Ok(runtime(&destination));
    }
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(".install-{}-{nonce}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    create_private_directory(&staging)?;
    let result = (|| -> Result<()> {
        extract_archive(&staging)?;
        let extracted = staging.join("python");
        if !python_executable(&extracted).is_file() {
            return Err("embedded Python archive has no target interpreter".into());
        }
        fs::write(
            extracted.join(MARKER_FILE),
            format!("{RUNTIME_ID}\n{ARCHIVE_SHA256}\n"),
        )?;
        match fs::rename(&extracted, &destination) {
            Ok(()) => Ok(()),
            Err(_) if runtime_is_valid(&destination) => Ok(()),
            Err(error) => Err(error.into()),
        }
    })();
    let _ = fs::remove_dir_all(&staging);
    result?;
    if !runtime_is_valid(&destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err("embedded Python 3.12 failed its post-install validation".into());
    }
    Ok(runtime(&destination))
}

fn extract_archive(staging: &Path) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(ARCHIVE));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !is_python_archive_path(&path) {
            return Err(format!(
                "embedded Python archive contains an invalid path: {}",
                path.display()
            )
            .into());
        }
        if !entry.unpack_in(staging)? {
            return Err(format!(
                "embedded Python archive entry escaped its install root: {}",
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn is_python_archive_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(root)) if root == "python")
        && components.all(|component| matches!(component, Component::Normal(_)))
}

fn runtime(destination: &Path) -> EmbeddedPython {
    let executable = python_executable(destination);
    let path_directory = executable
        .parent()
        .expect("embedded Python executable has a parent")
        .to_owned();
    EmbeddedPython {
        executable,
        path_directory,
    }
}

fn runtime_is_valid(destination: &Path) -> bool {
    let marker = fs::read_to_string(destination.join(MARKER_FILE)).ok();
    if marker.as_deref() != Some(&format!("{RUNTIME_ID}\n{ARCHIVE_SHA256}\n")) {
        return false;
    }
    let executable = python_executable(destination);
    executable.is_file()
        && Command::new(executable)
            .args([
                "-c",
                "import sys; raise SystemExit(0 if sys.version_info[:2] == (3, 12) else 1)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn python_executable(destination: &Path) -> PathBuf {
    destination.join("python.exe")
}

#[cfg(not(windows))]
fn python_executable(destination: &Path) -> PathBuf {
    destination.join("bin/python3.12")
}

pub(crate) fn prepend_path(directory: &Path) -> Result<OsString> {
    let mut paths = vec![directory.to_owned()];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    Ok(env::join_paths(paths)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "me-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn embedded_python_extracts_once_and_runs_without_a_system_interpreter() {
        let home = temporary_home("embedded-python");
        let first = ensure(&home).unwrap();
        let output = Command::new(&first.executable)
            .args(["-c", "import sys; print(sys.version_info[:2])"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "(3, 12)");
        let second = ensure(&home).unwrap();
        assert_eq!(first.executable, second.executable);
        assert!(
            first
                .executable
                .starts_with(home.join("runtime/python").join(RUNTIME_ID))
        );
        let runtime_root = home.join("runtime/python").join(RUNTIME_ID);
        fs::write(runtime_root.join(MARKER_FILE), "corrupt").unwrap();
        let repaired = ensure(&home).unwrap();
        assert_eq!(repaired.executable, python_executable(&runtime_root));
        assert!(runtime_is_valid(&runtime_root));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn embedded_archive_is_pinned_to_the_build_target() {
        assert!(RUNTIME_ID.starts_with("cpython-3.12.13+20260718-"));
        assert_eq!(ARCHIVE_SHA256.len(), 64);
        assert!(ARCHIVE.len() > 10_000_000);
    }
}
