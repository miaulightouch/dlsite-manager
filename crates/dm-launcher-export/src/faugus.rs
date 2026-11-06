//! Faugus Launcher `games.json`.
use crate::discovery::{ExportEnv, FAUGUS_FLATPAK};
use crate::fsio::{read_optional, PendingWrite};
use crate::json_file::{parse_document, serialize_document};
use crate::{ExistingExport, ExportError, ExportSpec, Plan};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const GAMES_FILE: &str = "games.json";
const JSON_INDENT: &[u8] = b"    ";

pub(crate) fn game_id(work_id: &str) -> String {
    format!("dlsite-{}", work_id.to_lowercase())
}

/// Faugus moves this older file into its data folder only when the data folder
/// has none, so creating one first would strand the user's existing games.
fn legacy_games_file(env: &ExportEnv, data_dir: &Path) -> PathBuf {
    let flatpak_root = env.flatpak_root(FAUGUS_FLATPAK);
    let config_home = if data_dir.starts_with(&flatpak_root) {
        flatpak_root.join("config")
    } else {
        env.config_home.clone()
    };
    config_home.join("faugus-launcher").join(GAMES_FILE)
}

fn runner_value(env: &ExportEnv, spec: &ExportSpec) -> (String, Option<String>) {
    let Some(path) = &spec.tool.path else {
        return (
            spec.tool.id.clone(),
            Some(format!(
                "\"{}\" was not found among the installed tools; Faugus reports an error at launch if it cannot find it",
                spec.tool.id
            )),
        );
    };
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let parent = path.parent().map(canonical);
    let known = env
        .faugus_runner_dirs()
        .iter()
        .any(|dir| Some(canonical(dir)) == parent);
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if known => (name.to_owned(), None),
        _ => (path.to_string_lossy().into_owned(), None),
    }
}

fn parse_games(path: &Path, bytes: Option<&[u8]>) -> Result<Vec<Value>, ExportError> {
    match parse_document(path, bytes)? {
        None => Ok(Vec::new()),
        Some(Value::Array(games)) => Ok(games),
        Some(_) => Err(ExportError::Corrupt {
            path: path.to_path_buf(),
            message: "expected a list of games".to_owned(),
        }),
    }
}

fn is_game(entry: &Value, id: &str) -> bool {
    entry.get("gameid").and_then(Value::as_str) == Some(id)
}

/// Field defaults as Faugus itself fills them when it loads an entry.
fn new_game(id: &str) -> Value {
    json!({
        "gameid": id,
        "title": "",
        "path": "",
        "prefix": "",
        "launch_arguments": "",
        "game_arguments": "",
        "mangohud": "",
        "gamemode": "",
        "sdl_enabled": "",
        "runner": "",
        "addapp_enabled": "",
        "addapp": "",
        "addapp_bat": "",
        "addapp_delay": "",
        "addapp_first": "",
        "cover": "",
        "lossless_enabled": "",
        "lossless_multiplier": "",
        "lossless_flow": "",
        "lossless_performance": "",
        "lossless_hdr": "",
        "lossless_present": "",
        "playtime": 0,
        "hidden": false,
        "no_sleep": false,
        "category": false,
        "icon": "",
        "steamgriddb_id": "",
        "pre_launch": "",
        "post_launch": "",
        "steam_user": "",
        "disable_umu": "",
        "runtime": "",
        "last_played": "",
        "discord_rpc": ""
    })
}

pub(crate) fn plan(
    env: &ExportEnv,
    data_dir: &Path,
    spec: &ExportSpec,
) -> Result<Plan, ExportError> {
    let path = data_dir.join(GAMES_FILE);
    let original = read_optional(&path)?;
    if original.is_none() && legacy_games_file(env, data_dir).is_file() {
        return Err(ExportError::Refused(
            "Faugus still has its games in the older config folder; open Faugus once so it can move them, then export again"
                .to_owned(),
        ));
    }

    let id = game_id(&spec.work_id);
    let mut games = parse_games(&path, original.as_deref())?;
    let (runner, runner_warning) = runner_value(env, spec);
    let mut warnings: Vec<String> = runner_warning.into_iter().collect();
    let title_taken = games.iter().any(|game| {
        !is_game(game, &id)
            && game
                .get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| title.to_lowercase() == spec.display_name.to_lowercase())
    });
    if title_taken {
        warnings.push(format!(
            "Faugus already has another game named \"{}\"",
            spec.display_name
        ));
    }

    let position = games.iter().position(|game| is_game(game, &id));
    let created = position.is_none();
    let index = position.unwrap_or_else(|| {
        games.push(new_game(&id));
        games.len() - 1
    });
    let Some(game) = games[index].as_object_mut() else {
        unreachable!("the entry has a string gameid or was just created");
    };
    game.insert("title".to_owned(), json!(spec.display_name));
    game.insert("path".to_owned(), json!(spec.executable));
    game.insert("prefix".to_owned(), json!(spec.prefix));
    game.insert("runner".to_owned(), json!(runner));

    let document = Value::Array(games);
    let contents = serialize_document(original.as_deref(), &document, JSON_INDENT);
    Ok(Plan {
        created,
        writes: vec![PendingWrite {
            path,
            original,
            contents,
        }],
        warnings,
    })
}

pub(crate) fn read_existing(
    data_dir: &Path,
    work_id: &str,
) -> Result<Option<ExistingExport>, ExportError> {
    let path = data_dir.join(GAMES_FILE);
    let original = read_optional(&path)?;
    let games = parse_games(&path, original.as_deref())?;
    let id = game_id(work_id);
    let text = |game: &Value, key: &str| {
        game.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(games
        .iter()
        .find(|game| is_game(game, &id))
        .map(|game| ExistingExport {
            install_id: String::new(),
            steam_user_id: None,
            display_name: text(game, "title"),
            executable: text(game, "path"),
            prefix: text(game, "prefix"),
            compat_tool: text(game, "runner"),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ResolvedTool;
    use crate::fsio::commit_all;
    use std::fs;

    const FIXTURE: &str = r#"[
    {
        "gameid": "other-game",
        "title": "Other Game",
        "path": "/opt/other.exe",
        "prefix": "/home/u/Faugus/other",
        "runner": "GE-Proton10-1",
        "playtime": 1234,
        "future_field": {"keep": [1, 2.5, "日本語"]}
    },
    "not-an-object"
]"#;

    fn spec(home: &Path) -> ExportSpec {
        ExportSpec {
            work_id: "RJ1".to_owned(),
            display_name: "月光".to_owned(),
            executable: "/games/RJ1/Game.exe".to_owned(),
            prefix: "/prefixes/RJ1".to_owned(),
            tool: ResolvedTool {
                id: "Proton-DL".to_owned(),
                path: Some(home.join(".local/share/Steam/compatibilitytools.d/proton-dl")),
            },
            cover_url: None,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        env: ExportEnv,
        data: PathBuf,
    }

    fn fixture(games: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let env = ExportEnv::for_home(dir.path().to_path_buf());
        let data = env.data_home.join("faugus-launcher");
        fs::create_dir_all(&data).unwrap();
        if let Some(games) = games {
            fs::write(data.join(GAMES_FILE), games).unwrap();
        }
        Fixture {
            env,
            data,
            _dir: dir,
        }
    }

    fn run(fixture: &Fixture, spec: &ExportSpec) -> Vec<Value> {
        let plan = plan(&fixture.env, &fixture.data, spec).unwrap();
        commit_all(plan.writes).map_err(|f| f.error).unwrap();
        serde_json::from_slice(&fs::read(fixture.data.join(GAMES_FILE)).unwrap()).unwrap()
    }

    #[test]
    fn adds_an_entry_with_every_field_faugus_loads() {
        let fixture = fixture(Some(FIXTURE));
        let games = run(&fixture, &spec(&fixture.env.home));
        assert_eq!(games.len(), 3);
        let game = &games[2];
        assert_eq!(game["gameid"], "dlsite-rj1");
        assert_eq!(game["title"], "月光");
        assert_eq!(game["path"], "/games/RJ1/Game.exe");
        assert_eq!(game["prefix"], "/prefixes/RJ1");
        assert_eq!(game["runner"], "proton-dl");
        assert_eq!(game["playtime"], 0);
        assert_eq!(game["hidden"], false);
        // Field list from Faugus's GAME_FIELDS (faugus/utils.py).
        assert_eq!(game.as_object().unwrap().len(), 35);
    }

    #[test]
    fn preserves_other_entries_and_unknown_fields() {
        let fixture = fixture(Some(FIXTURE));
        let games = run(&fixture, &spec(&fixture.env.home));
        let before: Vec<Value> = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(games[..2], before[..]);
        let text = fs::read_to_string(fixture.data.join(GAMES_FILE)).unwrap();
        assert!(text.contains("日本語"));
        assert!(text.contains("\n    {\n        \"gameid\": \"other-game\""));
        assert!(text.find("gameid").unwrap() < text.find("playtime").unwrap());
    }

    #[test]
    fn reexport_is_idempotent_and_updates_in_place() {
        let fixture = fixture(Some(FIXTURE));
        let spec_first = spec(&fixture.env.home);
        run(&fixture, &spec_first);
        let second = plan(&fixture.env, &fixture.data, &spec_first).unwrap();
        assert!(!second.created);
        assert_eq!(
            second.writes[0].original.as_deref(),
            Some(second.writes[0].contents.as_slice())
        );

        let path = fixture.data.join(GAMES_FILE);
        let edited = fs::read_to_string(&path)
            .unwrap()
            .replace("\"playtime\": 0", "\"playtime\": 77");
        fs::write(&path, edited).unwrap();
        let mut renamed = spec_first;
        renamed.display_name = "Renamed".to_owned();
        let games = run(&fixture, &renamed);
        assert_eq!(games.len(), 3);
        assert_eq!(games[2]["title"], "Renamed");
        assert_eq!(games[2]["playtime"], 77);
    }

    #[test]
    fn reads_back_a_previous_export() {
        let fixture = fixture(None);
        run(&fixture, &spec(&fixture.env.home));
        let found = read_existing(&fixture.data, "RJ1").unwrap().unwrap();
        assert_eq!(found.prefix.as_deref(), Some("/prefixes/RJ1"));
        assert_eq!(found.compat_tool.as_deref(), Some("proton-dl"));
        assert!(read_existing(&fixture.data, "RJ2").unwrap().is_none());
    }

    #[test]
    fn tools_outside_faugus_folders_use_an_absolute_runner() {
        let fixture = fixture(None);
        let mut spec = spec(&fixture.env.home);
        spec.tool.path = Some(PathBuf::from("/usr/share/steam/compatibilitytools.d/x"));
        let games = run(&fixture, &spec);
        assert_eq!(
            games[0]["runner"],
            "/usr/share/steam/compatibilitytools.d/x"
        );
    }

    #[test]
    fn bare_runner_names_follow_faugus_not_a_custom_xdg_data_home() {
        let mut fixture = fixture(None);
        let home = fixture.env.home.clone();
        fixture.env.data_home = home.join("custom-data");
        let data = fixture.env.data_home.join("faugus-launcher");
        fs::create_dir_all(&data).unwrap();
        fixture.data = data;
        let mut spec = spec(&home);
        spec.tool.path = Some(home.join("custom-data/Steam/compatibilitytools.d/x"));
        let games = run(&fixture, &spec);
        assert_eq!(
            games[0]["runner"],
            home.join("custom-data/Steam/compatibilitytools.d/x")
                .to_str()
                .unwrap()
        );
    }

    #[test]
    fn number_values_survive_a_rewrite() {
        let fixture = fixture(Some(
            r#"[{"gameid":"x","a":0.30000000000000004,"b":18446744073709551615,"c":-9007199254740993,"d":1.7976931348623157e308}]"#,
        ));
        let games = run(&fixture, &spec(&fixture.env.home));
        let text = fs::read_to_string(fixture.data.join(GAMES_FILE)).unwrap();
        assert!(text.contains("0.30000000000000004"), "{text}");
        assert!(text.contains("18446744073709551615"), "{text}");
        assert!(text.contains("-9007199254740993"), "{text}");
        assert_eq!(games[0]["d"].as_f64().unwrap(), f64::MAX);
    }

    #[test]
    fn corrupt_files_fail_without_changes() {
        for bad in ["{not json", "{\"an\": \"object\"}", "[1,"] {
            let fixture = fixture(Some(bad));
            let error = plan(&fixture.env, &fixture.data, &spec(&fixture.env.home)).unwrap_err();
            assert!(matches!(error, ExportError::Corrupt { .. }), "{bad}");
            assert_eq!(
                fs::read_to_string(fixture.data.join(GAMES_FILE)).unwrap(),
                bad
            );
        }
    }

    #[test]
    fn an_unmigrated_legacy_file_is_refused() {
        let fixture = fixture(None);
        let legacy = fixture.env.config_home.join("faugus-launcher");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(GAMES_FILE), "[]").unwrap();
        let error = plan(&fixture.env, &fixture.data, &spec(&fixture.env.home)).unwrap_err();
        assert!(matches!(error, ExportError::Refused(_)));
        assert!(!fixture.data.join(GAMES_FILE).exists());
    }

    #[test]
    fn a_title_clash_warns() {
        let fixture = fixture(Some(FIXTURE));
        let mut spec = spec(&fixture.env.home);
        spec.display_name = "OTHER game".to_owned();
        let plan = plan(&fixture.env, &fixture.data, &spec).unwrap();
        assert_eq!(plan.warnings.len(), 1);
    }
}
