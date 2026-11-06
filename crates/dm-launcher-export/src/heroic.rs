//! Heroic sideload library plus per-game settings.
use crate::fsio::{read_optional, PendingWrite};
use crate::json_file::{parse_document, serialize_document};
use crate::{ExistingExport, ExportError, ExportSpec, Plan};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const LIBRARY_INDENT: &[u8] = b"\t";
const CONFIG_INDENT: &[u8] = b"  ";

pub(crate) fn app_name(work_id: &str) -> String {
    format!("dlsite-{}", work_id.to_lowercase())
}

fn library_path(config_dir: &Path) -> PathBuf {
    config_dir.join("sideload_apps/library.json")
}

fn game_config_path(config_dir: &Path, app: &str) -> PathBuf {
    config_dir.join("GamesConfig").join(format!("{app}.json"))
}

fn corrupt(path: &Path, message: &str) -> ExportError {
    ExportError::Corrupt {
        path: path.to_path_buf(),
        message: message.to_owned(),
    }
}

fn parse_object(path: &Path, bytes: Option<&[u8]>) -> Result<Map<String, Value>, ExportError> {
    match parse_document(path, bytes)? {
        None => Ok(Map::new()),
        Some(Value::Object(object)) => Ok(object),
        Some(_) => Err(corrupt(path, "expected a JSON object")),
    }
}

fn library_games<'a>(
    path: &Path,
    library: &'a mut Map<String, Value>,
) -> Result<&'a mut Vec<Value>, ExportError> {
    library
        .entry("games")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| corrupt(path, "\"games\" is not a list"))
}

fn is_app(entry: &Value, app: &str) -> bool {
    entry.get("app_name").and_then(Value::as_str) == Some(app)
}

fn update_library_entry(entry: &mut Map<String, Value>, app: &str, spec: &ExportSpec) {
    let folder = Path::new(&spec.executable)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned());
    entry.insert("runner".to_owned(), json!("sideload"));
    entry.insert("app_name".to_owned(), json!(app));
    entry.insert("title".to_owned(), json!(spec.display_name));
    let install = entry.entry("install").or_insert_with(|| json!({}));
    if !install.is_object() {
        *install = json!({});
    }
    if let Some(install) = install.as_object_mut() {
        install.insert("executable".to_owned(), json!(spec.executable));
        install.insert("platform".to_owned(), json!("Windows"));
        install.entry("is_dlc").or_insert(json!(false));
    }
    entry.insert("folder_name".to_owned(), json!(folder));
    entry.insert("is_installed".to_owned(), json!(true));
    entry.entry("canRunOffline").or_insert(json!(true));
    for key in ["art_cover", "art_square"] {
        let empty = entry
            .get(key)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty);
        if empty {
            entry.insert(
                key.to_owned(),
                json!(spec.cover_url.as_deref().unwrap_or_default()),
            );
        }
    }
}

fn update_game_config(
    path: &Path,
    config: &mut Map<String, Value>,
    app: &str,
    bin: &str,
    name: &str,
    prefix: &str,
) -> Result<(), ExportError> {
    let settings = config
        .entry(app)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| corrupt(path, "the game's settings are not an object"))?;
    settings.insert("winePrefix".to_owned(), json!(prefix));
    let same_tool = settings
        .get("wineVersion")
        .and_then(|version| version.get("bin"))
        .and_then(Value::as_str)
        == Some(bin);
    if !same_tool {
        settings.insert(
            "wineVersion".to_owned(),
            json!({ "bin": bin, "name": name, "type": "proton" }),
        );
    }
    config.entry("version").or_insert(json!("v0"));
    config.entry("explicit").or_insert(json!(true));
    Ok(())
}

pub(crate) fn plan(config_dir: &Path, spec: &ExportSpec) -> Result<Plan, ExportError> {
    let Some(tool_dir) = spec.tool.path.as_ref() else {
        return Err(ExportError::Invalid(format!(
            "Heroic needs an installed Proton folder; \"{}\" was not found",
            spec.tool.id
        )));
    };
    let bin = tool_dir.join("proton");
    if !bin.is_file() {
        return Err(ExportError::Invalid(format!(
            "Heroic needs a Proton folder with a proton script; {} has none",
            tool_dir.display()
        )));
    }
    let bin_text = bin.to_string_lossy();
    let tool_name = tool_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.tool.id.clone());
    let app = app_name(&spec.work_id);

    let config_path = game_config_path(config_dir, &app);
    let config_original = read_optional(&config_path)?;
    let mut config = parse_object(&config_path, config_original.as_deref())?;
    update_game_config(
        &config_path,
        &mut config,
        &app,
        &bin_text,
        &tool_name,
        &spec.prefix,
    )?;
    let config_contents = serialize_document(
        config_original.as_deref(),
        &Value::Object(config),
        CONFIG_INDENT,
    );

    let library = library_path(config_dir);
    let library_original = read_optional(&library)?;
    let mut document = parse_object(&library, library_original.as_deref())?;
    let games = library_games(&library, &mut document)?;
    let position = games.iter().position(|game| is_app(game, &app));
    let created = position.is_none();
    let index = position.unwrap_or_else(|| {
        games.push(json!({}));
        games.len() - 1
    });
    let Some(entry) = games[index].as_object_mut() else {
        unreachable!("the entry has a string app_name or was just created");
    };
    update_library_entry(entry, &app, spec);
    let library_contents = serialize_document(
        library_original.as_deref(),
        &Value::Object(document),
        LIBRARY_INDENT,
    );

    Ok(Plan {
        created,
        // Settings first: a library entry without them would run in Heroic's
        // shared default prefix.
        writes: vec![
            PendingWrite {
                path: config_path,
                original: config_original,
                contents: config_contents,
            },
            PendingWrite {
                path: library,
                original: library_original,
                contents: library_contents,
            },
        ],
        warnings: Vec::new(),
    })
}

pub(crate) fn read_existing(
    config_dir: &Path,
    work_id: &str,
) -> Result<Option<ExistingExport>, ExportError> {
    let app = app_name(work_id);
    let library = library_path(config_dir);
    let original = read_optional(&library)?;
    let mut document = parse_object(&library, original.as_deref())?;
    let games = library_games(&library, &mut document)?;
    let Some(entry) = games.iter().find(|game| is_app(game, &app)) else {
        return Ok(None);
    };
    let config_path = game_config_path(config_dir, &app);
    let config_bytes = read_optional(&config_path)?;
    let config = parse_object(&config_path, config_bytes.as_deref())?;
    let settings = config.get(&app);
    Ok(Some(ExistingExport {
        install_id: String::new(),
        display_name: entry
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        executable: entry
            .pointer("/install/executable")
            .and_then(Value::as_str)
            .map(str::to_owned),
        prefix: settings
            .and_then(|settings| settings.get("winePrefix"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        compat_tool: settings
            .and_then(|settings| settings.pointer("/wineVersion/bin"))
            .and_then(Value::as_str)
            .and_then(|bin| Path::new(bin).parent())
            .map(|tool| tool.to_string_lossy().into_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ResolvedTool;
    use crate::fsio::commit_all;
    use std::fs;

    const LIBRARY_FIXTURE: &str = r#"{
	"games": [
		{
			"runner": "sideload",
			"app_name": "abc123",
			"title": "Hand Added",
			"install": {"executable": "/x/y.exe", "platform": "Windows", "is_dlc": false},
			"art_cover": "file:///a.png",
			"art_square": "file:///a.png",
			"is_installed": true,
			"canRunOffline": true,
			"future_field": 1
		}
	],
	"__internal__": {"migrations": {"version": "2.0"}}
}"#;

    struct Fixture {
        dir: tempfile::TempDir,
        config: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("heroic");
        fs::create_dir_all(config.join("sideload_apps")).unwrap();
        fs::write(library_path(&config), LIBRARY_FIXTURE).unwrap();
        let tool = dir.path().join("tools/proton-dl");
        fs::create_dir_all(&tool).unwrap();
        fs::write(tool.join("proton"), "").unwrap();
        Fixture { dir, config }
    }

    fn spec(fixture: &Fixture) -> ExportSpec {
        ExportSpec {
            work_id: "RJ1".to_owned(),
            work_root: PathBuf::from("/games/RJ1"),
            display_name: "月光".to_owned(),
            executable: "/games/RJ1/Game.exe".to_owned(),
            prefix: "/prefixes/RJ1".to_owned(),
            tool: ResolvedTool {
                id: "Proton-DL".to_owned(),
                path: Some(fixture.dir.path().join("tools/proton-dl")),
                discovered: true,
            },
            cover_url: Some("https://img.example/cover.jpg".to_owned()),
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn adds_a_sideload_game_and_its_settings() {
        let fixture = fixture();
        let plan = plan(&fixture.config, &spec(&fixture)).unwrap();
        assert!(plan.created);
        commit_all(plan.writes).unwrap();

        let library = read_json(&library_path(&fixture.config));
        let game = &library["games"][1];
        assert_eq!(game["runner"], "sideload");
        assert_eq!(game["app_name"], "dlsite-rj1");
        assert_eq!(game["title"], "月光");
        assert_eq!(game["install"]["executable"], "/games/RJ1/Game.exe");
        assert_eq!(game["install"]["platform"], "Windows");
        assert_eq!(game["install"]["is_dlc"], false);
        assert_eq!(game["folder_name"], "/games/RJ1");
        assert_eq!(game["is_installed"], true);
        assert_eq!(game["canRunOffline"], true);
        assert_eq!(game["art_cover"], "https://img.example/cover.jpg");

        let config = read_json(&game_config_path(&fixture.config, "dlsite-rj1"));
        assert_eq!(config["version"], "v0");
        let settings = &config["dlsite-rj1"];
        assert_eq!(settings["winePrefix"], "/prefixes/RJ1");
        assert_eq!(settings["wineVersion"]["type"], "proton");
        assert_eq!(settings["wineVersion"]["name"], "proton-dl");
        assert!(settings["wineVersion"]["bin"]
            .as_str()
            .unwrap()
            .ends_with("tools/proton-dl/proton"));
    }

    #[test]
    fn preserves_other_games_store_metadata_and_settings() {
        let fixture = fixture();
        let path = game_config_path(&fixture.config, "dlsite-rj1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"dlsite-rj1":{"showFps":true,"winePrefix":"/old"},"version":"v0","explicit":false,"extra":1}"#,
        )
        .unwrap();
        commit_all(plan(&fixture.config, &spec(&fixture)).unwrap().writes).unwrap();

        let library = read_json(&library_path(&fixture.config));
        let before: Value = serde_json::from_str(LIBRARY_FIXTURE).unwrap();
        assert_eq!(library["games"][0], before["games"][0]);
        assert_eq!(library["__internal__"], before["__internal__"]);
        let config = read_json(&path);
        assert_eq!(config["dlsite-rj1"]["showFps"], true);
        assert_eq!(config["dlsite-rj1"]["winePrefix"], "/prefixes/RJ1");
        assert_eq!(config["explicit"], false);
        assert_eq!(config["extra"], 1);
    }

    #[test]
    fn reexport_is_idempotent_and_updates_in_place() {
        let fixture = fixture();
        let first = spec(&fixture);
        commit_all(plan(&fixture.config, &first).unwrap().writes).unwrap();

        let second = plan(&fixture.config, &first).unwrap();
        assert!(!second.created);
        for write in &second.writes {
            assert_eq!(write.original.as_deref(), Some(write.contents.as_slice()));
        }

        let path = library_path(&fixture.config);
        let customized = fs::read_to_string(&path)
            .unwrap()
            .replace("https://img.example/cover.jpg", "file:///mine.png");
        fs::write(&path, customized).unwrap();
        let mut renamed = first;
        renamed.display_name = "Renamed".to_owned();
        commit_all(plan(&fixture.config, &renamed).unwrap().writes).unwrap();
        let library = read_json(&path);
        assert_eq!(library["games"].as_array().unwrap().len(), 2);
        assert_eq!(library["games"][1]["title"], "Renamed");
        assert_eq!(library["games"][1]["art_cover"], "file:///mine.png");
    }

    #[test]
    fn reads_back_a_previous_export() {
        let fixture = fixture();
        assert!(read_existing(&fixture.config, "RJ1").unwrap().is_none());
        commit_all(plan(&fixture.config, &spec(&fixture)).unwrap().writes).unwrap();
        let found = read_existing(&fixture.config, "RJ1").unwrap().unwrap();
        assert_eq!(found.prefix.as_deref(), Some("/prefixes/RJ1"));
        assert_eq!(found.display_name.as_deref(), Some("月光"));
        assert!(found.compat_tool.unwrap().ends_with("tools/proton-dl"));
    }

    #[test]
    fn corrupt_files_fail_without_changes() {
        let fixture = fixture();
        let library = library_path(&fixture.config);
        for bad in ["{broken", "[]", r#"{"games": "nope"}"#] {
            fs::write(&library, bad).unwrap();
            let error = plan(&fixture.config, &spec(&fixture)).unwrap_err();
            assert!(matches!(error, ExportError::Corrupt { .. }), "{bad}");
            assert_eq!(fs::read_to_string(&library).unwrap(), bad);
        }

        fs::write(&library, LIBRARY_FIXTURE).unwrap();
        let config = game_config_path(&fixture.config, "dlsite-rj1");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "{broken").unwrap();
        assert!(matches!(
            plan(&fixture.config, &spec(&fixture)),
            Err(ExportError::Corrupt { .. })
        ));
        assert_eq!(fs::read_to_string(&library).unwrap(), LIBRARY_FIXTURE);
    }

    #[test]
    fn a_tool_without_a_proton_script_is_refused() {
        let fixture = fixture();
        let mut unknown = spec(&fixture);
        unknown.tool.path = None;
        assert!(matches!(
            plan(&fixture.config, &unknown),
            Err(ExportError::Invalid(_))
        ));
        let mut no_script = spec(&fixture);
        no_script.tool.path = Some(fixture.dir.path().to_path_buf());
        assert!(matches!(
            plan(&fixture.config, &no_script),
            Err(ExportError::Invalid(_))
        ));
    }
}
