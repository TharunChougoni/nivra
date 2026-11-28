//! Learned skills and the one-time import. Drafts under `skills-proposed/`
//! become proposed revisions; only approved revisions reach `skills/`, which
//! Rust alone writes. Shipped skills keep precedence: a draft may not reuse
//! their names.
use crate::runtime::{Config, Result, digest, rollover};
use nivra_core::autoresearch::slug_ok;
use nivra_store::research_memory::Store;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The skills shipped under `agent/skills/`. They always win a name.
pub(crate) const SHIPPED: &[&str] = &[
    "autoresearch",
    "backtest-methodology",
    "regime-check",
    "strategy-abi",
    "strategy-library",
    "strategy-review",
];
const MAX_SKILL_BYTES: u64 = 32_768;
const IMPORT_MARKER: &str = "autoresearch-import-books-v1";

fn private_dirs(root: &Path, dir: &Path) -> Result<()> {
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| "workspace_path_escape")?;
    let mut current = root.to_path_buf();
    for part in rel.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("workspace_symlink".into()),
            Err(_) => fs::DirBuilder::new().mode(0o700).create(&current)?,
        }
    }
    Ok(())
}

/// Write a file under `root` through a temporary sibling and a rename.
fn write_private(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("workspace_path_escape")?;
    private_dirs(root, parent)?;
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("workspace_symlink".into());
    }
    let temp = parent.join(format!(".autoresearch-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn regular_file(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    (meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= max)
        .then(|| fs::read(path).ok())
        .flatten()
}

fn entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut found: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(_, path)| {
            fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_symlink())
        })
        .collect();
    found.sort();
    found
}

/// The frontmatter a draft must carry: its own name, a description and the
/// findings it rests on as `findings: 3, 7`.
pub(crate) fn parse_draft(name: &str, text: &str) -> std::result::Result<Vec<i64>, &'static str> {
    let body = text
        .strip_prefix("---\n")
        .ok_or("skill_frontmatter_missing")?;
    let (front, _) = body
        .split_once("\n---")
        .ok_or("skill_frontmatter_missing")?;
    let field = |key: &str| {
        front
            .lines()
            .find_map(|line| line.strip_prefix(key).map(str::trim))
    };
    if field("name:") != Some(name) {
        return Err("skill_name_mismatch");
    }
    if field("description:").is_none_or(str::is_empty) {
        return Err("skill_description_missing");
    }
    field("findings:")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| id.parse().map_err(|_| "skill_findings_invalid"))
        .collect()
}

/// Stage every valid draft as a proposed revision. Returns what was refused.
pub(super) fn stage_proposals(
    workspace: &Path,
    store: &mut Store,
    now: i64,
) -> Result<Vec<(String, &'static str)>> {
    let mut refused = Vec::new();
    for (name, dir) in entries(&workspace.join("skills-proposed")) {
        let verdict = (|| -> std::result::Result<(Vec<u8>, Vec<i64>), &'static str> {
            if !slug_ok(&name, 64) || SHIPPED.contains(&name.as_str()) {
                return Err("skill_name_reserved");
            }
            if entries(&dir).iter().any(|(file, _)| file != "SKILL.md") {
                return Err("skill_extra_files");
            }
            let bytes = regular_file(&dir.join("SKILL.md"), MAX_SKILL_BYTES)
                .ok_or("skill_draft_unreadable")?;
            let text = std::str::from_utf8(&bytes).map_err(|_| "skill_draft_unreadable")?;
            let findings = parse_draft(&name, text)?;
            Ok((bytes, findings))
        })();
        match verdict {
            Ok((bytes, findings)) => {
                let known = findings
                    .iter()
                    .map(|id| store.finding(*id).map(|f| f.is_some()))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if known.iter().all(|&found| found) {
                    store.propose_skill(&name, &bytes, &findings, now)?;
                } else {
                    refused.push((name, "skill_findings_unknown"));
                }
            }
            Err(reason) => refused.push((name, reason)),
        }
    }
    Ok(refused)
}

/// Make `skills/` hold exactly the approved revisions.
pub(super) fn materialize(workspace: &Path, store: &Store) -> Result<usize> {
    let skills = workspace.join("skills");
    let approved = store.skills(Some("approved"))?;
    for skill in &approved {
        let bytes = store.object(&skill.digest)?;
        let path = skills.join(&skill.name).join("SKILL.md");
        if regular_file(&path, MAX_SKILL_BYTES).as_deref() != Some(bytes.as_slice()) {
            write_private(workspace, &path, &bytes)?;
        }
    }
    for (name, dir) in entries(&skills) {
        if !approved.iter().any(|skill| skill.name == name)
            && entries(&dir).iter().all(|(file, _)| file == "SKILL.md")
        {
            fs::remove_dir_all(&dir)?;
        }
    }
    Ok(approved.len())
}

/// The dated books under the sessions root, oldest first.
fn books(c: &Config) -> Result<Vec<(String, PathBuf)>> {
    Ok(entries(&rollover::root(c)?)
        .into_iter()
        .filter(|(name, path)| name.parse::<chrono::NaiveDate>().is_ok() && path.is_dir())
        .collect())
}

/// Copy scripts and notes from every dated book workspace into the research
/// workspace once, deduplicated by hash. Book skills become proposed drafts.
/// Old books are never changed.
pub(super) fn import_books(
    c: &Config,
    book_root: &Path,
    workspace: &Path,
    store: &mut Store,
    now: i64,
) -> Result<usize> {
    let marker = digest(IMPORT_MARKER.as_bytes());
    if store.imported(&marker)? {
        return Ok(0);
    }
    let mut imported = 0;
    let mut copy = |store: &mut Store, origin: &Path, dest: PathBuf| -> Result<()> {
        let Some(bytes) = regular_file(origin, 1_048_576) else {
            return Ok(());
        };
        let sha = digest(&bytes);
        if store.imported(&sha)? {
            return Ok(());
        }
        let dest = match regular_file(&dest, 1_048_576) {
            Some(existing) if existing == bytes => dest,
            Some(_) => {
                let stem = dest
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported");
                let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("txt");
                dest.with_file_name(format!("{stem}-{}.{ext}", &sha[..8]))
            }
            None => dest,
        };
        if regular_file(&dest, 1_048_576).is_none() {
            write_private(workspace, &dest, &bytes)?;
        }
        let rel = dest.strip_prefix(workspace).unwrap_or(&dest);
        store.record_import(&sha, &rel.to_string_lossy(), &origin.to_string_lossy(), now)?;
        imported += 1;
        Ok(())
    };
    for (date, book) in books(c)? {
        let source = book.join(book_root);
        for (name, path) in entries(&source.join("scripts")) {
            if name.ends_with(".py") || name.ends_with(".json") {
                let dir = if name.ends_with(".py") {
                    "scripts"
                } else {
                    "params"
                };
                copy(store, &path, workspace.join(dir).join(&name))?;
            }
        }
        let notes = source.join("notes");
        let mut stack = vec![notes.clone()];
        while let Some(dir) = stack.pop() {
            for (_, path) in entries(&dir) {
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(rel) = path.strip_prefix(&notes) {
                    copy(
                        store,
                        &path,
                        workspace.join("notes/imported").join(&date).join(rel),
                    )?;
                }
            }
        }
        for (name, dir) in entries(&source.join("skills")) {
            // The first book's draft of a name wins; later ones stay in their book.
            if slug_ok(&name, 64)
                && !SHIPPED.contains(&name.as_str())
                && !workspace.join("skills-proposed").join(&name).exists()
            {
                copy(
                    store,
                    &dir.join("SKILL.md"),
                    workspace
                        .join("skills-proposed")
                        .join(&name)
                        .join("SKILL.md"),
                )?;
            }
        }
    }
    store.record_import(&marker, "*", IMPORT_MARKER, now)?;
    Ok(imported)
}
