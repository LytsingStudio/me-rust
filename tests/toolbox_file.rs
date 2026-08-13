use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

fn python_312() -> Option<(OsString, Vec<OsString>)> {
    let mut candidates = Vec::new();
    if let Some(program) = env::var_os("ME_TOOLBOX_PYTHON") {
        candidates.push((program, Vec::new()));
    }
    candidates.push((OsString::from("python3.12"), Vec::new()));
    #[cfg(windows)]
    candidates.push((OsString::from("py"), vec![OsString::from("-3.12")]));
    if let Ok(output) = Command::new("pyenv").args(["prefix", "3.12"]).output()
        && output.status.success()
        && let Ok(prefix) = String::from_utf8(output.stdout)
    {
        let prefix = PathBuf::from(prefix.trim());
        for path in [
            prefix.join("bin/python3.12"),
            prefix.join("bin/python"),
            prefix.join("python.exe"),
        ] {
            if path.is_file() {
                candidates.push((path.into_os_string(), Vec::new()));
            }
        }
    }
    candidates.push((OsString::from("python"), Vec::new()));
    candidates.into_iter().find(|(program, arguments)| {
        Command::new(program)
            .args(arguments)
            .args([
                "-c",
                "import sys; raise SystemExit(0 if sys.version_info[:2] == (3, 12) else 1)",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn temporary_workspace() -> PathBuf {
    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!(
        "me-file-toolbox-integration-{}-{nonce}-{serial}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn utf16_le(text: &str, bom: bool) -> Vec<u8> {
    let mut bytes = if bom { vec![0xff, 0xfe] } else { Vec::new() };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn utf32_be(text: &str, bom: bool) -> Vec<u8> {
    let mut bytes = if bom {
        vec![0x00, 0x00, 0xfe, 0xff]
    } else {
        Vec::new()
    };
    for character in text.chars() {
        bytes.extend_from_slice(&(character as u32).to_be_bytes());
    }
    bytes
}

struct ToolboxProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl ToolboxProcess {
    fn start(workspace: &Path, script: &Path) -> Self {
        Self::start_with_io_encoding(workspace, script, None)
    }

    fn start_with_io_encoding(workspace: &Path, script: &Path, io_encoding: Option<&str>) -> Self {
        let Some((python, arguments)) = python_312() else {
            panic!("File toolbox integration test requires Python 3.12");
        };
        let mut command = Command::new(python);
        command
            .args(arguments)
            .arg(script)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(io_encoding) = io_encoding {
            command.env("PYTHONIOENCODING", io_encoding);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn request(&mut self, mut request: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        request["id"] = Value::from(id);
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        assert!(
            !line.is_empty(),
            "File.py closed before responding to {request}"
        );
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], id);
        frame
    }

    fn query(&mut self, command: &str, tool: Option<&str>) -> Value {
        let mut request = json!({"cmd": command});
        if let Some(tool) = tool {
            request["tool"] = Value::String(tool.to_owned());
        }
        self.request(request)
    }

    fn execute(&mut self, tool: &str, input: Value) -> Value {
        self.request(json!({"cmd":"execute", "tool":tool, "input":input}))
    }

    fn finish(mut self) {
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success());
    }
}

fn generated_file_toolbox(workspace: &Path) -> PathBuf {
    me::toolbox::ensure_default_toolboxes(workspace)
        .unwrap()
        .parent()
        .unwrap()
        .join("File.py")
}

#[test]
fn generated_file_toolbox_is_self_describing_while_stdin_remains_open() {
    let workspace = temporary_workspace();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);
    let tools = toolbox.query("getTools", None);
    assert_eq!(tools["type"], "result");
    assert_eq!(tools["output"].as_array().unwrap().len(), 13);
    assert_eq!(tools["output"][0], "Read");
    assert_eq!(tools["output"][6], "MakeDirectory");
    assert_eq!(tools["output"][12], "Delete");
    let tool_names = tools["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        toolbox.query("getBrief", None)["output"]
            .as_str()
            .unwrap()
            .contains("8-character")
    );
    for tool in &tool_names {
        for command in [
            "getInputSchema",
            "getOutputSchema",
            "getInstructions",
            "getRoute",
            "getExamples",
        ] {
            let frame = toolbox.query(command, Some(tool));
            assert_eq!(frame["type"], "result", "{command} failed for {tool}");
            assert!(!frame["output"].is_null());
            if matches!(command, "getInputSchema" | "getOutputSchema") {
                assert_eq!(frame["output"]["type"], "object");
            } else {
                assert!(!frame["output"].as_str().unwrap().is_empty());
            }
        }
    }
    assert!(tool_names.contains(&"ApplyPatch".to_owned()));
    assert!(!tool_names.contains(&"Edit".to_owned()));
    assert_eq!(
        toolbox.query("getInputSchema", Some("ApplyPatch"))["output"]["properties"]["patch"]["minLength"],
        1
    );
    let patch_instructions = toolbox.query("getInstructions", Some("ApplyPatch"));
    assert!(
        patch_instructions["output"]
            .as_str()
            .unwrap()
            .contains("standard unified diff")
    );
    let patch_examples = toolbox.query("getExamples", Some("ApplyPatch"));
    let patch_examples = patch_examples["output"].as_str().unwrap();
    assert!(patch_examples.contains("@@ -1,2 +1,2 @@"));
    let example_objects = patch_examples
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(example_objects.len(), 3);
    assert!(
        example_objects[2]["patch"]
            .as_str()
            .unwrap()
            .contains("\\ No newline at end of file")
    );
    assert_eq!(
        toolbox.query("getInputSchema", Some("Read"))["output"]["properties"]["encoding"]["default"],
        "auto"
    );
    let create_encodings = toolbox.query("getInputSchema", Some("Create"))["output"]["properties"]
        ["encoding"]["enum"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!create_encodings.contains(&json!("auto")));
    assert!(create_encodings.contains(&json!("gb18030")));
    assert_eq!(
        toolbox.query("getOutputSchema", Some("Read"))["output"]["properties"]["bom"]["type"],
        "boolean"
    );
    assert_eq!(
        toolbox.query("getInputSchema", Some("MakeDirectory"))["output"]["properties"]["parents"]["default"],
        false
    );
    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn file_jsonl_forces_utf8_when_the_host_requests_gbk() {
    let workspace = temporary_workspace();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start_with_io_encoding(&workspace, &script, Some("gbk"));
    let marker = "文件\u{e687}›";
    let response = toolbox.request(json!({
        "cmd":"getInputSchema",
        "tool":marker
    }));
    assert_eq!(response["type"], "error");
    assert_eq!(response["error"]["code"], "unknown_tool");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains(marker)
    );
    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn file_mutations_chain_hashes_and_never_add_implicit_text() {
    let workspace = temporary_workspace();
    fs::create_dir_all(workspace.join("archive")).unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let created = toolbox.execute(
        "Create",
        json!({"path":"notes.txt", "content":"alpha\nbeta"}),
    );
    assert_eq!(created["type"], "result");
    let hash1 = created["output"]["hash"].as_str().unwrap().to_owned();
    assert_eq!(hash1.len(), 8);
    assert!(hash1.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(created["output"]["previous_hash"], Value::Null);

    let read = toolbox.execute("Read", json!({"path":"notes.txt"}));
    assert_eq!(read["output"]["content"], "alpha\nbeta");
    assert_eq!(read["output"]["hash"], hash1);

    let edited = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"notes.txt",
            "expected_hash":hash1,
            "patch":"--- a/notes.txt\n+++ b/notes.txt\n@@ -1,2 +1,2 @@\n-alpha\n-beta\n\\ No newline at end of file\n+first\n+second\n\\ No newline at end of file\n"
        }),
    );
    assert_eq!(edited["output"]["previous_hash"], read["output"]["hash"]);
    assert_eq!(edited["output"]["hunks_applied"], 1);
    assert_eq!(edited["output"]["lines_added"], 2);
    assert_eq!(edited["output"]["lines_removed"], 2);
    let hash2 = edited["output"]["hash"].as_str().unwrap().to_owned();

    let appended = toolbox.execute(
        "Append",
        json!({"path":"notes.txt", "expected_hash":hash2, "content":" tail"}),
    );
    let hash3 = appended["output"]["hash"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "first\nsecond tail"
    );
    assert_eq!(appended["output"]["appended_bytes"], 5);

    let replaced = toolbox.execute(
        "Replace",
        json!({"path":"notes.txt", "expected_hash":hash3, "content":"whole\n"}),
    );
    let hash4 = replaced["output"]["hash"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "whole\n"
    );

    let moved = toolbox.execute(
        "Move",
        json!({
            "path":"notes.txt",
            "destination":"archive/notes.txt",
            "expected_hash":hash4
        }),
    );
    assert_eq!(moved["output"]["previous_hash"], moved["output"]["hash"]);
    assert!(!workspace.join("notes.txt").exists());
    assert!(workspace.join("archive/notes.txt").is_file());

    let deleted = toolbox.execute(
        "Delete",
        json!({
            "path":"archive/notes.txt",
            "expected_hash":moved["output"]["hash"]
        }),
    );
    assert_eq!(deleted["output"]["deleted_hash"], moved["output"]["hash"]);
    assert_eq!(deleted["output"]["exists"], false);
    assert!(!workspace.join("archive/notes.txt").exists());

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn apply_patch_supports_standard_multi_hunk_diffs_and_preserves_line_endings() {
    let workspace = temporary_workspace();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);
    let created = toolbox.execute(
        "Create",
        json!({
            "path":"sample.txt",
            "content":"one\r\ntwo\r\nthree\r\nfour\r\ndrop\r\nlast"
        }),
    );

    let patched = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"sample.txt",
            "expected_hash":created["output"]["hash"],
            "patch":"--- a/sample.txt\n+++ b/sample.txt\n@@ -1,2 +1,3 @@\n one\n-two\n+second\n+inserted\n@@ -4,3 +5,2 @@\n-four\n+fourth\n-drop\n last\n\\ No newline at end of file\n"
        }),
    );
    assert_eq!(patched["type"], "result", "patch failed: {patched}");
    assert_eq!(patched["output"]["operation"], "patched");
    assert_eq!(patched["output"]["hunks_applied"], 2);
    assert_eq!(patched["output"]["lines_added"], 3);
    assert_eq!(patched["output"]["lines_removed"], 3);
    assert_eq!(
        fs::read(workspace.join("sample.txt")).unwrap(),
        b"one\r\nsecond\r\ninserted\r\nthree\r\nfourth\r\nlast"
    );

    let empty = toolbox.execute("Create", json!({"path":"empty.txt", "content":""}));
    let inserted = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"empty.txt",
            "expected_hash":empty["output"]["hash"],
            "patch":"--- empty.txt\n+++ empty.txt\n@@ -0,0 +1,2 @@\n+alpha\n+omega\n\\ No newline at end of file\n"
        }),
    );
    assert_eq!(inserted["type"], "result", "empty patch failed: {inserted}");
    assert_eq!(
        fs::read_to_string(workspace.join("empty.txt")).unwrap(),
        "alpha\nomega"
    );

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn apply_patch_rejects_malformed_or_wrong_file_diffs_without_writing() {
    let workspace = temporary_workspace();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);
    let created = toolbox.execute(
        "Create",
        json!({"path":"safe.txt", "content":"alpha\nbeta\n"}),
    );
    let hash = created["output"]["hash"].as_str().unwrap();
    let cases = [
        (
            "wrong path",
            "--- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-alpha\n+changed\n",
        ),
        (
            "wrong counts",
            "--- a/safe.txt\n+++ b/safe.txt\n@@ -1,2 +1,2 @@\n-alpha\n+changed\n",
        ),
        (
            "markdown fence",
            "```diff\n--- a/safe.txt\n+++ b/safe.txt\n@@ -1 +1 @@\n-alpha\n+changed\n```\n",
        ),
        (
            "custom patch dialect",
            "*** Begin Patch\n*** Update File: safe.txt\n@@\n-alpha\n+changed\n*** End Patch\n",
        ),
    ];
    for (name, patch) in cases {
        let rejected = toolbox.execute(
            "ApplyPatch",
            json!({"path":"safe.txt", "expected_hash":hash, "patch":patch}),
        );
        assert_eq!(rejected["type"], "error", "{name} unexpectedly succeeded");
        assert_eq!(rejected["error"]["code"], "invalid_patch", "case={name}");
        assert_eq!(
            fs::read_to_string(workspace.join("safe.txt")).unwrap(),
            "alpha\nbeta\n",
            "case={name} mutated the file"
        );
    }

    let removed_edit = toolbox.execute(
        "Edit",
        json!({
            "path":"safe.txt",
            "expected_hash":hash,
            "edits":[{"find":"alpha", "replace":"changed"}]
        }),
    );
    assert_eq!(removed_edit["error"]["code"], "unknown_tool");
    assert_eq!(
        fs::read_to_string(workspace.join("safe.txt")).unwrap(),
        "alpha\nbeta\n"
    );

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn make_directory_supports_strict_and_recursive_creation_safely() {
    let workspace = temporary_workspace();
    let outside = workspace.parent().unwrap().join(format!(
        "me-file-directory-outside-{}",
        workspace.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&outside).unwrap();
    fs::write(workspace.join("occupied"), "file").unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let parent = toolbox.execute("MakeDirectory", json!({"path":"generated"}));
    assert_eq!(parent["type"], "result");
    assert_eq!(parent["output"]["path"], "generated");
    assert_eq!(parent["output"]["operation"], "directory_created");
    assert_eq!(parent["output"]["exists"], true);
    assert!(workspace.join("generated").is_dir());

    let child = toolbox.execute("MakeDirectory", json!({"path":"generated/nested"}));
    assert_eq!(child["type"], "result");
    assert!(workspace.join("generated/nested").is_dir());

    let existing_directory = toolbox.execute("MakeDirectory", json!({"path":"generated"}));
    assert_eq!(existing_directory["error"]["code"], "already_exists");
    let existing_file = toolbox.execute("MakeDirectory", json!({"path":"occupied"}));
    assert_eq!(existing_file["error"]["code"], "already_exists");

    let missing_parent = toolbox.execute("MakeDirectory", json!({"path":"missing/child"}));
    assert_eq!(missing_parent["error"]["code"], "parent_not_found");
    assert!(!workspace.join("missing").exists());

    let recursive = toolbox.execute(
        "MakeDirectory",
        json!({"path":"recursive/one/two/three", "parents":true}),
    );
    assert_eq!(recursive["type"], "result");
    assert_eq!(recursive["output"]["path"], "recursive/one/two/three");
    assert!(workspace.join("recursive/one/two/three").is_dir());
    let recursive_existing = toolbox.execute(
        "MakeDirectory",
        json!({"path":"recursive/one/two/three", "parents":true}),
    );
    assert_eq!(recursive_existing["error"]["code"], "already_exists");

    let invalid_parents =
        toolbox.execute("MakeDirectory", json!({"path":"invalid", "parents":"yes"}));
    assert_eq!(invalid_parents["error"]["code"], "invalid_arguments");
    assert!(!workspace.join("invalid").exists());

    let root = toolbox.execute("MakeDirectory", json!({"path":"."}));
    assert_eq!(root["error"]["code"], "invalid_path");
    let recursive_root = toolbox.execute("MakeDirectory", json!({"path":".", "parents":true}));
    assert_eq!(recursive_root["error"]["code"], "invalid_path");
    let escaped = toolbox.execute(
        "MakeDirectory",
        json!({"path":format!("../{}/child", outside.file_name().unwrap().to_string_lossy())}),
    );
    assert_eq!(escaped["error"]["code"], "outside_workspace");
    assert!(!outside.join("child").exists());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, workspace.join("outside-link")).unwrap();
        let symlink_escape = toolbox.execute(
            "MakeDirectory",
            json!({"path":"outside-link/missing/child", "parents":true}),
        );
        assert_eq!(symlink_escape["error"]["code"], "outside_workspace");
        assert!(!outside.join("child").exists());
    }

    toolbox.finish();
    fs::remove_dir_all(outside).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn stale_hash_and_invalid_patches_fail_without_mutating_the_file() {
    let workspace = temporary_workspace();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);
    let created = toolbox.execute(
        "Create",
        json!({"path":"state.txt", "content":"same\nsame\nend\n"}),
    );
    let hash = created["output"]["hash"].as_str().unwrap();

    let mismatch = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"state.txt",
            "expected_hash":hash,
            "patch":"--- a/state.txt\n+++ b/state.txt\n@@ -1 +1 @@\n-missing\n+changed\n"
        }),
    );
    assert_eq!(mismatch["type"], "error");
    assert_eq!(mismatch["error"]["code"], "patch_conflict");
    assert_eq!(
        fs::read_to_string(workspace.join("state.txt")).unwrap(),
        "same\nsame\nend\n"
    );

    let late_failure = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"state.txt",
            "expected_hash":hash,
            "patch":"--- a/state.txt\n+++ b/state.txt\n@@ -1 +1 @@\n-same\n+temporary\n@@ -3 +3 @@\n-missing\n+never\n"
        }),
    );
    assert_eq!(late_failure["error"]["code"], "patch_conflict");
    assert_eq!(
        fs::read_to_string(workspace.join("state.txt")).unwrap(),
        "same\nsame\nend\n"
    );

    fs::write(workspace.join("state.txt"), "external\n").unwrap();
    let stale = toolbox.execute(
        "Append",
        json!({"path":"state.txt", "expected_hash":hash, "content":"should-not-appear"}),
    );
    assert_eq!(stale["type"], "error");
    assert_eq!(stale["error"]["code"], "conflict");
    assert!(
        stale["error"]["message"]
            .as_str()
            .unwrap()
            .contains("current_hash=")
    );
    assert_eq!(
        fs::read_to_string(workspace.join("state.txt")).unwrap(),
        "external\n"
    );

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn read_list_find_search_stat_and_bytes_have_stable_structured_results() {
    let workspace = temporary_workspace();
    fs::create_dir_all(workspace.join("src/nested")).unwrap();
    fs::write(workspace.join("src/a.txt"), "zero\nNeedle one\nlast\n").unwrap();
    fs::write(workspace.join("src/nested/b.txt"), "needle two\n").unwrap();
    fs::write(workspace.join("src/blob.bin"), [0, 1, 2, 255]).unwrap();
    fs::write(workspace.join("src/.hidden.txt"), "Needle hidden\n").unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let read = toolbox.execute(
        "Read",
        json!({"path":"src/a.txt", "start_line":2, "max_lines":1}),
    );
    assert_eq!(read["output"]["content"], "Needle one\n");
    assert_eq!(read["output"]["start_line"], 2);
    assert_eq!(read["output"]["end_line"], 2);
    assert_eq!(read["output"]["truncated"], true);

    let bytes = toolbox.execute(
        "ReadBytes",
        json!({"path":"src/blob.bin", "offset":1, "length":2}),
    );
    assert_eq!(bytes["output"]["base64"], "AQI=");
    assert_eq!(bytes["output"]["length"], 2);
    assert_eq!(bytes["output"]["eof"], false);

    let list = toolbox.execute(
        "List",
        json!({"path":"src", "depth":2, "include_hidden":false}),
    );
    let listed = list["output"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        vec![
            "src/a.txt",
            "src/blob.bin",
            "src/nested",
            "src/nested/b.txt"
        ]
    );

    let find = toolbox.execute(
        "Find",
        json!({"patterns":["src/**/*.txt"], "include_hidden":false}),
    );
    assert_eq!(
        find["output"]["results"],
        json!(["src/a.txt", "src/nested/b.txt"])
    );

    let search = toolbox.execute(
        "Search",
        json!({
            "path":"src",
            "query":"needle",
            "case_sensitive":false,
            "context_before":1,
            "context_after":1
        }),
    );
    assert_eq!(search["output"]["matches"].as_array().unwrap().len(), 2);
    assert_eq!(search["output"]["matches"][0]["line"], 2);
    assert_eq!(search["output"]["matches"][0]["column"], 1);
    assert_eq!(search["output"]["matches"][0]["before"], json!(["zero"]));
    assert_eq!(search["output"]["skipped_binary"], 1);

    let status = toolbox.execute(
        "Stat",
        json!({"paths":["src/a.txt", "src", "not-here.txt"]}),
    );
    assert_eq!(status["output"]["entries"][0]["type"], "file");
    assert_eq!(
        status["output"]["entries"][0]["hash"]
            .as_str()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(status["output"]["entries"][1]["type"], "directory");
    assert_eq!(status["output"]["entries"][2]["exists"], false);

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn file_toolbox_rejects_escape_overwrite_unknown_fields_and_mutable_symlinks() {
    let workspace = temporary_workspace();
    let outside = workspace.parent().unwrap().join(format!(
        "me-file-outside-{}",
        workspace.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&outside, "outside").unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let escaped = toolbox.execute(
        "Read",
        json!({"path":format!("../{}", outside.file_name().unwrap().to_string_lossy())}),
    );
    assert_eq!(escaped["type"], "error");
    assert_eq!(escaped["error"]["code"], "outside_workspace");

    let created = toolbox.execute("Create", json!({"path":"safe.txt", "content":"safe"}));
    let duplicate = toolbox.execute("Create", json!({"path":"safe.txt", "content":"overwrite"}));
    assert_eq!(duplicate["error"]["code"], "already_exists");
    assert_eq!(
        fs::read_to_string(workspace.join("safe.txt")).unwrap(),
        "safe"
    );

    let unknown = toolbox.execute(
        "Append",
        json!({
            "path":"safe.txt",
            "expected_hash":created["output"]["hash"],
            "content":"x",
            "surprise":true
        }),
    );
    assert_eq!(unknown["error"]["code"], "invalid_arguments");
    assert_eq!(
        fs::read_to_string(workspace.join("safe.txt")).unwrap(),
        "safe"
    );

    let lock = toolbox.execute("Stat", json!({"paths":[".me/file-toolbox.lock"]}));
    let protected = toolbox.execute(
        "Delete",
        json!({
            "path":".me/file-toolbox.lock",
            "expected_hash":lock["output"]["entries"][0]["hash"]
        }),
    );
    assert_eq!(protected["error"]["code"], "protected_path");

    let directory_delete = toolbox.execute(
        "Delete",
        json!({"path":"src", "expected_hash":created["output"]["hash"]}),
    );
    assert_eq!(directory_delete["type"], "error");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(workspace.join("safe.txt"), workspace.join("link.txt")).unwrap();
        let symlink_delete = toolbox.execute(
            "Delete",
            json!({"path":"link.txt", "expected_hash":created["output"]["hash"]}),
        );
        assert_eq!(symlink_delete["error"]["code"], "unsupported_file_type");
        assert!(workspace.join("safe.txt").is_file());
    }

    toolbox.finish();
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn detects_common_text_encodings_and_reports_bom_and_confidence() {
    let workspace = temporary_workspace();
    fs::write(
        workspace.join("simplified.txt"),
        b"\xbc\xf2\xcc\xe5\xd6\xd0\xce\xc4\xc4\xda\xc8\xdd\xa3\xac\xc4\xe3\xba\xc3\xca\xc0\xbd\xe7\xa1\xa3\r\n\xb5\xda\xb6\xfe\xd0\xd0",
    )
    .unwrap();
    fs::write(
        workspace.join("traditional.txt"),
        b"\xc1c\xc5\xe9\xa4\xa4\xa4\xe5\xa4\xba\xaee\xa1A\xa7A\xa6n\xa5@\xac\xc9\xa1C\r\n\xb2\xc4\xa4G\xa6\xe6",
    )
    .unwrap();
    fs::write(
        workspace.join("japanese.txt"),
        b"\x93\xfa\x96{\x8c\xea\x82\xcc\x95\xb6\x8f\xcd\x82\xc5\x82\xb7\x81B\x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd\x90\xa2\x8aE\x81B\n\x93\xf1\x8ds\x96\xda",
    )
    .unwrap();
    fs::write(
        workspace.join("korean.txt"),
        b"\xc7\xd1\xb1\xb9\xbe\xee \xb9\xae\xc0\xe5\xc0\xd4\xb4\xcf\xb4\xd9. \xbe\xc8\xb3\xe7\xc7\xcf\xbc\xbc\xbf\xe4 \xbc\xbc\xb0\xe8.\n\xb5\xce \xb9\xf8\xc2\xb0 \xc1\xd9",
    )
    .unwrap();
    fs::write(
        workspace.join("western.txt"),
        b"Caf\xe9 d\xe9j\xe0 vu \x96 r\xe9sum\xe9 and na\xefve.",
    )
    .unwrap();
    fs::write(workspace.join("utf16.txt"), utf16_le("alpha\r\n中文", true)).unwrap();
    fs::write(
        workspace.join("utf16-no-bom.txt"),
        utf16_le("plain ASCII\r\nsecond", false),
    )
    .unwrap();
    fs::write(workspace.join("utf32.txt"), utf32_be("A中\n", true)).unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    for (path, encoding, content, bom) in [
        (
            "simplified.txt",
            "gb18030",
            "简体中文内容，你好世界。\r\n第二行",
            false,
        ),
        (
            "traditional.txt",
            "big5",
            "繁體中文內容，你好世界。\r\n第二行",
            false,
        ),
        (
            "japanese.txt",
            "shift_jis",
            "日本語の文章です。こんにちは世界。\n二行目",
            false,
        ),
        (
            "korean.txt",
            "euc_kr",
            "한국어 문장입니다. 안녕하세요 세계.\n두 번째 줄",
            false,
        ),
        (
            "western.txt",
            "windows-1252",
            "Café déjà vu – résumé and naïve.",
            false,
        ),
        ("utf16.txt", "utf-16-le", "alpha\r\n中文", true),
        (
            "utf16-no-bom.txt",
            "utf-16-le",
            "plain ASCII\r\nsecond",
            false,
        ),
        ("utf32.txt", "utf-32-be", "A中\n", true),
    ] {
        let read = toolbox.execute("Read", json!({"path":path}));
        assert_eq!(read["type"], "result", "failed to read {path}: {read}");
        assert_eq!(read["output"]["encoding"], encoding);
        assert_eq!(read["output"]["content"], content);
        assert_eq!(read["output"]["bom"], bom);
        assert!(read["output"]["encoding_confidence"].as_f64().unwrap() >= 0.78);
    }

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn text_mutations_preserve_detected_encoding_bom_and_original_line_endings() {
    let workspace = temporary_workspace();
    let legacy_initial = b"\xd6\xd0\xce\xc4\r\n";
    fs::write(workspace.join("legacy.txt"), legacy_initial).unwrap();
    fs::write(
        workspace.join("unicode.txt"),
        utf16_le("alpha\r\n中文", true),
    )
    .unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let legacy = toolbox.execute("Read", json!({"path":"legacy.txt"}));
    assert_eq!(legacy["output"]["encoding"], "gb18030");
    let edited = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"legacy.txt",
            "expected_hash":legacy["output"]["hash"],
            "patch":"--- a/legacy.txt\n+++ b/legacy.txt\n@@ -1 +1 @@\n-中文\n+内容\n"
        }),
    );
    assert_eq!(edited["output"]["encoding"], "gb18030");
    assert_eq!(edited["output"]["bom"], false);
    assert_eq!(
        fs::read(workspace.join("legacy.txt")).unwrap(),
        b"\xc4\xda\xc8\xdd\r\n"
    );
    let appended = toolbox.execute(
        "Append",
        json!({
            "path":"legacy.txt",
            "expected_hash":edited["output"]["hash"],
            "content":"你好"
        }),
    );
    assert_eq!(appended["type"], "result", "append failed: {appended}");
    assert_eq!(appended["output"]["appended_bytes"], 4);
    assert_eq!(
        fs::read(workspace.join("legacy.txt")).unwrap(),
        b"\xc4\xda\xc8\xdd\r\n\xc4\xe3\xba\xc3"
    );
    let replaced = toolbox.execute(
        "Replace",
        json!({
            "path":"legacy.txt",
            "expected_hash":appended["output"]["hash"],
            "content":"简体中文\r\n"
        }),
    );
    assert_eq!(replaced["output"]["encoding"], "gb18030");
    assert_eq!(
        fs::read(workspace.join("legacy.txt")).unwrap(),
        b"\xbc\xf2\xcc\xe5\xd6\xd0\xce\xc4\r\n"
    );
    let search = toolbox.execute("Search", json!({"path":"legacy.txt", "query":"中文"}));
    assert_eq!(search["output"]["matches"].as_array().unwrap().len(), 1);

    let unicode = toolbox.execute("Read", json!({"path":"unicode.txt"}));
    let unicode_edited = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"unicode.txt",
            "expected_hash":unicode["output"]["hash"],
            "patch":"--- a/unicode.txt\n+++ b/unicode.txt\n@@ -1 +1 @@\n-alpha\n+beta\n"
        }),
    );
    let unicode_appended = toolbox.execute(
        "Append",
        json!({
            "path":"unicode.txt",
            "expected_hash":unicode_edited["output"]["hash"],
            "content":"你好"
        }),
    );
    assert_eq!(unicode_appended["output"]["encoding"], "utf-16-le");
    assert_eq!(unicode_appended["output"]["bom"], true);
    assert_eq!(
        fs::read(workspace.join("unicode.txt")).unwrap(),
        utf16_le("beta\r\n中文你好", true)
    );

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn explicit_encoding_handles_ambiguity_and_unrepresentable_text_never_mutates() {
    let workspace = temporary_workspace();
    fs::write(workspace.join("ambiguous.txt"), b"\x81\x40").unwrap();
    let script = generated_file_toolbox(&workspace);
    let mut toolbox = ToolboxProcess::start(&workspace, &script);

    let uncertain = toolbox.execute("Read", json!({"path":"ambiguous.txt"}));
    assert_eq!(uncertain["type"], "error");
    assert_eq!(uncertain["error"]["code"], "encoding_uncertain");
    let explicit = toolbox.execute(
        "Read",
        json!({"path":"ambiguous.txt", "encoding":"gb18030"}),
    );
    assert_eq!(explicit["output"]["content"], "丂");
    assert_eq!(explicit["output"]["encoding_confidence"], 1.0);
    let uncertain_write = toolbox.execute(
        "Append",
        json!({
            "path":"ambiguous.txt",
            "expected_hash":explicit["output"]["hash"],
            "content":"text"
        }),
    );
    assert_eq!(uncertain_write["error"]["code"], "encoding_uncertain");
    assert_eq!(
        fs::read(workspace.join("ambiguous.txt")).unwrap(),
        b"\x81\x40"
    );

    let created = toolbox.execute(
        "Create",
        json!({
            "path":"western.txt",
            "content":"Café – résumé",
            "encoding":"windows-1252"
        }),
    );
    assert_eq!(created["type"], "result");
    let before = fs::read(workspace.join("western.txt")).unwrap();
    let rejected_patch = toolbox.execute(
        "ApplyPatch",
        json!({
            "path":"western.txt",
            "expected_hash":created["output"]["hash"],
            "patch":"--- a/western.txt\n+++ b/western.txt\n@@ -1 +1 @@\n-Café – résumé\n\\ No newline at end of file\n+Café – 中文\n\\ No newline at end of file\n"
        }),
    );
    assert_eq!(rejected_patch["type"], "error");
    assert_eq!(rejected_patch["error"]["code"], "encoding_error");
    assert_eq!(fs::read(workspace.join("western.txt")).unwrap(), before);

    let rejected = toolbox.execute(
        "Append",
        json!({
            "path":"western.txt",
            "expected_hash":created["output"]["hash"],
            "content":"中文"
        }),
    );
    assert_eq!(rejected["type"], "error");
    assert_eq!(rejected["error"]["code"], "encoding_error");
    assert_eq!(fs::read(workspace.join("western.txt")).unwrap(), before);

    let invalid_bom = toolbox.execute(
        "Create",
        json!({
            "path":"legacy-bom.txt",
            "content":"text",
            "encoding":"gb18030",
            "bom":true
        }),
    );
    assert_eq!(invalid_bom["error"]["code"], "invalid_encoding");
    assert!(!workspace.join("legacy-bom.txt").exists());

    let unicode = toolbox.execute(
        "Create",
        json!({
            "path":"created-utf16.txt",
            "content":"中文\r\n",
            "encoding":"utf-16-le",
            "bom":true
        }),
    );
    assert_eq!(unicode["type"], "result");
    assert_eq!(unicode["output"]["bom"], true);
    assert_eq!(
        fs::read(workspace.join("created-utf16.txt")).unwrap(),
        utf16_le("中文\r\n", true)
    );
    let mismatched = toolbox.execute(
        "Read",
        json!({"path":"created-utf16.txt", "encoding":"utf-8"}),
    );
    assert_eq!(mismatched["error"]["code"], "encoding_mismatch");

    let auto_create = toolbox.execute(
        "Create",
        json!({"path":"auto.txt", "content":"text", "encoding":"auto"}),
    );
    assert_eq!(auto_create["error"]["code"], "invalid_encoding");
    assert!(!workspace.join("auto.txt").exists());

    toolbox.finish();
    fs::remove_dir_all(workspace).unwrap();
}
