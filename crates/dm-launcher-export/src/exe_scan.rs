//! Find candidate Windows executables inside a work's folder.
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 12;
const MAX_ENTRIES: usize = 100_000;
/// Candidates at or below this score are listed but never pre-selected.
const SUGGESTION_FLOOR: i64 = -40;
const NON_GAME_NAMES: &[&str] = &[
    "unins",
    "setup",
    "install",
    "redist",
    "vc_",
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "directx",
    "dotnet",
    "ndp",
    "crash",
    "update",
    "patch",
    "readme",
    "manual",
];
const SECONDARY_NAMES: &[&str] = &[
    "config", "setting", "launcher", "tool", "editor", "register",
];
const NON_GAME_FOLDERS: &[&str] = &[
    "redist",
    "directx",
    "dotnet",
    "vcredist",
    "installer",
    "support",
    "tools",
    "__macosx",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExeCandidate {
    pub path: String,
    pub relative_path: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExeScan {
    /// Best guess first.
    pub candidates: Vec<ExeCandidate>,
    pub suggested: Option<String>,
    pub truncated: bool,
}

pub fn is_exe_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

/// Walk `root` without following symlinks. Paths that are not valid UTF-8 cannot
/// be stored in the launchers' text formats and are skipped.
pub fn scan(root: &Path, title: &str) -> ExeScan {
    let mut found: Vec<(i64, ExeCandidate)> = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited = 0usize;
    let mut truncated = false;
    while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                truncated = true;
                break;
            }
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                } else {
                    truncated = true;
                }
            } else if metadata.is_file() && entry.file_name().to_str().is_some_and(is_exe_name) {
                let (Some(text), Ok(relative)) = (path.to_str(), path.strip_prefix(root)) else {
                    continue;
                };
                let Some(relative_text) = relative.to_str() else {
                    continue;
                };
                found.push((
                    score(relative_text, metadata.len(), title),
                    ExeCandidate {
                        path: text.to_owned(),
                        relative_path: relative_text.to_owned(),
                        size_bytes: metadata.len(),
                    },
                ));
            }
        }
    }
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.relative_path.cmp(&b.1.relative_path))
    });
    let suggested = found
        .first()
        .filter(|(score, _)| *score > SUGGESTION_FLOOR)
        .map(|(_, candidate)| candidate.path.clone());
    ExeScan {
        candidates: found.into_iter().map(|(_, candidate)| candidate).collect(),
        suggested,
        truncated,
    }
}

fn score(relative: &str, size: u64, title: &str) -> i64 {
    let lower = relative.to_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    let mut score = -5 * parts.len() as i64;
    if NON_GAME_NAMES.iter().any(|name| stem.contains(name)) {
        score -= 100;
    }
    if SECONDARY_NAMES.iter().any(|name| stem.contains(name)) {
        score -= 30;
    }
    if parts
        .iter()
        .any(|part| NON_GAME_FOLDERS.iter().any(|name| part.contains(name)))
    {
        score -= 50;
    }
    if stem == "game" {
        score += 20;
    }
    let title = title.to_lowercase();
    if stem.chars().count() >= 3 && title.contains(stem) {
        score += 30;
    }
    if size >= 10 * 1024 * 1024 {
        score += 15;
    } else if size >= 1024 * 1024 {
        score += 10;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, relative: &str, size: usize) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; size]).unwrap();
    }

    #[test]
    fn prefers_the_game_over_installers_and_redistributables() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "unins000.exe", 2_000_000);
        touch(dir.path(), "Setup.EXE", 2_000_000);
        touch(dir.path(), "_CommonRedist/vcredist_x86.exe", 2_000_000);
        touch(dir.path(), "Config.exe", 2_000_000);
        touch(dir.path(), "data/game.bin", 10);
        touch(dir.path(), "Game.exe", 1_500_000);
        let scan = scan(dir.path(), "Some Title");
        assert_eq!(scan.candidates.len(), 5);
        assert_eq!(scan.candidates[0].relative_path, "Game.exe");
        assert_eq!(
            scan.suggested.as_deref(),
            Some(scan.candidates[0].path.as_str())
        );
    }

    #[test]
    fn a_name_matching_the_title_wins_among_ordinary_executables() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "viewer.exe", 1_000);
        touch(dir.path(), "MoonLight.exe", 1_000);
        let scan = scan(dir.path(), "MoonLight Serenade");
        assert_eq!(scan.candidates[0].relative_path, "MoonLight.exe");
    }

    #[test]
    fn only_uninstallers_are_listed_but_not_suggested() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "unins000.exe", 1_000);
        let scan = scan(dir.path(), "Title");
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.suggested, None);
    }

    #[test]
    fn a_folder_without_executables_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "voice/track01.wav", 10);
        assert_eq!(scan(dir.path(), "Title"), ExeScan::default());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        touch(outside.path(), "evil.exe", 10);
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("evil.exe"),
            dir.path().join("direct.exe"),
        )
        .unwrap();
        assert!(scan(dir.path(), "Title").candidates.is_empty());
    }
}
