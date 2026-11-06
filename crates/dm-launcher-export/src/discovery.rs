//! Locate launcher installs and compatibility tools under an injectable home.
use crate::vdf_text;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const STEAM_FLATPAK: &str = "com.valvesoftware.Steam";
pub(crate) const FAUGUS_FLATPAK: &str = "io.github.Faugus.faugus-launcher";
const HEROIC_FLATPAK: &str = "com.heroicgameslauncher.hgl";
/// Difference between a SteamID64 and the account id used under `userdata`.
const STEAM_ID64_BASE: u64 = 76_561_197_960_265_728;

/// Every location the exporter looks at. Tests point this at a temp directory.
#[derive(Clone, Debug)]
pub struct ExportEnv {
    pub home: PathBuf,
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub extra_compat_tool_dirs: Vec<PathBuf>,
    pub proc_root: PathBuf,
}

impl ExportEnv {
    pub fn for_home(home: PathBuf) -> Self {
        Self {
            data_home: home.join(".local/share"),
            config_home: home.join(".config"),
            data_dirs: vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ],
            extra_compat_tool_dirs: Vec::new(),
            proc_root: PathBuf::from("/proc"),
            home,
        }
    }

    pub fn from_process() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|home| !home.is_empty())?);
        let mut env = Self::for_home(home);
        if let Some(path) = absolute_env_path("XDG_DATA_HOME") {
            env.data_home = path;
        }
        if let Some(path) = absolute_env_path("XDG_CONFIG_HOME") {
            env.config_home = path;
        }
        if let Some(paths) = std::env::var_os("XDG_DATA_DIRS") {
            let paths: Vec<PathBuf> = std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .collect();
            if !paths.is_empty() {
                env.data_dirs = paths;
            }
        }
        if let Some(paths) = std::env::var_os("STEAM_EXTRA_COMPAT_TOOLS_PATHS") {
            env.extra_compat_tool_dirs = std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .collect();
        }
        Some(env)
    }

    pub(crate) fn flatpak_root(&self, id: &str) -> PathBuf {
        self.home.join(".var/app").join(id)
    }

    /// Faugus resolves runner names against these two folders.
    pub(crate) fn faugus_runner_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.data_home.join("Steam/compatibilitytools.d"),
            self.flatpak_root(STEAM_FLATPAK)
                .join(".local/share/Steam/compatibilitytools.d"),
        ]
    }
}

fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LauncherKind {
    Steam,
    Faugus,
    Heroic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallFlavor {
    Native,
    Flatpak,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteamUser {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherInstall {
    /// Opaque handle the export request refers back to.
    pub id: String,
    pub launcher: LauncherKind,
    pub flavor: InstallFlavor,
    pub path: String,
    pub steam_users: Vec<SteamUser>,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatTool {
    /// Steam's internal name, used in `CompatToolMapping`.
    pub id: String,
    pub display_name: String,
    pub path: Option<String>,
}

pub fn discover_installs(env: &ExportEnv) -> Vec<LauncherInstall> {
    let running = running_launchers(&env.proc_root);
    let mut installs = Vec::new();
    for (root, flavor) in steam_roots(env) {
        installs.push(LauncherInstall {
            id: format!("steam:{}", root.display()),
            launcher: LauncherKind::Steam,
            flavor,
            path: root.to_string_lossy().into_owned(),
            steam_users: steam_users(&root),
            running: running.steam,
        });
    }
    let faugus = [
        (
            env.data_home.join("faugus-launcher"),
            env.config_home.join("faugus-launcher"),
            InstallFlavor::Native,
        ),
        (
            env.flatpak_root(FAUGUS_FLATPAK)
                .join("data/faugus-launcher"),
            env.flatpak_root(FAUGUS_FLATPAK)
                .join("config/faugus-launcher"),
            InstallFlavor::Flatpak,
        ),
    ];
    for (data, config, flavor) in faugus {
        if data.is_dir() || config.is_dir() {
            installs.push(LauncherInstall {
                id: format!("faugus:{}", data.display()),
                launcher: LauncherKind::Faugus,
                flavor,
                path: data.to_string_lossy().into_owned(),
                steam_users: Vec::new(),
                running: running.faugus,
            });
        }
    }
    let heroic = [
        (env.config_home.join("heroic"), InstallFlavor::Native),
        (
            env.flatpak_root(HEROIC_FLATPAK).join("config/heroic"),
            InstallFlavor::Flatpak,
        ),
    ];
    for (config, flavor) in heroic {
        if config.is_dir() {
            installs.push(LauncherInstall {
                id: format!("heroic:{}", config.display()),
                launcher: LauncherKind::Heroic,
                flavor,
                path: config.to_string_lossy().into_owned(),
                steam_users: Vec::new(),
                running: running.heroic,
            });
        }
    }
    installs
}

fn steam_roots(env: &ExportEnv) -> Vec<(PathBuf, InstallFlavor)> {
    let flatpak_root = env.flatpak_root(STEAM_FLATPAK);
    let candidates = [
        (env.data_home.join("Steam"), InstallFlavor::Native),
        (env.home.join(".local/share/Steam"), InstallFlavor::Native),
        (env.home.join(".steam/steam"), InstallFlavor::Native),
        (env.home.join(".steam/root"), InstallFlavor::Native),
        (
            flatpak_root.join(".local/share/Steam"),
            InstallFlavor::Flatpak,
        ),
        (flatpak_root.join(".steam/steam"), InstallFlavor::Flatpak),
    ];
    let mut roots: Vec<(PathBuf, InstallFlavor)> = Vec::new();
    for (candidate, flavor) in candidates {
        if !candidate.join("userdata").is_dir() {
            continue;
        }
        let root = candidate.canonicalize().unwrap_or(candidate);
        if !roots.iter().any(|(known, _)| *known == root) {
            roots.push((root, flavor));
        }
    }
    roots
}

fn steam_users(root: &Path) -> Vec<SteamUser> {
    let names = steam_persona_names(root);
    let mut users: Vec<SteamUser> = fs::read_dir(root.join("userdata"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != "0" && !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        .map(|id| SteamUser {
            name: names
                .iter()
                .find(|(account, _)| *account == id)
                .map(|(_, name)| name.clone()),
            id,
        })
        .collect();
    users.sort_by(|a, b| a.id.cmp(&b.id));
    users
}

fn steam_persona_names(root: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(root.join("config/loginusers.vdf")) else {
        return Vec::new();
    };
    let Ok(nodes) = vdf_text::parse(&text) else {
        return Vec::new();
    };
    let Some(users) = vdf_text::find(&nodes, "users") else {
        return Vec::new();
    };
    users
        .children()
        .iter()
        .filter_map(|user| {
            let account = user.key.parse::<u64>().ok()?.checked_sub(STEAM_ID64_BASE)?;
            let name = vdf_text::find(user.children(), "PersonaName")?.value()?;
            Some((account.to_string(), name.to_owned()))
        })
        .collect()
}

#[derive(Default)]
struct Running {
    steam: bool,
    faugus: bool,
    heroic: bool,
}

/// Best effort: an unreadable `/proc` reports nothing as running.
fn running_launchers(proc_root: &Path) -> Running {
    let mut running = Running::default();
    for entry in fs::read_dir(proc_root).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if !name
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        let comm = comm.trim();
        let command = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let command = String::from_utf8_lossy(&command);
        let argument_is = |wanted: &[&str]| {
            command.split('\0').take(3).any(|argument| {
                let base = argument.rsplit('/').next().unwrap_or(argument);
                wanted.contains(&base)
            })
        };
        running.steam |= matches!(comm, "steam" | "steamwebhelper");
        running.heroic |= comm == "heroic" || argument_is(&["heroic"]);
        running.faugus |=
            comm == "faugus-launcher" || argument_is(&["faugus-launcher", "faugus.launcher"]);
    }
    running
}

pub fn discover_compat_tools(env: &ExportEnv) -> Vec<CompatTool> {
    let roots = steam_roots(env);
    let mut directories: Vec<PathBuf> = roots
        .iter()
        .map(|(root, _)| root.join("compatibilitytools.d"))
        .collect();
    directories.extend(env.faugus_runner_dirs());
    directories.extend(env.extra_compat_tool_dirs.iter().cloned());
    directories.extend(
        env.data_dirs
            .iter()
            .map(|dir| dir.join("steam/compatibilitytools.d")),
    );

    let mut tools: Vec<CompatTool> = Vec::new();
    let mut add = |tool: CompatTool| {
        if !tools.iter().any(|known| known.id == tool.id) {
            tools.push(tool);
        }
    };
    for directory in directories {
        let mut entries: Vec<PathBuf> = fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        entries.sort();
        for entry in entries {
            read_tool_directory(&entry).into_iter().for_each(&mut add);
        }
    }
    for (root, _) in &roots {
        for library in steam_libraries(root) {
            for tool in bundled_proton_tools(&library) {
                add(tool);
            }
        }
    }
    tools
}

/// Tools declared by `compatibilitytool.vdf`, or the folder name for a folder
/// that only contains a `proton` script.
pub(crate) fn read_tool_directory(directory: &Path) -> Vec<CompatTool> {
    let mut tools = Vec::new();
    if let Some(nodes) = fs::read_to_string(directory.join("compatibilitytool.vdf"))
        .ok()
        .and_then(|text| vdf_text::parse(&text).ok())
    {
        if let Some(entries) = vdf_text::find_path(&nodes, &["compatibilitytools", "compat_tools"])
        {
            for entry in entries.children() {
                let install_path = vdf_text::find(entry.children(), "install_path")
                    .and_then(vdf_text::Node::value)
                    .unwrap_or(".");
                let path = match install_path {
                    "." => directory.to_path_buf(),
                    other => directory.join(other),
                };
                tools.push(CompatTool {
                    id: entry.key.clone(),
                    display_name: vdf_text::find(entry.children(), "display_name")
                        .and_then(vdf_text::Node::value)
                        .unwrap_or(&entry.key)
                        .to_owned(),
                    path: path.to_str().map(str::to_owned),
                });
            }
        }
    }
    if tools.is_empty() && directory.join("proton").is_file() {
        if let (Some(name), Some(path)) = (
            directory.file_name().and_then(|name| name.to_str()),
            directory.to_str(),
        ) {
            tools.push(CompatTool {
                id: name.to_owned(),
                display_name: name.to_owned(),
                path: Some(path.to_owned()),
            });
        }
    }
    tools
}

fn steam_libraries(root: &Path) -> Vec<PathBuf> {
    let mut libraries = vec![root.to_path_buf()];
    if let Some(nodes) = fs::read_to_string(root.join("config/libraryfolders.vdf"))
        .ok()
        .and_then(|text| vdf_text::parse(&text).ok())
    {
        if let Some(folders) = vdf_text::find(&nodes, "libraryfolders") {
            for folder in folders.children() {
                if let Some(path) =
                    vdf_text::find(folder.children(), "path").and_then(vdf_text::Node::value)
                {
                    let path = PathBuf::from(path);
                    if !libraries.contains(&path) {
                        libraries.push(path);
                    }
                }
            }
        }
    }
    libraries
}

fn bundled_proton_tools(library: &Path) -> Vec<CompatTool> {
    let mut tools: Vec<CompatTool> = fs::read_dir(library.join("steamapps/common"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("proton").is_file())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = entry.path().to_str()?.to_owned();
            Some(CompatTool {
                id: bundled_proton_id(&name)?,
                display_name: name,
                path: Some(path),
            })
        })
        .collect();
    tools.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    tools
}

/// Steam's internal name for a bundled Proton folder. Valve publishes no list;
/// this follows the names other Steam tools have recorded from `config.vdf`.
pub(crate) fn bundled_proton_id(folder: &str) -> Option<String> {
    let rest = folder.strip_prefix("Proton")?.trim_start();
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let token = rest.split_whitespace().next()?.to_lowercase();
    if token.chars().next()?.is_ascii_digit() {
        let token = token.strip_suffix(".0").unwrap_or(&token);
        return Some(format!("proton_{}", token.replace('.', "")));
    }
    matches!(token.as_str(), "experimental" | "hotfix").then(|| format!("proton_{token}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTool {
    pub id: String,
    pub path: Option<PathBuf>,
    /// Found in a folder Steam reads, so a Steam mapping to `id` can work.
    pub discovered: bool,
}

/// Interpret the compatibility tool field: a discovered id, display name or
/// folder name, an absolute tool folder, or free text kept as a Steam tool name.
pub(crate) fn resolve_tool(tools: &[CompatTool], input: &str) -> ResolvedTool {
    let input = input.trim();
    let from_tool = |tool: &CompatTool| ResolvedTool {
        id: tool.id.clone(),
        path: tool.path.as_ref().map(PathBuf::from),
        discovered: true,
    };
    if let Some(tool) = tools.iter().find(|tool| tool.id == input) {
        return from_tool(tool);
    }
    if let Some(tool) = tools.iter().find(|tool| {
        tool.display_name == input
            || tool.path.as_deref() == Some(input)
            || tool
                .path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .is_some_and(|name| name == input)
    }) {
        return from_tool(tool);
    }
    let path = Path::new(input);
    if path.is_absolute() && path.is_dir() {
        let id = read_tool_directory(path)
            .into_iter()
            .next()
            .map(|tool| tool.id)
            .or_else(|| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| input.to_owned());
        return ResolvedTool {
            id,
            path: Some(path.to_path_buf()),
            discovered: false,
        };
    }
    ResolvedTool {
        id: input.to_owned(),
        path: None,
        discovered: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &Path) -> ExportEnv {
        let mut env = ExportEnv::for_home(home.to_path_buf());
        env.data_dirs = vec![home.join("system-share")];
        env.proc_root = home.join("proc");
        env
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn tool_manifest(id: &str, display: &str) -> String {
        format!(
            "// comment\n\"compatibilitytools\"\n{{\n  \"compat_tools\"\n  {{\n    \"{id}\"\n    {{\n      \"install_path\" \".\"\n      \"display_name\" \"{display}\"\n      \"from_oslist\" \"windows\"\n      \"to_oslist\" \"linux\"\n    }}\n  }}\n}}\n"
        )
    }

    #[test]
    fn nothing_is_detected_in_an_empty_home() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_installs(&env(dir.path())).is_empty());
        assert!(discover_compat_tools(&env(dir.path())).is_empty());
    }

    #[test]
    fn detects_native_and_flatpak_installs_and_merges_steam_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let native = home.join(".local/share/Steam");
        fs::create_dir_all(native.join("userdata/1234/config")).unwrap();
        fs::create_dir_all(native.join("userdata/0")).unwrap();
        fs::create_dir_all(native.join("userdata/anonymous")).unwrap();
        fs::create_dir_all(home.join(".steam")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&native, home.join(".steam/steam")).unwrap();
        write(
            &native.join("config/loginusers.vdf"),
            "\"users\"\n{\n\t\"76561197960266962\"\n\t{\n\t\t\"PersonaName\"\t\"Miau\"\n\t}\n}\n",
        );
        fs::create_dir_all(
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/userdata/99"),
        )
        .unwrap();
        fs::create_dir_all(home.join(".local/share/faugus-launcher")).unwrap();
        fs::create_dir_all(home.join(".config/heroic")).unwrap();
        fs::create_dir_all(home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"))
            .unwrap();
        fs::create_dir_all(
            home.join(".var/app/io.github.Faugus.faugus-launcher/config/faugus-launcher"),
        )
        .unwrap();

        let installs = discover_installs(&env(home));
        let steam: Vec<_> = installs
            .iter()
            .filter(|install| install.launcher == LauncherKind::Steam)
            .collect();
        assert_eq!(steam.len(), 2);
        assert_eq!(steam[0].flavor, InstallFlavor::Native);
        assert_eq!(
            steam[0].steam_users,
            vec![SteamUser {
                id: "1234".to_owned(),
                name: Some("Miau".to_owned())
            }]
        );
        assert_eq!(steam[1].flavor, InstallFlavor::Flatpak);
        let count = |kind| installs.iter().filter(|i| i.launcher == kind).count();
        assert_eq!(count(LauncherKind::Faugus), 2);
        assert_eq!(count(LauncherKind::Heroic), 2);
        let flatpak_faugus = installs
            .iter()
            .find(|i| i.launcher == LauncherKind::Faugus && i.flavor == InstallFlavor::Flatpak)
            .unwrap();
        assert!(flatpak_faugus
            .path
            .ends_with(".var/app/io.github.Faugus.faugus-launcher/data/faugus-launcher"));
    }

    #[test]
    fn running_launchers_are_read_from_proc() {
        let dir = tempfile::tempdir().unwrap();
        let proc_root = dir.path().join("proc");
        write(&proc_root.join("100/comm"), "steamwebhelper\n");
        write(&proc_root.join("100/cmdline"), "x");
        write(&proc_root.join("200/comm"), "python3\n");
        write(
            &proc_root.join("200/cmdline"),
            "/usr/bin/python3\0/usr/bin/faugus-launcher\0",
        );
        write(&proc_root.join("notapid/comm"), "heroic\n");
        let running = running_launchers(&proc_root);
        assert!(running.steam);
        assert!(running.faugus);
        assert!(!running.heroic);
    }

    #[test]
    fn lists_custom_system_and_bundled_tools_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let steam = home.join(".local/share/Steam");
        fs::create_dir_all(steam.join("userdata/1")).unwrap();
        write(
            &steam.join("compatibilitytools.d/proton-dl/compatibilitytool.vdf"),
            &tool_manifest("Proton-DL", "Proton-DL 11"),
        );
        write(&steam.join("compatibilitytools.d/plain-tool/proton"), "");
        write(&steam.join("compatibilitytools.d/not-a-tool/readme"), "");
        write(
            &home.join("system-share/steam/compatibilitytools.d/cachy/compatibilitytool.vdf"),
            &tool_manifest("proton-cachyos", "CachyOS"),
        );
        write(
            &home.join("system-share/steam/compatibilitytools.d/dup/compatibilitytool.vdf"),
            &tool_manifest("Proton-DL", "duplicate"),
        );
        write(
            &steam.join("steamapps/common/Proton - Experimental/proton"),
            "",
        );
        write(
            &steam.join("steamapps/common/Proton EasyAntiCheat Runtime/readme"),
            "",
        );
        let second = home.join("second-library");
        write(
            &second.join("steamapps/common/Proton 9.0 (Beta)/proton"),
            "",
        );
        write(
            &steam.join("config/libraryfolders.vdf"),
            &format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\"{}\"\n\t}}\n}}\n",
                second.display()
            ),
        );

        let tools = discover_compat_tools(&env(home));
        let ids: Vec<&str> = tools.iter().map(|tool| tool.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "plain-tool",
                "Proton-DL",
                "proton-cachyos",
                "proton_experimental",
                "proton_9"
            ]
        );
        let proton_dl = tools.iter().find(|tool| tool.id == "Proton-DL").unwrap();
        assert_eq!(proton_dl.display_name, "Proton-DL 11");
        assert!(proton_dl
            .path
            .as_deref()
            .unwrap()
            .ends_with("compatibilitytools.d/proton-dl"));
    }

    #[test]
    fn bundled_ids_follow_steams_naming() {
        for (folder, id) in [
            ("Proton - Experimental", Some("proton_experimental")),
            ("Proton Hotfix", Some("proton_hotfix")),
            ("Proton 9.0 (Beta)", Some("proton_9")),
            ("Proton 10.0", Some("proton_10")),
            ("Proton 6.3", Some("proton_63")),
            ("Proton 5.13", Some("proton_513")),
            ("Proton BattlEye Runtime", None),
            ("Steamworks Shared", None),
        ] {
            assert_eq!(bundled_proton_id(folder).as_deref(), id, "{folder}");
        }
    }

    #[test]
    fn free_text_resolves_to_known_tools_paths_or_a_plain_name() {
        let dir = tempfile::tempdir().unwrap();
        let tool_dir = dir.path().join("custom/My-Proton");
        write(&tool_dir.join("proton"), "");
        let tools = vec![CompatTool {
            id: "Proton-DL".to_owned(),
            display_name: "Proton-DL 11".to_owned(),
            path: Some("/x/compatibilitytools.d/proton-dl".to_owned()),
        }];
        for input in [
            "Proton-DL",
            " Proton-DL 11 ",
            "proton-dl",
            "/x/compatibilitytools.d/proton-dl",
        ] {
            assert_eq!(resolve_tool(&tools, input).id, "Proton-DL", "{input}");
        }
        let by_path = resolve_tool(&tools, tool_dir.to_str().unwrap());
        assert_eq!(by_path.id, "My-Proton");
        assert_eq!(by_path.path.as_deref(), Some(tool_dir.as_path()));
        assert_eq!(
            resolve_tool(&tools, "unknown_tool"),
            ResolvedTool {
                id: "unknown_tool".to_owned(),
                path: None,
                discovered: false
            }
        );
    }
}
