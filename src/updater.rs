use std::{
    env, fs,
    fs::File,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::Result;

pub const RELEASE_REPOSITORY: &str = "LytsingStudio/me-rust";
const CHECKSUM_ASSET: &str = "SHA256SUMS";
const GITHUB_API_VERSION: &str = "2022-11-28";
const UPDATE_USER_AGENT: &str = concat!("me-rust/", env!("CARGO_PKG_VERSION"));

#[cfg(any(windows, test))]
const WINDOWS_UPDATE_POWERSHELL_ARGS: [&str; 3] = ["-NoLogo", "-NoProfile", "-NonInteractive"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UpdatePlatform {
    asset: &'static str,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
}

impl UpdatePlatform {
    fn detect() -> Result<Self> {
        Self::for_target(env::consts::OS, env::consts::ARCH)
    }

    fn for_target(os: &str, arch: &str) -> Result<Self> {
        let asset = match (os, arch) {
            ("macos", "aarch64") => "me-macos-arm64",
            ("macos", "x86_64") => "me-macos-x86_64",
            ("linux", "aarch64") => "me-linux-arm64",
            ("linux", "x86_64") => "me-linux-x86_64",
            ("windows", "x86_64") => "me-windows-x86_64.exe",
            _ => return Err(format!("me update does not support {os}/{arch}").into()),
        };
        Ok(Self { asset })
    }
}

pub fn update() -> Result<()> {
    let platform = UpdatePlatform::detect()?;
    let client = update_client()?;
    let release = latest_release(&client)?;
    let latest_tag = release.tag_name.as_str();
    let current_tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    if latest_tag == current_tag {
        println!("me is already up to date: {current_tag}");
        return Ok(());
    }
    if release_version(&latest_tag)? < release_version(&current_tag)? {
        println!(
            "the running me version {current_tag} is newer than the latest published release {latest_tag}; no update was installed"
        );
        return Ok(());
    }

    println!("updating me: {current_tag} -> {latest_tag}");
    let temporary = UpdateTempDirectory::create()?;
    download_release(&client, &release, platform.asset, temporary.path())?;

    let executable = temporary.path().join(platform.asset);
    let checksums = temporary.path().join(CHECKSUM_ASSET);
    verify_release_asset(&executable, &checksums, platform.asset)?;

    let destination = env::current_exe()
        .map_err(|error| format!("cannot locate the running me executable: {error}"))?;
    let scheduled_after_exit = deploy_executable(&executable, &destination)?;
    if scheduled_after_exit {
        println!(
            "downloaded me {latest_tag}; Windows will replace the executable after this process exits\nexecutable: {}\nglobal configuration: unchanged",
            destination.display()
        );
    } else {
        println!(
            "updated me to {latest_tag}\nexecutable: {}\nglobal configuration: unchanged",
            destination.display()
        );
    }
    Ok(())
}

fn update_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .user_agent(UPDATE_USER_AGENT)
        .build()?)
}

fn latest_release(client: &reqwest::blocking::Client) -> Result<GitHubRelease> {
    let url = format!("https://api.github.com/repos/{RELEASE_REPOSITORY}/releases/latest");
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send()
        .map_err(|error| format!("cannot query the latest public me release: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "cannot query the latest public me release: HTTP {status}{}",
            response_detail(&detail)
        )
        .into());
    }
    let release: GitHubRelease = response
        .json()
        .map_err(|error| format!("cannot decode the latest public me release: {error}"))?;
    if release.tag_name.trim().is_empty() {
        return Err("GitHub returned an empty latest release tag".into());
    }
    Ok(release)
}

fn download_release(
    client: &reqwest::blocking::Client,
    release: &GitHubRelease,
    asset: &str,
    directory: &Path,
) -> Result<()> {
    for name in [asset, CHECKSUM_ASSET] {
        let release_asset = release_asset(release, name)?;
        download_asset(client, release_asset, &directory.join(name))?;
    }
    Ok(())
}

fn release_asset<'a>(release: &'a GitHubRelease, name: &str) -> Result<&'a GitHubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|candidate| candidate.name == name)
        .ok_or_else(|| {
            format!(
                "release {} does not contain required asset {name}",
                release.tag_name
            )
            .into()
        })
}

fn download_asset(
    client: &reqwest::blocking::Client,
    asset: &GitHubReleaseAsset,
    destination: &Path,
) -> Result<()> {
    let mut response = client
        .get(&asset.browser_download_url)
        .send()
        .map_err(|error| format!("cannot download release asset {}: {error}", asset.name))?;
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "cannot download release asset {}: HTTP {status}{}",
            asset.name,
            response_detail(&detail)
        )
        .into());
    }
    let mut file = File::create(destination)?;
    std::io::copy(&mut response, &mut file)
        .map_err(|error| format!("cannot save release asset {}: {error}", asset.name))?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn response_detail(body: &str) -> String {
    let detail = body.trim().chars().take(512).collect::<String>();
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

fn release_version(tag: &str) -> Result<(u64, u64, u64)> {
    let core = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    let values = core
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid me release tag: {tag}"))?;
    let [major, minor, patch] = values.as_slice() else {
        return Err(format!("invalid me release tag: {tag}").into());
    };
    Ok((*major, *minor, *patch))
}

fn verify_release_asset(executable: &Path, manifest: &Path, asset: &str) -> Result<()> {
    if !executable.is_file() {
        return Err(format!("release asset was not downloaded: {asset}").into());
    }
    if executable.metadata()?.len() == 0 {
        return Err(format!("downloaded release asset is empty: {asset}").into());
    }
    let manifest = fs::read_to_string(manifest)
        .map_err(|error| format!("cannot read downloaded {CHECKSUM_ASSET}: {error}"))?;
    let expected = checksum_for_asset(&manifest, asset)?;
    let actual = sha256_file(executable)?;
    if actual != expected {
        return Err(format!(
            "release checksum mismatch for {asset}: expected {expected}, received {actual}"
        )
        .into());
    }
    Ok(())
}

fn checksum_for_asset(manifest: &str, asset: &str) -> Result<String> {
    let mut found = None;
    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let Some(checksum) = fields.next() else {
            continue;
        };
        let Some(file) = fields.next() else {
            continue;
        };
        if file.trim_start_matches('*') != asset {
            continue;
        }
        if found.is_some() {
            return Err(format!("{CHECKSUM_ASSET} contains duplicate entries for {asset}").into());
        }
        let checksum = checksum.to_ascii_lowercase();
        if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(
                format!("{CHECKSUM_ASSET} contains an invalid checksum for {asset}").into(),
            );
        }
        found = Some(checksum);
    }
    found.ok_or_else(|| format!("{CHECKSUM_ASSET} does not contain {asset}").into())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn deploy_executable(downloaded: &Path, destination: &Path) -> Result<bool> {
    match atomic_install_unix(downloaded, destination) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "write permission is required for {}; requesting sudo",
                destination.display()
            );
            privileged_install_unix(downloaded, destination)?;
            Ok(false)
        }
        Err(error) => Err(format!(
            "cannot replace the current executable {}: {error}",
            destination.display()
        )
        .into()),
    }
}

#[cfg(unix)]
fn atomic_install_unix(downloaded: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let staging = sibling_staging_path(destination)?;
    let result = (|| {
        fs::copy(downloaded, &staging)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))?;
        File::open(&staging)?.sync_all()?;
        fs::rename(&staging, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

#[cfg(unix)]
fn privileged_install_unix(downloaded: &Path, destination: &Path) -> Result<()> {
    let staging = sibling_staging_path(destination)?;
    run_checked(
        Command::new("sudo")
            .arg("install")
            .arg("-m")
            .arg("755")
            .arg(downloaded)
            .arg(&staging),
        "install the downloaded executable",
    )?;
    if let Err(error) = run_checked(
        Command::new("sudo")
            .arg("mv")
            .arg("-f")
            .arg(&staging)
            .arg(destination),
        "replace the current executable",
    ) {
        eprintln!(
            "warning: the staged update remains at {}",
            staging.display()
        );
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn run_checked(command: &mut Command, operation: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| format!("cannot {operation}: {error}"))?;
    if !status.success() {
        return Err(format!("cannot {operation}: command exited with {status}").into());
    }
    Ok(())
}

#[cfg(windows)]
fn deploy_executable(downloaded: &Path, destination: &Path) -> Result<bool> {
    let staging = sibling_staging_path(destination)?;
    fs::copy(downloaded, &staging).map_err(|error| {
        format!(
            "cannot stage the Windows update beside {}: {error}; run me from an elevated terminal if it is installed in a protected directory",
            destination.display()
        )
    })?;

    let source = powershell_literal(&staging);
    let target = powershell_literal(destination);
    let script = windows_update_script(std::process::id(), &source, &target);
    let mut helper = Command::new("powershell.exe");
    helper
        .args(WINDOWS_UPDATE_POWERSHELL_ARGS)
        .arg("-Command")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let spawned = helper.spawn();
    if let Err(error) = spawned {
        let _ = fs::remove_file(&staging);
        return Err(format!("cannot start the Windows update helper: {error}").into());
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn windows_update_script(process_id: u32, source: &str, target: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Wait-Process -Id {} -ErrorAction SilentlyContinue; \
         $done=$false; for($i=0; $i -lt 50; $i++) {{ try {{ Move-Item -LiteralPath '{source}' -Destination '{target}' -Force; $done=$true; break }} catch {{ Start-Sleep -Milliseconds 200 }} }}; \
         if(-not $done) {{ Remove-Item -LiteralPath '{source}' -Force -ErrorAction SilentlyContinue; exit 1 }}",
        process_id
    )
}

#[cfg(windows)]
fn powershell_literal(path: &Path) -> String {
    powershell_literal_text(&path.to_string_lossy())
}

#[cfg(any(windows, test))]
fn powershell_literal_text(path: &str) -> String {
    let normalized = if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    };
    normalized.replace('\'', "''")
}

#[cfg(not(any(unix, windows)))]
fn deploy_executable(_downloaded: &Path, _destination: &Path) -> Result<bool> {
    Err("me update is not supported on this platform".into())
}

fn sibling_staging_path(destination: &Path) -> std::io::Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "current executable has no parent directory",
        )
    })?;
    let file = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("me");
    for _ in 0..16 {
        let suffix = random_suffix().map_err(std::io::Error::other)?;
        let candidate = parent.join(format!(".{file}.update-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "cannot allocate a unique update staging path",
    ))
}

fn random_suffix() -> Result<String> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random)?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

struct UpdateTempDirectory {
    path: PathBuf,
}

impl UpdateTempDirectory {
    fn create() -> Result<Self> {
        for _ in 0..16 {
            let path = env::temp_dir().join(format!("me-update-{}", random_suffix()?));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("cannot create update directory: {error}").into());
                }
            }
        }
        Err("cannot allocate a unique update directory".into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UpdateTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    fn temporary_directory(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "me-updater-test-{name}-{}-{}",
            std::process::id(),
            random_suffix().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn release_assets_match_every_supported_target() {
        assert_eq!(
            UpdatePlatform::for_target("macos", "aarch64")
                .unwrap()
                .asset,
            "me-macos-arm64"
        );
        assert_eq!(
            UpdatePlatform::for_target("macos", "x86_64").unwrap().asset,
            "me-macos-x86_64"
        );
        assert_eq!(
            UpdatePlatform::for_target("linux", "aarch64")
                .unwrap()
                .asset,
            "me-linux-arm64"
        );
        assert_eq!(
            UpdatePlatform::for_target("linux", "x86_64").unwrap().asset,
            "me-linux-x86_64"
        );
        assert_eq!(
            UpdatePlatform::for_target("windows", "x86_64")
                .unwrap()
                .asset,
            "me-windows-x86_64.exe"
        );
        assert!(UpdatePlatform::for_target("windows", "aarch64").is_err());
    }

    #[test]
    fn release_versions_are_numeric_and_never_require_a_downgrade() {
        assert_eq!(release_version("v0.0.164").unwrap(), (0, 0, 164));
        assert_eq!(release_version("1.2.3-beta.1").unwrap(), (1, 2, 3));
        assert!(release_version("v1.2").is_err());
        assert!(release_version("latest").is_err());
        assert!(release_version("v0.0.163").unwrap() < release_version("v0.0.164").unwrap());
    }

    #[test]
    fn public_release_metadata_selects_assets_by_exact_name() {
        let release: GitHubRelease = serde_json::from_str(
            r#"{
                "tag_name":"v0.0.267",
                "assets":[
                    {"name":"me-linux-x86_64","browser_download_url":"https://github.com/LytsingStudio/me-rust/releases/download/v0.0.267/me-linux-x86_64"},
                    {"name":"SHA256SUMS","browser_download_url":"https://github.com/LytsingStudio/me-rust/releases/download/v0.0.267/SHA256SUMS"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(release.tag_name, "v0.0.267");
        assert_eq!(
            release_asset(&release, "me-linux-x86_64").unwrap().name,
            "me-linux-x86_64"
        );
        assert!(release_asset(&release, "me-linux-arm64").is_err());
    }

    #[test]
    fn update_temporary_directory_removes_partial_downloads_on_drop() {
        let path = {
            let temporary = UpdateTempDirectory::create().unwrap();
            let path = temporary.path().to_owned();
            fs::write(path.join("partial-download"), b"partial").unwrap();
            assert!(path.exists());
            path
        };

        assert!(!path.exists());
    }

    #[test]
    fn release_asset_download_streams_directly_to_the_target_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /asset "));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nrelease-bytes!",
                )
                .unwrap();
        });
        let directory = temporary_directory("download");
        let destination = directory.join("asset");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let asset = GitHubReleaseAsset {
            name: "asset".into(),
            browser_download_url: format!("http://{address}/asset"),
        };

        download_asset(&client, &asset, &destination).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"release-bytes!");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn windows_helper_normalizes_verbatim_paths_and_quotes_literals() {
        assert_eq!(
            WINDOWS_UPDATE_POWERSHELL_ARGS,
            ["-NoLogo", "-NoProfile", "-NonInteractive"]
        );
        assert!(
            WINDOWS_UPDATE_POWERSHELL_ARGS
                .iter()
                .all(|argument| !argument.eq_ignore_ascii_case("-WindowStyle"))
        );
        assert_eq!(
            powershell_literal_text(r"\\?\C:\Users\O'Brien\me.exe"),
            r"C:\Users\O''Brien\me.exe"
        );
        assert_eq!(
            powershell_literal_text(r"\\?\UNC\server\share\me.exe"),
            r"\\server\share\me.exe"
        );
        let script = windows_update_script(42, "C:\\staged-me.exe", "C:\\me.exe");
        assert!(script.contains("Wait-Process -Id 42"));
        assert!(script.contains("Move-Item -LiteralPath 'C:\\staged-me.exe'"));
        assert!(script.contains("Remove-Item -LiteralPath 'C:\\staged-me.exe'"));
    }

    #[test]
    fn checksum_manifest_requires_one_exact_valid_entry() {
        let digest = "a".repeat(64);
        let manifest = format!(
            "{}  me-linux-x86_64\n{} *me-windows-x86_64.exe\n",
            digest,
            "B".repeat(64)
        );
        assert_eq!(
            checksum_for_asset(&manifest, "me-linux-x86_64").unwrap(),
            digest
        );
        assert_eq!(
            checksum_for_asset(&manifest, "me-windows-x86_64.exe").unwrap(),
            "b".repeat(64)
        );
        assert!(checksum_for_asset(&manifest, "me").is_err());
        assert!(checksum_for_asset("bad  me-linux-x86_64", "me-linux-x86_64").is_err());
        let duplicate = format!("{digest}  me\n{digest}  me\n");
        assert!(checksum_for_asset(&duplicate, "me").is_err());
    }

    #[test]
    fn release_verification_hashes_the_exact_downloaded_bytes() {
        let directory = temporary_directory("checksum");
        let asset = directory.join("me-linux-x86_64");
        let manifest = directory.join(CHECKSUM_ASSET);
        fs::write(&asset, b"abc").unwrap();
        fs::write(
            &manifest,
            b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  me-linux-x86_64\n",
        )
        .unwrap();

        verify_release_asset(&asset, &manifest, "me-linux-x86_64").unwrap();
        fs::write(&asset, b"changed").unwrap();
        assert!(verify_release_asset(&asset, &manifest, "me-linux-x86_64").is_err());
        fs::write(&asset, b"").unwrap();
        assert!(verify_release_asset(&asset, &manifest, "me-linux-x86_64").is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_install_atomically_replaces_only_the_executable() {
        use std::os::unix::fs::PermissionsExt;

        let directory = temporary_directory("install");
        let downloaded = directory.join("downloaded-me");
        let destination = directory.join("me");
        let configuration = directory.join("models.toml");
        fs::write(&downloaded, b"new executable").unwrap();
        fs::write(&destination, b"old executable").unwrap();
        fs::write(&configuration, b"keep configuration").unwrap();

        atomic_install_unix(&downloaded, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new executable");
        assert_eq!(fs::read(&configuration).unwrap(), b"keep configuration");
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".update-")
        }));
        fs::remove_dir_all(directory).unwrap();
    }
}
