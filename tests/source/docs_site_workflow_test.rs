//! What may publish the documentation site, as `.github/workflows/` declares
//! it.
//!
//! The site is release tags only, and two prohibitions keep it that way: no
//! deploy runs for an untagged commit, and what is deployed is the tag
//! assembler's output and never the checked-out tree's book. The assembler's
//! own half is driven in `tests/docs_site_assembly_test.rs`. This half is
//! structural, because no test here can make GitHub fire a trigger or
//! evaluate a job condition: it parses the workflow YAML, and evaluates the
//! build job's `if:` with a reader of GitHub's expression syntax that knows
//! only the operators and functions that condition uses and fails on any
//! other.
//!
//! Scope: every `*.yml` directly under `.github/workflows/`. A deploy step
//! pulled in through a composite action or another repository's reusable
//! workflow is invisible here, and so is any repository setting, the
//! `github-pages` environment's branch policy included.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use saphyr::{LoadableYamlNode, YamlOwned};

const PAGES: &str = "pages.yml";
const RELEASE: &str = "release.yml";
const ASSEMBLER: &str = "scripts/assemble-docs-site.py";

fn workflow_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows")
}

fn load(file: &str) -> YamlOwned {
    let path = workflow_dir().join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    YamlOwned::load_from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{} holds no YAML document", path.display()))
}

fn workflow_files() -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(workflow_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".yml"))
        .collect();
    files.sort();
    files
}

fn keys(node: &YamlOwned) -> BTreeSet<String> {
    node.as_mapping()
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn jobs(workflow: &YamlOwned) -> Vec<(String, &YamlOwned)> {
    workflow
        .as_mapping_get("jobs")
        .and_then(YamlOwned::as_mapping)
        .expect("workflow has a jobs mapping")
        .iter()
        .map(|(k, v)| (k.as_str().unwrap().to_owned(), v))
        .collect()
}

fn job<'a>(workflow: &'a YamlOwned, name: &str) -> &'a YamlOwned {
    workflow
        .as_mapping_get("jobs")
        .and_then(|j| j.as_mapping_get(name))
        .unwrap_or_else(|| panic!("{PAGES} has no `{name}` job"))
}

fn steps(job: &YamlOwned) -> &[YamlOwned] {
    job.as_mapping_get("steps")
        .and_then(YamlOwned::as_sequence)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn str_at<'a>(node: &'a YamlOwned, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(node, |n, key| n.as_mapping_get(key))
        .and_then(YamlOwned::as_str)
}

fn needs(job: &YamlOwned) -> Vec<String> {
    match job.as_mapping_get("needs") {
        Some(n) if n.as_str().is_some() => vec![n.as_str().unwrap().to_owned()],
        Some(n) => n
            .as_sequence()
            .map(|s| {
                s.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The environment a job deploys to, written as a name or as a mapping
/// holding one.
fn environment(job: &YamlOwned) -> Option<&str> {
    let env = job.as_mapping_get("environment")?;
    env.as_str().or_else(|| str_at(env, &["name"]))
}

fn action_name(uses: &str) -> &str {
    uses.split('@').next().unwrap()
}

#[test]
fn the_site_deploys_after_a_release_run_completes_or_by_hand_and_on_nothing_else() {
    let pages = load(PAGES);
    let on = pages.as_mapping_get("on").expect("pages.yml declares `on`");
    assert_eq!(
        keys(on),
        BTreeSet::from(["workflow_dispatch".to_owned(), "workflow_run".to_owned()]),
        "pages.yml must be started only by a completed Release run or by hand. A push, \
         pull_request or schedule trigger deploys from a commit no release tag names"
    );

    let run = on.as_mapping_get("workflow_run").unwrap();
    let watched: Vec<&str> = run
        .as_mapping_get("workflows")
        .and_then(YamlOwned::as_sequence)
        .map(|s| s.iter().filter_map(YamlOwned::as_str).collect())
        .unwrap_or_default();
    let release = load(RELEASE);
    let release_name = str_at(&release, &["name"]).expect("release.yml has a name");
    assert_eq!(
        watched,
        [release_name],
        "pages.yml must watch the workflow release.yml names, and only that one"
    );
    let types: Vec<&str> = run
        .as_mapping_get("types")
        .and_then(YamlOwned::as_sequence)
        .map(|s| s.iter().filter_map(YamlOwned::as_str).collect())
        .unwrap_or_default();
    assert_eq!(types, ["completed"]);
}

/// A Release run's `workflow_run` payload, as the build job's condition reads
/// it, or a dispatch.
fn event(
    name: &str,
    conclusion: Option<&str>,
    trigger: Option<&str>,
    head_branch: Option<&str>,
) -> BTreeMap<String, String> {
    let mut context = BTreeMap::from([("github.event_name".to_owned(), name.to_owned())]);
    for (key, value) in [
        ("conclusion", conclusion),
        ("event", trigger),
        ("head_branch", head_branch),
    ] {
        if let Some(value) = value {
            context.insert(format!("github.event.workflow_run.{key}"), value.to_owned());
        }
    }
    context
}

#[test]
fn the_build_runs_for_a_successful_release_tag_push_or_a_dispatch_and_for_no_other_run() {
    let pages = load(PAGES);
    let condition = str_at(job(&pages, "build"), &["if"]).expect(
        "pages.yml's build job carries an `if:`; without one every completed Release run deploys",
    );

    let release = |conclusion, trigger, head_branch| {
        event(
            "workflow_run",
            Some(conclusion),
            Some(trigger),
            Some(head_branch),
        )
    };
    let cases: &[(&str, BTreeMap<String, String>, bool)] = &[
        (
            "a dispatch",
            event("workflow_dispatch", None, None, None),
            true,
        ),
        (
            "a release tag push that succeeded",
            release("success", "push", "v0.25.0"),
            true,
        ),
        (
            "a prerelease tag push that succeeded",
            release("success", "push", "v1.0.0-rc.1"),
            true,
        ),
        (
            "a release tag push that failed",
            release("failure", "push", "v0.25.0"),
            false,
        ),
        (
            "a release tag push that timed out",
            release("timed_out", "push", "v0.25.0"),
            false,
        ),
        (
            "a pull request run whose branch is shaped like a tag",
            release("success", "pull_request", "v0.25.0"),
            false,
        ),
        (
            "a pushed ref that is not a version",
            release("success", "push", "main"),
            false,
        ),
        (
            "a pushed ref that starts with v but has no dot",
            release("success", "push", "vendor-bump"),
            false,
        ),
        (
            "a pushed ref with a dot that does not start with v",
            release("success", "push", "release-1.2"),
            false,
        ),
    ];

    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(label, context, expected)| {
            let ran = truthy(&evaluate(condition, context));
            (ran != *expected)
                .then(|| format!("{label}: build {}", if ran { "runs" } else { "is skipped" }))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "pages.yml's build job condition `{condition}` decides these runs wrongly:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_deploy_runs_only_behind_the_gated_build() {
    let pages = load(PAGES);
    let deployers: Vec<String> = jobs(&pages)
        .into_iter()
        .filter(|(_, j)| environment(j) == Some("github-pages"))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        deployers,
        ["deploy"],
        "pages.yml should deploy from exactly one job, `deploy`"
    );
    let deploy = job(&pages, "deploy");
    assert_eq!(needs(deploy), ["build"]);
    assert!(
        deploy.as_mapping_get("if").is_none(),
        "pages.yml's deploy job carries its own `if:`. It is skipped whenever the build job \
         is only while it has none, since a job runs after a skipped dependency only when its \
         condition says so"
    );
}

#[test]
fn what_is_deployed_is_the_assemblers_output() {
    let pages = load(PAGES);
    let build = steps(job(&pages, "build"));

    let assembles: Vec<&str> = build
        .iter()
        .filter_map(|s| str_at(s, &["run"]))
        .filter(|run| run.contains(ASSEMBLER))
        .collect();
    let [assemble] = assembles.as_slice() else {
        panic!("pages.yml's build job should run {ASSEMBLER} once, found {assembles:?}");
    };
    let out = assemble
        .split_once("--out ")
        .map(|(_, rest)| match rest.trim_start().strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap(),
            None => rest.split_whitespace().next().unwrap_or_default(),
        })
        .unwrap_or_else(|| panic!("{ASSEMBLER} is run without --out: {assemble}"));

    let uploads: Vec<&str> = build
        .iter()
        .filter(|s| str_at(s, &["uses"]).map(action_name) == Some("actions/upload-pages-artifact"))
        .map(|s| str_at(s, &["with", "path"]).expect("upload-pages-artifact names a path"))
        .collect();
    assert_eq!(
        uploads,
        [out],
        "pages.yml must upload exactly the directory {ASSEMBLER} writes; anything else \
         publishes a book the assembler did not build from a tag"
    );

    let builds_a_book: Vec<&str> = build
        .iter()
        .filter_map(|s| str_at(s, &["run"]))
        .filter(|run| run.contains("mdbook build"))
        .collect();
    assert!(
        builds_a_book.is_empty(),
        "pages.yml builds a book itself, which reads the checked-out tree: {builds_a_book:?}"
    );
}

#[test]
fn no_other_workflow_publishes_to_pages() {
    let files = workflow_files();
    assert!(
        files.iter().any(|f| f == PAGES) && files.iter().any(|f| f == RELEASE),
        "the workflow listing missed pages.yml or release.yml: {files:?}"
    );

    let pages_actions = ["actions/deploy-pages", "actions/upload-pages-artifact"];
    let mut offenders = Vec::new();
    for file in files.iter().filter(|f| *f != PAGES) {
        let workflow = load(file);
        for (name, job) in jobs(&workflow) {
            if environment(job) == Some("github-pages") {
                offenders.push(format!("{file}: job `{name}` deploys to github-pages"));
            }
            if str_at(job, &["uses"]).is_some_and(|u| u.ends_with(PAGES)) {
                offenders.push(format!("{file}: job `{name}` calls {PAGES}"));
            }
            for step in steps(job) {
                if let Some(uses) = str_at(step, &["uses"]) {
                    if pages_actions.contains(&action_name(uses)) {
                        offenders.push(format!("{file}: job `{name}` uses {uses}"));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "only pages.yml may publish the site, under the conditions the tests above pin:\n  {}",
        offenders.join("\n  ")
    );
}

/// A value of GitHub's expression language, as far as the conditions read
/// here need one.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Str(String),
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Str(s) => !s.is_empty(),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Word(String),
    Op(&'static str),
}

fn tokenize(expression: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = expression.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '\'' {
            chars.next();
            let mut s = String::new();
            loop {
                match chars.next() {
                    Some('\'') if chars.peek() == Some(&'\'') => {
                        chars.next();
                        s.push('\'');
                    }
                    Some('\'') => break,
                    Some(other) => s.push(other),
                    None => panic!("unterminated string in `{expression}`"),
                }
            }
            tokens.push(Token::Str(s));
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    word.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Word(word));
        } else {
            let two: String = chars.clone().take(2).collect();
            let op = ["==", "!=", "&&", "||"]
                .into_iter()
                .find(|op| *op == two)
                .or_else(|| {
                    ["(", ")", ",", "!"]
                        .into_iter()
                        .find(|op| op.starts_with(c))
                })
                .unwrap_or_else(|| {
                    panic!("this reader does not know `{c}` in `{expression}`; extend it")
                });
            for _ in 0..op.len() {
                chars.next();
            }
            tokens.push(Token::Op(op));
        }
    }
    tokens
}

struct Reader<'a> {
    tokens: Vec<Token>,
    at: usize,
    context: &'a BTreeMap<String, String>,
}

impl Reader<'_> {
    fn peek_op(&self, op: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token::Op(o)) if *o == op)
    }

    fn expect_op(&mut self, op: &str) {
        assert!(self.peek_op(op), "expected `{op}` at token {}", self.at);
        self.at += 1;
    }

    fn or(&mut self) -> Value {
        let mut left = self.and();
        while self.peek_op("||") {
            self.at += 1;
            let right = self.and();
            left = if truthy(&left) { left } else { right };
        }
        left
    }

    fn and(&mut self) -> Value {
        let mut left = self.unary();
        while self.peek_op("&&") {
            self.at += 1;
            let right = self.unary();
            left = if truthy(&left) { right } else { left };
        }
        left
    }

    fn unary(&mut self) -> Value {
        if self.peek_op("!") {
            self.at += 1;
            return Value::Bool(!truthy(&self.unary()));
        }
        let left = self.primary();
        for (op, equal) in [("==", true), ("!=", false)] {
            if self.peek_op(op) {
                self.at += 1;
                let right = self.primary();
                return Value::Bool(loosely_equal(&left, &right) == equal);
            }
        }
        left
    }

    fn primary(&mut self) -> Value {
        let token = self
            .tokens
            .get(self.at)
            .cloned()
            .expect("expression ended early");
        self.at += 1;
        match token {
            Token::Op("(") => {
                let inner = self.or();
                self.expect_op(")");
                inner
            }
            Token::Str(s) => Value::Str(s),
            Token::Word(w) if w == "true" || w == "false" => Value::Bool(w == "true"),
            Token::Word(w) if w == "null" => Value::Null,
            Token::Word(w) if self.peek_op("(") => {
                self.at += 1;
                let mut args = vec![self.or()];
                while self.peek_op(",") {
                    self.at += 1;
                    args.push(self.or());
                }
                self.expect_op(")");
                call(&w, &args)
            }
            Token::Word(w) => self
                .context
                .get(&w)
                .map(|v| Value::Str(v.clone()))
                .unwrap_or(Value::Null),
            other => panic!("this reader does not know {other:?}; extend it"),
        }
    }
}

/// GitHub compares strings ignoring case.
fn loosely_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Str(a), Value::Str(b)) => a.eq_ignore_ascii_case(b),
        _ => left == right,
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.to_ascii_lowercase(),
    }
}

/// GitHub's string functions, which ignore case too.
fn call(function: &str, args: &[Value]) -> Value {
    let [haystack, needle] = args else {
        panic!("{function} takes two arguments here, got {args:?}");
    };
    let (haystack, needle) = (text(haystack), text(needle));
    Value::Bool(match function {
        "startsWith" => haystack.starts_with(&needle),
        "contains" => haystack.contains(&needle),
        other => panic!("this reader does not know the function {other}; extend it"),
    })
}

fn evaluate(condition: &str, context: &BTreeMap<String, String>) -> Value {
    let expression = condition
        .trim()
        .strip_prefix("${{")
        .and_then(|e| e.strip_suffix("}}"))
        .unwrap_or(condition);
    let mut reader = Reader {
        tokens: tokenize(expression),
        at: 0,
        context,
    };
    let value = reader.or();
    assert_eq!(
        reader.at,
        reader.tokens.len(),
        "this reader stopped before the end of `{expression}`"
    );
    value
}

#[test]
fn the_condition_reader_follows_github_semantics_on_what_the_build_job_uses() {
    let context = BTreeMap::from([
        ("github.event_name".to_owned(), "workflow_run".to_owned()),
        ("a.tag".to_owned(), "V1.2.3".to_owned()),
    ]);
    let holds = |e: &str| truthy(&evaluate(e, &context));
    assert!(holds("github.event_name == 'WORKFLOW_RUN'"));
    assert!(!holds("github.event_name != 'workflow_run'"));
    assert!(holds("startsWith(a.tag, 'v') && contains(a.tag, '.')"));
    assert!(!holds("startsWith(a.tag, 'x') && contains(a.tag, '.')"));
    assert!(holds("false || (true && !(missing.key == 'x'))"));
    assert!(!holds("missing.key"));
    assert!(holds("${{ 'it''s' == 'IT''S' }}"));
}
