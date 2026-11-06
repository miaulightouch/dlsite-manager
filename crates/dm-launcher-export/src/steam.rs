//! Steam non-Steam shortcuts (`shortcuts.vdf`) and tool mapping (`config.vdf`).
use crate::discovery::ResolvedTool;
use crate::fsio::{read_optional, PendingWrite};
use crate::vdf_binary::{self, Map, Value};
use crate::vdf_text::{self, Body};
use crate::{ExistingExport, ExportError, ExportSpec, Plan};
use std::path::{Path, PathBuf};

const PREFIX_VARIABLE: &str = "STEAM_COMPAT_DATA_PATH";
/// Marks a shortcut as created by this app for one work. Launch options are the
/// one field Steam must keep verbatim, and the prefix already depends on them.
const MARKER_VARIABLE: &str = "DLSITE_MANAGER_WORK";
/// Shell builtins that cannot run `%command%` with the assignments before them.
const NON_WRAPPER_WORDS: &[&str] = &[
    "alias", "cd", "declare", "eval", "export", "local", "readonly", "set", "source", "unset", ".",
];
/// Characters a shell may expand inside the double quotes around `Exe`.
const UNSAFE_EXE_CHARACTERS: &[char] = &['"', '$', '`', '\\'];
const MAPPING_PATH: [&str; 5] = [
    "InstallConfigStore",
    "Software",
    "Valve",
    "Steam",
    "CompatToolMapping",
];

/// Steam's shortcut id: CRC-32 of the quoted Exe plus AppName, high bit set.
pub(crate) fn shortcut_appid(exe: &str, name: &str) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(exe.as_bytes());
    hasher.update(name.as_bytes());
    hasher.finalize() | 0x8000_0000
}

fn shortcuts_path(root: &Path, user: &str) -> PathBuf {
    root.join("userdata")
        .join(user)
        .join("config/shortcuts.vdf")
}

fn corrupt(path: &Path, message: impl Into<String>) -> ExportError {
    ExportError::Corrupt {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn parse_shortcuts(path: &Path, bytes: &[u8]) -> Result<Map, ExportError> {
    if bytes.is_empty() {
        return Ok(Map::default());
    }
    let map = vdf_binary::parse(bytes).map_err(|message| corrupt(path, message))?;
    match map.get("shortcuts") {
        Some(Value::Map(_)) => Ok(map),
        _ => Err(corrupt(path, "the shortcuts section is missing")),
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

fn launch_options_text(entry: &Map) -> String {
    entry.get_string("LaunchOptions").unwrap_or_default()
}

/// Only shortcuts this app marked for this work count as that work's, so a
/// hand-added shortcut or a nested work's shortcut is never rewritten.
fn is_work_entry(entry: &Map, work_id: &str) -> bool {
    let marker = format!("{MARKER_VARIABLE}={work_id}");
    shell_words(&launch_options_text(entry)).contains(&marker.as_str())
}

/// `Ok((appid, created, marked entries))`
fn upsert_shortcut(root: &mut Map, spec: &ExportSpec) -> Result<(u32, bool, usize), ExportError> {
    let exe = format!("\"{}\"", spec.executable);
    let start_dir = Path::new(&spec.executable)
        .parent()
        .map(|parent| format!("\"{}\"", parent.display()))
        .unwrap_or_default();
    if root.get("shortcuts").is_none() {
        root.0
            .push((b"shortcuts".to_vec(), Value::Map(Map::default())));
    }
    let Some(Value::Map(shortcuts)) = root.get_mut("shortcuts") else {
        unreachable!("the section was checked or just inserted");
    };

    let matches: Vec<usize> = shortcuts
        .0
        .iter()
        .enumerate()
        .filter_map(|(index, (_, value))| match value {
            Value::Map(entry) if is_work_entry(entry, &spec.work_id) => Some(index),
            _ => None,
        })
        .collect();

    if let Some(&index) = matches.first() {
        let Value::Map(entry) = &mut shortcuts.0[index].1 else {
            unreachable!("matches only holds map entries");
        };
        if let Some(Value::String(bytes)) = entry.get("LaunchOptions") {
            if std::str::from_utf8(bytes).is_err() {
                return Err(ExportError::Refused(
                    "this work's Steam launch options are not valid UTF-8 and cannot be updated without changing them"
                        .to_owned(),
                ));
            }
        }
        let options =
            merge_launch_options(&launch_options_text(entry), &spec.work_id, &spec.prefix)
                .ok_or_else(|| {
                    ExportError::Refused(
                        "this work's Steam launch options are too complex to set the Wine prefix safely; simplify them to environment variables, wrappers, %command% and arguments, then export again"
                            .to_owned(),
                    )
                })?;
        let appid = match entry.get("appid") {
            Some(Value::Int32(appid)) if *appid != 0 => *appid,
            _ => {
                let appid = shortcut_appid(&exe, &spec.display_name);
                entry.set("appid", Value::Int32(appid));
                appid
            }
        };
        for (key, value) in [
            ("AppName", spec.display_name.as_str()),
            ("Exe", exe.as_str()),
            ("StartDir", start_dir.as_str()),
            ("LaunchOptions", options.as_str()),
        ] {
            entry.set(key, Value::String(value.as_bytes().to_vec()));
        }
        return Ok((appid, false, matches.len()));
    }

    let appid = shortcut_appid(&exe, &spec.display_name);
    let collides = shortcuts.0.iter().any(|(_, value)| {
        matches!(value, Value::Map(entry) if entry.get("appid") == Some(&Value::Int32(appid)))
    });
    if collides {
        return Err(ExportError::Refused(format!(
            "Steam already has a shortcut for this executable named \"{}\" that this app did not create; rename or remove it, or export under another name",
            spec.display_name
        )));
    }
    let next_index = shortcuts
        .0
        .iter()
        .filter_map(|(key, _)| std::str::from_utf8(key).ok()?.parse::<u64>().ok())
        .max()
        .map_or(0, |max| max + 1);
    let options = merge_launch_options("", &spec.work_id, &spec.prefix)
        .expect("empty launch options are always simple");
    let text = |value: &str| Value::String(value.as_bytes().to_vec());
    let entry = Map(vec![
        (b"appid".to_vec(), Value::Int32(appid)),
        (b"AppName".to_vec(), text(&spec.display_name)),
        (b"Exe".to_vec(), text(&exe)),
        (b"StartDir".to_vec(), text(&start_dir)),
        (b"icon".to_vec(), text("")),
        (b"ShortcutPath".to_vec(), text("")),
        (b"LaunchOptions".to_vec(), text(&options)),
        (b"IsHidden".to_vec(), Value::Int32(0)),
        (b"AllowDesktopConfig".to_vec(), Value::Int32(1)),
        (b"AllowOverlay".to_vec(), Value::Int32(1)),
        (b"OpenVR".to_vec(), Value::Int32(0)),
        (b"Devkit".to_vec(), Value::Int32(0)),
        (b"DevkitGameID".to_vec(), text("")),
        (b"DevkitOverrideAppID".to_vec(), Value::Int32(0)),
        (b"LastPlayTime".to_vec(), Value::Int32(0)),
        (b"FlatpakAppID".to_vec(), text("")),
        (b"tags".to_vec(), Value::Map(Map::default())),
    ]);
    shortcuts
        .0
        .push((next_index.to_string().into_bytes(), Value::Map(entry)));
    Ok((appid, true, 0))
}

/// Split shell-like words, keeping each word's source text and quotes.
fn shell_words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                escaped = true;
                start.get_or_insert(index);
            }
            (Some(_), _) => {}
            (None, '\'' | '"') => {
                quote = Some(character);
                start.get_or_insert(index);
            }
            (None, c) if c.is_whitespace() => {
                if let Some(begin) = start.take() {
                    words.push(&text[begin..index]);
                }
            }
            (None, _) => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(begin) = start {
        words.push(&text[begin..]);
    }
    words
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn is_prefix_assignment(word: &str) -> bool {
    word.strip_prefix(PREFIX_VARIABLE)
        .is_some_and(|rest| rest.starts_with('='))
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// True when text has syntax outside quotes that makes it more than one simple
/// command, or a substitution anywhere a shell would run it.
fn has_compound_syntax(text: &str) -> bool {
    let mut quote = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => {}
            (_, '\\') => {
                characters.next();
            }
            (_, '`') => return true,
            (_, '$') if characters.peek() == Some(&'(') => return true,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(character),
            (None, ';' | '&' | '|' | '<' | '>' | '(' | ')' | '{' | '}' | '\n' | '#') => {
                return true
            }
            (None, _) => {}
        }
    }
    false
}

/// Point Proton at the work's prefix and mark the shortcut, keeping the user's
/// other options. `None` when the options are not one simple command, because
/// an assignment in front of them might not reach the game.
pub(crate) fn merge_launch_options(existing: &str, work_id: &str, prefix: &str) -> Option<String> {
    if has_compound_syntax(existing) {
        return None;
    }
    let words = shell_words(existing);
    let command_count = words.iter().filter(|word| **word == "%command%").count();
    if command_count > 1 || (command_count == 0 && existing.contains("%command%")) {
        return None;
    }
    let (before, arguments) = match words.iter().position(|word| *word == "%command%") {
        Some(position) => (&words[..position], &words[position + 1..]),
        None => (&words[..0], &words[..]),
    };
    let assignments = before.iter().take_while(|word| is_assignment(word)).count();
    let (environment, wrappers) = before.split_at(assignments);
    if wrappers
        .iter()
        .any(|word| word.contains(PREFIX_VARIABLE) || NON_WRAPPER_WORDS.contains(word))
    {
        return None;
    }

    let mut merged = vec![
        format!("{PREFIX_VARIABLE}={}", shell_quote(prefix)),
        format!("{MARKER_VARIABLE}={work_id}"),
    ];
    let marker = format!("{MARKER_VARIABLE}=");
    merged.extend(
        environment
            .iter()
            .filter(|word| !is_prefix_assignment(word) && !word.starts_with(&marker))
            .map(|word| (*word).to_owned()),
    );
    merged.extend(wrappers.iter().map(|word| (*word).to_owned()));
    merged.push("%command%".to_owned());
    merged.extend(arguments.iter().map(|word| (*word).to_owned()));
    Some(merged.join(" "))
}

fn prefix_from_launch_options(options: &str) -> Option<String> {
    let word = shell_words(options)
        .into_iter()
        .find(|word| is_prefix_assignment(word))?;
    let value = &word[PREFIX_VARIABLE.len() + 1..];
    let mut out = String::new();
    let mut quote = None;
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (Some('\''), c) => out.push(c),
            (_, '\\') => out.extend(characters.next()),
            (_, c) => out.push(c),
        }
    }
    Some(out)
}

/// Add or update one app's tool, changing nothing else in the file.
pub(crate) fn set_compat_mapping(text: &str, appid: u32, tool: &str) -> Result<String, String> {
    let appid_key = appid.to_string();
    if text.trim().is_empty() {
        return Ok(render_missing(&MAPPING_PATH, 0, &appid_key, tool));
    }
    let nodes = vdf_text::parse(text)?;
    let chain = mapping_chain(&nodes)?;
    if chain.is_empty() {
        return Err(format!("the {} section is missing", MAPPING_PATH[0]));
    }
    let deepest = chain[chain.len() - 1];
    let Body::Block { children, close } = &deepest.body else {
        return Err(format!("{} is not a section", deepest.key));
    };
    let parent_depth = chain.len() - 1;
    if chain.len() < MAPPING_PATH.len() {
        let rendered = render_missing(
            &MAPPING_PATH[chain.len()..],
            parent_depth + 1,
            &appid_key,
            tool,
        );
        return Ok(splice_into_block(text, *close, rendered));
    }
    let (level, mapping_close, mapping_depth) = (children.as_slice(), *close, parent_depth);
    let same_key = level
        .iter()
        .filter(|node| node.key.eq_ignore_ascii_case(&appid_key))
        .count();
    if same_key > 1 {
        return Err(format!("mapping {appid_key} appears more than once"));
    }
    let updated = match vdf_text::find(level, &appid_key).map(|node| &node.body) {
        None => splice_into_block(
            text,
            mapping_close,
            render_entry(mapping_depth + 1, &appid_key, tool),
        ),
        Some(Body::Value { .. }) => return Err(format!("mapping {appid_key} is not a section")),
        Some(Body::Block { children, close }) => {
            match vdf_text::find(children, "name").map(|node| &node.body) {
                Some(Body::Value { span, .. }) => {
                    let mut out = text.to_owned();
                    out.replace_range(span.clone(), &vdf_text::quote(tool));
                    out
                }
                Some(Body::Block { .. }) => {
                    return Err(format!("mapping {appid_key} has an invalid name"))
                }
                None => {
                    let tabs = "\t".repeat(mapping_depth + 2);
                    let line = format!("{tabs}\"name\"\t\t{}\n", vdf_text::quote(tool));
                    splice_into_block(text, *close, line)
                }
            }
        }
    };
    Ok(updated)
}

/// The deepest run of sections leading to the mapping. Section names can occur
/// twice with different case ("Valve" and "valve"); the fullest one wins, and a
/// tie is refused, because which one Steam reads is not known.
fn mapping_chain(nodes: &[vdf_text::Node]) -> Result<Vec<&vdf_text::Node>, String> {
    fn walk<'a>(level: &'a [vdf_text::Node], path: &[&str]) -> Vec<Vec<&'a vdf_text::Node>> {
        let Some((name, rest)) = path.split_first() else {
            return vec![Vec::new()];
        };
        let mut chains = Vec::new();
        for node in level
            .iter()
            .filter(|node| node.key.eq_ignore_ascii_case(name))
        {
            for tail in walk(node.children(), rest) {
                let mut chain = vec![node];
                chain.extend(tail);
                chains.push(chain);
            }
        }
        if chains.is_empty() {
            chains.push(Vec::new());
        }
        chains
    }
    let chains = walk(nodes, &MAPPING_PATH);
    let longest = chains.iter().map(Vec::len).max().unwrap_or(0);
    let mut fullest = chains.into_iter().filter(|chain| chain.len() == longest);
    let chain = fullest.next().unwrap_or_default();
    if fullest.next().is_some() {
        return Err(format!(
            "duplicate \"{}\" sections make the tool mapping ambiguous",
            MAPPING_PATH[longest.saturating_sub(1)]
        ));
    }
    Ok(chain)
}

fn render_entry(depth: usize, appid: &str, tool: &str) -> String {
    let outer = "\t".repeat(depth);
    let inner = "\t".repeat(depth + 1);
    format!(
        "{outer}\"{appid}\"\n{outer}{{\n{inner}\"name\"\t\t{}\n{inner}\"config\"\t\t\"\"\n{inner}\"priority\"\t\t\"250\"\n{outer}}}\n",
        vdf_text::quote(tool)
    )
}

fn render_missing(path: &[&str], depth: usize, appid: &str, tool: &str) -> String {
    let Some((name, rest)) = path.split_first() else {
        return render_entry(depth, appid, tool);
    };
    let tabs = "\t".repeat(depth);
    format!(
        "{tabs}\"{name}\"\n{tabs}{{\n{}{tabs}}}\n",
        render_missing(rest, depth + 1, appid, tool)
    )
}

fn splice_into_block(text: &str, close: usize, rendered: String) -> String {
    let line_start = text[..close].rfind('\n').map_or(0, |index| index + 1);
    let mut out = text.to_owned();
    if text[line_start..close].trim().is_empty() && line_start > 0 {
        out.insert_str(line_start, &rendered);
    } else {
        out.insert_str(close, &format!("\n{rendered}"));
    }
    out
}

fn lookup_mapping(text: &str, appid: u32) -> Option<String> {
    let nodes = vdf_text::parse(text).ok()?;
    let chain = mapping_chain(&nodes).ok()?;
    if chain.len() != MAPPING_PATH.len() {
        return None;
    }
    let entry = vdf_text::find(chain[chain.len() - 1].children(), &appid.to_string())?;
    Some(
        vdf_text::find(entry.children(), "name")?
            .value()?
            .to_owned(),
    )
}

pub(crate) fn plan(
    root: &Path,
    users: &[String],
    spec: &ExportSpec,
    tool_visible: bool,
) -> Result<Plan, ExportError> {
    // Steam starts shortcuts through a shell, and Exe sits in double quotes there.
    if spec.executable.contains(UNSAFE_EXE_CHARACTERS) {
        return Err(ExportError::Invalid(
            "Steam shortcuts cannot use an executable path containing \", $, ` or \\; rename the folder or file"
                .to_owned(),
        ));
    }
    let mut writes = Vec::new();
    let mut warnings = Vec::new();
    let mut appids = Vec::new();
    let mut created = false;
    for user in users {
        let path = shortcuts_path(root, user);
        let original = read_optional(&path)?;
        let mut document = parse_shortcuts(&path, original.as_deref().unwrap_or_default())?;
        let (appid, new_entry, match_count) = upsert_shortcut(&mut document, spec)?;
        created |= new_entry;
        if match_count > 1 {
            warnings.push(format!(
                "Steam user {user} has {match_count} shortcuts for this work; only the first was updated"
            ));
        }
        if !appids.contains(&appid) {
            appids.push(appid);
        }
        writes.push(PendingWrite {
            path,
            original,
            contents: vdf_binary::serialize(&document),
        });
    }

    let config_path = root.join("config/config.vdf");
    let config_original = read_optional(&config_path)?;
    let mut config = match &config_original {
        Some(bytes) => std::str::from_utf8(bytes)
            .map_err(|_| corrupt(&config_path, "the file is not valid UTF-8"))?
            .to_owned(),
        None => String::new(),
    };
    for appid in appids {
        config = set_compat_mapping(&config, appid, &spec.tool.id)
            .map_err(|message| corrupt(&config_path, message))?;
        if lookup_mapping(&config, appid).as_deref() != Some(spec.tool.id.as_str()) {
            return Err(corrupt(
                &config_path,
                "the tool mapping could not be verified",
            ));
        }
    }
    writes.push(PendingWrite {
        path: config_path,
        original: config_original,
        contents: config.into_bytes(),
    });

    if !tool_visible {
        warnings.push(unknown_tool_warning(&spec.tool));
    }
    Ok(Plan {
        created,
        writes,
        warnings,
    })
}

fn unknown_tool_warning(tool: &ResolvedTool) -> String {
    format!(
        "\"{}\" was not found in the folders this Steam install reads; Steam ignores a mapping to a tool it does not know",
        tool.id
    )
}

/// Read what an earlier export stored, so a new export starts from those values.
pub(crate) fn read_existing(
    root: &Path,
    users: &[String],
    work_id: &str,
) -> Result<Vec<ExistingExport>, ExportError> {
    let config = read_optional(&root.join("config/config.vdf"))?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();
    let mut found = Vec::new();
    for user in users {
        let path = shortcuts_path(root, user);
        let Some(bytes) = read_optional(&path)? else {
            continue;
        };
        let document = parse_shortcuts(&path, &bytes)?;
        let Some(Value::Map(shortcuts)) = document.get("shortcuts") else {
            continue;
        };
        for (_, value) in &shortcuts.0 {
            let Value::Map(entry) = value else { continue };
            if !is_work_entry(entry, work_id) {
                continue;
            }
            let appid = match entry.get("appid") {
                Some(Value::Int32(appid)) => Some(*appid),
                _ => None,
            };
            found.push(ExistingExport {
                install_id: String::new(),
                steam_user_id: Some(user.clone()),
                display_name: entry.get_string("AppName"),
                executable: entry.get_string("Exe").map(|exe| unquote(&exe).to_owned()),
                prefix: entry
                    .get_string("LaunchOptions")
                    .and_then(|options| prefix_from_launch_options(&options)),
                compat_tool: appid.and_then(|appid| lookup_mapping(&config, appid)),
            });
            break;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const WORK_ROOT: &str = "/games/RJ1";

    fn spec(name: &str) -> ExportSpec {
        ExportSpec {
            work_id: "RJ1".to_owned(),
            display_name: name.to_owned(),
            executable: format!("{WORK_ROOT}/Game.exe"),
            prefix: "/prefixes/RJ1".to_owned(),
            tool: ResolvedTool {
                id: "Proton-DL".to_owned(),
                path: Some(PathBuf::from("/tools/proton-dl")),
            },
            cover_url: None,
        }
    }

    /// Bytes shaped like Steam's own file, with fields the exporter must keep.
    fn existing_shortcuts() -> Vec<u8> {
        let mut doc = Vec::new();
        doc.extend_from_slice(b"\x00shortcuts\x00");
        doc.extend_from_slice(b"\x000\x00");
        doc.extend_from_slice(b"\x02appid\x00\x01\x02\x03\x84");
        doc.extend_from_slice(b"\x01AppName\x00Other Game\x00");
        doc.extend_from_slice(b"\x01Exe\x00\"/opt/other/run\"\x00");
        doc.extend_from_slice(b"\x01StartDir\x00\"/opt/other\"\x00");
        doc.extend_from_slice(b"\x01icon\x00/opt/other.png\x00");
        doc.extend_from_slice(b"\x01LaunchOptions\x00--fullscreen\x00");
        doc.extend_from_slice(b"\x02LastPlayTime\x00\x10\x20\x30\x40");
        doc.extend_from_slice(b"\x01FutureField\x00keep me\x00");
        doc.extend_from_slice(b"\x07FutureBig\x00\x01\x02\x03\x04\x05\x06\x07\x08");
        doc.extend_from_slice(b"\x00tags\x00\x010\x00favorite\x00\x08");
        doc.extend_from_slice(b"\x08");
        doc.extend_from_slice(b"\x08\x08");
        doc
    }

    const CONFIG: &str = "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"AutoUpdateWindowEnabled\"\t\t\"0\"\n\t\t\t\t\"CompatToolMapping\"\n\t\t\t\t{\n\t\t\t\t\t\"0\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"proton_experimental\"\n\t\t\t\t\t\t\"config\"\t\t\"\"\n\t\t\t\t\t\t\"priority\"\t\t\"75\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t\t\"Unrelated\"\t\t\"C:\\\\x\"\n\t\t\t}\n\t\t}\n\t}\n\t\"Music\"\n\t{\n\t\t\"CrawlSteamInstallFolders\"\t\t\"1\"\n\t}\n}\n";

    #[test]
    fn appid_matches_values_computed_independently_with_zlib() {
        assert_eq!(
            shortcut_appid("\"/games/RJ1/Game.exe\"", "Title"),
            3540063994
        );
        assert_eq!(
            shortcut_appid("\"/games/日本語/Game.exe\"", "月光"),
            2675963692
        );
    }

    #[test]
    fn untouched_shortcuts_round_trip_byte_for_byte() {
        let bytes = existing_shortcuts();
        let path = Path::new("x");
        assert_eq!(
            vdf_binary::serialize(&parse_shortcuts(path, &bytes).unwrap()),
            bytes
        );
    }

    #[test]
    fn adding_keeps_every_existing_entry_and_uses_the_next_index() {
        let mut document = parse_shortcuts(Path::new("x"), &existing_shortcuts()).unwrap();
        let before = document.clone();
        let (appid, created, _) = upsert_shortcut(&mut document, &spec("月光")).unwrap();
        assert!(created);
        assert_eq!(
            appid,
            shortcut_appid(&format!("\"{WORK_ROOT}/Game.exe\""), "月光")
        );

        let Some(Value::Map(shortcuts)) = document.get("shortcuts") else {
            panic!("missing shortcuts");
        };
        let Some(Value::Map(old_shortcuts)) = before.get("shortcuts") else {
            panic!("missing shortcuts");
        };
        assert_eq!(shortcuts.0[0], old_shortcuts.0[0]);
        assert_eq!(shortcuts.0[1].0, b"1");
        let Value::Map(entry) = &shortcuts.0[1].1 else {
            panic!("expected a map");
        };
        assert_eq!(entry.get("appid"), Some(&Value::Int32(appid)));
        assert_eq!(entry.get_string("AppName").unwrap(), "月光");
        assert_eq!(
            entry.get_string("Exe").unwrap(),
            format!("\"{WORK_ROOT}/Game.exe\"")
        );
        assert_eq!(
            entry.get_string("StartDir").unwrap(),
            format!("\"{WORK_ROOT}\"")
        );
        assert_eq!(
            entry.get_string("LaunchOptions").unwrap(),
            "STEAM_COMPAT_DATA_PATH='/prefixes/RJ1' DLSITE_MANAGER_WORK=RJ1 %command%"
        );

        let reparsed = vdf_binary::parse(&vdf_binary::serialize(&document)).unwrap();
        assert_eq!(reparsed, document);
    }

    #[test]
    fn reexport_updates_in_place_and_keeps_appid_and_user_fields() {
        let mut document = parse_shortcuts(Path::new("x"), &existing_shortcuts()).unwrap();
        let (appid, _, _) = upsert_shortcut(&mut document, &spec("Name")).unwrap();
        {
            let Some(Value::Map(shortcuts)) = document.get_mut("shortcuts") else {
                panic!();
            };
            let Value::Map(entry) = &mut shortcuts.0[1].1 else {
                panic!();
            };
            entry.set("LastPlayTime", Value::Int32(99));
            entry.set(
                "LaunchOptions",
                Value::String(
                    b"MANGOHUD=1 STEAM_COMPAT_DATA_PATH=/old DLSITE_MANAGER_WORK=RJ1 %command% -w"
                        .to_vec(),
                ),
            );
        }
        let mut changed = spec("Renamed");
        changed.executable = format!("{WORK_ROOT}/bin/Other.exe");
        let (same_appid, created, _) = upsert_shortcut(&mut document, &changed).unwrap();
        assert!(!created);
        assert_eq!(same_appid, appid);
        let Some(Value::Map(shortcuts)) = document.get("shortcuts") else {
            panic!();
        };
        assert_eq!(shortcuts.0.len(), 2);
        let Value::Map(entry) = &shortcuts.0[1].1 else {
            panic!();
        };
        assert_eq!(entry.get("LastPlayTime"), Some(&Value::Int32(99)));
        assert_eq!(entry.get_string("AppName").unwrap(), "Renamed");
        assert_eq!(
            entry.get_string("StartDir").unwrap(),
            format!("\"{WORK_ROOT}/bin\"")
        );
        assert_eq!(
            entry.get_string("LaunchOptions").unwrap(),
            "STEAM_COMPAT_DATA_PATH='/prefixes/RJ1' DLSITE_MANAGER_WORK=RJ1 MANGOHUD=1 %command% -w"
        );
    }

    fn hand_added_entry(exe: &str, name: &str, options: &[u8]) -> Value {
        Value::Map(Map(vec![
            (b"appid".to_vec(), Value::Int32(shortcut_appid(exe, name))),
            (b"AppName".to_vec(), Value::String(name.as_bytes().to_vec())),
            (b"Exe".to_vec(), Value::String(exe.as_bytes().to_vec())),
            (b"LaunchOptions".to_vec(), Value::String(options.to_vec())),
        ]))
    }

    fn entries(document: &Map) -> &Map {
        let Some(Value::Map(shortcuts)) = document.get("shortcuts") else {
            panic!("missing shortcuts");
        };
        shortcuts
    }

    fn document_with(entry: Value) -> Map {
        Map(vec![(
            b"shortcuts".to_vec(),
            Value::Map(Map(vec![(b"0".to_vec(), entry)])),
        )])
    }

    #[test]
    fn a_hand_added_shortcut_in_the_work_folder_is_left_alone() {
        let hand_added = hand_added_entry(
            &format!("\"{WORK_ROOT}/Game.exe\""),
            "My Own Name",
            b"-windowed",
        );
        let mut document = document_with(hand_added.clone());
        let (_, created, _) = upsert_shortcut(&mut document, &spec("Exported")).unwrap();
        assert!(created);
        assert_eq!(entries(&document).0.len(), 2);
        assert_eq!(entries(&document).0[0].1, hand_added);
    }

    #[test]
    fn a_nested_works_shortcut_is_not_taken_over() {
        let mut outer = spec("Outer");
        outer.work_id = "RJ1".to_owned();
        outer.executable = "/lib/Series/Outer.exe".to_owned();
        let mut nested = spec("Nested");
        nested.work_id = "RJ2".to_owned();
        nested.executable = "/lib/Series/RJ2/Nested.exe".to_owned();

        let mut document = Map::default();
        upsert_shortcut(&mut document, &nested).unwrap();
        let before = entries(&document).0[0].1.clone();
        let (_, created, _) = upsert_shortcut(&mut document, &outer).unwrap();
        assert!(created);
        assert_eq!(entries(&document).0[0].1, before);

        // A work id that merely starts with another is a different marker.
        let mut longer = spec("Longer");
        longer.work_id = "RJ12".to_owned();
        longer.executable = format!("{WORK_ROOT}/Longer.exe");
        let (_, created, _) = upsert_shortcut(&mut document, &longer).unwrap();
        assert!(created);
    }

    #[test]
    fn a_same_name_hand_added_shortcut_is_refused_instead_of_duplicating_its_appid() {
        let exe = format!("\"{WORK_ROOT}/Game.exe\"");
        let hand_added = hand_added_entry(&exe, "Title", b"");
        let mut document = document_with(hand_added.clone());
        assert!(matches!(
            upsert_shortcut(&mut document, &spec("Title")),
            Err(ExportError::Refused(_))
        ));
        assert_eq!(entries(&document).0[0].1, hand_added);
    }

    #[test]
    fn non_utf8_launch_options_are_refused_and_kept() {
        let mut options = b"DLSITE_MANAGER_WORK=RJ1 %command% -name=".to_vec();
        options.push(0xff);
        let entry = hand_added_entry("\"/x.exe\"", "X", &options);
        let mut document = document_with(entry.clone());
        assert!(matches!(
            upsert_shortcut(&mut document, &spec("Title")),
            Err(ExportError::Refused(_))
        ));
        assert_eq!(entries(&document).0[0].1, entry);
    }

    #[test]
    fn compound_launch_options_on_our_shortcut_are_refused_and_kept() {
        let options = b"cd /x; DLSITE_MANAGER_WORK=RJ1 %command%";
        let entry = hand_added_entry("\"/x.exe\"", "X", options);
        let mut document = document_with(entry.clone());
        assert!(matches!(
            upsert_shortcut(&mut document, &spec("Title")),
            Err(ExportError::Refused(_))
        ));
        assert_eq!(entries(&document).0[0].1, entry);
    }

    #[test]
    fn corrupt_and_foreign_shortcuts_are_rejected() {
        let path = Path::new("x");
        let valid = existing_shortcuts();
        assert!(parse_shortcuts(path, &valid[..valid.len() - 1]).is_err());
        assert!(parse_shortcuts(path, b"not a vdf at all").is_err());
        assert!(parse_shortcuts(path, b"\x00other\x00\x08\x08").is_err());
        assert!(parse_shortcuts(path, b"").is_ok());
    }

    #[test]
    fn launch_options_merge_handles_simple_commands_and_repeats() {
        let merge = |text: &str| merge_launch_options(text, "RJ1", "/p");
        let head = "STEAM_COMPAT_DATA_PATH='/p' DLSITE_MANAGER_WORK=RJ1";

        let once = merge_launch_options("", "RJ1", "/p/it's here").unwrap();
        assert_eq!(
            once,
            "STEAM_COMPAT_DATA_PATH='/p/it'\\''s here' DLSITE_MANAGER_WORK=RJ1 %command%"
        );
        assert_eq!(
            merge_launch_options(&once, "RJ1", "/p/it's here").unwrap(),
            once
        );
        assert_eq!(prefix_from_launch_options(&once).unwrap(), "/p/it's here");

        assert_eq!(
            merge("-windowed").unwrap(),
            format!("{head} %command% -windowed")
        );
        assert_eq!(
            merge("FOO=\"a b\" gamemoderun %command%").unwrap(),
            format!("{head} FOO=\"a b\" gamemoderun %command%")
        );
        assert_eq!(
            merge("STEAM_COMPAT_DATA_PATH=/old mangohud %command% --x='a;b'").unwrap(),
            format!("{head} mangohud %command% --x='a;b'")
        );
        assert_eq!(
            prefix_from_launch_options("STEAM_COMPAT_DATA_PATH=\"/a b\" %command%").unwrap(),
            "/a b"
        );
        assert_eq!(prefix_from_launch_options("%command%"), None);
    }

    #[test]
    fn launch_options_a_prefix_would_not_reach_are_refused() {
        for options in [
            "cd /x; %command%",
            "cd /x && %command%",
            "sh -c \"exec %command%\"",
            "\"%command%\"",
            "export STEAM_COMPAT_DATA_PATH=/x; %command%",
            "export %command%",
            "env STEAM_COMPAT_DATA_PATH=/x %command%",
            "%command% %command%",
            "foo | %command%",
            "%command% > /tmp/log",
            "A=$(date) %command%",
            "A=`date` %command%",
            "A=\"$(date)\" %command%",
            "%command% & wait",
            "line1\n%command%",
            "{ %command%; }",
        ] {
            assert_eq!(
                merge_launch_options(options, "RJ1", "/p"),
                None,
                "{options}"
            );
        }
    }

    #[test]
    fn exe_paths_a_shell_would_expand_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = steam_fixture(dir.path());
        for name in ["a$b", "a`b", "a\"b", "a\\b"] {
            let mut bad = spec("Title");
            bad.executable = format!("{WORK_ROOT}/{name}.exe");
            assert!(
                matches!(
                    plan(&root, &["111".to_owned()], &bad, true),
                    Err(ExportError::Invalid(_))
                ),
                "{name}"
            );
        }
        let mut quote = spec("Title");
        quote.executable = format!("{WORK_ROOT}/Let's Play.exe");
        assert!(plan(&root, &["111".to_owned()], &quote, true).is_ok());
    }

    #[test]
    fn duplicate_case_sections_use_the_one_with_the_mapping_or_refuse() {
        let body = |inner: &str| {
            format!("\"InstallConfigStore\"\n{{\n\t\"Software\"\n\t{{\n{inner}\t}}\n}}\n")
        };
        let empty_valve = "\t\t\"valve\"\n\t\t{\n\t\t}\n";
        let mapped = "\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"CompatToolMapping\"\n\t\t\t\t{\n\t\t\t\t}\n\t\t\t}\n\t\t}\n";

        let text = body(&format!("{empty_valve}{mapped}"));
        let updated = set_compat_mapping(&text, 5, "T").unwrap();
        assert_eq!(lookup_mapping(&updated, 5).unwrap(), "T");
        assert!(updated.contains(empty_valve));
        assert!(updated.find("\"5\"").unwrap() > updated.find("CompatToolMapping").unwrap());

        let ambiguous = body(&format!("{mapped}{}", mapped.replace("Valve", "valve")));
        assert!(set_compat_mapping(&ambiguous, 5, "T")
            .unwrap_err()
            .contains("ambiguous"));
        assert_eq!(lookup_mapping(&ambiguous, 5), None);
    }

    #[test]
    fn mapping_insert_changes_only_the_mapping_block() {
        let updated = set_compat_mapping(CONFIG, 3540063994, "Proton-DL").unwrap();
        let entry = "\t\t\t\t\t\"3540063994\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"Proton-DL\"\n\t\t\t\t\t\t\"config\"\t\t\"\"\n\t\t\t\t\t\t\"priority\"\t\t\"250\"\n\t\t\t\t\t}\n";
        assert_eq!(updated.replacen(entry, "", 1), CONFIG);
        assert_eq!(lookup_mapping(&updated, 3540063994).unwrap(), "Proton-DL");
        assert_eq!(lookup_mapping(&updated, 0).unwrap(), "proton_experimental");
    }

    #[test]
    fn mapping_update_is_idempotent_and_preserves_other_fields() {
        let first = set_compat_mapping(CONFIG, 5, "Proton-DL").unwrap();
        assert_eq!(set_compat_mapping(&first, 5, "Proton-DL").unwrap(), first);
        let customized = first.replace("\"250\"", "\"123\"");
        let changed = set_compat_mapping(&customized, 5, "Other \"Tool\"").unwrap();
        assert!(changed.contains("\"123\""));
        assert_eq!(lookup_mapping(&changed, 5).unwrap(), "Other \"Tool\"");
        assert_eq!(
            changed.replace("Other \\\"Tool\\\"", "Proton-DL"),
            customized
        );
    }

    #[test]
    fn mapping_creates_missing_sections_at_any_depth() {
        let empty = set_compat_mapping("", 5, "T").unwrap();
        assert_eq!(lookup_mapping(&empty, 5).unwrap(), "T");

        let no_mapping = "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"Keep\"\t\t\"1\"\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let added = set_compat_mapping(no_mapping, 5, "T").unwrap();
        assert_eq!(lookup_mapping(&added, 5).unwrap(), "T");
        assert!(added.contains("\"Keep\"\t\t\"1\""));

        let lowercase = "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"valve\"\n\t\t{\n\t\t\t\"steam\"\n\t\t\t{\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let added = set_compat_mapping(lowercase, 5, "T").unwrap();
        assert_eq!(lookup_mapping(&added, 5).unwrap(), "T");
        assert!(added.contains("\"valve\""));

        let single_line = "\"InstallConfigStore\" { \"Software\" { } }";
        let added = set_compat_mapping(single_line, 5, "T").unwrap();
        assert_eq!(lookup_mapping(&added, 5).unwrap(), "T");
    }

    #[test]
    fn corrupt_config_is_rejected() {
        for bad in [
            "\"InstallConfigStore\"\n{\n",
            "garbage",
            "\"SomethingElse\" { }",
            "\"InstallConfigStore\" { \"Software\" \"value\" }",
        ] {
            assert!(set_compat_mapping(bad, 5, "T").is_err(), "accepted {bad:?}");
        }
    }

    fn steam_fixture(dir: &Path) -> PathBuf {
        let root = dir.join("Steam");
        for user in ["111", "222"] {
            fs::create_dir_all(root.join("userdata").join(user)).unwrap();
        }
        fs::create_dir_all(root.join("userdata/111/config")).unwrap();
        fs::write(
            root.join("userdata/111/config/shortcuts.vdf"),
            existing_shortcuts(),
        )
        .unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/config.vdf"), CONFIG).unwrap();
        root
    }

    #[test]
    fn plan_covers_each_user_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = steam_fixture(dir.path());
        let users = vec!["111".to_owned(), "222".to_owned()];
        let plan_result = plan(&root, &users, &spec("Title"), true).unwrap();
        assert!(plan_result.warnings.is_empty());
        assert!(plan_result.created);
        assert_eq!(plan_result.writes.len(), 3);
        crate::fsio::commit_all(plan_result.writes).unwrap();

        let existing = read_existing(&root, &users, "RJ1").unwrap();
        assert_eq!(existing.len(), 2);
        for found in existing {
            assert_eq!(found.display_name.as_deref(), Some("Title"));
            assert_eq!(found.prefix.as_deref(), Some("/prefixes/RJ1"));
            assert_eq!(found.compat_tool.as_deref(), Some("Proton-DL"));
        }

        let again = plan(&root, &users, &spec("Title"), true).unwrap();
        assert!(!again.created);
        for write in again.writes {
            assert_eq!(write.original.as_deref(), Some(write.contents.as_slice()));
        }
    }

    #[test]
    fn a_corrupt_file_for_one_user_blocks_the_whole_steam_plan() {
        let dir = tempfile::tempdir().unwrap();
        let root = steam_fixture(dir.path());
        let shortcuts = root.join("userdata/111/config/shortcuts.vdf");
        fs::write(&shortcuts, b"corrupted").unwrap();
        let config_before = fs::read(root.join("config/config.vdf")).unwrap();
        let users = vec!["222".to_owned(), "111".to_owned()];
        assert!(matches!(
            plan(&root, &users, &spec("Title"), true),
            Err(ExportError::Corrupt { .. })
        ));
        assert_eq!(fs::read(&shortcuts).unwrap(), b"corrupted");
        assert_eq!(
            fs::read(root.join("config/config.vdf")).unwrap(),
            config_before
        );
        assert!(!root.join("userdata/222/config/shortcuts.vdf").exists());
    }

    #[test]
    fn a_tool_this_install_cannot_see_warns() {
        let dir = tempfile::tempdir().unwrap();
        let root = steam_fixture(dir.path());
        let plan = plan(&root, &["111".to_owned()], &spec("Title"), false).unwrap();
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("this Steam install"));
    }
}
