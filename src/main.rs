use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fmt::Display,
    fs::{self, read_dir, File},
    io::Read,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use enumflags2::{bitflags, BitFlags};
use serde::Deserialize;
use serde_with::{serde_as, DeserializeFromStr, OneOrMany};

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
    #[serde(default)]
    silent: bool,
    #[serde(default)]
    file_type: Option<String>,
}

#[serde_as]
#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    auto_commands: Vec<AutoCommand>,
    #[serde(default)]
    keys: HashMap<MapFlags, HashMap<String, MaybePrefixedMapping>>,
    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_>")]
    set: Vec<String>,
    #[serde(default)]
    set_value: HashMap<String, Value>,
    #[serde(default)]
    r#let: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Value {
    Int(i64),
    String(String),
    Bool(bool),
}
impl Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(value) => write!(f, "{value}"),
            Value::String(value) => write!(f, r#""{value}""#),
            Value::Bool(true) => write!(f, "yes"),
            Value::Bool(false) => write!(f, "no"),
        }
    }
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
    Recursive,
}

#[derive(DeserializeFromStr, Hash, PartialEq, Eq)]
struct MapFlags {
    flags: BitFlags<MapFlag>,
    file_type: Option<String>,
    label: Option<String>,
}

impl FromStr for MapFlags {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use MapFlag::*;
        let mut flags = HashSet::new();
        let (s, mut label) = match s.split_once("_") {
            Some((s, label)) => (s, Some(label.to_string())),
            None => (s, None),
        };
        let mut file_type = None;

        for c in s.to_ascii_lowercase().chars() {
            flags.insert(match c {
                'i' => Insert,
                'n' => Normal,
                'v' => Visual,
                'l' => Leader,
                'c' => Command,
                'r' => Recursive,
                'f' => match (label, file_type) {
                    (Some(l), None) => {
                        match l.split_once("_") {
                            Some((ft, l)) => {
                                file_type = Some(ft.to_string());
                                label = Some(l.to_string());
                            }
                            None => {
                                file_type = Some(l.to_string());
                                label = None;
                            }
                        };
                        continue;
                    }
                    (_, Some(_)) => bail!("Duplicate filetype flag not supported: `{}`", s),
                    (None, _) => bail!("Filetype flag only supported when filetype is given"),
                },
                _ => bail!("Unsuported flag for Mapping: `{}`", c),
            });
        }
        let flags = flags.into_iter().collect();
        Ok(MapFlags {
            flags,
            label,
            file_type,
        })
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
    let mut vimscript: HashMap<Option<String>, Vec<String>> = HashMap::new();
    fn mut_or_default<'map>(
        map: &'map mut HashMap<Option<String>, Vec<String>>,
        key: &Option<String>,
    ) -> &'map mut Vec<String> {
        if !map.contains_key(key) {
            map.insert(key.clone(), Vec::new());
        }
        map.get_mut(key).expect("Inserted missing key")
    }

    for (config, filename) in configs {
        {
            let vimscript = mut_or_default(&mut vimscript, &None);
            vimscript.push(format!("\n\n\" File: {}", filename));
            vimscript.push("\n\" Keybindings:".to_string());
        }
        for (
            MapFlags {
                flags,
                label,
                file_type,
            },
            k,
        ) in config.keys
        {
            let vimscript = mut_or_default(&mut vimscript, &file_type);
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
            let cmd = if flags.contains(MapFlag::Recursive) {
                "map"
            } else {
                "noremap"
            };
            for (mut key, mut binding) in kbs {
                if flags.contains(MapFlag::Leader) {
                    key = format!("<LEADER>{}", key);
                }
                binding = binding.replace('|', r"\|");
                if flags.contains(MapFlag::Command) {
                    binding = format!("<CMD>{}<CR>", binding);
                }
                let cmd = format!(
                    "{} <silent> {} {}",
                    cmd,
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
            silent,
            file_type,
        } in config.auto_commands
        {
            let vimscript = mut_or_default(&mut vimscript, &None);
            let triggers = triggers.join(",");
            let matching = matching.unwrap_or_else(|| {
                if file_type.is_some() {
                    "<buffer>".to_string()
                } else {
                    "*".to_string()
                }
            });
            let silent = if silent { "silent!" } else { "" };
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
                    vimscript.push(format!(
                        "autocmd {} {} {} {}",
                        triggers, matching, silent, cmd
                    ))
                } else {
                    vimscript.push(format!(
                        "autocmd {} {} {} if {} | execute '{}' | endif",
                        triggers,
                        matching,
                        silent,
                        condition,
                        cmd.replace('\'', r"\'")
                    ))
                }
            }
        }

        {
            // TODO implemnt file_type for set
            let global = mut_or_default(&mut vimscript, &None);

            for set in config.set {
                global.push(format!("set {}", set));
            }

            for (name, value) in config.set_value {
                global.push(format!(r#"set {}={}"#, name, value));
            }

            for (name, value) in config.r#let {
                global.push(format!("let {}={}", name, value));
            }
        }
    }

    for vimscript in vimscript {
        let ft_plugin_dir = nvim_dir.join("ftplugin");
        match vimscript {
            (None, vimscript) => {
                let plugin_dir = nvim_dir.join("plugin");
                fs::create_dir_all(&plugin_dir)?;

                fs::write(plugin_dir.join("config.vim"), vimscript.join("\n"))?;
            }
            (Some(file_type), vimscript) => {
                fs::create_dir_all(&ft_plugin_dir)?;

                fs::write(
                    ft_plugin_dir.join(file_type + "_config.vim"),
                    vimscript.join("\n"),
                )?;
            }
        }
    }
    Ok(())
}
