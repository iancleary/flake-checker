#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs::{read_to_string, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use clap::Parser;
use handlebars::Handlebars;
use serde::{Deserialize, Serialize};

/// A flake.lock checker for Nix projects.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// The path to the flake.lock file to check.
    #[clap(default_value = "flake.lock")]
    flake_lock_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("couldn't access flake.lock: {0}")]
    Io(#[from] std::io::Error),

    #[error("couldn't parse flake.lock: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Deserialize)]
struct Original {
    owner: Option<String>,
    repo: Option<String>,
    #[serde(alias = "type")]
    node_type: String,
    #[serde(alias = "ref")]
    git_ref: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Locked {
    #[serde(alias = "lastModified")]
    last_modified: i64,
    #[serde(alias = "narHash")]
    nar_hash: String,
    owner: Option<String>,
    repo: Option<String>,
    rev: Option<String>,
    #[serde(alias = "type")]
    node_type: String,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Input {
    String(String),
    List(Vec<String>),
}

// TODO: make this an enum rather than a struct
#[derive(Clone, Deserialize)]
struct Node {
    inputs: Option<HashMap<String, Input>>,
    locked: Option<Locked>,
    original: Option<Original>,
}

#[derive(Clone, Deserialize)]
struct FlakeLock {
    nodes: HashMap<String, Node>,
    root: String,
    version: usize,
}

trait Check {
    fn run(&self, flake_lock: &FlakeLock) -> Vec<Issue>;
}

struct Refs {
    allowed_refs: Vec<String>,
}

impl Check for Refs {
    fn run(&self, flake_lock: &FlakeLock) -> Vec<Issue> {
        let mut issues = vec![];
        let nixpkgs_deps = nixpkgs_deps(&flake_lock.nodes);
        for (name, dep) in nixpkgs_deps {
            if let Some(original) = &dep.original {
                if let Some(ref git_ref) = original.git_ref {
                    if !self.allowed_refs.contains(git_ref) {
                        issues.push(Issue {
                        kind: IssueKind::Disallowed,
                        message: format!("dependency `{name}` has a Git ref of `{git_ref}` which is not explicitly allowed"),
                    });
                    }
                }
            }
        }
        issues
    }
}

struct MaxAge {
    max_days: i64,
}

impl Check for MaxAge {
    fn run(&self, flake_lock: &FlakeLock) -> Vec<Issue> {
        let mut issues = vec![];
        let nixpkgs_deps = nixpkgs_deps(&flake_lock.nodes);
        for (name, dep) in nixpkgs_deps {
            if let Some(locked) = &dep.locked {
                let now_timestamp = Utc::now().timestamp();
                let diff = now_timestamp - locked.last_modified;
                let num_days_old = Duration::seconds(diff).num_days();

                if num_days_old > self.max_days {
                    issues.push(Issue {
                        kind: IssueKind::Outdated,
                        message: format!(
                            "dependency `{name}` is **{num_days_old}** days old, which is over the max of **{}**",
                            self.max_days
                        ),
                    });
                }
            }
        }
        issues
    }
}

#[derive(Deserialize)]
struct Config {
    allowed_refs: Vec<String>,
    max_days: i64,
}

fn check_flake_lock(flake_lock: &FlakeLock, config: &Config) -> Vec<Issue> {
    let mut is1 = (MaxAge {
        max_days: config.max_days,
    })
    .run(flake_lock);

    let mut is2 = (Refs {
        allowed_refs: config.allowed_refs.to_vec(),
    })
    .run(flake_lock);

    // TODO: find a more elegant way to concat results
    is1.append(&mut is2);
    is1
}

fn nixpkgs_deps(nodes: &HashMap<String, Node>) -> HashMap<String, Node> {
    // TODO: select based on locked.type="github" and original.repo="nixpkgs"
    nodes
        .iter()
        .filter(|(k, _)| k.starts_with("nixpkgs"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

// TODO: re-introduce logging
fn warn(path: &str, message: &str) {
    println!("::warning file={path}::{message}");
}

#[derive(Serialize)]
enum IssueKind {
    #[serde(rename = "disallowed")]
    Disallowed,
    #[serde(rename = "outdated")]
    Outdated,
}

#[derive(Serialize)]
struct Issue {
    kind: IssueKind,
    message: String,
}

struct Summary {
    issues: Vec<Issue>,
}

impl Summary {
    fn generate_markdown(&self) {
        let mut data = BTreeMap::new();
        data.insert("issues", &self.issues);
        let mut handlebars = Handlebars::new();
        handlebars
            .register_template_string("summary.md", include_str!("./templates/summary.md"))
            .expect("summary template not found");
        let summary_md = handlebars
            .render("summary.md", &data)
            .expect("markdown render error");
        let summary_md_filepath =
            std::env::var("GITHUB_STEP_SUMMARY").expect("summary markdown file not found");
        let mut summary_md_file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary_md_filepath)
            .expect("error creating/reading summary markdown file");
        summary_md_file
            .write_all(summary_md.as_bytes())
            .expect("error writing summary markdown to file");
    }
}

fn main() -> Result<(), Error> {
    let Cli { flake_lock_path } = Cli::parse();
    let flake_lock_path = flake_lock_path
        .as_path()
        .to_str()
        .expect("flake.lock file not found based on supplied path"); // TODO: handle this better
    let flake_lock_file = read_to_string(flake_lock_path)?;
    let flake_lock: FlakeLock = serde_json::from_str(&flake_lock_file)?;

    let config_file = include_str!("./policy.json");
    let config: Config =
        serde_json::from_str(config_file).expect("inline policy.json file is malformed");

    let issues = check_flake_lock(&flake_lock, &config);
    let summary = Summary { issues };
    summary.generate_markdown();

    Ok(())
}
