//! Retired vocabulary from the pre-pi model harness. Each tracked source,
//! document and packaging file may hold at most the occurrences its allowance
//! records, and an allowance must shrink when its file does: names leave the
//! repository and never come back. CHANGELOG.md history is out of scope.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const ALLOWANCE: &str = "rust/nivra-tools/retired-names.txt";
const ROOTS: [&str; 3] = ["rust", "docs", "packaging"];
const SKIP_DIRS: [&str; 5] = ["target", "node_modules", "bundle", "fixtures", ".git"];
const EXTENSIONS: [&str; 10] = [
    "rs", "md", "ts", "tsx", "json", "toml", "service", "timer", "svg", "mjs",
];

/// A rule matches a lowercase needle; `bounded` needles must not touch an
/// ASCII letter or digit on either side.
struct Rule {
    name: &'static str,
    needles: &'static [&'static str],
    bounded: bool,
}

const RULES: [Rule; 11] = [
    Rule {
        name: "luna",
        needles: &["luna"],
        bounded: true,
    },
    Rule {
        name: "supervisor",
        needles: &["supervisor"],
        bounded: false,
    },
    Rule {
        name: "orchestrator",
        needles: &["orchestrator"],
        bounded: false,
    },
    Rule {
        name: "owner_lane",
        needles: &[
            "decision_owner",
            "decision owner",
            "owner_research",
            "owner research",
            "ownerresearch",
            "owner_task",
            "owner task",
            "owner_review",
            "owner review",
            "owner_turn",
            "owner turn",
            "owner_lane",
            "owner lane",
            "sol-owner",
        ],
        bounded: false,
    },
    Rule {
        name: "decide",
        needles: &["decide"],
        bounded: true,
    },
    Rule {
        name: "fact_digest",
        needles: &[
            "factual digest",
            "fact digest",
            "digest turn",
            "digest role",
            "digest lane",
            "luna-latest",
            "digest_id",
            "trigger::digest",
        ],
        bounded: false,
    },
    Rule {
        name: "scorer",
        needles: &["scorer"],
        bounded: false,
    },
    Rule {
        name: "wake",
        needles: &["wake", "wakes", "wake_", "_wake"],
        bounded: true,
    },
    Rule {
        name: "harness_run",
        needles: &["harness run", "harness_run", "harness-run"],
        bounded: false,
    },
    // Book capital is the allocation ledger's sum (capital sleeves).
    Rule {
        name: "initial_allocation",
        needles: &["initial_allocation"],
        bounded: false,
    },
    // The sleeve policy sizes strategy buys; `min_order_rupees` is unrelated.
    Rule {
        name: "order_rupees",
        needles: &["`order_rupees`", "\"order_rupees\"", "'order_rupees'"],
        bounded: false,
    },
];

fn bounded_at(line: &[u8], start: usize, end: usize) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric();
    (start == 0 || !word(line[start - 1])) && (end == line.len() || !word(line[end]))
}

fn count(rule: &Rule, text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut hits = 0;
    let mut at = 0;
    while at < bytes.len() {
        let found = rule.needles.iter().find(|needle| {
            bytes[at..].starts_with(needle.as_bytes())
                && (!rule.bounded || bounded_at(bytes, at, at + needle.len()))
        });
        match found {
            Some(needle) => {
                hits += 1;
                at += needle.len();
            }
            None => at += 1,
        }
    }
    hits
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|_| format!("retired_names_unreadable {}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|_| "retired_names_unreadable".to_string())?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|_| "retired_names_unreadable".to_string())?;
        let name = entry.file_name();
        if kind.is_dir() {
            if !SKIP_DIRS.iter().any(|skip| name == *skip) {
                walk(root, &path, files)?;
            }
        } else if kind.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| EXTENSIONS.contains(&ext))
            && path != root.join(ALLOWANCE)
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Current occurrences keyed by (rule, repository-relative path).
pub fn scan(root: &Path) -> Result<BTreeMap<(String, String), usize>, String> {
    let mut files = Vec::new();
    for top in ROOTS {
        let dir = root.join(top);
        if dir.is_dir() {
            walk(root, &dir, &mut files)?;
        }
    }
    let mut found = BTreeMap::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "retired_names_path_invalid".to_string())?
            .to_string_lossy()
            .into_owned();
        for rule in &RULES {
            let hits = count(rule, &text);
            if hits > 0 {
                found.insert((rule.name.to_owned(), relative.clone()), hits);
            }
        }
    }
    Ok(found)
}

/// The allowance file format: `count<TAB>rule<TAB>path`, one per line.
pub fn render(found: &BTreeMap<(String, String), usize>) -> String {
    found
        .iter()
        .map(|((rule, path), hits)| format!("{hits}\t{rule}\t{path}\n"))
        .collect()
}

fn allowance(root: &Path) -> Result<BTreeMap<(String, String), usize>, Vec<String>> {
    let text = fs::read_to_string(root.join(ALLOWANCE)).unwrap_or_default();
    let mut allowed = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split('\t');
        match (
            fields.next().and_then(|n| n.parse().ok()),
            fields.next(),
            fields.next(),
        ) {
            (Some(hits), Some(rule), Some(path)) if fields.next().is_none() => {
                allowed.insert((rule.to_owned(), path.to_owned()), hits);
            }
            _ => return Err(vec![format!("retired_names_allowance_invalid: {line}")]),
        }
    }
    Ok(allowed)
}

pub fn check(root: &Path) -> Result<(), Vec<String>> {
    let found = scan(root).map_err(|error| vec![error])?;
    let allowed = allowance(root)?;
    let mut errors = Vec::new();
    for ((rule, path), hits) in &found {
        let limit = allowed
            .get(&(rule.clone(), path.clone()))
            .copied()
            .unwrap_or(0);
        if *hits > limit {
            errors.push(format!(
                "retired_name_added {rule} {path}: {hits} found, {limit} allowed; use the pi lane vocabulary"
            ));
        }
    }
    for ((rule, path), limit) in &allowed {
        let hits = found
            .get(&(rule.clone(), path.clone()))
            .copied()
            .unwrap_or(0);
        if hits < *limit {
            errors.push(format!(
                "retired_name_allowance_stale {rule} {path}: lower {limit} to {hits} in {ALLOWANCE}"
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planted_retired_name_fails_and_allowance_only_shrinks() {
        let root = tempfile_root();
        fs::create_dir_all(root.join("rust/nivra-tools")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(
            root.join("docs/lanes.md"),
            "The decision lane reads the brief.\n",
        )
        .unwrap();
        assert_eq!(check(&root), Ok(()));

        fs::write(
            root.join("docs/lanes.md"),
            "Luna writes the factual digest; the supervisor wakes.\nAwake and decided are fine.\n",
        )
        .unwrap();
        let errors = check(&root).unwrap_err();
        for rule in ["luna", "fact_digest", "supervisor", "wake"] {
            assert!(
                errors
                    .iter()
                    .any(|e| e.starts_with(&format!("retired_name_added {rule} docs/lanes.md"))),
                "{rule}: {errors:?}"
            );
        }
        assert!(!errors.iter().any(|e| e.contains(" decide ")), "{errors:?}");

        let current = render(&scan(&root).unwrap());
        fs::write(root.join(ALLOWANCE), &current).unwrap();
        assert_eq!(check(&root), Ok(()));

        fs::write(
            root.join("docs/lanes.md"),
            "The review lane runs after the seal.\n",
        )
        .unwrap();
        let stale = check(&root).unwrap_err();
        assert!(
            stale
                .iter()
                .all(|e| e.starts_with("retired_name_allowance_stale")),
            "{stale:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn tempfile_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nivra-retired-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
}
