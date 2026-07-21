//! Runtime support for `std::flag` - batteries-included CLI parsing.
//! GNU-style long + short flags (`--verbose`, `-v`), equals-form
//! (`--port=8080`), value-follows (`--port 8080`), bool flags,
//! `--`-terminator, auto-generated `--help`, and friendly error
//! messages. Integrates with [`crate::errors::Error`].

#![forbid(unsafe_code)]
#![allow(missing_docs)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
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

/// Scalar type accepted by a structured command argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    String,
    Int,
    Uint,
    Float,
    Bool,
    Duration,
    KeyValue,
}

/// Number of values accepted by a positional argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    One,
    Optional,
    Many,
    OneOrMore,
}

type Validator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct OptionSpec {
    name: String,
    short: Option<char>,
    summary: String,
    kind: ArgType,
    required: bool,
    repeated: bool,
    default: Option<String>,
    env: Option<String>,
    choices: Vec<String>,
    conflicts: Vec<String>,
    requires: Vec<String>,
    validator: Option<Validator>,
}

/// Declarative option definition used by [`Command`].
#[derive(Clone)]
pub struct OptionArg(OptionSpec);

impl OptionArg {
    #[must_use]
    pub fn new(name: impl Into<String>, kind: ArgType, summary: impl Into<String>) -> Self {
        Self(OptionSpec {
            name: name.into(),
            short: None,
            summary: summary.into(),
            kind,
            required: false,
            repeated: false,
            default: None,
            env: None,
            choices: Vec::new(),
            conflicts: Vec::new(),
            requires: Vec::new(),
            validator: None,
        })
    }
    #[must_use]
    pub fn short(mut self, value: char) -> Self {
        self.0.short = Some(value);
        self
    }
    #[must_use]
    pub fn required(mut self) -> Self {
        self.0.required = true;
        self
    }
    #[must_use]
    pub fn repeated(mut self) -> Self {
        self.0.repeated = true;
        self
    }
    #[must_use]
    pub fn default(mut self, value: impl Into<String>) -> Self {
        self.0.default = Some(value.into());
        self
    }
    #[must_use]
    pub fn env(mut self, name: impl Into<String>) -> Self {
        self.0.env = Some(name.into());
        self
    }
    #[must_use]
    pub fn choices(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.choices = values.into_iter().map(Into::into).collect();
        self
    }
    #[must_use]
    pub fn conflicts_with(mut self, name: impl Into<String>) -> Self {
        self.0.conflicts.push(name.into());
        self
    }
    #[must_use]
    pub fn requires(mut self, name: impl Into<String>) -> Self {
        self.0.requires.push(name.into());
        self
    }
    #[must_use]
    pub fn validate(
        mut self,
        callback: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.0.validator = Some(Arc::new(callback));
        self
    }
}

/// Declarative positional definition.
#[derive(Debug, Clone)]
pub struct Positional {
    name: String,
    summary: String,
    kind: ArgType,
    cardinality: Cardinality,
    choices: Vec<String>,
}

impl Positional {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: ArgType,
        cardinality: Cardinality,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
            kind,
            cardinality,
            choices: Vec::new(),
        }
    }
    #[must_use]
    pub fn choices(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.choices = values.into_iter().map(Into::into).collect();
        self
    }
}

/// Parsed immutable result. Values are retained as their validated source text
/// so callers can use one snapshot repeatedly without mutable parser cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    command_path: Vec<String>,
    options: BTreeMap<String, Vec<String>>,
    positionals: BTreeMap<String, Vec<String>>,
}

impl Parsed {
    #[must_use]
    pub fn command_path(&self) -> &[String] {
        &self.command_path
    }
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.options
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }
    #[must_use]
    pub fn get_all(&self, name: &str) -> &[String] {
        self.options.get(name).map_or(&[], Vec::as_slice)
    }
    #[must_use]
    pub fn positional(&self, name: &str) -> Option<&str> {
        self.positionals
            .get(name)
            .and_then(|v| v.first())
            .map(String::as_str)
    }
    #[must_use]
    pub fn positionals(&self, name: &str) -> &[String] {
        self.positionals.get(name).map_or(&[], Vec::as_slice)
    }
    /// Returns repeated `key=value` option values as an immutable map.
    #[must_use]
    pub fn key_values(&self, name: &str) -> BTreeMap<&str, &str> {
        self.get_all(name)
            .iter()
            .filter_map(|value| value.split_once('='))
            .collect()
    }
}

/// Non-exiting parse result. Applications retain control of output and status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Parsed(Parsed),
    Help(String),
    Version(String),
}

/// Immutable reusable structured command model.
#[derive(Clone)]
pub struct Command {
    name: String,
    about: String,
    version: Option<String>,
    options: Vec<OptionSpec>,
    positionals: Vec<Positional>,
    subcommands: Vec<Command>,
}

impl Command {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            about: String::new(),
            version: None,
            options: Vec::new(),
            positionals: Vec::new(),
            subcommands: Vec::new(),
        }
    }
    #[must_use]
    pub fn about(mut self, text: impl Into<String>) -> Self {
        self.about = text.into();
        self
    }
    #[must_use]
    pub fn version(mut self, text: impl Into<String>) -> Self {
        self.version = Some(text.into());
        self
    }
    #[must_use]
    pub fn option(mut self, option: OptionArg) -> Self {
        self.options.push(option.0);
        self
    }
    #[must_use]
    pub fn positional_arg(mut self, positional: Positional) -> Self {
        self.positionals.push(positional);
        self
    }
    #[must_use]
    pub fn subcommand(mut self, command: Command) -> Self {
        self.subcommands.push(command);
        self
    }

    pub fn parse<I, S>(&self, args: I) -> Result<ParseOutcome, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let tokens: Vec<String> = args.into_iter().map(Into::into).collect();
        let tokens = if tokens.first().is_some_and(|value| value == &self.name) {
            &tokens[1..]
        } else {
            &tokens[..]
        };
        self.parse_tokens(tokens, vec![self.name.clone()])
    }

    #[allow(
        clippy::too_many_lines,
        reason = "token parsing keeps option, subcommand, and positional state in one pass"
    )]
    fn parse_tokens(&self, tokens: &[String], path: Vec<String>) -> Result<ParseOutcome, Error> {
        let mut options: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut raw_positionals = Vec::new();
        let mut index = 0;
        let mut options_enabled = true;
        while index < tokens.len() {
            let token = &tokens[index];
            if options_enabled && token == "--" {
                options_enabled = false;
                index += 1;
                continue;
            }
            if options_enabled && matches!(token.as_str(), "--help" | "-h") {
                return Ok(ParseOutcome::Help(self.usage_for(&path)));
            }
            if options_enabled && token == "--version" {
                return self
                    .version
                    .as_ref()
                    .map(|v| ParseOutcome::Version(v.clone()))
                    .ok_or_else(|| {
                        Error::new(format!("{}: unknown option `--version`", path.join(" ")))
                    });
            }
            if options_enabled && token.starts_with("--") {
                let (name, attached) = token[2..]
                    .split_once('=')
                    .map_or((&token[2..], None), |(a, b)| (a, Some(b)));
                let spec = self
                    .options
                    .iter()
                    .find(|value| value.name == name)
                    .ok_or_else(|| self.unknown_option(token, &path))?;
                let (value, consumed) = option_value(spec, attached, tokens.get(index + 1), token)?;
                insert_option(&mut options, spec, value)?;
                index += consumed;
                continue;
            }
            if options_enabled && token.starts_with('-') && token.len() > 1 {
                let short = token[1..].chars().next().expect("nonempty short option");
                let spec = self
                    .options
                    .iter()
                    .find(|value| value.short == Some(short))
                    .ok_or_else(|| self.unknown_option(token, &path))?;
                let rest = &token[(1 + short.len_utf8())..];
                if !rest.is_empty() && spec.kind == ArgType::Bool {
                    return Err(Error::new(format!(
                        "{}: ambiguous short group `{token}`",
                        path.join(" ")
                    )));
                }
                let attached = (!rest.is_empty()).then_some(rest.trim_start_matches('='));
                let (value, consumed) = option_value(spec, attached, tokens.get(index + 1), token)?;
                insert_option(&mut options, spec, value)?;
                index += consumed;
                continue;
            }
            if options_enabled && raw_positionals.is_empty() {
                if let Some(command) = self
                    .subcommands
                    .iter()
                    .find(|command| command.name == *token)
                {
                    self.finish_options(&mut options, &path)?;
                    let mut child_path = path;
                    child_path.push(command.name.clone());
                    return match command.parse_tokens(&tokens[index + 1..], child_path)? {
                        ParseOutcome::Parsed(mut parsed) => {
                            for (name, values) in options {
                                if parsed.options.insert(name.clone(), values).is_some() {
                                    return Err(Error::new(format!(
                                        "option `--{name}` is defined by both parent and subcommand"
                                    )));
                                }
                            }
                            Ok(ParseOutcome::Parsed(parsed))
                        }
                        outcome => Ok(outcome),
                    };
                }
                if self.positionals.is_empty() && !self.subcommands.is_empty() {
                    let suggestion = self
                        .subcommands
                        .iter()
                        .map(|command| command.name.as_str())
                        .min_by_key(|name| edit_distance(token, name));
                    let suffix = suggestion
                        .filter(|name| edit_distance(token, name) <= 3)
                        .map_or(String::new(), |name| format!("; did you mean `{name}`?"));
                    return Err(Error::new(format!(
                        "{}: unknown subcommand `{token}`{suffix}",
                        path.join(" ")
                    )));
                }
            }
            raw_positionals.push(token.clone());
            index += 1;
        }
        self.finish_options(&mut options, &path)?;
        let positionals = bind_positionals(&self.positionals, raw_positionals, &path)?;
        if !self.subcommands.is_empty() && positionals.is_empty() {
            return Err(Error::new(format!(
                "{}: missing subcommand",
                path.join(" ")
            )));
        }
        Ok(ParseOutcome::Parsed(Parsed {
            command_path: path,
            options,
            positionals,
        }))
    }

    fn finish_options(
        &self,
        options: &mut BTreeMap<String, Vec<String>>,
        path: &[String],
    ) -> Result<(), Error> {
        for spec in &self.options {
            if !options.contains_key(&spec.name) {
                if let Some(env) = spec.env.as_ref().and_then(|name| std::env::var(name).ok()) {
                    insert_option(options, spec, env)?;
                } else if let Some(default) = &spec.default {
                    insert_option(options, spec, default.clone())?;
                } else if spec.required {
                    return Err(Error::new(format!(
                        "{}: missing required option `--{}`",
                        path.join(" "),
                        spec.name
                    )));
                }
            }
        }
        for spec in &self.options {
            if options.contains_key(&spec.name) {
                if let Some(other) = spec
                    .conflicts
                    .iter()
                    .find(|name| options.contains_key(*name))
                {
                    return Err(Error::new(format!(
                        "--{} conflicts with --{other}",
                        spec.name
                    )));
                }
                if let Some(other) = spec
                    .requires
                    .iter()
                    .find(|name| !options.contains_key(*name))
                {
                    return Err(Error::new(format!("--{} requires --{other}", spec.name)));
                }
            }
        }
        Ok(())
    }

    fn unknown_option(&self, token: &str, path: &[String]) -> Error {
        let needle = token.trim_start_matches('-');
        let suggestion = self
            .options
            .iter()
            .map(|option| option.name.as_str())
            .min_by_key(|name| edit_distance(needle, name));
        let suffix = suggestion
            .filter(|name| edit_distance(needle, name) <= 3)
            .map_or(String::new(), |name| format!("; did you mean `--{name}`?"));
        Error::new(format!(
            "{}: unknown option `{token}`{suffix}",
            path.join(" ")
        ))
    }

    #[must_use]
    pub fn usage(&self) -> String {
        self.usage_for(std::slice::from_ref(&self.name))
    }
    fn usage_for(&self, path: &[String]) -> String {
        let mut out = format!("Usage: {}", path.join(" "));
        if !self.options.is_empty() {
            out.push_str(" [OPTIONS]");
        }
        if !self.subcommands.is_empty() {
            out.push_str(" <COMMAND>");
        }
        for arg in &self.positionals {
            let label = match arg.cardinality {
                Cardinality::One => format!("<{}>", arg.name),
                Cardinality::Optional => format!("[{}]", arg.name),
                Cardinality::Many => format!("[{}...]", arg.name),
                Cardinality::OneOrMore => format!("<{}...>", arg.name),
            };
            out.push(' ');
            out.push_str(&label);
        }
        out.push('\n');
        if !self.about.is_empty() {
            out.push('\n');
            out.push_str(&self.about);
            out.push('\n');
        }
        if !self.options.is_empty() {
            out.push_str("\nOptions:\n");
            for option in &self.options {
                let short = option.short.map_or(String::new(), |v| format!("-{v}, "));
                let required = if option.required { " [required]" } else { "" };
                let env = option
                    .env
                    .as_ref()
                    .map_or(String::new(), |v| format!(" [env: {v}]"));
                let default = option
                    .default
                    .as_ref()
                    .map_or(String::new(), |v| format!(" [default: {v}]"));
                out.push_str(&format!(
                    "  {short}--{}\t{}{}{}{}\n",
                    option.name, option.summary, required, env, default
                ));
            }
        }
        if !self.positionals.is_empty() {
            out.push_str("\nArguments:\n");
            for arg in &self.positionals {
                out.push_str(&format!("  {}\t{}\n", arg.name, arg.summary));
            }
        }
        if !self.subcommands.is_empty() {
            out.push_str("\nCommands:\n");
            for command in &self.subcommands {
                out.push_str(&format!("  {}\t{}\n", command.name, command.about));
            }
        }
        out
    }

    /// Generates static completion data without executing validators.
    #[must_use]
    pub fn completions(&self, shell: CompletionShell) -> String {
        let words = self.completion_words().join(" ");
        match shell {
            CompletionShell::Bash => format!("complete -W '{words}' {}\n", self.name),
            CompletionShell::Zsh => format!("#compdef {}\n_arguments '*: :({words})'\n", self.name),
            CompletionShell::Fish => {
                words
                    .split_whitespace()
                    .fold(String::new(), |mut output, word| {
                        use std::fmt::Write as _;
                        let _ = writeln!(output, "complete -c {} -a '{word}'", self.name);
                        output
                    })
            }
            CompletionShell::PowerShell => format!(
                "Register-ArgumentCompleter -CommandName {} -ScriptBlock {{ '{words}'.Split(' ') }}\n",
                self.name
            ),
        }
    }
    fn completion_words(&self) -> Vec<String> {
        let mut out = vec!["--help".into()];
        if self.version.is_some() {
            out.push("--version".into());
        }
        out.extend(self.options.iter().map(|o| format!("--{}", o.name)));
        out.extend(self.subcommands.iter().map(|c| c.name.clone()));
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

fn option_value(
    spec: &OptionSpec,
    attached: Option<&str>,
    next: Option<&String>,
    token: &str,
) -> Result<(String, usize), Error> {
    if spec.kind == ArgType::Bool && attached.is_none() {
        return Ok(("true".into(), 1));
    }
    attached
        .map(|v| (v.to_string(), 1))
        .or_else(|| next.map(|v| (v.clone(), 2)))
        .ok_or_else(|| Error::new(format!("option `{token}` requires a value")))
}

fn insert_option(
    output: &mut BTreeMap<String, Vec<String>>,
    spec: &OptionSpec,
    value: String,
) -> Result<(), Error> {
    validate_arg(spec.kind, &value, &spec.choices, spec.validator.as_ref()).map_err(Error::new)?;
    if !spec.repeated && output.contains_key(&spec.name) {
        return Err(Error::new(format!(
            "option `--{}` may not be repeated",
            spec.name
        )));
    }
    output.entry(spec.name.clone()).or_default().push(value);
    Ok(())
}

fn validate_arg(
    kind: ArgType,
    value: &str,
    choices: &[String],
    validator: Option<&Validator>,
) -> Result<(), String> {
    let valid = match kind {
        ArgType::String => true,
        ArgType::Int => value.parse::<i64>().is_ok(),
        ArgType::Uint => value.parse::<u64>().is_ok(),
        ArgType::Float => value.parse::<f64>().is_ok(),
        ArgType::Bool => parse_bool(value).is_some(),
        ArgType::Duration => parse_duration(value).is_some(),
        ArgType::KeyValue => value
            .split_once('=')
            .is_some_and(|(key, _)| !key.is_empty()),
    };
    if !valid {
        return Err(format!("invalid {kind:?} value `{value}`"));
    }
    if !choices.is_empty() && !choices.iter().any(|choice| choice == value) {
        return Err(format!(
            "invalid value `{value}`; expected one of {}",
            choices.join(", ")
        ));
    }
    if let Some(callback) = validator {
        callback(value)?;
    }
    Ok(())
}

fn bind_positionals(
    specs: &[Positional],
    values: Vec<String>,
    path: &[String],
) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let mut output = BTreeMap::new();
    let mut index = 0;
    for spec in specs {
        let count = match spec.cardinality {
            Cardinality::One => 1,
            Cardinality::Optional => usize::from(index < values.len()),
            Cardinality::Many | Cardinality::OneOrMore => values.len() - index,
        };
        if matches!(spec.cardinality, Cardinality::One | Cardinality::OneOrMore) && count == 0 {
            return Err(Error::new(format!(
                "{}: missing required argument `{}`",
                path.join(" "),
                spec.name
            )));
        }
        let selected = values[index..index + count].to_vec();
        for value in &selected {
            validate_arg(spec.kind, value, &spec.choices, None).map_err(Error::new)?;
        }
        if !selected.is_empty() {
            output.insert(spec.name.clone(), selected);
        }
        index += count;
    }
    if index < values.len() {
        return Err(Error::new(format!(
            "{}: unexpected argument `{}`",
            path.join(" "),
            values[index]
        )));
    }
    Ok(output)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let mut costs: Vec<usize> = (0..=b.chars().count()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = i;
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let old = costs[j + 1];
            costs[j + 1] = if ca == cb {
                previous
            } else {
                1 + previous.min(costs[j]).min(old)
            };
            previous = old;
        }
    }
    *costs.last().unwrap_or(&0)
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
    fn structured_command_is_reentrant_and_validates_relationships() {
        let command = Command::new("tool")
            .version("1.2.3")
            .option(
                OptionArg::new("format", ArgType::String, "output format")
                    .choices(["json", "text"])
                    .default("text"),
            )
            .subcommand(
                Command::new("run")
                    .option(OptionArg::new("tag", ArgType::String, "tag").repeated())
                    .positional_arg(Positional::new(
                        "file",
                        ArgType::String,
                        Cardinality::One,
                        "input file",
                    )),
            );
        let args = ["tool", "run", "--tag", "a", "--tag=b", "main.gos"];
        let ParseOutcome::Parsed(first) = command.parse(args).unwrap() else {
            panic!("expected parsed result");
        };
        let ParseOutcome::Parsed(second) = command.parse(args).unwrap() else {
            panic!("expected parsed result");
        };
        assert_eq!(first, second);
        assert_eq!(first.command_path(), ["tool", "run"]);
        assert_eq!(first.get_all("tag"), ["a", "b"]);
        assert_eq!(first.positional("file"), Some("main.gos"));
    }

    #[test]
    fn completion_generation_covers_supported_shells() {
        let command =
            Command::new("tool").option(OptionArg::new("verbose", ArgType::Bool, "verbose"));
        for shell in [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
            CompletionShell::PowerShell,
        ] {
            assert!(command.completions(shell).contains("--verbose"));
        }
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
