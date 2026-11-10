//! Official protobuf field numbers must stay identical to upstream/master.
//!
//! This compares `proto/network.proto` with
//! `upstream/master:tauri-app/crates/micyou-protocol/proto/network.proto`
//! and also checks that Kotlin `Protocol.kt` uses the same numbers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

type Fields = BTreeMap<String, u32>;
type Messages = BTreeMap<String, Fields>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("workspace root")
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            while let Some(n) = chars.peek() {
                if *n == '\n' {
                    break;
                }
                chars.next();
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(n) = chars.next() {
                if n == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn parse_proto_messages(src: &str) -> Messages {
    let src = strip_comments(src);
    let mut messages = Messages::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &src[i..];
        if let Some(rel) = rest.find("message ") {
            i += rel + "message ".len();
            let name_start = src[i..]
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(off, _)| i + off)
                .unwrap_or(src.len());
            let name_end = src[name_start..]
                .char_indices()
                .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_')
                .map(|(off, _)| name_start + off)
                .unwrap_or(src.len());
            let name = src[name_start..name_end].to_string();
            if let Some(brace_rel) = src[name_end..].find('{') {
                let open = name_end + brace_rel;
                let close = match_brace(&src, open);
                messages.insert(name, parse_fields(&src[open + 1..close]));
                i = close + 1;
                continue;
            }
        }
        break;
    }
    messages
}

fn match_brace(src: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (idx, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + idx;
                }
            }
            _ => {}
        }
    }
    panic!("unclosed message body")
}

fn parse_fields(body: &str) -> Fields {
    let mut fields = Fields::new();
    for raw in body.split(';') {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("reserved") || line.starts_with("option") {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let number: u32 = rhs
            .trim()
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("not a field number: {line}"));
        let name = lhs
            .split_whitespace()
            .last()
            .unwrap_or_else(|| panic!("missing field name: {line}"))
            .to_string();
        fields.insert(name, number);
    }
    fields
}

fn parse_kotlin_class(src: &str, class_name: &str) -> Fields {
    let needle = format!("data class {class_name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("{class_name} not found in Protocol.kt"));
    let open = start + needle.len() - 1;
    let close = match_paren(&src, open);
    let body = &src[open + 1..close];
    let mut fields = Fields::new();
    let mut tokens = body.split('@').filter(|t| t.starts_with("ProtoNumber("));
    while let Some(token) = tokens.next() {
        let number_start = "ProtoNumber(".len();
        let number_end = token[number_start..]
            .find(')')
            .unwrap_or_else(|| panic!("unclosed ProtoNumber in {class_name}"));
        let number: u32 = token[number_start..number_start + number_end]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("bad ProtoNumber in {class_name}: {token}"));
        let after = &token[number_start + number_end + 1..];
        let val_idx = after
            .find("val ")
            .unwrap_or_else(|| panic!("ProtoNumber without val in {class_name}: {token}"));
        let name_start = val_idx + 4;
        let name_end = after[name_start..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map(|off| name_start + off)
            .unwrap_or(after.len());
        fields.insert(after[name_start..name_end].trim().to_string(), number);
    }
    fields
}

fn match_paren(src: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut chars = src[open..].char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open + idx;
                }
            }
            _ => {}
        }
    }
    panic!("unclosed class parameter list")
}

fn load_upstream_proto(repo: &Path) -> (String, String) {
    let output = Command::new("git")
        .current_dir(repo)
        .args([
            "show",
            "upstream/master:tauri-app/crates/micyou-protocol/proto/network.proto",
        ])
        .output()
        .expect("git show upstream proto");
    assert!(
        output.status.success(),
        "git show upstream/master proto failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rev = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "upstream/master"])
        .output()
        .expect("git rev-parse upstream/master");
    assert!(rev.status.success(), "missing upstream/master");
    let sha = String::from_utf8(rev.stdout).unwrap();
    (
        String::from_utf8(output.stdout).expect("upstream proto utf-8"),
        sha.trim().to_string(),
    )
}

fn assert_official_subset(local: &Messages, upstream: &Messages) {
    for (message, official_fields) in upstream {
        let local_fields = local
            .get(message)
            .unwrap_or_else(|| panic!("fork proto is missing official message {message}"));
        for (name, number) in official_fields {
            let local_number = local_fields.get(name).unwrap_or_else(|| {
                panic!("fork {message} is missing official field {name}={number}")
            });
            assert_eq!(
                local_number, number,
                "fork {message}.{name} is {local_number}, official is {number}"
            );
        }
        for (name, number) in local_fields {
            if official_fields.contains_key(name) {
                continue;
            }
            assert!(
                *number >= 1000,
                "fork-only {message}.{name}={number} must be >= 1000 so official can grow below 1000"
            );
        }
    }
}

fn assert_same_fields(label: &str, left: &Fields, right: &Fields) {
    assert_eq!(
        left, right,
        "{label} field numbers differ: proto={left:?} other={right:?}"
    );
}

#[test]
fn official_field_numbers_match_upstream_master() {
    let repo = repo_root();
    let local_proto = std::fs::read_to_string(repo.join(
        "tauri-app/crates/micyou-protocol/proto/network.proto",
    ))
    .expect("local network.proto");
    let (upstream_proto, sha) = load_upstream_proto(&repo);
    let local = parse_proto_messages(&local_proto);
    let upstream = parse_proto_messages(&upstream_proto);
    assert!(
        !upstream.is_empty(),
        "failed to parse upstream proto at {sha}"
    );
    assert_official_subset(&local, &upstream);

    let kotlin = std::fs::read_to_string(repo.join(
        "composeApp/src/main/kotlin/com/lanrhyme/micyou/network/Protocol.kt",
    ))
    .expect("Protocol.kt");
    for message in ["ConnectMessage", "MessageWrapper"] {
        let proto_fields = local
            .get(message)
            .unwrap_or_else(|| panic!("{message} missing from proto"));
        let kotlin_fields = parse_kotlin_class(&kotlin, message);
        assert_same_fields(message, proto_fields, &kotlin_fields);
    }
    eprintln!("official field numbers match upstream/master {sha}");
}
