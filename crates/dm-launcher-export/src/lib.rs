//! Export a downloaded work to Linux game launchers.
//!
//! Launcher file formats follow the launchers' public sources; see
//! `docs/designs/2026-11-07.00-launcher-export.md`. Launchers only start the
//! work's own executable.
mod discovery;
mod exe_scan;
mod faugus;
mod fsio;
mod heroic;
mod json_file;
mod steam;
mod vdf_binary;
mod vdf_text;

pub use discovery::{
    discover_compat_tools, discover_installs, CompatTool, ExportEnv, InstallFlavor,
    LauncherInstall, LauncherKind, SteamUser,
};
pub use exe_scan::ExeCandidate;
pub use fsio::{FileAction, FileChange};

use discovery::{resolve_tool, steam_can_see_tool, ResolvedTool};
use fsio::{commit_all, CommitFailure, PendingWrite};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Refused(String),
    /// The launcher is running; the request may repeat with acknowledgement.
    #[error("{0}")]
    NeedsAcknowledgement(String),
    /// An earlier export used another prefix; the request may repeat with consent.
    #[error("{0}")]
    PrefixChange(String),
    #[error("{}: {message}; the file was not changed", path.display())]
    Corrupt { path: PathBuf, message: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The feature is limited to Linux; the crate still builds everywhere.
pub const fn is_supported() -> bool {
    cfg!(target_os = "linux")
}

#[derive(Clone, Debug)]
pub struct WorkRef {
    pub work_id: String,
    pub root: PathBuf,
    pub title: String,
    pub cover_url: Option<String>,
    /// Folders of other works; a prefix may not overlap them either.
    pub other_work_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistingExport {
    pub install_id: String,
    pub steam_user_id: Option<String>,
    pub display_name: Option<String>,
    pub executable: Option<String>,
    pub prefix: Option<String>,
    pub compat_tool: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub executables: Vec<ExeCandidate>,
    pub suggested_executable: Option<String>,
    pub scan_truncated: bool,
    pub default_display_name: String,
    pub default_prefix: String,
    pub installs: Vec<LauncherInstall>,
    pub compat_tools: Vec<CompatTool>,
    pub existing: Vec<ExistingExport>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTarget {
    pub install_id: String,
    #[serde(default)]
    pub steam_user_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub executable: String,
    pub display_name: String,
    pub compat_tool: String,
    pub prefix: String,
    pub targets: Vec<ExportTarget>,
    /// Export even to launchers that are running and may overwrite the result.
    #[serde(default)]
    pub acknowledge_running: bool,
    /// Move launchers whose earlier export used a different prefix.
    #[serde(default)]
    pub allow_prefix_change: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetStatus {
    Created,
    Updated,
    Unchanged,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetResult {
    pub install_id: String,
    pub launcher: Option<LauncherKind>,
    pub status: TargetStatus,
    pub files: Vec<FileChange>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    /// The failure can be retried by acknowledging a running launcher.
    pub needs_acknowledgement: bool,
    /// The failure can be retried by allowing a prefix change.
    pub needs_prefix_change: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub prefix: String,
    pub results: Vec<TargetResult>,
}

#[derive(Debug)]
pub(crate) struct ExportSpec {
    pub work_id: String,
    pub display_name: String,
    pub executable: String,
    pub prefix: String,
    pub tool: ResolvedTool,
    pub cover_url: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub created: bool,
    pub writes: Vec<PendingWrite>,
    pub warnings: Vec<String>,
}

fn validate_work_id(work_id: &str) -> Result<(), ExportError> {
    let valid = !work_id.is_empty()
        && work_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ExportError::Invalid(format!(
            "work id {work_id:?} cannot be used in launcher files"
        )))
    }
}

fn canonical_work_root(work: &WorkRef) -> Result<PathBuf, ExportError> {
    work.root.canonicalize().map_err(|source| ExportError::Io {
        path: work.root.clone(),
        source,
    })
}

fn default_prefix(env: &ExportEnv, work_id: &str) -> PathBuf {
    env.home.join("Games/dlsite-manager/prefixes").join(work_id)
}

/// Gather everything the export form needs. Reads launcher files; writes nothing.
pub fn prepare_options(env: &ExportEnv, work: &WorkRef) -> Result<ExportOptions, ExportError> {
    validate_work_id(&work.work_id)?;
    let root = canonical_work_root(work)?;
    let scan = exe_scan::scan(&root, &work.title);
    let installs = discover_installs(env);
    let compat_tools = discover_compat_tools(env);
    let mut existing = Vec::new();
    let mut warnings = Vec::new();
    for install in &installs {
        let users: Vec<String> = install
            .steam_users
            .iter()
            .map(|user| user.id.clone())
            .collect();
        let found = existing_exports(install, &users, &work.work_id);
        match found {
            Ok(found) => existing.extend(found.into_iter().map(|mut entry| {
                entry.install_id = install.id.clone();
                entry.compat_tool = entry
                    .compat_tool
                    .map(|tool| resolve_tool(&compat_tools, &tool).id);
                entry
            })),
            Err(error) => warnings.push(error.to_string()),
        }
    }
    Ok(ExportOptions {
        executables: scan.candidates,
        suggested_executable: scan.suggested,
        scan_truncated: scan.truncated,
        default_display_name: work.title.clone(),
        default_prefix: default_prefix(env, &work.work_id)
            .to_string_lossy()
            .into_owned(),
        installs,
        compat_tools,
        existing,
        warnings,
    })
}

/// Canonicalize the part of `path` that exists, so symlinks cannot hide where a
/// folder that is not created yet will end up.
fn resolve_through_symlinks(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return rest.iter().rev().fold(resolved, |out, part| out.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn has_control_characters(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn validate_request(
    env: &ExportEnv,
    work: &WorkRef,
    request: &ExportRequest,
) -> Result<ExportSpec, ExportError> {
    validate_work_id(&work.work_id)?;
    if request.targets.is_empty() {
        return Err(ExportError::Invalid(
            "choose at least one launcher".to_owned(),
        ));
    }
    let display_name = request.display_name.trim();
    if display_name.is_empty() || has_control_characters(display_name) {
        return Err(ExportError::Invalid(
            "the display name must not be empty or contain control characters".to_owned(),
        ));
    }
    let compat_tool = request.compat_tool.trim();
    if compat_tool.is_empty() || has_control_characters(compat_tool) {
        return Err(ExportError::Invalid(
            "choose a compatibility tool".to_owned(),
        ));
    }

    let work_root = canonical_work_root(work)?;
    let executable_path = Path::new(&request.executable);
    let executable = executable_path
        .canonicalize()
        .map_err(|source| ExportError::Io {
            path: executable_path.to_path_buf(),
            source,
        })?;
    let is_exe = executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(exe_scan::is_exe_name);
    if !executable.starts_with(&work_root) || !executable.is_file() || !is_exe {
        return Err(ExportError::Invalid(
            "the executable must be an .exe file inside the work's folder".to_owned(),
        ));
    }
    let Some(executable) = executable.to_str().map(str::to_owned) else {
        return Err(ExportError::Invalid(
            "the executable path is not valid UTF-8".to_owned(),
        ));
    };

    let prefix = request.prefix.trim();
    let prefix_path = Path::new(prefix);
    if !prefix_path.is_absolute()
        || has_control_characters(prefix)
        || prefix_path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(ExportError::Invalid(
            "the Wine prefix must be an absolute path".to_owned(),
        ));
    }
    let resolved_prefix = resolve_through_symlinks(prefix_path);
    let overlaps =
        |folder: &Path| resolved_prefix.starts_with(folder) || folder.starts_with(&resolved_prefix);
    if overlaps(&work_root) {
        return Err(ExportError::Invalid(
            "the Wine prefix must not contain or be inside the work's folder, because re-downloading replaces that folder".to_owned(),
        ));
    }
    if work
        .other_work_roots
        .iter()
        .any(|folder| overlaps(&resolve_through_symlinks(folder)))
    {
        return Err(ExportError::Invalid(
            "the Wine prefix must not contain or be inside another work's folder".to_owned(),
        ));
    }

    Ok(ExportSpec {
        work_id: work.work_id.clone(),
        display_name: display_name.to_owned(),
        executable,
        prefix: prefix.to_owned(),
        tool: resolve_tool(&discover_compat_tools(env), compat_tool),
        cover_url: work.cover_url.clone(),
    })
}

fn running_refusal(launcher: LauncherKind) -> &'static str {
    match launcher {
        LauncherKind::Steam => {
            "Steam is running and overwrites these files when it exits; close Steam first or confirm the export anyway"
        }
        LauncherKind::Faugus => {
            "Faugus Launcher is running and rewrites its game list from memory, which drops new entries; close it first or confirm the export anyway"
        }
        LauncherKind::Heroic => {
            "Heroic is running and can overwrite a game's settings from memory; close it first or confirm the export anyway"
        }
    }
}

fn running_warning(launcher: LauncherKind) -> &'static str {
    match launcher {
        LauncherKind::Steam => "Steam was running: it may overwrite this export when it exits",
        LauncherKind::Faugus => {
            "Faugus Launcher was running: its next save can drop this export, and it shows the entry only after a restart"
        }
        LauncherKind::Heroic => {
            "Heroic was running: it can overwrite these settings until it is restarted"
        }
    }
}

fn existing_exports(
    install: &LauncherInstall,
    users: &[String],
    work_id: &str,
) -> Result<Vec<ExistingExport>, ExportError> {
    let path = Path::new(&install.path);
    match install.launcher {
        LauncherKind::Steam => steam::read_existing(path, users, work_id),
        LauncherKind::Faugus => {
            faugus::read_existing(path, work_id).map(|found| found.into_iter().collect())
        }
        LauncherKind::Heroic => {
            heroic::read_existing(path, work_id).map(|found| found.into_iter().collect())
        }
    }
}

fn export_target(
    env: &ExportEnv,
    target: &ExportTarget,
    install: &LauncherInstall,
    spec: &ExportSpec,
    request: &ExportRequest,
) -> Result<(Plan, Vec<String>), ExportError> {
    let mut warnings = Vec::new();
    if install.flavor == InstallFlavor::Flatpak {
        warnings.push(
            "Flatpak launchers need filesystem access to the work's folder, the Wine prefix and the compatibility tool"
                .to_owned(),
        );
    }
    if install.running {
        if !request.acknowledge_running {
            return Err(ExportError::NeedsAcknowledgement(
                running_refusal(install.launcher).to_owned(),
            ));
        }
        warnings.push(running_warning(install.launcher).to_owned());
    }
    if install.launcher == LauncherKind::Steam {
        if target.steam_user_ids.is_empty() {
            return Err(ExportError::Invalid(
                "choose at least one Steam user".to_owned(),
            ));
        }
        for user in &target.steam_user_ids {
            if !install.steam_users.iter().any(|known| known.id == *user) {
                return Err(ExportError::Invalid(format!("unknown Steam user {user}")));
            }
        }
    }
    if !request.allow_prefix_change {
        let moved = existing_exports(install, &target.steam_user_ids, &spec.work_id)?
            .into_iter()
            .any(|existing| existing.prefix.as_deref() != Some(spec.prefix.as_str()));
        if moved {
            return Err(ExportError::PrefixChange(
                "this launcher's earlier export uses a different Wine prefix; keep that prefix or confirm the move, because saves stay in the old one"
                    .to_owned(),
            ));
        }
    }
    let path = Path::new(&install.path);
    let plan = match install.launcher {
        LauncherKind::Steam => {
            let visible = steam_can_see_tool(env, path, install.flavor, &spec.tool.id);
            steam::plan(path, &target.steam_user_ids, spec, visible)?
        }
        LauncherKind::Faugus => faugus::plan(env, path, spec)?,
        LauncherKind::Heroic => heroic::plan(path, spec)?,
    };
    Ok((plan, warnings))
}

type TargetOutcome = (bool, Vec<FileChange>, Vec<String>);

fn run_target(
    env: &ExportEnv,
    target: &ExportTarget,
    install: Option<&LauncherInstall>,
    spec: &ExportSpec,
    request: &ExportRequest,
) -> Result<TargetOutcome, CommitFailure> {
    let plain = |error: ExportError| CommitFailure {
        error,
        files: Vec::new(),
        rollback_errors: Vec::new(),
    };
    let install = install.ok_or_else(|| {
        plain(ExportError::Invalid(format!(
            "launcher {} was not found",
            target.install_id
        )))
    })?;
    let (plan, mut warnings) = export_target(env, target, install, spec, request).map_err(plain)?;
    // Only once this launcher's export is known to be valid.
    std::fs::create_dir_all(&spec.prefix)
        .map_err(|source| ExportError::Io {
            path: PathBuf::from(&spec.prefix),
            source,
        })
        .map_err(plain)?;
    warnings.extend(plan.warnings);
    let files = commit_all(plan.writes)?;
    if files
        .iter()
        .all(|file| file.action == FileAction::Unchanged)
    {
        // Nothing was written, so a running launcher has nothing to overwrite.
        warnings.retain(|warning| warning != running_warning(install.launcher));
    }
    Ok((plan.created, files, warnings))
}

fn failed_result(
    target: &ExportTarget,
    launcher: Option<LauncherKind>,
    failure: CommitFailure,
) -> TargetResult {
    let mut error = failure.error.to_string();
    if !failure.rollback_errors.is_empty() {
        error.push_str(&format!(
            "; these files could not be restored, use their backups: {}",
            failure.rollback_errors.join("; ")
        ));
    } else if !failure.files.is_empty() {
        error.push_str("; files already written were restored");
    }
    TargetResult {
        install_id: target.install_id.clone(),
        launcher,
        status: TargetStatus::Failed,
        needs_acknowledgement: matches!(failure.error, ExportError::NeedsAcknowledgement(_)),
        needs_prefix_change: matches!(failure.error, ExportError::PrefixChange(_)),
        files: failure.files,
        warnings: Vec::new(),
        error: Some(error),
    }
}

/// Export to each target independently: one launcher's failure leaves its files
/// unchanged and does not stop the others.
pub fn export_work(
    env: &ExportEnv,
    work: &WorkRef,
    request: &ExportRequest,
) -> Result<ExportReport, ExportError> {
    if !is_supported() {
        return Err(ExportError::Refused(
            "Launcher export is only available on Linux".to_owned(),
        ));
    }
    run_export(env, work, request)
}

fn run_export(
    env: &ExportEnv,
    work: &WorkRef,
    request: &ExportRequest,
) -> Result<ExportReport, ExportError> {
    let spec = validate_request(env, work, request)?;
    let installs = discover_installs(env);
    let mut results = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        let install = installs.iter().find(|known| known.id == target.install_id);
        let launcher = install.map(|install| install.launcher);
        results.push(match run_target(env, target, install, &spec, request) {
            Ok((created, files, warnings)) => TargetResult {
                install_id: target.install_id.clone(),
                launcher,
                status: if files
                    .iter()
                    .all(|file| file.action == FileAction::Unchanged)
                {
                    TargetStatus::Unchanged
                } else if created {
                    TargetStatus::Created
                } else {
                    TargetStatus::Updated
                },
                files,
                warnings,
                error: None,
                needs_acknowledgement: false,
                needs_prefix_change: false,
            },
            Err(failure) => failed_result(target, launcher, failure),
        });
    }
    Ok(ExportReport {
        prefix: spec.prefix,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Home {
        dir: tempfile::TempDir,
        env: ExportEnv,
        work: WorkRef,
    }

    /// A temp home with all three launchers. Nothing here touches real configs.
    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let mut env = ExportEnv::for_home(path.clone());
        env.data_dirs = vec![path.join("system-share")];
        env.proc_root = path.join("proc");

        let steam = env.data_home.join("Steam");
        fs::create_dir_all(steam.join("userdata/111")).unwrap();
        let tool = steam.join("compatibilitytools.d/proton-dl");
        fs::create_dir_all(&tool).unwrap();
        fs::write(tool.join("proton"), "").unwrap();
        fs::write(
            tool.join("compatibilitytool.vdf"),
            "\"compatibilitytools\" { \"compat_tools\" { \"Proton-DL\" { \"install_path\" \".\" \"display_name\" \"Proton-DL\" } } }",
        )
        .unwrap();
        fs::create_dir_all(env.data_home.join("faugus-launcher")).unwrap();
        fs::create_dir_all(env.config_home.join("heroic")).unwrap();

        let root = path.join("library/RJ1");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Game.exe"), "").unwrap();
        fs::write(root.join("unins000.exe"), "").unwrap();
        let work = WorkRef {
            work_id: "RJ1".to_owned(),
            root,
            title: "Title".to_owned(),
            cover_url: None,
            other_work_roots: Vec::new(),
        };
        Home { dir, env, work }
    }

    fn request(home: &Home) -> ExportRequest {
        let options = prepare_options(&home.env, &home.work).unwrap();
        ExportRequest {
            executable: options.suggested_executable.unwrap(),
            display_name: options.default_display_name,
            compat_tool: "Proton-DL".to_owned(),
            prefix: options.default_prefix,
            targets: options
                .installs
                .iter()
                .map(|install| ExportTarget {
                    install_id: install.id.clone(),
                    steam_user_ids: install.steam_users.iter().map(|u| u.id.clone()).collect(),
                })
                .collect(),
            acknowledge_running: false,
            allow_prefix_change: false,
        }
    }

    fn statuses(report: &ExportReport) -> Vec<TargetStatus> {
        report.results.iter().map(|result| result.status).collect()
    }

    #[test]
    fn exports_to_all_launchers_then_settles_and_reads_back() {
        let home = home();
        let request = request(&home);
        let prefix = request.prefix.clone();
        assert!(prefix.ends_with("Games/dlsite-manager/prefixes/RJ1"));

        let first = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&first), vec![TargetStatus::Created; 3]);
        assert!(Path::new(&prefix).is_dir());

        let second = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&second), vec![TargetStatus::Unchanged; 3]);
        assert!(second
            .results
            .iter()
            .flat_map(|result| &result.files)
            .all(|file| file.backup.is_none()));

        let options = prepare_options(&home.env, &home.work).unwrap();
        assert_eq!(options.existing.len(), 3);
        assert!(options.warnings.is_empty());
        for existing in &options.existing {
            assert_eq!(existing.prefix.as_deref(), Some(prefix.as_str()));
            assert_eq!(existing.compat_tool.as_deref(), Some("Proton-DL"));
            assert_eq!(existing.display_name.as_deref(), Some("Title"));
        }
    }

    #[test]
    fn updating_creates_timestamped_backups_of_the_old_files() {
        let home = home();
        let mut request = request(&home);
        run_export(&home.env, &home.work, &request).unwrap();
        request.display_name = "Renamed".to_owned();
        let report = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&report), vec![TargetStatus::Updated; 3]);
        for file in report
            .results
            .iter()
            .flat_map(|result| &result.files)
            .filter(|file| file.action == FileAction::Updated)
        {
            let backup = file.backup.as_deref().unwrap();
            assert!(backup.contains(".dlsite-manager-"), "{backup}");
            assert_ne!(fs::read(backup).unwrap(), fs::read(&file.path).unwrap());
        }
    }

    #[test]
    fn a_corrupt_launcher_fails_alone() {
        let home = home();
        let faugus_file = home.env.data_home.join("faugus-launcher/games.json");
        fs::write(&faugus_file, "{corrupt").unwrap();
        let report = run_export(&home.env, &home.work, &request(&home)).unwrap();
        assert_eq!(
            statuses(&report),
            vec![
                TargetStatus::Created,
                TargetStatus::Failed,
                TargetStatus::Created
            ]
        );
        assert!(report.results[1]
            .error
            .as_deref()
            .unwrap()
            .contains("not changed"));
        assert_eq!(fs::read_to_string(&faugus_file).unwrap(), "{corrupt");
    }

    #[test]
    fn a_running_steam_needs_acknowledgement() {
        let home = home();
        let process = home.env.proc_root.join("42");
        fs::create_dir_all(&process).unwrap();
        fs::write(process.join("comm"), "steam\n").unwrap();
        let mut request = request(&home);
        request.targets.truncate(1);
        let shortcuts = home
            .env
            .data_home
            .join("Steam/userdata/111/config/shortcuts.vdf");

        let refused = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(refused.results[0].status, TargetStatus::Failed);
        assert!(refused.results[0]
            .error
            .as_deref()
            .unwrap()
            .contains("Steam is running"));
        assert!(refused.results[0].needs_acknowledgement);
        assert!(!shortcuts.exists());
        assert!(!Path::new(&request.prefix).exists());

        request.acknowledge_running = true;
        let accepted = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(accepted.results[0].status, TargetStatus::Created);
        assert!(accepted.results[0]
            .warnings
            .iter()
            .any(|w| w.contains("overwrite")));
        assert!(shortcuts.exists());
    }

    fn fake_process(home: &Home, pid: &str, comm: &str, command: &str) {
        let process = home.env.proc_root.join(pid);
        fs::create_dir_all(&process).unwrap();
        fs::write(process.join("comm"), comm).unwrap();
        fs::write(process.join("cmdline"), command).unwrap();
    }

    #[test]
    fn running_faugus_and_heroic_need_acknowledgement_too() {
        let home = home();
        fake_process(
            &home,
            "7",
            "python3\n",
            "/usr/bin/python3\0/usr/bin/faugus-launcher\0",
        );
        fake_process(&home, "8", "heroic\n", "/usr/bin/heroic\0");
        let mut request = request(&home);
        let games = home.env.data_home.join("faugus-launcher/games.json");

        let refused = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(
            statuses(&refused),
            vec![
                TargetStatus::Created,
                TargetStatus::Failed,
                TargetStatus::Failed
            ]
        );
        assert!(!refused.results[0].needs_acknowledgement);
        assert!(refused.results[1].needs_acknowledgement);
        assert!(refused.results[2].needs_acknowledgement);
        assert!(!games.exists());

        request.acknowledge_running = true;
        let accepted = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(accepted.results[1].status, TargetStatus::Created);
        assert!(accepted.results[1].warnings[0].contains("next save"));
        assert!(games.exists());
        assert!(accepted.results[2].warnings[0].contains("overwrite"));

        let unchanged = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&unchanged), vec![TargetStatus::Unchanged; 3]);
        assert!(unchanged
            .results
            .iter()
            .all(|result| result.warnings.is_empty()));
    }

    #[test]
    fn a_different_earlier_prefix_is_not_changed_without_consent() {
        let home = home();
        let mut request = request(&home);
        run_export(&home.env, &home.work, &request).unwrap();
        let original = request.prefix.clone();

        request.prefix = home.dir.path().join("moved").to_str().unwrap().to_owned();
        let refused = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&refused), vec![TargetStatus::Failed; 3]);
        assert!(refused
            .results
            .iter()
            .all(|result| result.needs_prefix_change));
        assert!(!Path::new(&request.prefix).exists());
        let options = prepare_options(&home.env, &home.work).unwrap();
        assert!(options
            .existing
            .iter()
            .all(|existing| existing.prefix.as_deref() == Some(original.as_str())));

        request.allow_prefix_change = true;
        let moved = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(statuses(&moved), vec![TargetStatus::Updated; 3]);
    }

    #[test]
    fn a_failed_commit_reports_files_already_written_and_their_backups() {
        let failure = CommitFailure {
            error: ExportError::Refused("second file failed".to_owned()),
            files: vec![FileChange {
                path: "/x/a".to_owned(),
                action: FileAction::Updated,
                backup: Some("/x/a.bak".to_owned()),
            }],
            rollback_errors: vec!["/x/a: denied".to_owned()],
        };
        let target = ExportTarget {
            install_id: "steam:/x".to_owned(),
            steam_user_ids: Vec::new(),
        };
        let result = failed_result(&target, Some(LauncherKind::Steam), failure);
        assert_eq!(result.status, TargetStatus::Failed);
        assert_eq!(result.files[0].backup.as_deref(), Some("/x/a.bak"));
        let error = result.error.unwrap();
        assert!(error.contains("second file failed") && error.contains("/x/a: denied"));
    }

    #[test]
    fn a_prefix_is_only_created_for_a_launcher_that_can_be_exported() {
        let home = home();
        let mut request = request(&home);
        fs::write(
            home.env.data_home.join("faugus-launcher/games.json"),
            "{corrupt",
        )
        .unwrap();
        request
            .targets
            .retain(|target| target.install_id.starts_with("faugus:"));
        let report = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(report.results[0].status, TargetStatus::Failed);
        assert!(!Path::new(&request.prefix).exists());
    }

    #[test]
    fn prefixes_overlapping_works_are_refused_even_through_symlinks() {
        let mut home = home();
        let good = request(&home);
        let other = home.dir.path().join("library/RJ2");
        fs::create_dir_all(&other).unwrap();
        home.work.other_work_roots = vec![other.clone()];
        #[cfg(unix)]
        std::os::unix::fs::symlink(&home.work.root, home.dir.path().join("link")).unwrap();

        let mut cases = vec![
            home.dir.path().join("library"),
            other.join("pfx"),
            home.work.root.join("not/created/yet"),
        ];
        #[cfg(unix)]
        cases.push(home.dir.path().join("link/pfx"));
        for prefix in cases {
            let mut request = good.clone();
            request.prefix = prefix.to_str().unwrap().to_owned();
            let error = run_export(&home.env, &home.work, &request).unwrap_err();
            assert!(matches!(error, ExportError::Invalid(_)), "{prefix:?}");
        }
    }

    #[test]
    fn a_tool_only_another_steam_install_sees_warns() {
        let home = home();
        let flatpak = home
            .env
            .flatpak_root("com.valvesoftware.Steam")
            .join(".local/share/Steam");
        fs::create_dir_all(flatpak.join("userdata/222")).unwrap();
        let report = run_export(&home.env, &home.work, &request(&home)).unwrap();
        let flatpak_result = report
            .results
            .iter()
            .find(|result| result.install_id.contains("valvesoftware"))
            .unwrap();
        assert!(flatpak_result
            .warnings
            .iter()
            .any(|warning| warning.contains("this Steam install")));
        assert!(report.results[0].warnings.is_empty());
    }

    #[test]
    fn invalid_requests_change_nothing() {
        let home = home();
        let good = request(&home);
        let outside = home.dir.path().join("outside.exe");
        fs::write(&outside, "").unwrap();
        let readme = home.work.root.join("readme.txt");
        fs::write(&readme, "").unwrap();
        type Change = Box<dyn Fn(&mut ExportRequest)>;
        let cases: Vec<(&str, Change)> = vec![
            ("blank name", Box::new(|r| r.display_name = "  ".to_owned())),
            ("control", Box::new(|r| r.display_name = "a\0b".to_owned())),
            ("blank tool", Box::new(|r| r.compat_tool = " ".to_owned())),
            ("no targets", Box::new(|r| r.targets.clear())),
            (
                "relative prefix",
                Box::new(|r| r.prefix = "relative/x".to_owned()),
            ),
            ("dotdot", Box::new(|r| r.prefix = "/a/../b".to_owned())),
            ("exe outside", {
                let outside = outside.to_str().unwrap().to_owned();
                Box::new(move |r| r.executable = outside.clone())
            }),
            ("not exe", {
                let readme = readme.to_str().unwrap().to_owned();
                Box::new(move |r| r.executable = readme.clone())
            }),
            ("prefix in work", {
                let inside = home.work.root.join("pfx").to_str().unwrap().to_owned();
                Box::new(move |r| r.prefix = inside.clone())
            }),
        ];
        for (name, change) in cases {
            let mut request = good.clone();
            change(&mut request);
            assert!(
                run_export(&home.env, &home.work, &request).is_err(),
                "{name}"
            );
        }
        assert!(!Path::new(&good.prefix).exists());
        assert!(!home
            .env
            .data_home
            .join("faugus-launcher/games.json")
            .exists());
    }

    #[test]
    fn unknown_targets_and_users_are_refused() {
        let home = home();
        let mut request = request(&home);
        request.targets[1].install_id = "faugus:/etc".to_owned();
        request.targets[0].steam_user_ids = vec!["999".to_owned()];
        let report = run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(report.results[0].status, TargetStatus::Failed);
        assert_eq!(report.results[1].status, TargetStatus::Failed);
        assert!(!Path::new("/etc/games.json").exists());
        assert_eq!(report.results[2].status, TargetStatus::Created);
    }

    #[test]
    fn prefixes_are_separate_per_work_and_survive_export() {
        let home = home();
        assert_ne!(
            default_prefix(&home.env, "RJ1"),
            default_prefix(&home.env, "RJ2")
        );
        let mut request = request(&home);
        let marker = Path::new(&request.prefix).join("pfx/marker");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "save").unwrap();
        run_export(&home.env, &home.work, &request).unwrap();
        request.allow_prefix_change = true;
        request.prefix = home
            .dir
            .path()
            .join("elsewhere")
            .to_str()
            .unwrap()
            .to_owned();
        run_export(&home.env, &home.work, &request).unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), "save");
    }

    #[test]
    fn work_ids_cannot_escape_launcher_folders() {
        let home = home();
        let mut work = home.work.clone();
        work.work_id = "../../evil".to_owned();
        assert!(prepare_options(&home.env, &work).is_err());
        assert!(run_export(&home.env, &work, &request(&home)).is_err());
    }

    #[test]
    fn a_work_without_executables_still_gets_options() {
        let home = home();
        fs::remove_file(home.work.root.join("Game.exe")).unwrap();
        fs::remove_file(home.work.root.join("unins000.exe")).unwrap();
        let options = prepare_options(&home.env, &home.work).unwrap();
        assert!(options.executables.is_empty());
        assert_eq!(options.suggested_executable, None);
        assert_eq!(options.installs.len(), 3);
    }
}
