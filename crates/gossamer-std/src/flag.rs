//! Runtime support for `std::flag` — batteries-included CLI parsing.
//! GNU-style long + short flags (`--verbose`, `-v`), equals-form
//! (`--port=8080`), value-follows (`--port 8080`), bool flags,
//! `--`-terminator, auto-generated `--help`, and friendly error
//! messages. Integrates with [`crate::errors::Error`].

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::errors::Error;

/// Underlying storage kind for a flag.
#[derive(Debug, Clone)]
enum Value {
    String(Rc<RefCell<String>>),
    Int(Rc<RefCell<i64>>),
    Uint(Rc<RefCell<u64>>),
    Float(Rc<RefCell<f64>>),
    Bool(Rc<RefCell<bool>>),
    Duration(Rc<RefCell<Duration>>),
    StringList(Rc<RefCell<Vec<String>>>),
}

#[derive(Debug, Clone)]
struct Definition {
    name: String,
    short: Option<char>,
    summary: String,
    value: Value,
}

/// A configured flag set.
///
/// Each `T::<type>(...)` method returns an `Rc<RefCell<T>>`-shaped
/// handle, making the flag's *current* value readable at any later
/// point.
pub struct Set {
    program: String,
    defs: Vec<Definition>,
}

impl Set {
    /// Constructs a new, empty flag set tagged with the program name
    /// for `--help` output.
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            defs: Vec::new(),
        }
    }

    /// Registers a `--name VALUE` string flag with a default value.
    pub fn string(
        &mut self,
        name: &str,
        default: impl Into<String>,
        summary: impl Into<String>,
    ) -> Rc<RefCell<String>> {
        let cell = Rc::new(RefCell::new(default.into()));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::String(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers a signed-integer flag.
    pub fn int(
        &mut self,
        name: &str,
        default: i64,
        summary: impl Into<String>,
    ) -> Rc<RefCell<i64>> {
        let cell = Rc::new(RefCell::new(default));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::Int(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers an unsigned-integer flag.
    pub fn uint(
        &mut self,
        name: &str,
        default: u64,
        summary: impl Into<String>,
    ) -> Rc<RefCell<u64>> {
        let cell = Rc::new(RefCell::new(default));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::Uint(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers a 64-bit float flag.
    pub fn float(
        &mut self,
        name: &str,
        default: f64,
        summary: impl Into<String>,
    ) -> Rc<RefCell<f64>> {
        let cell = Rc::new(RefCell::new(default));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::Float(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers a boolean flag. Use `--name` (sets true) or
    /// `--name=false` (sets false). No implicit negation forms.
    pub fn bool(
        &mut self,
        name: &str,
        default: bool,
        summary: impl Into<String>,
    ) -> Rc<RefCell<bool>> {
        let cell = Rc::new(RefCell::new(default));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::Bool(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers a duration flag (`--timeout 5s`, `--timeout 250ms`).
    pub fn duration(
        &mut self,
        name: &str,
        default: Duration,
        summary: impl Into<String>,
    ) -> Rc<RefCell<Duration>> {
        let cell = Rc::new(RefCell::new(default));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::Duration(Rc::clone(&cell)),
        });
        cell
    }

    /// Registers a repeatable string flag. Each occurrence appends to
    /// the backing vector.
    pub fn string_list(
        &mut self,
        name: &str,
        summary: impl Into<String>,
    ) -> Rc<RefCell<Vec<String>>> {
        let cell = Rc::new(RefCell::new(Vec::<String>::new()));
        self.defs.push(Definition {
            name: name.to_string(),
            short: None,
            summary: summary.into(),
            value: Value::StringList(Rc::clone(&cell)),
        });
        cell
    }

    /// Associates a one-character short alias with the most recently
    /// registered flag (`fs.string(...); fs.short('a');`).
    pub fn short(&mut self, letter: char) {
        if let Some(last) = self.defs.last_mut() {
            last.short = Some(letter);
        }
    }

    /// Parses `args` (typically `os::args()`), updates backing cells,
    /// and returns the positional arguments that follow any flags.
    ///
    /// `args[0]` is treated as the program name and skipped. `--help`
    /// `-h` prints usage to stdout and returns an empty positional
    /// list.
    pub fn parse<I>(&self, args: I) -> Result<Vec<String>, Error>
    where
        I: IntoIterator<Item = String>,
    {
        let mut iter = args.into_iter();
        let _program = iter.next();
        let tokens: Vec<String> = iter.collect();
        let mut positional = Vec::new();
        let mut idx = 0;
        while idx < tokens.len() {
            let arg = &tokens[idx];
            if arg == "--" {
                positional.extend_from_slice(&tokens[idx + 1..]);
                return Ok(positional);
            }
            if arg == "--help" || arg == "-h" {
                println!("{}", self.usage());
                return Ok(Vec::new());
            }
            if let Some(rest) = arg.strip_prefix("--") {
                idx += self.apply_long(rest, idx, &tokens)?;
                continue;
            }
            if let Some(rest) = arg.strip_prefix('-') {
                if rest.is_empty() {
                    positional.push(arg.clone());
                    idx += 1;
                    continue;
                }
                idx += self.apply_short(rest, idx, &tokens)?;
                continue;
            }
            positional.push(arg.clone());
            idx += 1;
        }
        Ok(positional)
    }

    fn apply_long(&self, rest: &str, idx: usize, tokens: &[String]) -> Result<usize, Error> {
        let (name, explicit_value) = match rest.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (rest.to_string(), None),
        };
        let def = self
            .find(&name)
            .ok_or_else(|| Error::new(format!("unknown flag `--{name}`")))?;
        self.apply_value(def, explicit_value, idx, tokens, 2)
    }

    fn apply_short(&self, rest: &str, idx: usize, tokens: &[String]) -> Result<usize, Error> {
        let letter = rest.chars().next().unwrap();
        let remainder = &rest[letter.len_utf8()..];
        let def = self
            .find_short(letter)
            .ok_or_else(|| Error::new(format!("unknown flag `-{letter}`")))?;
        let explicit_value = if remainder.is_empty() {
            None
        } else if let Some(stripped) = remainder.strip_prefix('=') {
            Some(stripped.to_string())
        } else {
            Some(remainder.to_string())
        };
        self.apply_value(def, explicit_value, idx, tokens, 1)
    }

    fn apply_value(
        &self,
        def: &Definition,
        explicit_value: Option<String>,
        idx: usize,
        tokens: &[String],
        prefix_cost: usize,
    ) -> Result<usize, Error> {
        let _ = prefix_cost;
        let (raw, consumed) = match (&def.value, explicit_value) {
            (Value::Bool(_), Some(text)) => (text, 1),
            (Value::Bool(cell), None) => {
                *cell.borrow_mut() = true;
                return Ok(1);
            }
            (_, Some(text)) => (text, 1),
            (_, None) => {
                let Some(next) = tokens.get(idx + 1) else {
                    return Err(Error::new(format!(
                        "flag `--{}` requires a value",
                        def.name
                    )));
                };
                (next.clone(), 2)
            }
        };
        match &def.value {
            Value::String(cell) => *cell.borrow_mut() = raw,
            Value::Int(cell) => {
                *cell.borrow_mut() = raw
                    .parse()
                    .map_err(|_| Error::new(format!("flag `--{}` expects an int", def.name)))?;
            }
            Value::Uint(cell) => {
                *cell.borrow_mut() = raw
                    .parse()
                    .map_err(|_| Error::new(format!("flag `--{}` expects a uint", def.name)))?;
            }
            Value::Float(cell) => {
                *cell.borrow_mut() = raw
                    .parse()
                    .map_err(|_| Error::new(format!("flag `--{}` expects a float", def.name)))?;
            }
            Value::Bool(cell) => {
                *cell.borrow_mut() = parse_bool(&raw)
                    .ok_or_else(|| Error::new(format!("flag `--{}` expects a bool", def.name)))?;
            }
            Value::Duration(cell) => {
                *cell.borrow_mut() = parse_duration(&raw).ok_or_else(|| {
                    Error::new(format!(
                        "flag `--{}` expects a duration like `5s`",
                        def.name
                    ))
                })?;
            }
            Value::StringList(cell) => cell.borrow_mut().push(raw),
        }
        Ok(consumed)
    }

    fn find(&self, name: &str) -> Option<&Definition> {
        self.defs.iter().find(|d| d.name == name)
    }

    fn find_short(&self, letter: char) -> Option<&Definition> {
        self.defs.iter().find(|d| d.short == Some(letter))
    }

    /// Returns the auto-generated usage string.
    #[must_use]
    pub fn usage(&self) -> String {
        let mut out = format!("usage: {} [FLAGS] [POSITIONAL]\n\nflags:\n", self.program);
        for def in &self.defs {
            let label = match def.short {
                Some(ch) => format!("  -{ch}, --{}", def.name),
                None => format!("      --{}", def.name),
            };
            out.push_str(&format!("{label:<30} {}\n", def.summary));
        }
        out
    }
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Some(rest) = text.strip_suffix("ns") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_nanos(value));
    }
    if let Some(rest) = text.strip_suffix("ms") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_millis(value));
    }
    if let Some(rest) = text.strip_suffix("us") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_micros(value));
    }
    if let Some(rest) = text.strip_suffix("s") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_secs(value));
    }
    if let Some(rest) = text.strip_suffix("m") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_secs(value * 60));
    }
    if let Some(rest) = text.strip_suffix("h") {
        let value: u64 = rest.parse().ok()?;
        return Some(Duration::from_secs(value * 3600));
    }
    text.parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(extras: &[&str]) -> Vec<String> {
        let mut out = vec!["prog".to_string()];
        out.extend(extras.iter().map(|s| (*s).to_string()));
        out
    }

    #[test]
    fn string_flag_honours_default_when_absent() {
        let mut fs = Set::new("demo");
        let addr = fs.string("addr", "127.0.0.1", "listen addr");
        let positional = fs.parse(argv(&[])).unwrap();
        assert_eq!(*addr.borrow(), "127.0.0.1");
        assert!(positional.is_empty());
    }

    #[test]
    fn int_flag_parses_space_separated_value() {
        let mut fs = Set::new("demo");
        let port = fs.int("port", 80, "port");
        fs.parse(argv(&["--port", "8080"])).unwrap();
        assert_eq!(*port.borrow(), 8080);
    }

    #[test]
    fn int_flag_parses_equals_form() {
        let mut fs = Set::new("demo");
        let port = fs.int("port", 80, "port");
        fs.parse(argv(&["--port=9000"])).unwrap();
        assert_eq!(*port.borrow(), 9000);
    }

    #[test]
    fn short_alias_maps_to_long() {
        let mut fs = Set::new("demo");
        let verbose = fs.bool("verbose", false, "be loud");
        fs.short('v');
        fs.parse(argv(&["-v"])).unwrap();
        assert!(*verbose.borrow());
    }

    #[test]
    fn bool_flag_accepts_explicit_value() {
        let mut fs = Set::new("demo");
        let on = fs.bool("on", false, "toggle");
        fs.parse(argv(&["--on=false"])).unwrap();
        assert!(!*on.borrow());
    }

    #[test]
    fn duration_flag_parses_seconds_and_ms() {
        let mut fs = Set::new("demo");
        let d = fs.duration("tick", Duration::from_secs(1), "tick");
        fs.parse(argv(&["--tick", "250ms"])).unwrap();
        assert_eq!(*d.borrow(), Duration::from_millis(250));
    }

    #[test]
    fn string_list_flag_collects_repeats() {
        let mut fs = Set::new("demo");
        let tags = fs.string_list("tag", "repeatable tag");
        fs.parse(argv(&["--tag", "a", "--tag", "b", "--tag", "c"]))
            .unwrap();
        let snap = tags.borrow().clone();
        assert_eq!(
            snap,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn dash_dash_terminates_flag_parsing() {
        let mut fs = Set::new("demo");
        let flag = fs.bool("x", false, "x");
        let positional = fs.parse(argv(&["--", "--x", "trailing"])).unwrap();
        assert!(!*flag.borrow());
        assert_eq!(positional, vec!["--x".to_string(), "trailing".to_string()]);
    }

    #[test]
    fn unknown_flag_is_a_clean_error() {
        let mut fs = Set::new("demo");
        fs.string("known", "", "");
        let err = fs.parse(argv(&["--nope"])).unwrap_err();
        assert!(err.message().contains("unknown flag"));
    }

    #[test]
    fn missing_value_is_a_clean_error() {
        let mut fs = Set::new("demo");
        fs.int("port", 0, "");
        let err = fs.parse(argv(&["--port"])).unwrap_err();
        assert!(err.message().contains("requires a value"));
    }

    #[test]
    fn usage_mentions_every_flag() {
        let mut fs = Set::new("demo");
        fs.string("addr", "", "listen address");
        fs.bool("verbose", false, "be loud");
        let text = fs.usage();
        assert!(text.contains("--addr"));
        assert!(text.contains("--verbose"));
        assert!(text.contains("listen address"));
    }
}
