//! Public local Markdown links and Cargo release version checks.
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}
fn resolve_missing(path: &Path) -> PathBuf {
    let mut cursor = path;
    let mut suffix = vec![];
    loop {
        if let Ok(mut resolved) = fs::canonicalize(cursor) {
            for component in suffix.into_iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(name) = cursor.file_name() else {
            return path.to_path_buf();
        };
        suffix.push(name.to_owned());
        let Some(parent) = cursor.parent() else {
            return path.to_path_buf();
        };
        cursor = parent;
    }
}
fn markdown_names(root: &Path, dir: &str, unavailable: &str) -> Result<Vec<String>, Vec<String>> {
    let mut names = vec![];
    for entry in fs::read_dir(root.join(dir)).map_err(|_| vec![unavailable.into()])? {
        let entry = entry.map_err(|_| vec![unavailable.into()])?;
        if entry.path().extension().is_some_and(|ext| ext == "md") {
            names.push(format!("{dir}/{}", entry.file_name().to_string_lossy()));
        }
    }
    Ok(names)
}
fn links(root: &Path, path: &Path, source: &str) -> Vec<String> {
    let mut errors = vec![];
    let mut start = 0;
    while let Some(at) = source[start..].find("](") {
        let at = start + at;
        start = at + 2;
        let preceding = &source[..at];
        if preceding
            .rfind('[')
            .is_none_or(|left| preceding.rfind(']').is_some_and(|right| right > left))
        {
            continue;
        }
        let end = source[start..]
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(source.len() - start);
        let target = &source[start..start + end];
        start += end;
        if target.is_empty()
            || target.chars().any(char::is_whitespace)
            || ["#", "http://", "https://", "mailto:"]
                .iter()
                .any(|prefix| target.starts_with(prefix))
        {
            continue;
        }
        let relative = target.split('#').next().unwrap_or("");
        if relative.is_empty() {
            continue;
        }
        let candidate = normalize(&path.parent().unwrap_or(root).join(relative));
        let resolved = resolve_missing(&candidate);
        let label = path.strip_prefix(root).unwrap_or(path).display();
        if !resolved.starts_with(root) {
            errors.push(format!("{label}: link escapes repository: {target}"));
        } else if !resolved.is_file() {
            errors.push(format!("{label}: missing link target {target}"));
        }
    }
    errors
}
pub fn check(root: &Path) -> Result<(), Vec<String>> {
    let root = fs::canonicalize(root).map_err(|_| vec!["repository_unavailable".into()])?;
    let mut paths = vec![
        "README.md".into(),
        "AGENTS.md".into(),
        "CHANGELOG.md".into(),
        "fixtures/README.md".into(),
        "rust/README.md".into(),
    ];
    paths.extend(markdown_names(&root, "docs", "documentation_unavailable")?);
    paths.sort();
    let mut errors = vec![];
    for name in paths {
        let path = root.join(&name);
        match crate::safe_file(&root, &name)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "invalid_markdown_encoding"))
        {
            Ok(text) => errors.extend(links(&root, &path, &text)),
            Err(_) => errors.push(format!("missing documented file {name}")),
        }
    }
    let changelog = crate::safe_file(&root, "CHANGELOG.md").unwrap_or_default();
    if !String::from_utf8_lossy(&changelog).lines().any(|line| {
        line.split_whitespace().take(2).collect::<Vec<_>>() == vec!["##", env!("CARGO_PKG_VERSION")]
    }) {
        errors.push("changelog_workspace_version_mismatch".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
