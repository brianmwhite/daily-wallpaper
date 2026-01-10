use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use inquire::Select;

#[derive(Clone, Copy, Debug)]
enum BumpKind {
    Major,
    Minor,
    Patch,
}

impl BumpKind {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "major" => Some(Self::Major),
            "minor" => Some(Self::Minor),
            "patch" => Some(Self::Patch),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Patch => "patch",
        }
    }
}

fn usage() -> &'static str {
    "Usage: cargo run --bin bump_version -- [major|minor|patch]\n\nIf no argument is provided, an interactive menu is shown."
}

fn select_bump_kind() -> Result<BumpKind, String> {
    let choices = ["major", "minor", "patch"];
    let selection = Select::new("Select version bump type:", choices.to_vec())
        .prompt()
        .map_err(|err| format!("Menu selection failed: {err}"))?;
    BumpKind::from_str(selection).ok_or_else(|| "Invalid selection".to_string())
}

fn read_package_version(cargo_toml: &Path) -> Result<String, String> {
    let contents = fs::read_to_string(cargo_toml)
        .map_err(|err| format!("Failed to read {}: {err}", cargo_toml.display()))?;

    let mut in_package = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[package]") {
            in_package = true;
            continue;
        }
        if in_package && trimmed.starts_with('[') && !trimmed.starts_with("[package]") {
            break;
        }
        if in_package {
            let before_comment = trimmed.splitn(2, '#').next().unwrap_or("");
            let mut parts = before_comment.splitn(2, '=');
            let key = parts.next().unwrap_or("").trim();
            if key == "version" {
                let value = parts.next().unwrap_or("").trim();
                let version = value.trim_matches('"');
                if version.is_empty() {
                    return Err("Version field is empty".to_string());
                }
                return Ok(version.to_string());
            }
        }
    }

    Err("Could not find package version in Cargo.toml".to_string())
}

fn bump_version(current: &str, kind: BumpKind) -> Result<String, String> {
    let parts: Vec<&str> = current.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("Invalid semver: {current}"));
    }

    let mut major = parts[0]
        .parse::<u64>()
        .map_err(|_| format!("Invalid semver: {current}"))?;
    let mut minor = parts[1]
        .parse::<u64>()
        .map_err(|_| format!("Invalid semver: {current}"))?;
    let mut patch = parts[2]
        .parse::<u64>()
        .map_err(|_| format!("Invalid semver: {current}"))?;

    match kind {
        BumpKind::Major => {
            major += 1;
            minor = 0;
            patch = 0;
        }
        BumpKind::Minor => {
            minor += 1;
            patch = 0;
        }
        BumpKind::Patch => {
            patch += 1;
        }
    }

    Ok(format!("{major}.{minor}.{patch}"))
}

fn update_cargo_toml(cargo_toml: &Path, new_version: &str) -> Result<(), String> {
    let contents = fs::read_to_string(cargo_toml)
        .map_err(|err| format!("Failed to read {}: {err}", cargo_toml.display()))?;

    let mut in_package = false;
    let mut updated = false;
    let mut output = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[package]") {
            in_package = true;
            output.push(line.to_string());
            continue;
        }
        if in_package && trimmed.starts_with('[') && !trimmed.starts_with("[package]") {
            in_package = false;
        }

        if in_package && !updated {
            let mut line_parts = line.splitn(2, '#');
            let before_comment = line_parts.next().unwrap_or("");
            let comment = line_parts.next();
            let mut assignment_parts = before_comment.splitn(2, '=');
            let key = assignment_parts.next().unwrap_or("").trim();
            if key == "version" {
                let indent: String = before_comment
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                let mut new_line = format!("{indent}version = \"{new_version}\"");
                if let Some(comment) = comment {
                    new_line.push_str(" #");
                    new_line.push_str(comment.trim_start());
                }
                output.push(new_line);
                updated = true;
                continue;
            }
        }

        output.push(line.to_string());
    }

    if !updated {
        return Err("Could not update version in Cargo.toml".to_string());
    }

    let updated_contents = output.join("\n") + "\n";
    fs::write(cargo_toml, updated_contents)
        .map_err(|err| format!("Failed to write {}: {err}", cargo_toml.display()))?;

    Ok(())
}

fn regenerate_lockfile() -> Result<(), String> {
    let status = Command::new("cargo")
        .arg("generate-lockfile")
        .status()
        .map_err(|err| format!("Failed to run cargo generate-lockfile: {err}"))?;

    if !status.success() {
        return Err(format!("cargo generate-lockfile failed with {status}"));
    }

    Ok(())
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();

    if args.len() > 1 {
        return Err(usage().to_string());
    }

    let kind = if let Some(arg) = args.pop() {
        if arg == "-h" || arg == "--help" {
            println!("{}", usage());
            return Ok(());
        }
        BumpKind::from_str(&arg).ok_or_else(|| usage().to_string())?
    } else {
        select_bump_kind()?
    };

    let cargo_toml = Path::new("Cargo.toml");
    let current = read_package_version(cargo_toml)?;
    let next = bump_version(&current, kind)?;

    update_cargo_toml(cargo_toml, &next)?;
    regenerate_lockfile()?;

    println!("Version bumped: {current} -> {next} ({})", kind.as_str());
    Ok(())
}

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("{err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
