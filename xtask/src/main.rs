#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::{error::Error, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    let task = std::env::args().nth(1);
    if task.as_deref() == Some("licenses") {
        let target = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "x86_64-pc-windows-msvc".into());
        if target.is_empty()
            || !target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err("Invalid Cargo target".into());
        }
        let output = Command::new("cargo")
            .args([
                "tree",
                "--workspace",
                "--all-features",
                "--target",
                &target,
                "-e",
                "normal",
                "--prefix",
                "none",
                "--format",
                "{p}|{l}",
            ])
            .output()?;
        if !output.status.success() {
            return Err("cargo tree failed".into());
        }
        let tree = String::from_utf8(output.stdout)?;
        let mut issues: Vec<String> = tree
            .lines()
            .filter_map(|line| {
                let (package, license) = line.split_once('|')?;
                let package = package.split(" (").next().unwrap_or(package);
                let license = license.trim().trim_end_matches(" (*)").trim();
                if license_allowed(license) {
                    None
                } else {
                    Some(format!(
                        "{package}: {}",
                        if license.is_empty() {
                            "no license declared"
                        } else {
                            license
                        }
                    ))
                }
            })
            .collect();
        issues.sort();
        issues.dedup();
        if !issues.is_empty() {
            for issue in &issues {
                eprintln!("{issue}");
            }
            return Err("Missing or non-allowlisted license identifiers; review required".into());
        }
        println!("Dependency license expressions passed for {target}");
        return Ok(());
    }
    if task.as_deref() != Some("layers") {
        return Err("Usage: cargo xtask layers | licenses [target]".into());
    }
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    check_layers(&metadata)?;
    println!("TradeCraft workspace dependency layers passed");
    Ok(())
}

fn license_allowed(expression: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "ISC",
        "MIT",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "Apache-2.0",
        "Unicode-3.0",
        "OFL-1.1",
        "Ubuntu-font-1.0",
        "Bitstream-Vera",
        "CC0-1.0",
        "CC-BY-4.0",
        "CC-BY-SA-4.0",
        "LicenseRef-Public-Domain",
    ];
    let mut found = false;
    for word in expression
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .filter(|word| !word.is_empty())
    {
        if word == "OR" || word == "AND" {
            continue;
        }
        if !ALLOWED.contains(&word) {
            return false;
        }
        found = true;
    }
    found
}

fn check_layers(metadata: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    let packages = metadata
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or("missing package metadata")?;
    let ranks = [
        ("tradecraft-core", 0),
        ("tradecraft-providers", 1),
        ("tradecraft-engine", 2),
        ("tradecraft-ui-egui", 3),
        ("tradecraft", 4),
    ];
    for package in packages {
        let Some(name) = package.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some((_, rank)) = ranks.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        let dependencies = package
            .get("dependencies")
            .and_then(|v| v.as_array())
            .ok_or("missing dependency metadata")?;
        for dependency in dependencies {
            let Some(dep) = dependency.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if name != "tradecraft-ui-egui"
                && matches!(
                    dep,
                    "egui" | "eframe" | "egui-charts" | "winit" | "rfd" | "wgpu"
                )
            {
                return Err(format!("{name} must not depend on UI dependency {dep}").into());
            }
            if let Some((_, dep_rank)) = ranks.iter().find(|(n, _)| *n == dep)
                && dep_rank >= rank
            {
                return Err(format!(
                    "{name} cannot depend on {dep}: dependencies must point down the layers"
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_layers, license_allowed};
    use serde_json::json;

    #[test]
    fn rejects_ui_dependencies_and_upward_edges() {
        let bad_toolkit =
            json!({"packages":[{"name":"tradecraft-core", "dependencies":[{"name":"egui"}]}]});
        let bad_edge = json!({"packages":[{"name":"tradecraft-providers", "dependencies":[{"name":"tradecraft-engine"}]}]});
        let allowed = json!({"packages":[{"name":"tradecraft-engine", "dependencies":[{"name":"tradecraft-core"}, {"name":"tradecraft-providers"}]}]});
        assert!(check_layers(&bad_toolkit).is_err());
        assert!(check_layers(&bad_edge).is_err());
        assert!(check_layers(&allowed).is_ok());
    }
    #[test]
    fn license_check_requires_all_identifiers_to_be_allowed() {
        assert!(license_allowed("MIT OR Apache-2.0"));
        assert!(license_allowed("(MIT OR Apache-2.0) AND OFL-1.1"));
        assert!(!license_allowed("MIT OR Zlib"));
        assert!(!license_allowed(""));
    }
}
