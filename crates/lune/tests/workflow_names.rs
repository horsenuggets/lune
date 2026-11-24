#![allow(clippy::cargo_common_metadata)]

// Pin display names independently of compiler targets, runners, and release artifacts.

use std::collections::BTreeSet;

const CI: &str = include_str!("../../../.github/workflows/ci.yaml");
const CHECKS: &str = include_str!("../../../.github/workflows/release-checks.yaml");
const RELEASE: &str = include_str!("../../../.github/workflows/release.yaml");
const TERRAFORM: &str = include_str!("../../../terraform/Main.tf");
const PLATFORMS: &[(&str, &str, &str, &str)] = &[
    (
        "Windows x64",
        "windows-2022",
        "x86_64-pc-windows-msvc",
        "windows-x64",
    ),
    (
        "Windows arm64",
        "windows-11-arm",
        "aarch64-pc-windows-msvc",
        "windows-arm64",
    ),
    (
        "Linux x64",
        "ubuntu-22.04",
        "x86_64-unknown-linux-gnu",
        "linux-x64",
    ),
    (
        "Linux arm64",
        "ubuntu-22.04-arm",
        "aarch64-unknown-linux-gnu",
        "linux-arm64",
    ),
    ("macOS x64", "macos-15", "x86_64-apple-darwin", "macos-x64"),
    (
        "macOS arm64",
        "macos-15",
        "aarch64-apple-darwin",
        "macos-arm64",
    ),
];

fn matrix_rows(workflow: &str) -> Vec<Vec<(&str, &str)>> {
    let mut rows: Vec<Vec<(&str, &str)>> = Vec::new();
    let mut in_matrix = false;
    for line in workflow.lines() {
        if let Some(name) = line.strip_prefix("          - name: ") {
            rows.push(vec![("name", name)]);
            in_matrix = true;
        } else if in_matrix {
            if let Some(field) = line.strip_prefix("            ") {
                let (key, value) = field.split_once(": ").expect("matrix scalar field");
                rows.last_mut().unwrap().push((key, value));
            } else if !line.trim().is_empty() {
                in_matrix = false;
            }
        }
    }
    rows
}

fn expanded_names(workflow: &str) -> Vec<String> {
    let rows = matrix_rows(workflow);
    let mut names = Vec::new();
    for line in workflow.lines() {
        if let Some(name) = line.strip_prefix("    name: ") {
            if name.contains("${{ matrix.name }}") {
                assert_ne!(rows, [] as [Vec<(&str, &str)>; 0]);
                for row in &rows {
                    names.push(name.replace("${{ matrix.name }}", row[0].1));
                }
            } else {
                names.push(name.to_owned());
            }
        }
    }
    assert_eq!(names.len(), names.iter().collect::<BTreeSet<_>>().len());
    assert!(names.iter().all(|name| !name.contains("${{")));
    names
}

fn required_checks(terraform: &str, branch: &str) -> Vec<String> {
    let marker = format!("{branch}_checks = [");
    let list = terraform
        .split_once(&marker)
        .unwrap()
        .1
        .split_once(']')
        .unwrap()
        .0;
    let checks: Vec<_> = list
        .lines()
        .filter_map(|line| {
            line.trim()
                .trim_end_matches(',')
                .strip_prefix('"')?
                .strip_suffix('"')
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(checks.len(), checks.iter().collect::<BTreeSet<_>>().len());
    checks
}

#[test]
fn canonical_names_preserve_every_platform_tuple() {
    for (workflow, prefix, artifacts) in [
        (CI, "CI - ", false),
        (CHECKS, "Build test - ", false),
        (RELEASE, "Build - ", true),
    ] {
        let expected: Vec<_> = PLATFORMS
            .iter()
            .map(|&(name, runner, target, suffix)| {
                let mut row = vec![
                    ("name".to_owned(), name.to_owned()),
                    ("runner-os".to_owned(), runner.to_owned()),
                ];
                if artifacts {
                    row.push((
                        "artifact-name".to_owned(),
                        format!("lune-${{{{ needs.init.outputs.version }}}}-{suffix}"),
                    ));
                }
                row.push(("cargo-target".to_owned(), target.to_owned()));
                row
            })
            .collect();
        let actual: Vec<Vec<_>> = matrix_rows(workflow)
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect()
            })
            .collect();
        assert_eq!(actual, expected);
        let names = expanded_names(workflow);
        let matrix_names: Vec<_> = names
            .iter()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect();
        assert_eq!(
            matrix_names,
            PLATFORMS
                .iter()
                .map(|row| format!("{prefix}{}", row.0))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn terraform_matches_actual_pr_job_names_on_both_branches() {
    for branch in ["main", "release"] {
        let mut actual = expanded_names(CI);
        if branch == "release" {
            actual.extend(expanded_names(CHECKS));
        }
        assert_eq!(actual.len(), actual.iter().collect::<BTreeSet<_>>().len());
        let mut required = required_checks(TERRAFORM, branch);
        actual.sort();
        required.sort();
        assert_eq!(actual, required);
    }
}
