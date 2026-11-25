use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, Write};
use std::sync::LazyLock;

use regex::Regex;
use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
use serde::Deserialize;
use serde_json::json;

const SESSION: &str = "00000000-0000-4000-8000-000000000000";
static SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}|sk-[A-Za-z0-9_-]{20,}",
        r"|AIza[0-9A-Za-z_-]{30,}|gh[pousr]_[A-Za-z0-9]{36,}",
        r"|github_pat_[A-Za-z0-9_]{22,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}",
        r"|xox[baprs]-[A-Za-z0-9-]{10,}",
        r"|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}",
        r"|-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"
    ))
    .expect("static credential pattern")
});

pub fn has_secret(value: &str) -> bool {
    SECRET.is_match(value)
}

pub(super) fn mask_credentials(text: &str) -> Cow<'_, str> {
    SECRET.replace_all(text, "[credential omitted]")
}

#[derive(Default, Deserialize)]
pub struct Facts {
    pub homes: Vec<String>,
    pub session: String,
    #[serde(default)]
    pub member_sessions: Vec<(String, String)>,
    pub identifiers: Vec<(String, String)>,
    pub terms: Vec<String>,
}

fn forms(term: &str) -> [String; 6] {
    let quoted = serde_json::to_string(term).expect("JSON string");
    let body = &quoted[1..quoted.len() - 1];
    let mut ascii = String::new();
    for c in body.chars() {
        if c < '\u{7f}' {
            ascii.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(ascii, "\\u{unit:04x}").unwrap();
            }
        }
    }
    let mut url = String::new();
    for b in term.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            url.push(char::from(b));
        } else {
            write!(url, "%{b:02X}").unwrap();
        }
    }
    [
        term.into(),
        body.into(),
        ascii,
        term.replace('/', "\\/"),
        url,
        term.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    ]
}

fn encoded_scalar(text: &str) -> Option<(char, usize)> {
    if text.starts_with("\\u") {
        let unit = |at| {
            let digits = text.get(at..at + 4)?;
            digits
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
                .then_some(())?;
            u16::from_str_radix(digits, 16).ok()
        };
        let first = unit(2)?;
        let (scalar, len) = if (0xd800..=0xdbff).contains(&first) {
            if text.get(6..8)? != "\\u" {
                return None;
            }
            let second = unit(8)?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return None;
            }
            (
                0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00,
                12,
            )
        } else {
            (u32::from(first), 6)
        };
        return Some((char::from_u32(scalar)?, len));
    }
    if text.starts_with('%') {
        let byte = |at| {
            if text.as_bytes().get(at) != Some(&b'%') {
                return None;
            }
            let digits = text.get(at + 1..at + 3)?;
            digits
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
                .then_some(())?;
            u8::from_str_radix(digits, 16).ok()
        };
        let first = byte(0)?;
        let len = match first {
            0x00..=0x7f => return Some((char::from(first), 3)),
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return None,
        };
        let mut bytes = [0; 4];
        bytes[0] = first;
        for (i, b) in bytes.iter_mut().enumerate().take(len).skip(1) {
            *b = byte(i * 3)?;
        }
        let c = std::str::from_utf8(&bytes[..len]).ok()?.chars().next()?;
        return Some((c, len * 3));
    }
    None
}

fn scalars(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut at = 0;
    let mut escaped_backslash = false;
    std::iter::from_fn(move || {
        let tail = &text[at..];
        let (c, len) = if escaped_backslash {
            escaped_backslash = false;
            ('\\', 1)
        } else if tail.starts_with("\\\\") {
            escaped_backslash = true;
            ('\\', 1)
        } else {
            encoded_scalar(tail).or_else(|| tail.chars().next().map(|c| (c, c.len_utf8())))?
        };
        let start = at;
        at += len;
        Some((start, c))
    })
}

struct Literal {
    folded: Box<str>,
    rule: usize,
    left: bool,
    right: bool,
}

#[derive(Default)]
struct ByLength {
    power: u64,
    literals: HashMap<u64, Vec<Literal>>,
}

fn step(hash: u64, base: u64, c: char) -> u64 {
    hash.wrapping_mul(base).wrapping_add(c as u64 + 1)
}

fn boundary_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, 'ı' | 'İ' | 'ſ' | 'K')
}

pub struct Replacements {
    lengths: BTreeMap<usize, ByLength>,
    base: u64,
    starts: HashMap<char, bool>,
    fold: HashMap<char, char>,
    rules: Vec<(String, String)>,
    homes: Vec<String>,
    unreplaced_accounts: BTreeSet<String>,
}

impl Replacements {
    pub fn new(facts: &Facts, kept: &[String]) -> Result<Self, &'static str> {
        let mut r = Self {
            lengths: BTreeMap::new(),
            starts: HashMap::new(),
            base: RandomState::new().hash_one(0u8) | 257,
            fold: HashMap::new(),
            rules: Vec::new(),
            homes: Vec::new(),
            unreplaced_accounts: BTreeSet::new(),
        };
        for home in facts.homes.iter().collect::<BTreeSet<_>>() {
            let drive = home.as_bytes().get(1) == Some(&b':');
            let windows = drive || home.contains('\\') || home.starts_with("//");
            let separator = |c| c == '/' || (windows && c == '\\');
            let path = home.trim_end_matches(separator);
            let tail = if drive { &path[2..] } else { path };
            if windows
                && tail.starts_with(['/', '\\'])
                && tail.chars().nth(1).is_some_and(separator)
                && tail.split(separator).filter(|s| !s.is_empty()).count() <= 2
            {
                continue;
            }
            let user = tail.rsplit(separator).next().unwrap_or("");
            if matches!(
                user.to_lowercase().as_str(),
                "" | "root"
                    | "user"
                    | "admin"
                    | "ubuntu"
                    | "debian"
                    | "ec2-user"
                    | "runner"
                    | "vscode"
                    | "codespace"
                    | "node"
            ) {
                continue;
            }
            let n = user.chars().count();
            let account: String = "user".chars().cycle().take(n).collect();
            let standin = format!("{}{account}", &path[..path.len() - user.len()]);
            r.add(path, &standin);
            if windows {
                r.add(&path.replace('\\', "/"), &standin.replace('\\', "/"));
                r.add(&path.replace('/', "\\"), &standin.replace('/', "\\"));
            }
            r.homes.push(standin);
            if n >= 4 {
                r.add(user, &account);
            } else {
                r.unreplaced_accounts.insert(user.into());
            }
        }
        r.add(&facts.session, SESSION);
        for (original, label) in &facts.member_sessions {
            r.add(original, label);
        }
        for (original, label) in &facts.identifiers {
            if original.chars().count() >= 8 {
                r.add(original, label);
            }
        }
        let secrets: BTreeSet<_> = kept
            .iter()
            .flat_map(|s| SECRET.find_iter(s).map(|m| m.as_str()))
            .collect();
        for (i, s) in secrets.into_iter().enumerate() {
            r.add(s, &format!("[secret-{}]", i + 1));
        }
        for (i, s) in facts.terms.iter().enumerate() {
            r.add(s, &format!("[term-{}]", i + 1));
        }
        Ok(r)
    }

    fn fold_key(&mut self, c: char) -> char {
        if let Some(&key) = self.fold.get(&c) {
            return key;
        }
        if matches!(c, 'I' | 'i' | 'ı' | 'İ') {
            self.fold
                .extend(['I', 'i', 'ı', 'İ'].map(|variant| (variant, 'i')));
            return 'i';
        }
        let mut class = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
        class.case_fold_simple();
        let key = class.ranges()[0].start();
        for range in class.ranges() {
            for variant in range.start()..=range.end() {
                self.fold.insert(variant, key);
            }
        }
        key
    }

    fn add(&mut self, original: &str, label: &str) {
        if original.is_empty() || original.to_lowercase() == label.to_lowercase() {
            return;
        }
        let id = self.rules.len();
        self.rules.push((original.into(), label.into()));
        let forms = forms(original);
        for (i, form) in forms.iter().enumerate() {
            if forms[..i].contains(form) {
                continue;
            }
            let mut folded = String::with_capacity(form.len());
            let (mut hash, mut power, mut len) = (0, 1u64, 0);
            for (_, c) in scalars(form) {
                let key = self.fold_key(c);
                folded.push(key);
                hash = step(hash, self.base, key);
                power = power.wrapping_mul(self.base);
                len += 1;
            }
            let left = folded.starts_with(boundary_char);
            if let Some(first) = folded.chars().next() {
                *self.starts.entry(first).or_insert(true) &= left;
            }
            let group = self.lengths.entry(len).or_insert_with(|| ByLength {
                power,
                ..ByLength::default()
            });
            let bucket = group.literals.entry(hash).or_default();
            if !bucket.iter().any(|p| p.folded.as_ref() == folded) {
                bucket.push(Literal {
                    right: folded.ends_with(boundary_char),
                    folded: folded.into(),
                    rule: id,
                    left,
                });
            }
        }
    }

    fn matches<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = (usize, usize, &'a str, &'a str)> {
        let mut index = Vec::new();
        let mut folded = String::new();
        let mut hash = 0;
        if !self.lengths.is_empty() {
            for (at, c) in scalars(text) {
                index.push((at, folded.len(), hash));
                let key = self.fold.get(&c).copied().unwrap_or(c);
                folded.push(key);
                hash = step(hash, self.base, key);
            }
        }
        let count = index.len();
        index.push((text.len(), folded.len(), hash));
        let mut pos = 0;
        std::iter::from_fn(move || {
            while pos < count {
                let start = pos;
                pos += 1;
                let (at, a, prefix) = index[start];
                let Some(left) = folded[a..].chars().next().and_then(|c| self.starts.get(&c))
                else {
                    continue;
                };
                if *left && folded[..a].ends_with(boundary_char) {
                    continue;
                }
                for (&len, group) in self.lengths.range(..=count - start).rev() {
                    let (end, b, suffix) = index[start + len];
                    let hash = suffix.wrapping_sub(prefix.wrapping_mul(group.power));
                    if let Some(bucket) = group.literals.get(&hash) {
                        for p in bucket {
                            if !(p.left && folded[..a].ends_with(boundary_char)
                                || p.right && folded[b..].starts_with(boundary_char))
                                && *p.folded == folded[a..b]
                            {
                                pos = start + len;
                                let (original, label) = &self.rules[p.rule];
                                return Some((at, end, original.as_str(), label.as_str()));
                            }
                        }
                    }
                }
            }
            None
        })
    }

    pub fn text(&self, value: &str) -> String {
        self.replace(value, |_, _, _, _| {})
    }

    fn replace(&self, value: &str, mut span: impl FnMut(usize, usize, &str, &str)) -> String {
        let mut result = String::new();
        let mut at = 0;
        for (start, end, original, label) in self.matches(value) {
            span(start, end, original, label);
            result.push_str(&value[at..start]);
            result.push_str(label);
            at = end;
        }
        result.push_str(&value[at..]);
        result
    }

    pub fn has_residue(&self, value: &str) -> bool {
        self.matches(value).next().is_some() || has_secret(value)
    }
}

#[derive(Deserialize)]
struct Init {
    facts: Facts,
    kept: Vec<String>,
}

/// Serves a local JSONL pipe. Errors never contain request data.
pub fn serve(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut lines = input.lines();
    let invalid = || io::Error::other("invalid evidence-redact request");
    let Some(line) = lines.next() else {
        return Ok(());
    };
    let init: Init = serde_json::from_str(&line?).map_err(|_| invalid())?;
    let r = Replacements::new(&init.facts, &init.kept).map_err(|_| invalid())?;
    let mut labels = BTreeMap::new();
    for (original, label) in &r.rules {
        labels.entry(original).or_insert(label);
    }
    writeln!(
        output,
        "{}",
        json!({"labels":labels,"homes":r.homes,"unreplaced_accounts":r.unreplaced_accounts,"session":SESSION})
    )?;
    output.flush()?;
    for line in lines {
        let value: String = serde_json::from_str(&line?).map_err(|_| invalid())?;
        let mut spans = Vec::new();
        let (mut byte, mut cp) = (0, 0);
        let text = r.replace(&value, |start, end, original, label| {
            let a = cp + value[byte..start].chars().count();
            cp = a + value[start..end].chars().count();
            byte = end;
            spans.push((a, cp, original.to_owned(), label.to_owned()));
        });
        writeln!(
            output,
            "{}",
            json!({"text":text,"spans":spans,"secret":has_secret(&value)})
        )?;
        output.flush()?;
    }
    Ok(())
}
