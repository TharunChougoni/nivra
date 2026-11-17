//! Off-host artifact assembly. Reads local Git metadata; never creates/pushes tags.
use crate::{digest, safe_file};
use std::{fs, path::Path, process::Command};
const ASSET: &str = "nivra-x86_64-unknown-linux-musl";
const PROMPTS_ASSET: &str = "default-prompts.json";
fn prompt_files() -> impl Iterator<Item = &'static str> {
    include_str!("../../../packaging/rust/prompts/inventory")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}
fn git(root: &Path, args: &[&str]) -> Result<String, &'static str> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|_| "release_git_unavailable")?;
    if !out.status.success() {
        return Err("release_git_invalid");
    }
    Ok(String::from_utf8(out.stdout)
        .map_err(|_| "release_git_invalid")?
        .trim()
        .into())
}
pub fn assemble(root: &Path, tag: &str, binary: &Path, output: &Path) -> Result<(), &'static str> {
    if tag != format!("v{}", env!("CARGO_PKG_VERSION")) {
        return Err("release_version_mismatch");
    }
    if git(root, &["cat-file", "-t", &format!("refs/tags/{tag}")])? != "tag" {
        return Err("annotated_tag_required");
    }
    let revision = git(root, &["rev-parse", "HEAD"])?;
    if git(root, &["rev-parse", &format!("refs/tags/{tag}^{{}}")])? != revision {
        return Err("release_commit_mismatch");
    }
    if !git(root, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("release_source_dirty");
    }
    let changelog = safe_file(root, "CHANGELOG.md")?;
    if !String::from_utf8_lossy(&changelog).lines().any(|s| {
        s.split_whitespace().take(2).collect::<Vec<_>>() == vec!["##", env!("CARGO_PKG_VERSION")]
    }) {
        return Err("release_changelog_mismatch");
    }
    let bytes = fs::read(binary).map_err(|_| "release_binary_unreadable")?;
    if bytes.len() > 32 * 1024 * 1024 || !static_elf(&bytes) {
        return Err("static_musl_artifact_required");
    }
    fs::create_dir(output).map_err(|_| "immutable_release_output_required")?;
    fs::write(output.join(ASSET), &bytes).map_err(|_| "release_write_failed")?;
    let sha = digest(&bytes);
    let mut prompts = serde_json::Map::new();
    for name in prompt_files() {
        let prompt = safe_file(root, &format!("packaging/rust/prompts/{name}"))?;
        if prompt.is_empty() || prompt.len() > 16_384 {
            return Err("release_prompt_invalid");
        }
        let text = String::from_utf8(prompt).map_err(|_| "release_prompt_invalid")?;
        prompts.insert((*name).to_owned(), serde_json::Value::String(text));
    }
    let prompts_bytes = serde_json::to_vec(&prompts).map_err(|_| "release_prompt_invalid")?;
    if prompts_bytes.len() > 65_536 {
        return Err("release_prompt_bundle_oversize");
    }
    let prompts_sha = digest(&prompts_bytes);
    fs::write(output.join(PROMPTS_ASSET), &prompts_bytes).map_err(|_| "release_write_failed")?;
    fs::write(
        output.join("default-prompts.sha256"),
        format!("{prompts_sha}  {PROMPTS_ASSET}\n"),
    )
    .map_err(|_| "release_write_failed")?;
    fs::write(
        output.join(format!("{ASSET}.sha256")),
        format!("{sha}  {ASSET}\n"),
    )
    .map_err(|_| "release_write_failed")?;
    fs::write(output.join("release.json"),serde_json::to_vec_pretty(&serde_json::json!({"tag":tag,"source_commit":revision,"target":"x86_64-unknown-linux-musl","sha256":sha})).map_err(|_|"release_manifest_failed")?).map_err(|_|"release_write_failed")?;
    Ok(())
}
fn static_elf(bytes: &[u8]) -> bool {
    fn number(bytes: &[u8], offset: usize, size: usize) -> Option<u64> {
        let slice = bytes.get(offset..offset.checked_add(size)?)?;
        Some(
            slice
                .iter()
                .enumerate()
                .fold(0, |v, (i, b)| v | (u64::from(*b) << (i * 8))),
        )
    }
    let inspect = || -> Option<bool> {
        if bytes.get(..6)? != b"\x7fELF\x02\x01" || number(bytes, 18, 2)? != 62 {
            return Some(false);
        }
        let offset = usize::try_from(number(bytes, 32, 8)?).ok()?;
        let size = usize::try_from(number(bytes, 54, 2)?).ok()?;
        let count = usize::try_from(number(bytes, 56, 2)?).ok()?;
        if size < 56 || count == 0 || count > 1024 {
            return Some(false);
        }
        for index in 0..count {
            let at = offset.checked_add(index.checked_mul(size)?)?;
            bytes.get(at..at.checked_add(56)?)?;
            let kind = number(bytes, at, 4)?;
            if kind == 3 {
                return Some(false);
            }
            if kind == 2 {
                let start = usize::try_from(number(bytes, at.checked_add(8)?, 8)?).ok()?;
                let length = usize::try_from(number(bytes, at.checked_add(32)?, 8)?).ok()?;
                let data = bytes.get(start..start.checked_add(length)?)?;
                if data.len() % 16 != 0 {
                    return Some(false);
                }
                for entry in data.chunks_exact(16) {
                    match number(entry, 0, 8)? {
                        0 => break,
                        1 => return Some(false),
                        _ => {}
                    }
                }
            }
        }
        Some(true)
    };
    inspect().unwrap_or(false)
}
