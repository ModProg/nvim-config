use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::{self, read_dir, File},
    io::Read,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use enumflags2::{bitflags, BitFlags};
use serde::Deserialize;
use serde_with::{serde_as, DeserializeFromStr, FromInto, OneOrMany};

#[serde_as]
#[derive(Deserialize)]
struct AutoCommand {
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    triggers: Vec<String>,
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    cmd: Vec<String>,
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    lua: Vec<String>,
    matching: Option<String>,
    #[serde(default)]
    event: HashMap<String, String>,
}

#[serde_as]
#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    auto_commands: Vec<AutoCommand>,
    #[serde(default)]
    #[serde_as(deserialize_as = "HashMap<FromInto<MapFlagParser>, _>")]
    keys: HashMap<(BitFlags<MapFlag>, Option<String>), HashMap<String, MaybePrefixedMapping>>,
}

#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum MapFlag {
    Insert,
    Normal,
    Visual,
    Leader,
    Command,
}

#[derive(DeserializeFromStr)]
struct MapFlagParser {
    flags: HashSet<MapFlag>,
    label: Option<String>,
}

impl FromStr for MapFlagParser {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use MapFlag::*;
        let mut flags = HashSet::new();
        let (s, label) = match s.split_once("_") {
            Some((s, label)) => (s, Some(label.to_string())),
            None => (s, None),
        };

        for c in s.to_ascii_lowercase().chars() {
            flags.insert(match c {
                'i' => Insert,
                'n' => Normal,
                'v' => Visual,
                'l' => Leader,
                'c' => Command,
                '_' => break,
                _ => bail!("Unsuported flag for Mapping: `{}`", c),
            });
        }
        Ok(MapFlagParser { flags, label })
    }
}

impl From<MapFlagParser> for (BitFlags<MapFlag>, Option<String>) {
    fn from(mfp: MapFlagParser) -> Self {
        (mfp.flags.into_iter().collect(), mfp.label)
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MaybePrefixedMapping {
    Mapping(String),
    PrefixedMappings(HashMap<String, String>),
}

fn main() -> Result<()> {
    let nvim_dir = dirs::config_dir()
        .expect("There should be a config_dir")
        .join("nvim");
    let config_folder = nvim_dir.join("config");

    let config_files = read_dir(config_folder)?;

    let mut configs: Vec<(Config, String)> = vec![];

    for config_file in config_files {
        let config_file = config_file?.path();
        if let (Some(filename), Some(extension)) = (
            config_file
                .clone()
                .file_name()
                .map(OsStr::to_string_lossy)
                .map(|s| s.to_string()),
            config_file.extension(),
        ) {
            match extension.to_string_lossy().to_lowercase().as_str() {
                "yaml" | "yml" => {
                    configs.push((
                        serde_yaml::from_reader(File::open(config_file)?)
                            .with_context(|| format!("Failed to parse file: {}", filename))?,
                        filename,
                    ));
                }
                "toml" => {
                    configs.push((
                        toml::from_str(&{
                            let mut string = String::new();
                            File::open(config_file)?.read_to_string(&mut string)?;
                            string
                        })
                        .with_context(|| format!("Failed to parse file: {}", filename))?,
                        filename,
                    ));
                }
                _ => (),
            }
        }
    }
    let mut vimscript: Vec<String> = Vec::new();

    for (config, filename) in configs {
        vimscript.push(format!("\n\n\" File: {}", filename));
        vimscript.push("\n\" Keybindings:".to_string());
        for ((flags, label), k) in config.keys {
            if let Some(label) = label {
                vimscript.push(format!("\" {}", label));
            }
            let mut kbs: Vec<(String, String)> = Vec::new();
            for (key, binding) in k {
                match binding {
                    MaybePrefixedMapping::Mapping(binding) => {
                        kbs.push((key, binding));
                    }
                    MaybePrefixedMapping::PrefixedMappings(binding) => {
                        for (suffix, binding) in binding {
                            kbs.push((format!("{}{}", key, suffix), binding));
                        }
                    }
                }
            }
            for (mut key, mut binding) in kbs {
                if flags.contains(MapFlag::Leader) {
                    key = format!("<LEADER>{}", key);
                }
                binding = binding.replace('|', r"\|");
                if flags.contains(MapFlag::Command) {
                    binding = format!("<CMD>{}<CR>", binding);
                }
                let cmd = format!(
                    "noremap <silent> {} {}",
                    key.split_ascii_whitespace().collect::<String>(),
                    binding
                );
                if flags.contains(MapFlag::Insert) {
                    vimscript.push(format!("i{}", cmd));
                }
                if flags.contains(MapFlag::Normal) {
                    vimscript.push(format!("n{}", cmd));
                }
                if flags.contains(MapFlag::Visual) {
                    vimscript.push(format!("v{}", cmd));
                }
            }
        }
        for AutoCommand {
            triggers,
            cmd,
            lua,
            matching,
            event,
        } in config.auto_commands
        {
            let triggers = triggers.join(",");
            let matching = matching.unwrap_or_else(|| "*".to_string());
            let condition = event
                .iter()
                .map(|(key, value)| format!("v:event.{} is '{}'", key, value))
                .collect::<Vec<_>>()
                .join(" && ");

            for cmd in cmd
                .into_iter()
                .chain(lua.iter().map(|value| format!("lua {}", value)))
            {
                if condition.is_empty() {
                    vimscript.push(format!("autocmd {} {} {}", triggers, matching, cmd))
                } else {
                    vimscript.push(format!(
                        "autocmd {} {} if {} | {} | endif",
                        triggers, matching, condition, cmd
                    ))
                }
            }
        }
    }

    let plugin_dir = nvim_dir.join("plugin");
    fs::create_dir_all(&plugin_dir)?;

    fs::write(plugin_dir.join("config.vim"), vimscript.join("\n"))?;
    Ok(())
}
