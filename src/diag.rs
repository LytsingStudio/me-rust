use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Local;
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT},
};
use serde_json::{Value, json};

use crate::Result;

pub const DIAG_REPOSITORY: &str = "LytsingStudio/me-rust-diag-collects";

const GITHUB_API_URL: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const MAX_GITHUB_FILE_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticUpload {
    pub archive_name: String,
    pub archive_bytes: u64,
    pub url: String,
}

pub fn upload_workspace(workspace: &Path) -> Result<DiagnosticUpload> {
    let source = workspace.join(".me");
    if !source.is_dir() {
        return Err(format!(
            "workspace diagnostic directory {} does not exist",
            source.display()
        )
        .into());
    }

    let archive_name = diagnostic_archive_name()?;
    let archive_path =
        std::env::temp_dir().join(format!(".me-diag-{}-{archive_name}", std::process::id()));
    let (temporary, archive_bytes) = TemporaryArchive::create(&source, archive_path)?;
    if archive_bytes > MAX_GITHUB_FILE_BYTES {
        return Err(format!(
            "diagnostic archive is {archive_bytes} bytes; GitHub repository files are limited to {MAX_GITHUB_FILE_BYTES} bytes"
        )
        .into());
    }

    let token = github_token()?;
    let url = upload_archive(
        GITHUB_API_URL,
        DIAG_REPOSITORY,
        &archive_name,
        temporary.path(),
        &token,
    )?;
    Ok(DiagnosticUpload {
        archive_name,
        archive_bytes,
        url,
    })
}

fn diagnostic_archive_name() -> Result<String> {
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random)?;
    let unique = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format_archive_name(
        &Local::now().format("%Y%m%d-%H%M%S").to_string(),
        &unique[..7],
    ))
}

fn format_archive_name(timestamp: &str, unique: &str) -> String {
    format!("me-diag-upload-{timestamp}-{unique}.tar.zst")
}

#[cfg(test)]
fn create_archive(source: &Path, destination: &Path) -> Result<u64> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    write_archive(source, file)
}

fn write_archive(source: &Path, file: fs::File) -> Result<u64> {
    let encoder = zstd::Encoder::new(file, 3)?;
    let mut archive = tar::Builder::new(encoder);
    archive.follow_symlinks(false);
    archive.append_dir_all(".me", source)?;
    let encoder = archive.into_inner()?;
    let file = encoder.finish()?;
    file.sync_all()?;
    Ok(file.metadata()?.len())
}

fn github_token() -> Result<String> {
    let output = Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .output()
        .map_err(|error| {
            format!(
                "GitHub CLI `gh` is required for diagnostic uploads and could not be started: {error}"
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "GitHub CLI is not logged in with access to {DIAG_REPOSITORY}: {}",
            detail.trim()
        )
        .into());
    }
    let token = String::from_utf8(output.stdout)?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err("GitHub CLI returned an empty authentication token".into());
    }
    Ok(token)
}

fn upload_archive(
    api_url: &str,
    repository: &str,
    archive_name: &str,
    archive_path: &Path,
    token: &str,
) -> Result<String> {
    let content = STANDARD.encode(fs::read(archive_path)?);
    let body = json!({
        "message": format!("upload diagnostic archive {archive_name}"),
        "content": content,
    });
    let response = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?
        .put(format!(
            "{}/repos/{repository}/contents/{archive_name}",
            api_url.trim_end_matches('/')
        ))
        .headers(github_headers(token)?)
        .json(&body)
        .send()?;
    let status = response.status();
    let response_body = response.text()?;
    if status != StatusCode::CREATED {
        let detail = serde_json::from_str::<Value>(&response_body)
            .ok()
            .and_then(|value| value["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| abbreviated(&response_body, 500));
        return Err(format!("GitHub diagnostic upload failed with HTTP {status}: {detail}").into());
    }
    serde_json::from_str::<Value>(&response_body)?["content"]["html_url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "GitHub diagnostic upload response has no content URL".into())
}

fn github_headers(token: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let authorization = HeaderValue::from_str(&format!("Bearer {token}"))?;
    headers.insert(AUTHORIZATION, authorization);
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "x-github-api-version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("me/", env!("CARGO_PKG_VERSION"))),
    );
    Ok(headers)
}

fn abbreviated(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let text = characters.by_ref().take(limit).collect::<String>();
    if characters.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

struct TemporaryArchive {
    path: PathBuf,
}

impl TemporaryArchive {
    fn create(source: &Path, path: PathBuf) -> Result<(Self, u64)> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let temporary = Self { path };
        let bytes = write_archive(source, file)?;
        Ok((temporary, bytes))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryArchive {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[test]
    fn archive_name_has_timestamp_unique_code_and_extension() {
        assert_eq!(
            format_archive_name("20260728-191501", "1a2b3c4"),
            "me-diag-upload-20260728-191501-1a2b3c4.tar.zst"
        );
    }

    #[test]
    fn missing_workspace_fails_before_authentication() {
        let workspace = test_directory("missing");
        let error = upload_workspace(&workspace).unwrap_err().to_string();
        assert!(error.contains(".me"));
        assert!(error.contains("does not exist"));
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn temporary_archive_is_removed_when_its_guard_is_dropped() {
        let workspace = test_directory("temporary");
        let source = workspace.join(".me");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("config.toml"), b"opaque").unwrap();
        let destination = workspace.join("temporary.tar.zst");

        let (temporary, bytes) = TemporaryArchive::create(&source, destination.clone()).unwrap();
        assert!(bytes > 0);
        assert!(destination.is_file());
        drop(temporary);
        assert!(!destination.exists());
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn archive_contains_every_file_without_parsing_or_cleaning_contents() {
        let workspace = test_directory("archive");
        let source = workspace.join(".me");
        fs::create_dir_all(source.join("edb/nested")).unwrap();
        let binary = (0_u8..=u8::MAX).collect::<Vec<_>>();
        fs::write(source.join("config.toml"), b"not valid toml\nsecret=raw").unwrap();
        fs::write(source.join("edb/main.edb"), &binary).unwrap();
        fs::write(source.join("edb/nested/.hidden"), b"\0private\n").unwrap();
        fs::write(source.join("empty"), []).unwrap();
        let destination = workspace.join("diagnostic.tar.zst");

        create_archive(&source, &destination).unwrap();

        let decoder = zstd::Decoder::new(fs::File::open(&destination).unwrap()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let mut files = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.header().entry_type().is_file() {
                let mut content = Vec::new();
                entry.read_to_end(&mut content).unwrap();
                files.insert(entry.path().unwrap().into_owned(), content);
            }
        }
        assert_eq!(
            files,
            BTreeMap::from([
                (
                    PathBuf::from(".me/config.toml"),
                    b"not valid toml\nsecret=raw".to_vec()
                ),
                (PathBuf::from(".me/edb/main.edb"), binary),
                (
                    PathBuf::from(".me/edb/nested/.hidden"),
                    b"\0private\n".to_vec()
                ),
                (PathBuf::from(".me/empty"), Vec::new()),
            ])
        );
        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn archive_preserves_symlinks_instead_of_following_them() {
        use std::os::unix::fs::symlink;

        let workspace = test_directory("symlink");
        let source = workspace.join(".me");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("target"), b"inside").unwrap();
        symlink("target", source.join("link")).unwrap();
        let destination = workspace.join("diagnostic.tar.zst");

        create_archive(&source, &destination).unwrap();

        let decoder = zstd::Decoder::new(fs::File::open(&destination).unwrap()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let links = archive
            .entries()
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry.header().entry_type().is_symlink().then(|| {
                    (
                        entry.path().unwrap().into_owned(),
                        entry.link_name().unwrap().map(|path| path.into_owned()),
                    )
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            links,
            vec![(PathBuf::from(".me/link"), Some(PathBuf::from("target")))]
        );
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn github_request_uploads_the_exact_archive_bytes() {
        let directory = test_directory("upload");
        let archive_path = directory.join("archive.tar.zst");
        let archive_bytes = b"opaque diagnostic archive\0with binary";
        fs::write(&archive_path, archive_bytes).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with(
                "PUT /repos/LytsingStudio/me-rust-diag-collects/contents/me-diag-upload-test.tar.zst HTTP/1.1\r\n"
            ));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token\r\n")
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-github-api-version: 2022-11-28\r\n")
            );
            let body = request.split_once("\r\n\r\n").unwrap().1;
            let body: Value = serde_json::from_str(body).unwrap();
            assert_eq!(
                STANDARD.decode(body["content"].as_str().unwrap()).unwrap(),
                archive_bytes
            );
            assert_eq!(
                body["message"],
                "upload diagnostic archive me-diag-upload-test.tar.zst"
            );
            let response =
                br#"{"content":{"html_url":"https://github.example/diagnostic/archive"}}"#;
            write!(
                stream,
                "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                String::from_utf8_lossy(response)
            )
            .unwrap();
        });

        let url = upload_archive(
            &format!("http://{address}"),
            DIAG_REPOSITORY,
            "me-diag-upload-test.tar.zst",
            &archive_path,
            "test-token",
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(url, "https://github.example/diagnostic/archive");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn github_failure_reports_status_without_exposing_token() {
        let directory = test_directory("failure");
        let archive_path = directory.join("archive.tar.zst");
        fs::write(&archive_path, b"archive").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            let response = br#"{"message":"repository access denied"}"#;
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                String::from_utf8_lossy(response)
            )
            .unwrap();
        });

        let error = upload_archive(
            &format!("http://{address}"),
            DIAG_REPOSITORY,
            "me-diag-upload-test.tar.zst",
            &archive_path,
            "must-not-appear",
        )
        .unwrap_err()
        .to_string();
        server.join().unwrap();
        assert!(error.contains("403"));
        assert!(error.contains("repository access denied"));
        assert!(!error.contains("must-not-appear"));
        fs::remove_dir_all(directory).unwrap();
    }

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0, "HTTP request ended before its body was complete");
            bytes.extend_from_slice(&buffer[..count]);
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= header_end + 4 + content_length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    fn test_directory(label: &str) -> PathBuf {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("me-diag-{label}-{suffix}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
