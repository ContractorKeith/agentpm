use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const BASELINE: &str = "tests/comment-baseline.txt";

fn comment_lines(source: &str) -> Vec<usize> {
    let s: Vec<char> = source.chars().collect();
    let mut lines = Vec::new();
    let (mut i, mut line) = (0, 1);
    let mark = |lines: &mut Vec<usize>, line: usize| {
        if lines.last() != Some(&line) {
            lines.push(line);
        }
    };
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    while i < s.len() {
        let c = s[i];
        let next = s.get(i + 1).copied();
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c == '/' && next == Some('/') {
            mark(&mut lines, line);
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < s.len() {
                if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if s[i] == '\n' {
                        line += 1;
                    } else {
                        mark(&mut lines, line);
                    }
                    i += 1;
                }
            }
            mark(&mut lines, line);
        } else if c == '"' {
            i += 1;
            while i < s.len() && s[i] != '"' {
                if s[i] == '\\' {
                    i += 1;
                }
                if s.get(i) == Some(&'\n') {
                    line += 1;
                }
                i += 1;
            }
            i += 1;
        } else if c == 'r' && (i == 0 || !ident(s[i - 1]) || s[i - 1] == 'b' || s[i - 1] == 'c') {
            let mut j = i + 1;
            while s.get(j) == Some(&'#') {
                j += 1;
            }
            if s.get(j) != Some(&'"') {
                i += 1;
                continue;
            }
            let hashes = j - i - 1;
            i = j + 1;
            while i < s.len() {
                if s[i] == '"' && (1..=hashes).all(|k| s.get(i + k) == Some(&'#')) {
                    i += hashes + 1;
                    break;
                }
                if s[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
        } else if c == '\'' {
            if next == Some('\\') {
                i += 2;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if s.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    lines
}

fn rust_files(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                rust_files(&path, root, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

fn actual() -> BTreeMap<String, usize> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(root, root, &mut files);
    let mut counts = BTreeMap::new();
    for file in files {
        let source = fs::read_to_string(root.join(&file)).unwrap();
        let lines = comment_lines(&source);
        if std::env::var_os("AGENTPM_LIST_COMMENTS").is_some() {
            for n in &lines {
                let text = source.lines().nth(n - 1).unwrap_or_default().trim();
                println!("{}:{n}: {text}", file.display());
            }
        }
        if !lines.is_empty() {
            counts.insert(file.display().to_string(), lines.len());
        }
    }
    counts
}

fn baseline() -> BTreeMap<String, usize> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE);
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (count, file) = l.trim().split_once(' ').expect("`<count> <path>` per line");
            (file.to_string(), count.parse().expect("count"))
        })
        .collect()
}

#[test]
fn comments_only_ratchet_down() {
    let (actual, allowed) = (actual(), baseline());
    let mut problems = Vec::new();
    for (file, &n) in &actual {
        let max = allowed.get(file).copied().unwrap_or(0);
        if n > max {
            problems.push(format!(
                "{file}: {n} comment lines, {max} allowed. Comments are banned: put the reason in a name, a test or docs/adr/"
            ));
        }
    }
    for (file, &max) in &allowed {
        let n = actual.get(file).copied().unwrap_or(0);
        if n < max {
            problems.push(format!(
                "{file}: {n} comment lines, baseline still allows {max}; lower {BASELINE}"
            ));
        }
    }
    if std::env::var_os("AGENTPM_BLESS_COMMENTS").is_some()
        && problems.iter().all(|p| p.contains("lower"))
    {
        let body: String = actual.iter().map(|(f, n)| format!("{n} {f}\n")).collect();
        fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE), body).unwrap();
        return;
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn scanner_ignores_comment_markers_inside_literals() {
    let source = concat!(
        "let url = \"https://example.com\";\n",
        "let raw = r#\"a // b \"# ;\n",
        "let bytes = br\"/* x */\";\n",
        "let slash = '/'; let quote = '\\''; fn f<'a>(x: &'a str) {}\n",
        "let r#type = 1; let esc = \"\\\"//\";\n",
    );
    assert!(comment_lines(source).is_empty());
}

#[test]
fn scanner_finds_every_comment_form() {
    let source = concat!(
        "//! module\n",
        "/// item\n",
        "let x = 1; // trailing\n",
        "/* block\n",
        "   /* nested */ still */\n",
        "let y = 2;\n",
    );
    assert_eq!(comment_lines(source), vec![1, 2, 3, 4, 5]);
}
