#!/usr/bin/env python3
# ME-RUST-MANAGED-TOOLBOX
"""ME-RUST default workspace File toolbox."""

from __future__ import annotations

import contextlib
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
import tempfile
import unicodedata
from dataclasses import dataclass
from typing import Any, Iterator


def fail_startup(message: str) -> "None":
    print(message, file=sys.stderr, flush=True)
    raise SystemExit(1)


if sys.version_info[:2] != (3, 12):
    fail_startup(
        "File toolbox requires Python 3.12; "
        f"received {sys.version_info.major}.{sys.version_info.minor}"
    )

sys.stdin.reconfigure(encoding="utf-8", errors="strict")
sys.stdout.reconfigure(encoding="utf-8", errors="strict", newline="\n")
sys.stderr.reconfigure(encoding="utf-8", errors="strict", newline="\n")


ROOT = Path.cwd().resolve(strict=True)
LOCK_PATH = ROOT / ".me" / "file-toolbox.lock"
HASH_PATTERN = re.compile(r"^[0-9a-f]{8}$")
HUNK_PATTERN = re.compile(
    r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?: .*)?$"
)
MAX_EDIT_OPERATIONS = 128
EDIT_TIP = (
    "The file was edited. Its previous line numbers and hash are now stale, "
    "and the new hash is intentionally not returned. Before editing this file "
    "again, you MUST use File.Read or File.Search to obtain refreshed numbered "
    "lines and the latest hash."
)
EDIT_BYTES_TIP = (
    "The file was edited. Its previous byte offsets and hash are now stale, "
    "and the new hash is intentionally not returned. Before editing this file "
    "again, you MUST use File.ReadBytes to obtain refreshed bytes and the "
    "latest hash."
)
TEXT_ENCODINGS = [
    "auto",
    "utf-8",
    "utf-16-le",
    "utf-16-be",
    "utf-32-le",
    "utf-32-be",
    "gb18030",
    "big5",
    "shift_jis",
    "euc_kr",
    "windows-1252",
]
ENCODING_CODECS = {
    "utf-8": "utf-8",
    "utf-16-le": "utf-16-le",
    "utf-16-be": "utf-16-be",
    "utf-32-le": "utf-32-le",
    "utf-32-be": "utf-32-be",
    "gb18030": "gb18030",
    "big5": "big5",
    "shift_jis": "shift_jis",
    "euc_kr": "euc_kr",
    "windows-1252": "cp1252",
}
ENCODING_ALIASES = {
    "utf8": "utf-8",
    "utf_8": "utf-8",
    "utf16le": "utf-16-le",
    "utf16be": "utf-16-be",
    "utf32le": "utf-32-le",
    "utf32be": "utf-32-be",
    "gbk": "gb18030",
    "cp936": "gb18030",
    "shift-jis": "shift_jis",
    "sjis": "shift_jis",
    "euc-kr": "euc_kr",
    "cp1252": "windows-1252",
}
BOMS = [
    (b"\x00\x00\xfe\xff", "utf-32-be"),
    (b"\xff\xfe\x00\x00", "utf-32-le"),
    (b"\xef\xbb\xbf", "utf-8"),
    (b"\xfe\xff", "utf-16-be"),
    (b"\xff\xfe", "utf-16-le"),
]
ENCODING_SCHEMA = {"type": "string", "enum": TEXT_ENCODINGS}
CREATE_ENCODING_SCHEMA = {"type": "string", "enum": TEXT_ENCODINGS[1:]}
TOOLS = [
    "Read",
    "ReadBytes",
    "EditBytes",
    "List",
    "Find",
    "Search",
    "Stat",
    "MakeDirectory",
    "Create",
    "Edit",
    "Append",
    "Replace",
    "Move",
    "Delete",
]


class ToolError(Exception):
    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


@dataclass(frozen=True)
class TextDocument:
    raw: bytes
    text: str
    encoding: str
    confidence: float
    bom: bytes


@dataclass
class PatchLine:
    kind: str
    text: str
    no_newline: bool = False


@dataclass(frozen=True)
class PatchHunk:
    old_start: int
    old_count: int
    new_start: int
    new_count: int
    lines: tuple[PatchLine, ...]


@dataclass(frozen=True)
class TextLine:
    text: str
    ending: str


@dataclass(frozen=True)
class ResolvedEdit:
    index: int
    target_line_start: int
    target_line_end: int
    source_start: int
    source_end: int
    new_lines: tuple[str, ...]
    replacement_text: str
    replacement_bytes: int
    kind: str


@dataclass(frozen=True)
class ResolvedByteEdit:
    index: int
    target_offset: int
    target_length: int
    source_start: int
    source_end: int
    data: bytes
    kind: str


def object_schema(
    properties: dict[str, Any], required: list[str] | None = None
) -> dict[str, Any]:
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


PATH_SCHEMA = {"type": "string", "minLength": 1}
HASH_SCHEMA = {"type": "string", "pattern": r"^[0-9a-f]{8}$"}
STRING_ARRAY = {
    "type": "array",
    "items": {"type": "string", "minLength": 1},
    "maxItems": 256,
}

INPUT_SCHEMAS: dict[str, dict[str, Any]] = {
    "Read": object_schema(
        {
            "path": PATH_SCHEMA,
            "start_line": {"type": "integer", "minimum": 1, "default": 1},
            "encoding": {**ENCODING_SCHEMA, "default": "auto"},
            "max_lines": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10000,
                "default": 500,
            },
        },
        ["path"],
    ),
    "ReadBytes": object_schema(
        {
            "path": PATH_SCHEMA,
            "offset": {"type": "integer", "minimum": 0, "default": 0},
            "length": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1048576,
                "default": 65536,
            },
        },
        ["path"],
    ),
    "EditBytes": object_schema(
        {
            "path": PATH_SCHEMA,
            "expected_hash": HASH_SCHEMA,
            "edits": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_EDIT_OPERATIONS,
                "description": "Atomic byte edit operations whose offsets all refer to the same original file identified by expected_hash.",
                "items": object_schema(
                    {
                        "target_offset": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 2**63 - 1,
                            "description": "Zero-based original byte offset at which the selected half-open range begins.",
                        },
                        "target_length": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 2**63 - 1,
                            "description": "Number of original bytes selected. Zero denotes an insertion point.",
                        },
                        "data": {
                            "type": "string",
                            "description": "Replacement bytes as lowercase two-digit hexadecimal values separated by one space. An empty string deletes a non-empty selected range.",
                        },
                    },
                    ["target_offset", "target_length", "data"],
                ),
            },
        },
        ["path", "expected_hash", "edits"],
    ),
    "List": object_schema(
        {
            "path": {"type": "string", "minLength": 1, "default": "."},
            "depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 32,
                "default": 1,
            },
            "include_hidden": {"type": "boolean", "default": False},
            "max_entries": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10000,
                "default": 1000,
            },
        }
    ),
    "Find": object_schema(
        {
            "path": {"type": "string", "minLength": 1, "default": "."},
            "patterns": {
                "type": "array",
                "items": {"type": "string", "minLength": 1},
                "minItems": 1,
                "maxItems": 64,
            },
            "exclude": STRING_ARRAY,
            "include_hidden": {"type": "boolean", "default": False},
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10000,
                "default": 1000,
            },
        },
        ["patterns"],
    ),
    "Search": object_schema(
        {
            "path": {"type": "string", "minLength": 1, "default": "."},
            "query": {"type": "string", "minLength": 1},
            "regex": {"type": "boolean", "default": False},
            "case_sensitive": {"type": "boolean", "default": True},
            "globs": STRING_ARRAY,
            "context_before": {
                "type": "integer",
                "minimum": 0,
                "maximum": 20,
                "default": 0,
            },
            "context_after": {
                "type": "integer",
                "minimum": 0,
                "maximum": 20,
                "default": 0,
            },
            "max_matches": {
                "type": "integer",
                "minimum": 1,
                "maximum": 5000,
                "default": 500,
            },
        },
        ["query"],
    ),
    "Stat": object_schema(
        {
            "paths": {
                "type": "array",
                "items": PATH_SCHEMA,
                "minItems": 1,
                "maxItems": 256,
            }
        },
        ["paths"],
    ),
    "MakeDirectory": object_schema(
        {
            "path": PATH_SCHEMA,
            "parents": {"type": "boolean", "default": False},
        },
        ["path"],
    ),
    "Create": object_schema(
        {
            "path": PATH_SCHEMA,
            "content": {"type": "string"},
            "encoding": {**CREATE_ENCODING_SCHEMA, "default": "utf-8"},
            "bom": {"type": "boolean", "default": False},
        },
        ["path", "content"],
    ),
    "Edit": object_schema(
        {
            "path": PATH_SCHEMA,
            "expected_hash": HASH_SCHEMA,
            "encoding": {**ENCODING_SCHEMA, "default": "auto"},
            "edits": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_EDIT_OPERATIONS,
                "description": "Atomic edit operations whose line coordinates all refer to the same original file identified by expected_hash.",
                "items": object_schema(
                    {
                        "target_line_start": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 2**31 - 1,
                            "description": "First 1-based original source line to replace. For insertion, this must equal target_line_end + 1.",
                        },
                        "target_line_end": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 2**31 - 1,
                            "description": "Last inclusive 1-based original source line to replace. For insertion, this is one less than target_line_start.",
                        },
                        "new_lines": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "minLength": 1,
                            },
                            "description": "Exact replacement physical lines. Every item must contain exactly one line and end in LF, CRLF, or CR. An empty array deletes the selected lines.",
                        },
                    },
                    ["target_line_start", "target_line_end", "new_lines"],
                ),
            },
        },
        ["path", "expected_hash", "edits"],
    ),
    "Append": object_schema(
        {
            "path": PATH_SCHEMA,
            "expected_hash": HASH_SCHEMA,
            "encoding": {**ENCODING_SCHEMA, "default": "auto"},
            "content": {"type": "string"},
        },
        ["path", "expected_hash", "content"],
    ),
    "Replace": object_schema(
        {
            "path": PATH_SCHEMA,
            "expected_hash": HASH_SCHEMA,
            "encoding": {**ENCODING_SCHEMA, "default": "auto"},
            "content": {"type": "string"},
        },
        ["path", "expected_hash", "content"],
    ),
    "Move": object_schema(
        {
            "path": PATH_SCHEMA,
            "destination": PATH_SCHEMA,
            "expected_hash": HASH_SCHEMA,
        },
        ["path", "destination", "expected_hash"],
    ),
    "Delete": object_schema(
        {"path": PATH_SCHEMA, "expected_hash": HASH_SCHEMA},
        ["path", "expected_hash"],
    ),
}


OUTPUT_SCHEMAS: dict[str, dict[str, Any]] = {
    "Read": object_schema(
        {
            "path": PATH_SCHEMA,
            "lines": {
                "type": "object",
                "additionalProperties": {"type": ["string", "object"]},
                "description": "Text keyed by its 1-based file line number, minimally zero-padded to the width of total_lines so serialized keys stay in numeric order. Normal values are exact strings including their original line ending; an oversized value may become a safe text_fragments object only in model context.",
            },
            "start_line": {"type": "integer"},
            "end_line": {"type": "integer"},
            "total_lines": {"type": "integer"},
            "eof": {"type": "boolean"},
            "truncated": {"type": "boolean"},
            "hash": HASH_SCHEMA,
            "size": {"type": "integer"},
            "encoding": CREATE_ENCODING_SCHEMA,
            "encoding_confidence": {"type": "number", "minimum": 0, "maximum": 1},
            "bom": {"type": "boolean"},
        },
        [
            "path",
            "lines",
            "start_line",
            "end_line",
            "total_lines",
            "eof",
            "truncated",
            "hash",
            "size",
            "encoding",
            "encoding_confidence",
            "bom",
        ],
    ),
    "ReadBytes": object_schema(
        {
            "path": PATH_SCHEMA,
            "data": {
                "type": "string",
                "description": "Bytes as lowercase two-digit hexadecimal values separated by one space.",
            },
            "offset": {"type": "integer"},
            "length": {"type": "integer"},
            "size": {"type": "integer"},
            "eof": {"type": "boolean"},
            "hash": HASH_SCHEMA,
        },
        ["path", "data", "offset", "length", "size", "eof", "hash"],
    ),
    "EditBytes": object_schema(
        {
            "path": PATH_SCHEMA,
            "operation": {"type": "string", "enum": ["bytes_edited"]},
            "previous_hash": HASH_SCHEMA,
            "edit_results": {
                "type": "array",
                "items": object_schema(
                    {
                        "index": {"type": "integer"},
                        "state": {"type": "string", "enum": ["succeeded"]},
                        "kind": {
                            "type": "string",
                            "enum": ["replace", "delete", "insert"],
                        },
                        "target_offset": {"type": "integer"},
                        "target_length": {"type": "integer"},
                        "selected_bytes": {"type": "integer"},
                        "replacement_bytes": {"type": "integer"},
                    },
                    [
                        "index",
                        "state",
                        "kind",
                        "target_offset",
                        "target_length",
                        "selected_bytes",
                        "replacement_bytes",
                    ],
                ),
            },
            "previous_size": {"type": "integer"},
            "size": {"type": "integer"},
            "tip": {"type": "string"},
        },
        [
            "path",
            "operation",
            "previous_hash",
            "edit_results",
            "previous_size",
            "size",
            "tip",
        ],
    ),
    "List": object_schema(
        {
            "path": PATH_SCHEMA,
            "entries": {"type": "array", "items": {"type": "object"}},
            "truncated": {"type": "boolean"},
        },
        ["path", "entries", "truncated"],
    ),
    "Find": object_schema(
        {
            "path": PATH_SCHEMA,
            "results": {"type": "array", "items": {"type": "string"}},
            "truncated": {"type": "boolean"},
        },
        ["path", "results", "truncated"],
    ),
    "Search": object_schema(
        {
            "path": PATH_SCHEMA,
            "matches": {
                "type": "array",
                "items": object_schema(
                    {
                        "path": PATH_SCHEMA,
                        "hash": HASH_SCHEMA,
                        "column": {"type": "integer"},
                        "match_length": {"type": "integer"},
                        "before": {
                            "type": "object",
                            "additionalProperties": {"type": "string"},
                            "description": "Exact complete context lines before the match, keyed by their 1-based, minimally zero-padded file line numbers.",
                        },
                        "match_text": {
                            "type": "object",
                            "additionalProperties": {"type": ["string", "object"]},
                            "minProperties": 1,
                            "maxProperties": 1,
                            "description": "The exact matched file line under its 1-based, minimally zero-padded line number. Safe model-context truncation may replace only this value with a text_fragments object.",
                        },
                        "after": {
                            "type": "object",
                            "additionalProperties": {"type": "string"},
                            "description": "Exact complete context lines after the match, keyed by their 1-based, minimally zero-padded file line numbers.",
                        },
                    },
                    [
                        "path",
                        "hash",
                        "column",
                        "match_length",
                        "before",
                        "match_text",
                        "after",
                    ],
                ),
            },
            "skipped_binary": {"type": "integer"},
            "truncated": {"type": "boolean"},
        },
        ["path", "matches", "skipped_binary", "truncated"],
    ),
    "Stat": object_schema(
        {"entries": {"type": "array", "items": {"type": "object"}}},
        ["entries"],
    ),
    "MakeDirectory": object_schema(
        {
            "path": PATH_SCHEMA,
            "operation": {"type": "string"},
            "exists": {"type": "boolean"},
        },
        ["path", "operation", "exists"],
    ),
}

for _tool in ("Create", "Edit", "Append", "Replace"):
    OUTPUT_SCHEMAS[_tool] = object_schema(
        {
            "path": PATH_SCHEMA,
            "operation": {"type": "string"},
            "previous_hash": {"type": ["string", "null"]},
            "hash": HASH_SCHEMA,
            "size": {"type": "integer"},
            "encoding": CREATE_ENCODING_SCHEMA,
            "encoding_confidence": {"type": "number", "minimum": 0, "maximum": 1},
            "bom": {"type": "boolean"},
        },
        [
            "path",
            "operation",
            "hash",
            "size",
            "encoding",
            "encoding_confidence",
            "bom",
        ],
    )
OUTPUT_SCHEMAS["Edit"]["properties"].update(
    {
        "edit_results": {
            "type": "array",
            "items": object_schema(
                {
                    "index": {"type": "integer"},
                    "state": {"type": "string", "enum": ["succeeded"]},
                    "kind": {
                        "type": "string",
                        "enum": ["replace", "delete", "insert"],
                    },
                    "target_line_start": {"type": "integer"},
                    "target_line_end": {"type": "integer"},
                    "selected_lines": {"type": "integer"},
                    "new_line_count": {"type": "integer"},
                    "replacement_bytes": {"type": "integer"},
                },
                [
                    "index",
                    "state",
                    "kind",
                    "target_line_start",
                    "target_line_end",
                    "selected_lines",
                    "new_line_count",
                    "replacement_bytes",
                ],
            ),
        },
        "previous_total_lines": {"type": "integer"},
        "total_lines": {"type": "integer"},
        "previous_size": {"type": "integer"},
        "tip": {"type": "string"},
    }
)
OUTPUT_SCHEMAS["Edit"]["required"].extend(
    [
        "edit_results",
        "previous_total_lines",
        "total_lines",
        "previous_size",
        "tip",
    ]
)
OUTPUT_SCHEMAS["Edit"]["properties"].pop("hash")
OUTPUT_SCHEMAS["Edit"]["required"].remove("hash")
OUTPUT_SCHEMAS["Append"]["properties"]["appended_bytes"] = {"type": "integer"}
OUTPUT_SCHEMAS["Move"] = object_schema(
    {
        "path": PATH_SCHEMA,
        "destination": PATH_SCHEMA,
        "operation": {"type": "string"},
        "previous_hash": HASH_SCHEMA,
        "hash": HASH_SCHEMA,
        "size": {"type": "integer"},
    },
    ["path", "destination", "operation", "previous_hash", "hash", "size"],
)
OUTPUT_SCHEMAS["Delete"] = object_schema(
    {
        "path": PATH_SCHEMA,
        "operation": {"type": "string"},
        "deleted_hash": HASH_SCHEMA,
        "exists": {"type": "boolean"},
    },
    ["path", "operation", "deleted_hash", "exists"],
)


ROUTES = {
    "Read": "Read a bounded text range with conservative automatic encoding detection when exact file content is needed.",
    "ReadBytes": "Read a bounded byte range for binary data, text whose encoding cannot be determined safely, or a File.EditBytes baseline.",
    "EditBytes": "Atomically replace, delete, or insert one or more independently located byte ranges after inspecting them with File.ReadBytes.",
    "List": "Inspect directory contents without invoking a shell.",
    "Find": "Find workspace paths by glob patterns.",
    "Search": "Search text across workspace files by literal text or regular expression.",
    "Stat": "Inspect existence, type, metadata, and current content hashes.",
    "MakeDirectory": "Create one explicit directory, optionally including its missing parent chain.",
    "Create": "Create a new text file in an explicit encoding, defaulting to UTF-8; never overwrite an existing file.",
    "Edit": "Atomically replace, delete, or insert one or more independently located line ranges in a known text file.",
    "Append": "Append exact text using the existing file's detected encoding without adding a newline.",
    "Replace": "Replace an entire known text file while preserving its detected encoding and BOM.",
    "Move": "Move one known regular file to a destination that does not exist.",
    "Delete": "Delete one explicit known regular file.",
}

INSTRUCTIONS = {
    "Read": "Line numbers are 1-based. The lines object maps each actual file line number to its exact text, including that line's original LF, CRLF, CR, or absent final line ending. Keys are minimally zero-padded to the digit width of total_lines solely to preserve numeric order in serialized JSON; interpret them as decimal line numbers. Missing numeric keys in a safely truncated model-visible result are omitted lines, not empty lines. Source file size is not artificially capped: the complete file is loaded into memory to detect its encoding, count lines, and compute its hash, while max_lines bounds the returned range. Auto detection checks BOM, Unicode encodings, strict UTF-8, then common legacy encodings conservatively. The result reports encoding, confidence, BOM presence, and the file's current 8-character concurrency fingerprint. If auto detection is uncertain, retry only when the encoding is known by setting encoding explicitly; otherwise use ReadBytes.",
    "ReadBytes": "Offsets are zero-based, and source file size is not artificially capped. The result data contains lowercase two-digit hexadecimal bytes separated by one space, without a 0x prefix. length is the number of bytes represented by data, and hash identifies the complete file rather than only the returned range. Use the returned bytes and hash as the baseline for File.EditBytes. If the model-context safety envelope reports truncate:true, data retains only the earliest complete bytes from the requested range; read another range before editing bytes that are not visible. truncate_info.ranges.bytes reports retained_offset_start, retained_offset_end_exclusive, removed_offset_start, and removed_offset_end_exclusive as absolute half-open byte ranges.",
    "EditBytes": (
        "EditBytes atomically applies one or more operations to one file. First use File.ReadBytes to inspect every target range and obtain the complete file hash, then pass that hash as expected_hash. target_offset is a zero-based original byte offset, and target_length selects the half-open original range [target_offset, target_offset + target_length). Every operation is independently located against the same original pre-edit snapshot. Earlier array items never shift later offsets, and array order is not execution order. The tool validates every operation before writing and commits the combined result once. A later operation cannot target bytes created by another operation in the same call; perform dependent work only after another ReadBytes.\n"
        "Use target_length > 0 with non-empty data to replace the selected bytes, target_length > 0 with data=\"\" to delete them, and target_length=0 with non-empty data to insert before target_offset. Offset 0 is the beginning; target_offset equal to the original file size is the only insertion point after the final byte and also inserts into an empty file. An empty insertion is invalid. Replacement ranges must not overlap. One original insertion point may appear only once. An insertion strictly inside a replaced range conflicts, while insertion exactly at either outer boundary is allowed. Every selected range must stay within the original file.\n"
        "data is exact binary content written as lowercase two-digit hexadecimal bytes separated by one space, without 0x prefixes; use an empty string only for deletion. Source file size is not artificially capped; the complete file is loaded into memory for the atomic edit. Unselected bytes and file permissions are preserved. Malformed hexadecimal data, invalid or overlapping ranges, duplicate insertion points, a stale hash, and all other failures leave the file unchanged. A successful result deliberately does not return the new hash. Its old byte offsets and hash are stale: before every later File.EditBytes on this file, call File.ReadBytes again and use the refreshed bytes and hash."
    ),
    "List": "Depth counts levels below path. Results are stable and symbolic-link directories are never traversed.",
    "Find": "Patterns and exclusions match workspace-relative POSIX paths. Results are stable and symbolic-link directories are never traversed.",
    "Search": "Literal search is the default. Source file size is not artificially capped; each candidate file is loaded into memory, and max_matches bounds the returned result. Each match returns the file's current hash plus before, match_text, and after as line-number-keyed objects using the same 1-based, minimally zero-padded keys and exact line text as File.Read. The sole key in match_text is the matching file line; column is 1-based within that line, so no separate line field is returned. Example: {\"path\":\"src/main.rs\",\"hash\":\"0123abcd\",\"column\":5,\"match_length\":7,\"before\":{\"041\":\"fn main() {\\n\"},\"match_text\":{\"042\":\"    runtime.start();\\n\"},\"after\":{\"043\":\"}\\n\"}}. The hash and numbered lines form a current File.Edit baseline. With top-level truncate:true, missing before or after keys are omitted context lines; the sole match_text value may instead be a text_fragments object, while its line key, hash, and match metadata remain intact. Text encoding is detected conservatively per file; binary and uncertain files are skipped.",
    "Stat": "A missing path is a normal result. Content hashes are returned only for ordinary files, not directories or symbolic links.",
    "MakeDirectory": "parents defaults to false, requiring the immediate parent to exist. Set parents=true to create every missing directory in the path. The target itself must not already exist; existing files, directories, and symbolic links return already_exists.",
    "Create": "The parent directory must already exist. encoding defaults to utf-8 because a new file has no bytes to inspect; bom defaults to false and is allowed only for UTF encodings. Creation fails if the destination exists.",
    "Edit": (
        "Edit atomically applies one or more operations to one text file. First use File.Read or File.Search to obtain the current numbered text and hash, then pass that hash as expected_hash. Every operation in edits is independently located against that one original pre-edit snapshot. Earlier array items never shift or otherwise change the meaning of later line numbers; array order is not execution order. The tool validates and locates every operation before writing, then commits the combined result once. If any operation is invalid, unencodable, overlapping, duplicated at the same insertion point, or otherwise ambiguous, the entire call fails and the file remains unchanged. A later operation cannot target text created by an earlier operation in the same call; perform that dependent work only after another Read or Search.\n"
        "Line numbers are 1-based and replacement endpoints are inclusive. For replacement or deletion, require 1 <= target_line_start <= target_line_end <= total_lines. For insertion, use the only empty-range form target_line_start = target_line_end + 1; it inserts before target_line_start. Thus 1/0 inserts at the beginning, N/N-1 inserts before line N, total_lines+1/total_lines inserts at the end, and 1/0 is also how an empty file receives its first content. Any larger reversed gap is invalid. Replacement ranges must not share original lines. An insertion must not lie inside a replaced range, and one original insertion point may appear only once; insertion exactly at a replacement's outer boundary is allowed.\n"
        "new_lines is an array of exact physical lines, deliberately matching the line values returned by File.Read and File.Search. Every item must contain exactly one line and must end in LF (\\n), CRLF (\\r\\n), or CR (\\r); an embedded line ending or a missing final line ending is a syntax error. Edit never adds, removes, converts, or guesses line endings. Use new_lines=[] to delete the selected complete lines and their endings. Use new_lines=[\"\\n\"] (or the matching CRLF/CR form) to retain one blank line. To merge several selected source lines, replace their whole range with one complete physical line. File.Edit intentionally cannot create an unterminated line. Insertion after an existing unterminated final line is rejected; include that final source line in a replacement instead.\n"
        "Edit does not search inside a line and does not accept a partial line fragment as a location. To change part of a line, provide that entire resulting physical line in new_lines, including unchanged surrounding characters and its ending. Source file size is not artificially capped; the complete file is loaded into memory for the atomic edit. The existing encoding, BOM, permissions, and all bytes represented by unselected text are preserved through one atomic commit. Unrepresentable text, malformed physical lines, invalid ranges, stale hashes, uncertain encodings, and all other failures leave the file unchanged. A successful File.Edit result deliberately does not return the new hash. Its old line numbers and hash are invalid for further editing: before every later File.Edit on this file, you must call File.Read or File.Search again and use the refreshed numbered lines and hash."
    ),
    "Append": "The file must exist and match expected_hash. Existing encoding and BOM are preserved. Content is appended exactly and no newline is added. Unrepresentable text returns encoding_error without modifying the file.",
    "Replace": "The file must exist and match expected_hash. Its detected encoding and BOM are preserved while the complete content is replaced atomically. Unrepresentable text returns encoding_error without modifying the file.",
    "Move": "The source must match expected_hash and the destination must not exist. A pure move preserves the content hash.",
    "Delete": "The file must match expected_hash. Directories and symbolic links are rejected. Success returns deleted_hash and exists=false.",
}

EXAMPLES = {
    "Read": '{"path":"src/main.rs","start_line":1,"max_lines":200}',
    "ReadBytes": '{"path":"assets/data.bin","offset":0,"length":65536}',
    "EditBytes": (
        "Assume File.ReadBytes returned offset=0, data=\"00 11 22 33 44 55\", size=6, and hash=0123abcd. Every edit below refers to those six original bytes.\n\n"
        "Replace original bytes 11 22 at [1,3) with aa bb:\n"
        '{"path":"assets/data.bin","expected_hash":"0123abcd","edits":[{"target_offset":1,"target_length":2,"data":"aa bb"}]}'
        "\n\nDelete original bytes 22 33 at [2,4):\n"
        '{"path":"assets/data.bin","expected_hash":"0123abcd","edits":[{"target_offset":2,"target_length":2,"data":""}]}'
        "\n\nInsert de ad before the first byte, and insert ff after the original final byte:\n"
        '{"path":"assets/data.bin","expected_hash":"0123abcd","edits":[{"target_offset":0,"target_length":0,"data":"de ad"},{"target_offset":6,"target_length":0,"data":"ff"}]}'
        "\n\nMultiple edits still use original offsets even when an earlier-position edit changes the length. This replaces original 11 with aa bb and original 44 with cc; result is 00 aa bb 22 33 cc 55:\n"
        '{"path":"assets/data.bin","expected_hash":"0123abcd","edits":[{"target_offset":1,"target_length":1,"data":"aa bb"},{"target_offset":4,"target_length":1,"data":"cc"}]}'
        "\n\nArray order is irrelevant. An insertion at offset 2 and a replacement beginning at offset 2 share an allowed outer boundary; the inserted byte appears before the replacement:\n"
        '{"path":"assets/data.bin","expected_hash":"0123abcd","edits":[{"target_offset":2,"target_length":1,"data":"bb"},{"target_offset":2,"target_length":0,"data":"aa"}]}'
        "\n\nCommon errors that reject the entire call include a range past size, target_length=0 with empty data, malformed or incomplete hexadecimal bytes, overlapping replacement ranges, duplicate insertion points, insertion strictly inside a replacement, a stale expected_hash, or attempting to target data inserted by another item. A successful result has no new hash; always call File.ReadBytes again before another EditBytes."
    ),
    "List": '{"path":"src","depth":2,"include_hidden":false}',
    "Find": '{"path":".","patterns":["**/*.rs"],"exclude":["target/**"]}',
    "Search": '{"path":"src","query":"ToolboxRuntime","globs":["**/*.rs"]}',
    "Stat": '{"paths":["Cargo.toml","src/main.rs","missing.txt"]}',
    "MakeDirectory": '{"path":"build/generated/assets","parents":true}',
    "Create": '{"path":"notes.txt","content":"first line\\n","encoding":"utf-8"}',
    "Edit": (
        "Assume File.Read returned 1=aaa\\n, 2=bbb\\n, 3=ccc\\n, 4=ddd\\n with hash 0123abcd unless stated otherwise. Every edit refers to those original lines. Every new_lines item is one exact physical line and therefore includes its final line ending.\n\n"
        "Positive — both replacements use original coordinates even though the first adds a line. Result: 111\\n, aaa\\n, bbb\\n, 333\\n, ccc\\n, ddd\\n:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":1,"target_line_end":1,"new_lines":["111\\n","aaa\\n"]},{"target_line_start":3,"target_line_end":3,"new_lines":["333\\n","ccc\\n"]}]}'
        "\n\nPositive — array order is irrelevant. This replaces original line 4, inserts before original line 2, and deletes original line 3 in one atomic call:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":4,"target_line_end":4,"new_lines":["last\\n"]},{"target_line_start":2,"target_line_end":1,"new_lines":["inserted\\n"]},{"target_line_start":3,"target_line_end":3,"new_lines":[]}]}'
        "\n\nTricky but valid — insertion exactly before a replaced range is an allowed outer-boundary insertion; inserted appears before updated:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":2,"target_line_end":1,"new_lines":["inserted\\n"]},{"target_line_start":2,"target_line_end":2,"new_lines":["updated\\n"]}]}'
        "\n\nReplace several lines with one and another original line with several:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":1,"target_line_end":2,"new_lines":["combined\\n"]},{"target_line_start":4,"target_line_end":4,"new_lines":["one\\n","two\\n","three\\n"]}]}'
        "\n\nDelete complete lines and endings with an empty array; clear text but retain one blank LF line with [\"\\n\"]:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":1,"target_line_end":1,"new_lines":[]},{"target_line_start":3,"target_line_end":3,"new_lines":["\\n"]}]}'
        "\n\nInsert at file start with 1/0 and after a newline-terminated four-line file with 5/4:\n"
        '{"path":"notes.txt","expected_hash":"0123abcd","edits":[{"target_line_start":1,"target_line_end":0,"new_lines":["header\\n"]},{"target_line_start":5,"target_line_end":4,"new_lines":["footer\\n"]}]}'
        "\n\nInsert into an empty file with 1/0:\n"
        '{"path":"empty.txt","expected_hash":"e3b0c442","edits":[{"target_line_start":1,"target_line_end":0,"new_lines":["first line\\n"]}]}'
        "\n\nAn unterminated final source line cannot be appended after directly. Replace that final line and include both complete resulting physical lines:\n"
        '{"path":"unterminated.txt","expected_hash":"0123abcd","edits":[{"target_line_start":4,"target_line_end":4,"new_lines":["original final text\\n","appended\\n"]}]}'
        "\n\nPreserve CRLF explicitly and rewrite a partial phrase by supplying the complete resulting source line:\n"
        '{"path":"windows.txt","expected_hash":"0123abcd","edits":[{"target_line_start":2,"target_line_end":2,"new_lines":["complete changed line\\r\\n"]}]}'
        "\n\nEscaped quotes, backslashes, and a tab remain exact text:\n"
        '{"path":"config.txt","expected_hash":"0123abcd","edits":[{"target_line_start":2,"target_line_end":2,"new_lines":["path = \\"C:\\\\Program Files\\\\ME\\"\\n","value\\t42\\n"]}]}'
        "\n\nCommon line-syntax errors — new_lines=[\"missing ending\"], new_lines=[\"two\\nlines\\n\"], or new_lines=[\"first\\n\",\"second\"] each rejects the entire call because every item must be exactly one terminated physical line. Other atomic errors include overlapping ranges, duplicate insertion points, insertion inside a replacement, an invalid range, a stale expected_hash, or an item that targets lines created by another item. A successful result has no new hash. Before another Edit, always call File.Read or File.Search again and use its refreshed hash and numbered lines."
    ),
    "Append": '{"path":"notes.txt","expected_hash":"0123abcd","content":"next line\\n"}',
    "Replace": '{"path":"notes.txt","expected_hash":"0123abcd","content":"complete new content\\n"}',
    "Move": '{"path":"notes.txt","destination":"archive/notes.txt","expected_hash":"0123abcd"}',
    "Delete": '{"path":"archive/notes.txt","expected_hash":"0123abcd"}',
}


def send(frame: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(frame, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def result(request_id: int, output: Any) -> None:
    send({"id": request_id, "type": "result", "output": output})


def error(request_id: int, exc: ToolError) -> None:
    send(
        {
            "id": request_id,
            "type": "error",
            "error": {
                "code": exc.code,
                "message": exc.message,
                "retryable": exc.retryable,
            },
        }
    )


def validate_object(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ToolError("invalid_arguments", "input must be a JSON object")
    return value


def string_arg(data: dict[str, Any], name: str, default: str | None = None) -> str:
    value = data.get(name, default)
    if not isinstance(value, str) or (name != "content" and not value):
        raise ToolError("invalid_arguments", f"{name} must be a non-empty string")
    if "\x00" in value:
        raise ToolError("invalid_arguments", f"{name} contains NUL")
    return value


def physical_lines_arg(data: dict[str, Any], edit_index: int) -> tuple[str, ...]:
    value = data.get("new_lines")
    if not isinstance(value, list):
        raise ToolError(
            "invalid_line_syntax",
            f"edits[{edit_index}].new_lines must be an array of complete physical lines",
        )
    result: list[str] = []
    for line_index, line in enumerate(value):
        if not isinstance(line, str):
            raise ToolError(
                "invalid_line_syntax",
                f"edits[{edit_index}].new_lines[{line_index}] must be a string",
            )
        if "\x00" in line:
            raise ToolError(
                "invalid_line_syntax",
                f"edits[{edit_index}].new_lines[{line_index}] contains NUL",
            )
        if line.endswith("\r\n"):
            body = line[:-2]
        elif line.endswith("\r") or line.endswith("\n"):
            body = line[:-1]
        else:
            raise ToolError(
                "invalid_line_syntax",
                f"edits[{edit_index}].new_lines[{line_index}] must end in LF, CRLF, or CR",
            )
        if "\r" in body or "\n" in body:
            raise ToolError(
                "invalid_line_syntax",
                f"edits[{edit_index}].new_lines[{line_index}] contains more than one physical line",
            )
        result.append(line)
    return tuple(result)


def hex_data_arg(data: dict[str, Any], edit_index: int) -> bytes:
    value = data.get("data")
    if not isinstance(value, str):
        raise ToolError(
            "invalid_byte_syntax", f"edits[{edit_index}].data must be a string"
        )
    tokens = [token for token in value.split(" ") if token]
    if any(
        len(token) != 2
        or any(character not in "0123456789abcdefABCDEF" for character in token)
        for token in tokens
    ):
        raise ToolError(
            "invalid_byte_syntax",
            f"edits[{edit_index}].data must contain complete two-digit hexadecimal bytes",
        )
    return bytes(int(token, 16) for token in tokens)


def int_arg(
    data: dict[str, Any], name: str, default: int, minimum: int, maximum: int
) -> int:
    value = data.get(name, default)
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise ToolError(
            "invalid_arguments", f"{name} must be an integer in {minimum}..={maximum}"
        )
    return value


def bool_arg(data: dict[str, Any], name: str, default: bool) -> bool:
    value = data.get(name, default)
    if not isinstance(value, bool):
        raise ToolError("invalid_arguments", f"{name} must be a boolean")
    return value


def encoding_arg(
    data: dict[str, Any], name: str = "encoding", default: str = "auto", allow_auto: bool = True
) -> str:
    value = string_arg(data, name, default).lower()
    value = ENCODING_ALIASES.get(value, value)
    if value not in ENCODING_CODECS and not (allow_auto and value == "auto"):
        allowed = TEXT_ENCODINGS if allow_auto else TEXT_ENCODINGS[1:]
        raise ToolError(
            "invalid_encoding",
            f"{name} must be one of: {', '.join(allowed)}",
        )
    return value


def string_list(
    data: dict[str, Any],
    name: str,
    default: list[str] | None = None,
    required: bool = False,
    max_items: int = 256,
) -> list[str]:
    value = data.get(name, default if default is not None else [])
    if not isinstance(value, list) or (required and not value) or len(value) > max_items:
        raise ToolError("invalid_arguments", f"{name} must be a valid string array")
    if any(not isinstance(item, str) or not item or "\x00" in item for item in value):
        raise ToolError("invalid_arguments", f"{name} must contain non-empty strings")
    return value


def ensure_within(path: Path) -> Path:
    try:
        path.relative_to(ROOT)
    except ValueError as exc:
        raise ToolError(
            "outside_workspace", f"path is outside workspace: {path}"
        ) from exc
    return path


def raw_path(value: str) -> Path:
    candidate = Path(value)
    return candidate if candidate.is_absolute() else ROOT / candidate


def existing_path(value: str) -> Path:
    try:
        return ensure_within(raw_path(value).resolve(strict=True))
    except FileNotFoundError as exc:
        raise ToolError("not_found", f"path does not exist: {value}") from exc
    except OSError as exc:
        raise ToolError("path_error", f"cannot resolve {value}: {exc}") from exc


def lexical_path(value: str) -> Path:
    candidate = raw_path(value)
    if candidate == ROOT:
        raise ToolError("invalid_path", "workspace root cannot be modified")
    try:
        parent = ensure_within(candidate.parent.resolve(strict=True))
    except FileNotFoundError as exc:
        raise ToolError("parent_not_found", f"parent directory does not exist: {value}") from exc
    except OSError as exc:
        raise ToolError("path_error", f"cannot resolve parent of {value}: {exc}") from exc
    path = parent / candidate.name
    ensure_within(path)
    if path == ROOT:
        raise ToolError("invalid_path", "workspace root cannot be modified")
    if path == LOCK_PATH:
        raise ToolError("protected_path", "File toolbox coordination lock cannot be modified")
    return path


def recursive_lexical_path(value: str) -> Path:
    candidate = raw_path(value)
    if candidate == ROOT or candidate.name in {"", ".", ".."}:
        raise ToolError("invalid_path", "workspace root cannot be modified")
    try:
        parent = ensure_within(candidate.parent.resolve(strict=False))
    except OSError as exc:
        raise ToolError("path_error", f"cannot resolve parent of {value}: {exc}") from exc
    path = ensure_within(parent / candidate.name)
    if path == ROOT:
        raise ToolError("invalid_path", "workspace root cannot be modified")
    if path == LOCK_PATH:
        raise ToolError("protected_path", "File toolbox coordination lock cannot be modified")
    return path


def inspection_path(value: str) -> Path:
    candidate = raw_path(value)
    if candidate == ROOT:
        return ROOT
    try:
        parent = ensure_within(candidate.parent.resolve(strict=False))
    except OSError as exc:
        raise ToolError("path_error", f"cannot resolve parent of {value}: {exc}") from exc
    return ensure_within(parent / candidate.name)


def relative_path(path: Path) -> str:
    relative = path.relative_to(ROOT)
    return "." if not relative.parts else relative.as_posix()


def sha256_bytes(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()[:8]


def hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()[:8]


def require_regular_file(path: Path, logical: str, reject_symlink: bool = False) -> None:
    if path == LOCK_PATH:
        raise ToolError("protected_path", "File toolbox coordination lock cannot be modified")
    lexical = raw_path(logical)
    if reject_symlink and lexical.is_symlink():
        raise ToolError("unsupported_file_type", f"symbolic links are not mutable: {logical}")
    if not path.is_file():
        raise ToolError("unsupported_file_type", f"path is not a regular file: {logical}")


def validate_expected_hash(value: Any) -> str:
    if not isinstance(value, str) or HASH_PATTERN.fullmatch(value) is None:
        raise ToolError(
            "invalid_arguments", "expected_hash must be exactly 8 lowercase hexadecimal characters"
        )
    return value


def verify_hash(path: Path, expected: str) -> str:
    current = hash_file(path)
    if current != expected:
        raise ToolError(
            "conflict",
            f"file changed: expected_hash={expected}, current_hash={current}",
            True,
        )
    return current


def verify_content_hash(content: bytes, expected: str) -> str:
    current = sha256_bytes(content)
    if current != expected:
        raise ToolError(
            "conflict",
            f"file changed: expected_hash={expected}, current_hash={current}",
            True,
        )
    return current


def bom_for(raw: bytes) -> tuple[bytes, str] | None:
    for marker, encoding in BOMS:
        if raw.startswith(marker):
            return marker, encoding
    return None


def decode_strict(payload: bytes, encoding: str, logical: str) -> str:
    try:
        text = payload.decode(ENCODING_CODECS[encoding], errors="strict")
    except UnicodeDecodeError as exc:
        raise ToolError(
            "encoding_error", f"file is not valid {encoding}: {logical}"
        ) from exc
    if "\x00" in text:
        raise ToolError("binary_file", f"decoded text contains NUL characters: {logical}")
    return text


def null_ratio(raw: bytes, offset: int, width: int) -> float:
    values = raw[offset::width]
    return values.count(0) / len(values) if values else 0.0


def bomless_unicode_candidate(raw: bytes) -> str | None:
    if len(raw) >= 8 and len(raw) % 4 == 0:
        ratios = [null_ratio(raw, offset, 4) for offset in range(4)]
        if min(ratios[1:]) >= 0.60 and ratios[0] <= 0.20:
            return "utf-32-le"
        if min(ratios[:3]) >= 0.60 and ratios[3] <= 0.20:
            return "utf-32-be"
    if len(raw) >= 4 and len(raw) % 2 == 0:
        even = null_ratio(raw, 0, 2)
        odd = null_ratio(raw, 1, 2)
        if odd >= 0.60 and even <= 0.20:
            return "utf-16-le"
        if even >= 0.60 and odd <= 0.20:
            return "utf-16-be"
    return None


COMMON_CJK = set(
    "的一是在不了有和人这中大为上个国我以要他时来用们生到作地于出就分对成会可主发年动同工也能下过子说产种面而方后多定行学法所民得经十三之进着等部度家电力里如水化高自二理起小物现实加量都两体制机当使点从业本去把性好应开它合还因由其些然前外天政四日那社义事平形相全表间样与关各重新线内数正心反你明看原又么利比或但质气第向道命此变条只没结解问意建月公无系军很情者最立代想已通并提直题党程展五果料象员革位入常文总次品式活设及管特件长求老头基资边流路级少图山统接知较将组见计别她手角期根论运农指几九区强放决西被干做必战先回则任取据处理世车价美间"
)
COMMON_KOREAN = set(
    "가간갈감강개거건게겨경고과관광구국군그기길김나난날남내너년노는니"
    "다대도동되된두들등라러로리마만말명모무문미바박반받방버번보본부"
    "분불비사산상서선성세소속수시신실아안않알앞어언없에여연영오와요"
    "용우원위유은을음의이인일자장재저전정제조주중지진차처천체초최추"
    "출치카큰타통파표하한할해현형호화회후히녕세어입두번째줄"
)


def legacy_quality(text: str, encoding: str) -> float:
    if not text:
        return 1.0
    bad = 0
    non_ascii = 0
    cjk = 0
    common_cjk = 0
    japanese = 0
    korean = 0
    common_korean = 0
    latin = 0
    for character in text:
        point = ord(character)
        category = unicodedata.category(character)
        if character in "\t\r\n":
            continue
        if category in {"Cc", "Cs", "Co", "Cn"}:
            bad += 1
            continue
        if point > 0x7F:
            non_ascii += 1
        if 0x3400 <= point <= 0x9FFF or 0xF900 <= point <= 0xFAFF:
            cjk += 1
            common_cjk += character in COMMON_CJK
        elif 0x3040 <= point <= 0x30FF:
            japanese += 1
        elif 0xAC00 <= point <= 0xD7AF:
            korean += 1
            common_korean += character in COMMON_KOREAN
        elif "LATIN" in unicodedata.name(character, ""):
            latin += 1
    if bad:
        return -1.0 - bad / len(text)
    if non_ascii == 0:
        return 1.0
    visible = max(1, non_ascii)
    base = 0.45
    if encoding in {"gb18030", "big5"}:
        base += 0.20 * cjk / visible
        base += 0.55 * common_cjk / max(1, cjk)
    elif encoding == "shift_jis":
        base += 0.80 * japanese / visible
        base += 0.15 * cjk / visible
    elif encoding == "euc_kr":
        base += 0.15 * korean / visible
        base += 0.55 * common_korean / max(1, korean)
    elif encoding == "windows-1252":
        base += 0.50 * latin / visible
        non_ascii_density = non_ascii / max(1, len(text))
        base -= 0.50 * max(0.0, non_ascii_density - 0.35)
        base -= 0.20 * (cjk + japanese + korean) / visible
    return base


def decode_text_bytes(raw: bytes, logical: str, requested: str = "auto") -> TextDocument:
    detected_bom = bom_for(raw)
    if requested != "auto":
        marker = b""
        if detected_bom is not None:
            marker, bom_encoding = detected_bom
            if bom_encoding != requested:
                raise ToolError(
                    "encoding_mismatch",
                    f"file BOM declares {bom_encoding}, not requested {requested}: {logical}",
                )
        text = decode_strict(raw[len(marker) :], requested, logical)
        if text.encode(ENCODING_CODECS[requested], errors="strict") != raw[len(marker) :]:
            raise ToolError(
                "encoding_error",
                f"file does not round-trip losslessly as requested {requested}: {logical}",
            )
        return TextDocument(raw, text, requested, 1.0, marker)

    if detected_bom is not None:
        marker, encoding = detected_bom
        text = decode_strict(raw[len(marker) :], encoding, logical)
        return TextDocument(raw, text, encoding, 1.0, marker)

    unicode_candidate = bomless_unicode_candidate(raw)
    if unicode_candidate is not None:
        text = decode_strict(raw, unicode_candidate, logical)
        return TextDocument(raw, text, unicode_candidate, 0.95, b"")

    try:
        text = raw.decode("utf-8", errors="strict")
    except UnicodeDecodeError:
        text = ""
    else:
        if "\x00" in text:
            raise ToolError("binary_file", f"decoded text contains NUL characters: {logical}")
        return TextDocument(raw, text, "utf-8", 1.0, b"")

    if b"\x00" in raw:
        raise ToolError("binary_file", f"file contains NUL bytes: {logical}")

    candidates: list[tuple[float, str, str]] = []
    for encoding in ("gb18030", "big5", "shift_jis", "euc_kr", "windows-1252"):
        codec = ENCODING_CODECS[encoding]
        try:
            decoded = raw.decode(codec, errors="strict")
            if decoded.encode(codec, errors="strict") != raw:
                continue
        except (UnicodeDecodeError, UnicodeEncodeError):
            continue
        candidates.append((legacy_quality(decoded, encoding), encoding, decoded))
    candidates.sort(reverse=True)
    if not candidates:
        raise ToolError("binary_file", f"file is not recognized as text: {logical}")
    best_score, best_encoding, best_text = candidates[0]
    if best_score < 0.70:
        names = ", ".join(candidate[1] for candidate in candidates[:3])
        raise ToolError(
            "encoding_uncertain",
            f"text encoding has low confidence ({names}): {logical}; specify encoding explicitly or use ReadBytes",
        )
    runner_up = candidates[1][0] if len(candidates) > 1 else 0.0
    gap = best_score - runner_up
    if len(candidates) > 1 and gap < 0.08:
        names = ", ".join(candidate[1] for candidate in candidates[:3])
        raise ToolError(
            "encoding_uncertain",
            f"text encoding is ambiguous ({names}): {logical}; specify encoding explicitly or use ReadBytes",
        )
    confidence = min(0.95, 0.78 + max(0.0, gap))
    return TextDocument(raw, best_text, best_encoding, round(confidence, 3), b"")


def encode_text(text: str, encoding: str, bom: bytes, logical: str) -> bytes:
    try:
        payload = text.encode(ENCODING_CODECS[encoding], errors="strict")
    except UnicodeEncodeError as exc:
        character = text[exc.start : exc.end]
        raise ToolError(
            "encoding_error",
            f"text {character!r} cannot be represented as {encoding}: {logical}",
        ) from exc
    return bom + payload


def create_bom(encoding: str, enabled: bool) -> bytes:
    if not enabled:
        return b""
    for marker, candidate in BOMS:
        if candidate == encoding:
            return marker
    raise ToolError("invalid_encoding", f"BOM is not supported for {encoding}")


def read_text_file(path: Path, logical: str, encoding: str = "auto") -> TextDocument:
    content = path.read_bytes()
    return decode_text_bytes(content, logical, encoding)


@contextlib.contextmanager
def mutation_lock() -> Iterator[None]:
    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOCK_PATH.open("a+b") as lock:
        lock.seek(0, os.SEEK_END)
        if lock.tell() == 0:
            lock.write(b"0")
            lock.flush()
        lock.seek(0)
        if os.name == "nt":
            import msvcrt

            msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
            try:
                yield
            finally:
                lock.seek(0)
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def atomic_replace(path: Path, content: bytes, mode: int) -> None:
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.me-", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, stat.S_IMODE(mode))
        os.replace(temporary, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def atomic_create(path: Path, content: bytes) -> None:
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.me-", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        current_umask = os.umask(0)
        os.umask(current_umask)
        os.chmod(temporary, 0o666 & ~current_umask)
        try:
            os.link(temporary, path)
        except FileExistsError as exc:
            raise ToolError("already_exists", f"destination already exists: {relative_path(path)}") from exc
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def path_type(mode: int) -> str:
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISLNK(mode):
        return "symlink"
    return "other"


def matches_any(path: str, patterns: list[str]) -> bool:
    for pattern in patterns:
        candidates = {pattern}
        pending = [pattern]
        while pending:
            candidate = pending.pop()
            marker = candidate.find("**/")
            if marker >= 0:
                without_empty_directory = candidate[:marker] + candidate[marker + 3 :]
                if without_empty_directory not in candidates:
                    candidates.add(without_empty_directory)
                    pending.append(without_empty_directory)
        if any(fnmatch.fnmatchcase(path, candidate) for candidate in candidates):
            return True
    return False


def walk_files(start: Path, include_hidden: bool) -> Iterator[Path]:
    if start.is_file():
        yield start
        return
    if not start.is_dir():
        raise ToolError("unsupported_file_type", f"search root is not a file or directory: {relative_path(start)}")
    for directory, names, files in os.walk(start, followlinks=False):
        names[:] = sorted(
            name
            for name in names
            if (include_hidden or not name.startswith("."))
            and not (Path(directory) / name).is_symlink()
        )
        for name in sorted(files):
            if include_hidden or not name.startswith("."):
                yield Path(directory) / name


def walk_entries(start: Path, include_hidden: bool) -> Iterator[Path]:
    if not start.is_dir():
        yield start
        return
    for directory, names, files in os.walk(start, followlinks=False):
        visible_directories = sorted(
            name for name in names if include_hidden or not name.startswith(".")
        )
        names[:] = [
            name
            for name in visible_directories
            if not (Path(directory) / name).is_symlink()
        ]
        for name in visible_directories:
            yield Path(directory) / name
        for name in sorted(files):
            if include_hidden or not name.startswith("."):
                yield Path(directory) / name


def split_text_file_lines(text: str) -> list[str]:
    lines: list[str] = []
    start = 0
    index = 0
    while index < len(text):
        if text[index] == "\r":
            index += 2 if index + 1 < len(text) and text[index + 1] == "\n" else 1
            lines.append(text[start:index])
            start = index
        elif text[index] == "\n":
            index += 1
            lines.append(text[start:index])
            start = index
        else:
            index += 1
    if start < len(text):
        lines.append(text[start:])
    return lines


def line_without_ending(line: str) -> str:
    if line.endswith("\r\n"):
        return line[:-2]
    if line.endswith("\r") or line.endswith("\n"):
        return line[:-1]
    return line


def execute_read(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    start_line = int_arg(data, "start_line", 1, 1, 2**31 - 1)
    max_lines = int_arg(data, "max_lines", 500, 1, 10000)
    encoding = encoding_arg(data)
    path = existing_path(logical)
    require_regular_file(path, logical)
    document = read_text_file(path, logical, encoding)
    lines = split_text_file_lines(document.text)
    start_index = min(start_line - 1, len(lines))
    selected = lines[start_index : start_index + max_lines]
    end_line = start_index + len(selected)
    eof = end_line >= len(lines)
    line_number_width = max(1, len(str(len(lines))))
    return {
        "path": relative_path(path),
        "lines": {
            str(start_index + offset + 1).zfill(line_number_width): line
            for offset, line in enumerate(selected)
        },
        "start_line": start_line,
        "end_line": end_line,
        "total_lines": len(lines),
        "eof": eof,
        "truncated": not eof,
        "hash": sha256_bytes(document.raw),
        "size": len(document.raw),
        "encoding": document.encoding,
        "encoding_confidence": document.confidence,
        "bom": bool(document.bom),
    }


def execute_read_bytes(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    offset = int_arg(data, "offset", 0, 0, 2**63 - 1)
    length = int_arg(data, "length", 65536, 1, 1048576)
    path = existing_path(logical)
    require_regular_file(path, logical)
    with path.open("rb") as source:
        size = os.fstat(source.fileno()).st_size
        source.seek(min(offset, size))
        chunk = source.read(length)
        source.seek(0)
        digest = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    actual_offset = min(offset, size)
    return {
        "path": relative_path(path),
        "data": " ".join(f"{byte:02x}" for byte in chunk),
        "offset": actual_offset,
        "length": len(chunk),
        "size": size,
        "eof": actual_offset + len(chunk) >= size,
        "hash": digest.hexdigest()[:8],
    }


def execute_list(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path", ".")
    depth = int_arg(data, "depth", 1, 1, 32)
    include_hidden = bool_arg(data, "include_hidden", False)
    max_entries = int_arg(data, "max_entries", 1000, 1, 10000)
    start = existing_path(logical)
    if not start.is_dir():
        raise ToolError("not_directory", f"path is not a directory: {logical}")
    entries: list[dict[str, Any]] = []
    pending: list[tuple[Path, int]] = [(start, 1)]
    truncated = False
    while pending:
        directory, level = pending.pop(0)
        try:
            children = sorted(directory.iterdir(), key=lambda item: item.name)
        except OSError as exc:
            raise ToolError("read_error", f"cannot list {relative_path(directory)}: {exc}") from exc
        next_directories: list[Path] = []
        for child in children:
            if not include_hidden and child.name.startswith("."):
                continue
            info = child.lstat()
            kind = path_type(info.st_mode)
            entry = {
                "path": relative_path(child),
                "type": kind,
                "size": info.st_size,
                "modified_ms": info.st_mtime_ns // 1_000_000,
            }
            entries.append(entry)
            if len(entries) >= max_entries:
                truncated = True
                pending.clear()
                break
            if kind == "directory" and level < depth:
                next_directories.append(child)
        pending.extend((child, level + 1) for child in next_directories)
    return {"path": relative_path(start), "entries": entries, "truncated": truncated}


def execute_find(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path", ".")
    patterns = string_list(data, "patterns", required=True, max_items=64)
    exclude = string_list(data, "exclude")
    include_hidden = bool_arg(data, "include_hidden", False)
    max_results = int_arg(data, "max_results", 1000, 1, 10000)
    start = existing_path(logical)
    results: list[str] = []
    for path in walk_entries(start, include_hidden):
        relative = relative_path(path)
        if matches_any(relative, exclude):
            continue
        if matches_any(relative, patterns) or matches_any(path.name, patterns):
            results.append(relative)
            if len(results) >= max_results:
                return {"path": relative_path(start), "results": results, "truncated": True}
    return {"path": relative_path(start), "results": results, "truncated": False}


def execute_search(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path", ".")
    query = string_arg(data, "query")
    use_regex = bool_arg(data, "regex", False)
    case_sensitive = bool_arg(data, "case_sensitive", True)
    globs = string_list(data, "globs")
    context_before = int_arg(data, "context_before", 0, 0, 20)
    context_after = int_arg(data, "context_after", 0, 0, 20)
    max_matches = int_arg(data, "max_matches", 500, 1, 5000)
    flags = 0 if case_sensitive else re.IGNORECASE
    try:
        pattern = re.compile(query if use_regex else re.escape(query), flags)
    except re.error as exc:
        raise ToolError("invalid_regex", str(exc)) from exc
    start = existing_path(logical)
    matches: list[dict[str, Any]] = []
    skipped_binary = 0
    for path in walk_files(start, False):
        relative = relative_path(path)
        if globs and not (matches_any(relative, globs) or matches_any(path.name, globs)):
            continue
        try:
            raw = path.read_bytes()
            file_hash = sha256_bytes(raw)
            text = decode_text_bytes(raw, relative).text
        except (ToolError, OSError):
            skipped_binary += 1
            continue
        lines = split_text_file_lines(text)
        line_number_width = max(1, len(str(len(lines))))
        for line_index, exact_line in enumerate(lines):
            searchable_line = line_without_ending(exact_line)
            for found in pattern.finditer(searchable_line):
                before_start = max(0, line_index - context_before)
                after_end = min(len(lines), line_index + 1 + context_after)
                matches.append(
                    {
                        "path": relative,
                        "hash": file_hash,
                        "column": found.start() + 1,
                        "match_length": found.end() - found.start(),
                        "before": {
                            str(index + 1).zfill(line_number_width): lines[index]
                            for index in range(before_start, line_index)
                        },
                        "match_text": {
                            str(line_index + 1).zfill(line_number_width): exact_line
                        },
                        "after": {
                            str(index + 1).zfill(line_number_width): lines[index]
                            for index in range(line_index + 1, after_end)
                        },
                    }
                )
                if len(matches) >= max_matches:
                    return {
                        "path": relative_path(start),
                        "matches": matches,
                        "skipped_binary": skipped_binary,
                        "truncated": True,
                    }
    return {
        "path": relative_path(start),
        "matches": matches,
        "skipped_binary": skipped_binary,
        "truncated": False,
    }


def execute_stat(data: dict[str, Any]) -> dict[str, Any]:
    logical_paths = string_list(data, "paths", required=True)
    entries: list[dict[str, Any]] = []
    for logical in logical_paths:
        path = inspection_path(logical)
        if not path.exists() and not path.is_symlink():
            entries.append({"path": relative_path(path), "exists": False})
            continue
        info = path.lstat()
        kind = path_type(info.st_mode)
        entry: dict[str, Any] = {
            "path": relative_path(path),
            "exists": True,
            "type": kind,
            "size": info.st_size,
            "modified_ms": info.st_mtime_ns // 1_000_000,
            "readonly": not os.access(path, os.W_OK),
        }
        if kind == "file":
            with path.open("rb") as source:
                digest = hashlib.sha256()
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(block)
                opened = os.fstat(source.fileno())
            entry["hash"] = digest.hexdigest()[:8]
            entry["size"] = opened.st_size
            entry["modified_ms"] = opened.st_mtime_ns // 1_000_000
        entries.append(entry)
    return {"entries": entries}


def mutation_result(
    path: Path,
    operation: str,
    previous_hash: str | None,
    content: bytes,
    encoding: str,
    confidence: float,
    bom: bytes,
) -> dict[str, Any]:
    return {
        "path": relative_path(path),
        "operation": operation,
        "previous_hash": previous_hash,
        "hash": sha256_bytes(content),
        "size": len(content),
        "encoding": encoding,
        "encoding_confidence": confidence,
        "bom": bool(bom),
    }


def execute_make_directory(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    parents = bool_arg(data, "parents", False)
    path = recursive_lexical_path(logical) if parents else lexical_path(logical)
    with mutation_lock():
        try:
            path.mkdir(parents=parents)
        except FileExistsError as exc:
            raise ToolError("already_exists", f"destination already exists: {logical}") from exc
        except OSError as exc:
            raise ToolError(
                "create_directory_error", f"cannot create directory {logical}: {exc}"
            ) from exc
    return {
        "path": relative_path(path),
        "operation": "directory_created",
        "exists": True,
    }


def execute_create(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    text = string_arg(data, "content", "")
    encoding = encoding_arg(data, default="utf-8", allow_auto=False)
    bom = create_bom(encoding, bool_arg(data, "bom", False))
    content = encode_text(text, encoding, bom, logical)
    path = lexical_path(logical)
    with mutation_lock():
        if path.exists() or path.is_symlink():
            raise ToolError("already_exists", f"destination already exists: {logical}")
        atomic_create(path, content)
    return mutation_result(path, "created", None, content, encoding, 1.0, bom)


def split_patch_lines(patch: str) -> list[str]:
    lines = patch.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line[:-1] if line.endswith("\r") else line for line in lines]


def patch_header_path(line: str, marker: str) -> str:
    if not line.startswith(marker):
        raise ToolError("invalid_patch", f"patch must begin with {marker.strip()} file header")
    value = line[len(marker) :].split("\t", 1)[0]
    if not value or value == "/dev/null":
        raise ToolError(
            "invalid_patch",
            "ApplyPatch requires an existing file; header paths cannot be empty or /dev/null",
        )
    return value.replace("\\", "/")


def header_matches_path(header: str, logical: str, prefix: str) -> bool:
    normalized = logical.replace("\\", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    return header == normalized or header == f"{prefix}/{normalized}"


def parse_unified_diff(patch: str, logical: str) -> list[PatchHunk]:
    lines = split_patch_lines(patch)
    if len(lines) < 3:
        raise ToolError(
            "invalid_patch",
            "patch requires --- and +++ file headers followed by at least one @@ hunk",
        )
    old_path = patch_header_path(lines[0], "--- ")
    new_path = patch_header_path(lines[1], "+++ ")
    if not header_matches_path(old_path, logical, "a"):
        raise ToolError(
            "invalid_patch",
            f"old header path {old_path!r} does not match path {logical!r}",
        )
    if not header_matches_path(new_path, logical, "b"):
        raise ToolError(
            "invalid_patch",
            f"new header path {new_path!r} does not match path {logical!r}",
        )

    hunks: list[PatchHunk] = []
    index = 2
    changed = False
    while index < len(lines):
        header = lines[index]
        match = HUNK_PATTERN.fullmatch(header)
        if match is None:
            if header.startswith(("--- ", "+++ ", "diff --git ", "index ")):
                detail = "multiple files and git metadata are not supported"
            else:
                detail = f"expected @@ hunk header at patch line {index + 1}"
            raise ToolError("invalid_patch", detail)
        old_start = int(match.group(1))
        old_count = int(match.group(2)) if match.group(2) is not None else 1
        new_start = int(match.group(3))
        new_count = int(match.group(4)) if match.group(4) is not None else 1
        if (old_count > 0 and old_start == 0) or (new_count > 0 and new_start == 0):
            raise ToolError(
                "invalid_patch",
                f"non-empty ranges in hunk {len(hunks) + 1} must start at line 1 or later",
            )
        index += 1
        body: list[PatchLine] = []
        while index < len(lines) and not lines[index].startswith("@@ "):
            line = lines[index]
            if line == "\\ No newline at end of file":
                if not body or body[-1].no_newline:
                    raise ToolError(
                        "invalid_patch",
                        f"misplaced no-newline marker at patch line {index + 1}",
                    )
                body[-1].no_newline = True
                index += 1
                continue
            if not line or line[0] not in " +-":
                raise ToolError(
                    "invalid_patch",
                    f"hunk line {index + 1} must begin with one space, +, or -",
                )
            entry = PatchLine(line[0], line[1:])
            body.append(entry)
            changed = changed or entry.kind in "+-"
            index += 1
        actual_old = sum(entry.kind in " -" for entry in body)
        actual_new = sum(entry.kind in " +" for entry in body)
        if actual_old != old_count or actual_new != new_count:
            raise ToolError(
                "invalid_patch",
                f"hunk {len(hunks) + 1} declares old/new counts {old_count}/{new_count} "
                f"but its body contains {actual_old}/{actual_new}",
            )
        hunks.append(
            PatchHunk(old_start, old_count, new_start, new_count, tuple(body))
        )
    if not hunks:
        raise ToolError("invalid_patch", "patch must contain at least one @@ hunk")
    if not changed:
        raise ToolError("invalid_patch", "patch contains no added or removed lines")
    return hunks


def split_text_lines(text: str) -> list[TextLine]:
    lines: list[TextLine] = []
    start = 0
    for match in re.finditer(r"\r\n|\n|\r", text):
        lines.append(TextLine(text[start : match.start()], match.group(0)))
        start = match.end()
    if start < len(text):
        lines.append(TextLine(text[start:], ""))
    return lines


def prevailing_line_ending(lines: list[TextLine]) -> str:
    counts: dict[str, int] = {}
    for line in lines:
        if line.ending:
            counts[line.ending] = counts.get(line.ending, 0) + 1
    return max(counts, key=counts.get) if counts else "\n"


def apply_unified_diff(text: str, hunks: list[PatchHunk]) -> tuple[str, int, int]:
    original = split_text_lines(text)
    result: list[TextLine] = []
    source_cursor = 0
    preferred_ending = prevailing_line_ending(original)
    added = 0
    removed = 0

    for hunk_index, hunk in enumerate(hunks, 1):
        source_index = hunk.old_start if hunk.old_count == 0 else hunk.old_start - 1
        if source_index < source_cursor:
            raise ToolError(
                "invalid_patch", f"hunk {hunk_index} overlaps or precedes an earlier hunk"
            )
        if source_index > len(original):
            raise ToolError(
                "patch_conflict",
                f"hunk {hunk_index} starts beyond the end of the file",
                True,
            )
        result.extend(original[source_cursor:source_index])
        expected_new_index = (
            hunk.new_start if hunk.new_count == 0 else hunk.new_start - 1
        )
        if len(result) != expected_new_index:
            raise ToolError(
                "invalid_patch",
                f"hunk {hunk_index} new-file start is inconsistent with earlier hunks",
            )
        cursor = source_index
        for entry in hunk.lines:
            if entry.kind in " -":
                if cursor >= len(original):
                    raise ToolError(
                        "patch_conflict",
                        f"hunk {hunk_index} expects content beyond the end of the file",
                        True,
                    )
                current = original[cursor]
                if current.text != entry.text:
                    raise ToolError(
                        "patch_conflict",
                        f"hunk {hunk_index} context mismatch at original line {cursor + 1}",
                        True,
                    )
                actual_no_newline = current.ending == ""
                if actual_no_newline != entry.no_newline:
                    raise ToolError(
                        "patch_conflict",
                        f"hunk {hunk_index} newline marker mismatch at original line {cursor + 1}",
                        True,
                    )
                cursor += 1
                if entry.kind == " ":
                    result.append(current)
                else:
                    removed += 1
            else:
                ending = "" if entry.no_newline else preferred_ending
                result.append(TextLine(entry.text, ending))
                added += 1
        source_cursor = cursor

    result.extend(original[source_cursor:])
    for line_index, line in enumerate(result[:-1], 1):
        if line.ending == "":
            raise ToolError(
                "invalid_patch",
                f"no-newline marker creates a non-final line at new line {line_index}",
            )
    return "".join(line.text + line.ending for line in result), added, removed


def execute_apply_patch(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    encoding = encoding_arg(data)
    patch = string_arg(data, "patch")
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        document = read_text_file(path, logical, encoding)
        raw = document.raw
        previous = verify_content_hash(document.raw, expected)
        hunks = parse_unified_diff(patch, relative_path(path))
        text, added, removed = apply_unified_diff(document.text, hunks)
        updated = encode_text(text, document.encoding, document.bom, logical)
        verify_hash(path, expected)
        atomic_replace(path, updated, path.stat().st_mode)
    output = mutation_result(
        path,
        "patched",
        previous,
        updated,
        document.encoding,
        document.confidence,
        document.bom,
    )
    output["hunks_applied"] = len(hunks)
    output["lines_added"] = added
    output["lines_removed"] = removed
    output["previous_size"] = len(raw)
    return output


def execute_edit(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    encoding = encoding_arg(data)
    requested_edits = data.get("edits")
    if (
        not isinstance(requested_edits, list)
        or not requested_edits
        or len(requested_edits) > MAX_EDIT_OPERATIONS
    ):
        raise ToolError(
            "invalid_arguments",
            f"edits must contain 1..={MAX_EDIT_OPERATIONS} operation objects",
        )
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        document = read_text_file(path, logical, encoding)
        previous = verify_content_hash(document.raw, expected)
        lines = split_text_file_lines(document.text)
        total_lines = len(lines)
        line_offsets = [0]
        for line in lines:
            line_offsets.append(line_offsets[-1] + len(line))
        resolved: list[ResolvedEdit] = []
        required_fields = {"target_line_start", "target_line_end", "new_lines"}
        for index, value in enumerate(requested_edits):
            item = validate_object(value)
            unexpected = sorted(set(item) - required_fields)
            missing = sorted(required_fields - set(item))
            if unexpected or missing:
                details = []
                if missing:
                    details.append(f"missing fields: {', '.join(missing)}")
                if unexpected:
                    details.append(f"unexpected fields: {', '.join(unexpected)}")
                raise ToolError(
                    "invalid_arguments",
                    f"edits[{index}] " + "; ".join(details),
                )
            target_start = int_arg(
                item, "target_line_start", 0, 1, 2**31 - 1
            )
            target_end = int_arg(item, "target_line_end", -1, 0, 2**31 - 1)
            new_lines = physical_lines_arg(item, index)
            inserting = target_start == target_end + 1
            replacing = 1 <= target_start <= target_end <= total_lines
            insertion_in_bounds = inserting and target_start <= total_lines + 1
            if not replacing and not insertion_in_bounds:
                raise ToolError(
                    "invalid_range",
                    f"edits[{index}] must satisfy 1 <= start <= end <= total_lines for replacement, "
                    "or start = end + 1 with start <= total_lines + 1 for insertion; "
                    f"received start={target_start}, end={target_end}, total_lines={total_lines}",
                )
            if inserting and not new_lines:
                raise ToolError(
                    "invalid_line_syntax",
                    f"edits[{index}].new_lines cannot be empty for an insertion",
                )
            if (
                inserting
                and target_start == total_lines + 1
                and total_lines > 0
                and not (lines[-1].endswith("\r") or lines[-1].endswith("\n"))
            ):
                raise ToolError(
                    "invalid_line_syntax",
                    f"edits[{index}] cannot insert after an unterminated final line; replace that final line instead",
                )
            source_start = line_offsets[target_start - 1]
            source_end = line_offsets[target_end]
            replacement_text = "".join(new_lines)
            replacement = encode_text(
                replacement_text, document.encoding, b"", logical
            )
            resolved.append(
                ResolvedEdit(
                    index=index,
                    target_line_start=target_start,
                    target_line_end=target_end,
                    source_start=source_start,
                    source_end=source_end,
                    new_lines=new_lines,
                    replacement_text=replacement_text,
                    replacement_bytes=len(replacement),
                    kind=(
                        "insert"
                        if inserting
                        else "delete"
                        if not new_lines
                        else "replace"
                    ),
                )
            )
        for left_index, left in enumerate(resolved):
            for right in resolved[left_index + 1 :]:
                left_inserting = left.source_start == left.source_end
                right_inserting = right.source_start == right.source_end
                conflict = False
                if left_inserting and right_inserting:
                    conflict = left.source_start == right.source_start
                elif left_inserting:
                    conflict = right.source_start < left.source_start < right.source_end
                elif right_inserting:
                    conflict = left.source_start < right.source_start < left.source_end
                else:
                    conflict = max(left.source_start, right.source_start) < min(
                        left.source_end, right.source_end
                    )
                if conflict:
                    raise ToolError(
                        "overlapping_edits",
                        f"edits[{left.index}] and edits[{right.index}] overlap or use the same original insertion point; all edit coordinates must be independent",
                    )
        ordered = sorted(
            resolved,
            key=lambda item: (
                item.source_start,
                0 if item.source_start == item.source_end else 1,
                item.source_end,
                item.index,
            ),
        )
        pieces: list[str] = []
        cursor = 0
        for item in ordered:
            pieces.append(document.text[cursor : item.source_start])
            pieces.append(item.replacement_text)
            cursor = item.source_end
        pieces.append(document.text[cursor:])
        updated_text = "".join(pieces)
        updated = encode_text(
            updated_text, document.encoding, document.bom, logical
        )
        mode = path.stat().st_mode
        verify_hash(path, expected)
        atomic_replace(path, updated, mode)
    output = mutation_result(
        path,
        "edited",
        previous,
        updated,
        document.encoding,
        document.confidence,
        document.bom,
    )
    output.pop("hash")
    output.update(
        {
            "edit_results": [
                {
                    "index": item.index,
                    "state": "succeeded",
                    "kind": item.kind,
                    "target_line_start": item.target_line_start,
                    "target_line_end": item.target_line_end,
                    "selected_lines": (
                        0
                        if item.source_start == item.source_end
                        else item.target_line_end - item.target_line_start + 1
                    ),
                    "new_line_count": len(item.new_lines),
                    "replacement_bytes": item.replacement_bytes,
                }
                for item in sorted(resolved, key=lambda item: item.index)
            ],
            "previous_total_lines": total_lines,
            "total_lines": len(split_text_file_lines(updated_text)),
            "previous_size": len(document.raw),
            "tip": EDIT_TIP,
        }
    )
    return output


def execute_edit_bytes(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    requested_edits = data.get("edits")
    if (
        not isinstance(requested_edits, list)
        or not requested_edits
        or len(requested_edits) > MAX_EDIT_OPERATIONS
    ):
        raise ToolError(
            "invalid_arguments",
            f"edits must contain 1..={MAX_EDIT_OPERATIONS} operation objects",
        )
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        with path.open("rb") as source:
            raw = source.read()
        previous = verify_content_hash(raw, expected)
        original_size = len(raw)
        resolved: list[ResolvedByteEdit] = []
        required_fields = {"target_offset", "target_length", "data"}
        for index, value in enumerate(requested_edits):
            item = validate_object(value)
            unexpected = sorted(set(item) - required_fields)
            missing = sorted(required_fields - set(item))
            if unexpected or missing:
                details = []
                if missing:
                    details.append(f"missing fields: {', '.join(missing)}")
                if unexpected:
                    details.append(f"unexpected fields: {', '.join(unexpected)}")
                raise ToolError(
                    "invalid_arguments", f"edits[{index}] " + "; ".join(details)
                )
            target_offset = int_arg(item, "target_offset", -1, 0, 2**63 - 1)
            target_length = int_arg(item, "target_length", -1, 0, 2**63 - 1)
            replacement = hex_data_arg(item, index)
            if target_offset > original_size or target_length > original_size - target_offset:
                raise ToolError(
                    "invalid_range",
                    f"edits[{index}] range [{target_offset}, {target_offset + target_length}) "
                    f"must fit within the original {original_size}-byte file",
                )
            if target_length == 0 and not replacement:
                raise ToolError(
                    "invalid_byte_syntax",
                    f"edits[{index}].data cannot be empty for an insertion",
                )
            resolved.append(
                ResolvedByteEdit(
                    index=index,
                    target_offset=target_offset,
                    target_length=target_length,
                    source_start=target_offset,
                    source_end=target_offset + target_length,
                    data=replacement,
                    kind=(
                        "insert"
                        if target_length == 0
                        else "delete"
                        if not replacement
                        else "replace"
                    ),
                )
            )
        for left_index, left in enumerate(resolved):
            for right in resolved[left_index + 1 :]:
                left_inserting = left.source_start == left.source_end
                right_inserting = right.source_start == right.source_end
                conflict = False
                if left_inserting and right_inserting:
                    conflict = left.source_start == right.source_start
                elif left_inserting:
                    conflict = right.source_start < left.source_start < right.source_end
                elif right_inserting:
                    conflict = left.source_start < right.source_start < left.source_end
                else:
                    conflict = max(left.source_start, right.source_start) < min(
                        left.source_end, right.source_end
                    )
                if conflict:
                    raise ToolError(
                        "overlapping_edits",
                        f"edits[{left.index}] and edits[{right.index}] overlap or use the same original insertion point; all byte edit coordinates must be independent",
                    )
        ordered = sorted(
            resolved,
            key=lambda item: (
                item.source_start,
                0 if item.source_start == item.source_end else 1,
                item.source_end,
                item.index,
            ),
        )
        pieces: list[bytes] = []
        cursor = 0
        for item in ordered:
            pieces.append(raw[cursor : item.source_start])
            pieces.append(item.data)
            cursor = item.source_end
        pieces.append(raw[cursor:])
        updated = b"".join(pieces)
        mode = path.stat().st_mode
        verify_hash(path, expected)
        atomic_replace(path, updated, mode)
    return {
        "path": relative_path(path),
        "operation": "bytes_edited",
        "previous_hash": previous,
        "edit_results": [
            {
                "index": item.index,
                "state": "succeeded",
                "kind": item.kind,
                "target_offset": item.target_offset,
                "target_length": item.target_length,
                "selected_bytes": item.target_length,
                "replacement_bytes": len(item.data),
            }
            for item in sorted(resolved, key=lambda item: item.index)
        ],
        "previous_size": original_size,
        "size": len(updated),
        "tip": EDIT_BYTES_TIP,
    }


def execute_append(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    encoding = encoding_arg(data)
    appended_text = string_arg(data, "content", "")
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        document = read_text_file(path, logical, encoding)
        previous = verify_content_hash(document.raw, expected)
        appended = encode_text(appended_text, document.encoding, b"", logical)
        updated = document.raw + appended
        verify_hash(path, expected)
        atomic_replace(path, updated, path.stat().st_mode)
    output = mutation_result(
        path,
        "appended",
        previous,
        updated,
        document.encoding,
        document.confidence,
        document.bom,
    )
    output["appended_bytes"] = len(appended)
    return output


def execute_replace(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    encoding = encoding_arg(data)
    text = string_arg(data, "content", "")
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        document = read_text_file(path, logical, encoding)
        previous = verify_content_hash(document.raw, expected)
        updated = encode_text(text, document.encoding, document.bom, logical)
        mode = path.stat().st_mode
        verify_hash(path, expected)
        atomic_replace(path, updated, mode)
    return mutation_result(
        path,
        "replaced",
        previous,
        updated,
        document.encoding,
        document.confidence,
        document.bom,
    )


def execute_move(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    destination = string_arg(data, "destination")
    expected = validate_expected_hash(data.get("expected_hash"))
    with mutation_lock():
        source = existing_path(logical)
        require_regular_file(source, logical, True)
        target = lexical_path(destination)
        if target.exists() or target.is_symlink():
            raise ToolError("already_exists", f"destination already exists: {destination}")
        previous = verify_hash(source, expected)
        size = source.stat().st_size
        try:
            source.rename(target)
        except OSError as exc:
            raise ToolError("move_error", f"cannot move {logical} to {destination}: {exc}") from exc
    return {
        "path": relative_path(source),
        "destination": relative_path(target),
        "operation": "moved",
        "previous_hash": previous,
        "hash": previous,
        "size": size,
    }


def execute_delete(data: dict[str, Any]) -> dict[str, Any]:
    logical = string_arg(data, "path")
    expected = validate_expected_hash(data.get("expected_hash"))
    with mutation_lock():
        path = existing_path(logical)
        require_regular_file(path, logical, True)
        deleted_hash = verify_hash(path, expected)
        try:
            path.unlink()
        except OSError as exc:
            raise ToolError("delete_error", f"cannot delete {logical}: {exc}") from exc
    return {
        "path": relative_path(path),
        "operation": "deleted",
        "deleted_hash": deleted_hash,
        "exists": False,
    }


EXECUTORS = {
    "Read": execute_read,
    "ReadBytes": execute_read_bytes,
    "EditBytes": execute_edit_bytes,
    "List": execute_list,
    "Find": execute_find,
    "Search": execute_search,
    "Stat": execute_stat,
    "MakeDirectory": execute_make_directory,
    "Create": execute_create,
    "Edit": execute_edit,
    "Append": execute_append,
    "Replace": execute_replace,
    "Move": execute_move,
    "Delete": execute_delete,
}


def handle(request: Any) -> None:
    if not isinstance(request, dict) or not isinstance(request.get("id"), int):
        raise ToolError("invalid_request", "request must contain an integer id")
    request_id = request["id"]
    command = request.get("cmd")
    if command == "getTools":
        result(request_id, TOOLS)
        return
    if command == "getBrief":
        result(
            request_id,
            "Read, search, and safely mutate files and explicitly create directories inside the workspace. Source file size is not artificially capped; operations that need complete contents load them into memory, while bounded query parameters limit model-visible results. Text operations conservatively detect common Unicode, East Asian, and Windows encodings, preserve the original encoding and BOM, and reject uncertain or lossy writes. Binary operations use zero-based byte ranges and canonical hexadecimal data. Mutations use an 8-character SHA-256-derived concurrency fingerprint. File.Edit and File.EditBytes apply all requested ranges against one original snapshot, deliberately omit the new hash, and require a refreshed matching read before another edit; other hash-based mutations may chain from the returned hash. This short value detects stale edits; it is not a security integrity digest.",
        )
        return
    tool = request.get("tool")
    if tool == "ApplyPatch":
        raise ToolError(
            "tool_disabled",
            "File.ApplyPatch is disabled. Use File.Edit instead.",
        )
    if tool not in TOOLS:
        raise ToolError("unknown_tool", f"unknown File tool: {tool}")
    if command == "getInputSchema":
        result(request_id, INPUT_SCHEMAS[tool])
    elif command == "getOutputSchema":
        result(request_id, OUTPUT_SCHEMAS[tool])
    elif command == "getInstructions":
        result(request_id, INSTRUCTIONS[tool])
    elif command == "getRoute":
        result(request_id, ROUTES[tool])
    elif command == "getExamples":
        result(request_id, EXAMPLES[tool])
    elif command == "execute":
        data = validate_object(request.get("input"))
        allowed = set(INPUT_SCHEMAS[tool]["properties"])
        unexpected = sorted(set(data) - allowed)
        if unexpected:
            raise ToolError(
                "invalid_arguments", f"unexpected input fields: {', '.join(unexpected)}"
            )
        result(request_id, EXECUTORS[tool](data))
    else:
        raise ToolError("unknown_command", f"unsupported command: {command}")


for line in sys.stdin:
    request_id = 0
    try:
        request = json.loads(line)
        if isinstance(request, dict) and isinstance(request.get("id"), int):
            request_id = request["id"]
        handle(request)
    except ToolError as exc:
        error(request_id, exc)
    except (OSError, ValueError, TypeError, shutil.Error) as exc:
        error(request_id, ToolError("execution_error", str(exc)))
