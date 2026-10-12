//! Shell completions.
//!
//! The scripts are small: they call back into ruckus (`COMPLETE=<shell> ruckus -- <words>`)
//! on every Tab, so layout, environment and distro names complete from what exists right now.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;
use clap_complete::CompletionCandidate;
use clap_complete::env::{Bash, EnvCompleter, Fish, Zsh};

use crate::config::Config;
use crate::{env, install};

/// The variable the scripts set to ask ruckus for completions.
pub const VAR: &str = "COMPLETE";

#[derive(Clone, Copy, ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

/// Run from a file in `$fpath`, the zsh script is the body of `_ruckus` rather than sourced.
/// It then only registers itself, so this also completes on that first Tab.
const ZSH_AUTOLOAD: &str = r#"
if [ "$funcstack[1]" = "_ruckus" ]; then
    _clap_dynamic_completer_ruckus "$@"
fi
"#;

/// The completion script for `shell`.
pub fn script(shell: Shell) -> Result<String> {
    let completer: &dyn EnvCompleter = match shell {
        Shell::Zsh => &Zsh,
        Shell::Bash => &Bash,
        Shell::Fish => &Fish,
    };
    let mut buf = vec![];
    // Call back into `ruckus` on PATH, not this binary's path, so scripts generated at
    // release time work wherever the binary ends up.
    completer.write_registration(VAR, "ruckus", "ruckus", "ruckus", &mut buf)?;
    let mut script = String::from_utf8(buf)?;
    if let Shell::Zsh = shell {
        script.push_str(ZSH_AUTOLOAD);
    }
    Ok(script)
}

// Completion must never print an error mid-line, so anything unreadable just completes
// nothing. `--config` on the same line is not seen here; RUCKUS_CONFIG is.

pub fn layouts() -> Vec<CompletionCandidate> {
    let explicit = std::env::var_os("RUCKUS_CONFIG").map(PathBuf::from);
    let Ok(config) = Config::load(explicit.as_deref()) else {
        return vec![];
    };
    config
        .layouts
        .into_iter()
        .map(|(name, layout)| {
            let help = (!layout.description.is_empty()).then(|| layout.description.into());
            CompletionCandidate::new(name).help(help)
        })
        .collect()
}

pub fn envs() -> Vec<CompletionCandidate> {
    file_stems(env::dir().ok(), |path| {
        path.extension().is_some_and(|ext| ext == "toml")
    })
}

pub fn distros() -> Vec<CompletionCandidate> {
    file_stems(install::distros_dir().ok(), |path| {
        path.join("ruckus.toml").is_file()
    })
}

/// Sorted names of the entries in `dir` that pass `keep`, without parsing them, so one
/// broken file does not stop the others completing.
fn file_stems(
    dir: Option<PathBuf>,
    keep: impl Fn(&std::path::Path) -> bool,
) -> Vec<CompletionCandidate> {
    let Some(entries) = dir.and_then(|dir| fs::read_dir(dir).ok()) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| keep(path))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
        .collect();
    names.sort();
    names.into_iter().map(CompletionCandidate::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_call_back_into_ruckus() {
        for shell in Shell::value_variants() {
            let script = script(*shell).unwrap();
            assert!(script.contains("COMPLETE="), "{script}");
            assert!(script.contains("ruckus"), "{script}");
        }
        let zsh = script(Shell::Zsh).unwrap();
        assert!(zsh.starts_with("#compdef ruckus"), "{zsh}");
        assert!(zsh.contains("compdef _clap_dynamic_completer_ruckus ruckus"));
    }
}
