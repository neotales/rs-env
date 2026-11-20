//! Expansion of `$NAME`, `${NAME…}` and, when explicitly enabled, `%NAME%` and
//! `$(…)`.
//!
//! The parser works on characters, so a value written in any UTF-8 encoding is
//! sliced on character boundaries. Every lookup and every store comes from the
//! caller through [`ExpandOptions`]; nothing here reads or writes the process
//! environment.

use std::fmt;
use std::process::Command;

use crate::{EnvError, Environment, Result};

type Lookup<'a> = Box<dyn FnMut(&str) -> Option<String> + 'a>;
type Setter<'a> = Box<dyn FnMut(&str, &str) -> std::result::Result<(), String> + 'a>;

/// How [`expand_with_options`] finds and stores values.
///
/// The type is a builder. Its defaults match the Go `env` package's `Expand`:
/// command substitution is off, Windows `%NAME%` references are off, `$$` is a
/// literal `$`, and `\$` is a literal `$`.
///
/// The forms accepted, in `value`:
///
/// - `$NAME` and `${NAME}`, looked up in the supplied lookup. A name in the
///   unbraced form is a run of letters, digits and underscores, so `$HOME/bin`
///   keeps its slash. A name that is not set expands to nothing.
/// - `${NAME:-default}`, which uses `default` when the value is missing or
///   empty. A `$` inside the default is expanded too.
/// - `${NAME:=default}`, which is `${NAME:-default}` and also stores the
///   default through the setter when one was supplied.
/// - `${NAME:?message}`, which is an error carrying exactly `message` when the
///   value is missing or empty.
/// - `${NAME:default}`, the same as `${NAME:-default}`.
/// - `%NAME%`, only with [`with_windows_variables(true)`](Self::with_windows_variables).
/// - `$(command)`, only with
///   [`with_command_substitution(true)`](Self::with_command_substitution). The
///   command is split into arguments and executed directly; no shell is
///   involved unless the command itself is one.
///
/// A reference that is missing its `}`, `)` or `%` is an error rather than a
/// partial result.
///
/// ```
/// use neotales_env::ExpandOptions;
///
/// let mut options = ExpandOptions::new(|name| {
///     (name == "HOME").then(|| "/home/dev".to_owned())
/// })
/// .with_windows_variables(true);
///
/// assert_eq!(
///     options.expand("%HOME%/bin").unwrap(),
///     "/home/dev/bin"
/// );
/// ```
pub struct ExpandOptions<'a> {
    lookup: Lookup<'a>,
    set: Option<Setter<'a>>,
    windows_variables: bool,
    command_substitution: bool,
    unix_args: Option<Vec<String>>,
    set_keys: Vec<String>,
}

impl fmt::Debug for ExpandOptions<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExpandOptions")
            .field("windows_variables", &self.windows_variables)
            .field("command_substitution", &self.command_substitution)
            .field("unix_args", &self.unix_args)
            .field("set", &self.set.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> ExpandOptions<'a> {
    /// Creates options that read values from `lookup`.
    ///
    /// Everything else starts at the Go `Expand` defaults: Windows variables
    /// and command substitution are off, and no positional arguments exist.
    #[must_use]
    pub fn new(lookup: impl FnMut(&str) -> Option<String> + 'a) -> Self {
        Self {
            lookup: Box::new(lookup),
            set: None,
            windows_variables: false,
            command_substitution: false,
            unix_args: None,
            set_keys: Vec::new(),
        }
    }

    /// Creates options that read values from `environment`.
    ///
    /// There is no setter, so `${NAME:=default}` returns the default without
    /// storing it.
    #[must_use]
    pub fn from_environment(environment: &'a Environment) -> Self {
        Self::new(move |name| environment.get(name).map(str::to_owned))
    }

    /// Supplies the function that `${NAME:=default}` calls to store a default.
    ///
    /// The function receives the name and the default, and its error text is
    /// reported as [`EnvError::SetFailed`] with the name attached. It is only
    /// called when the value is missing or empty.
    #[must_use]
    pub fn with_set(
        mut self,
        set: impl FnMut(&str, &str) -> std::result::Result<(), String> + 'a,
    ) -> Self {
        self.set = Some(Box::new(set));
        self
    }

    /// Enables or disables Windows `%NAME%` references. They are off by default.
    #[must_use]
    pub const fn with_windows_variables(mut self, enabled: bool) -> Self {
        self.windows_variables = enabled;
        self
    }

    /// Enables or disables `$(command)` substitution. It is off by default.
    ///
    /// Enabling this runs programs named by the expanded value. Leave it off
    /// for values that did not come from the caller.
    #[must_use]
    pub const fn with_command_substitution(mut self, enabled: bool) -> Self {
        self.command_substitution = enabled;
        self
    }

    /// Supplies the arguments that `${1}` and `$1` refer to.
    ///
    /// `arguments[0]` is the program name, matching `std::env::args` and
    /// `os.Args`. The process arguments are never read implicitly, so a caller
    /// that wants them passes `std::env::args()` here.
    #[must_use]
    pub fn with_unix_args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.unix_args = Some(arguments.into_iter().map(Into::into).collect());
        self
    }

    /// Expands `value` with these options.
    ///
    /// # Errors
    ///
    /// Returns [`EnvError::Syntax`] for an unterminated reference or a name a
    /// shell would reject, [`EnvError::Required`] for `${NAME:?message}`,
    /// [`EnvError::SetFailed`] when the supplied setter fails, and
    /// [`EnvError::CommandFailed`] when a substituted command cannot run or
    /// exits non-zero.
    pub fn expand(&mut self, value: &str) -> Result<String> {
        expand_with_options(value, self)
    }
}

/// Expands `value` using `environment` for lookups.
///
/// Windows `%NAME%` references and command substitution stay off; build an
/// [`ExpandOptions`] when either is wanted. There is no setter here, so
/// `${NAME:=default}` returns the default without storing it.
///
/// # Errors
///
/// Returns [`EnvError::Syntax`] when a reference is unterminated or names a
/// variable a shell would reject, and [`EnvError::Required`] for a
/// `${NAME:?message}` whose value is missing or empty.
///
/// ```
/// use neotales_env::{Environment, Platform, expand};
///
/// let mut environment = Environment::new(Platform::Unix);
/// environment.set("HOME", "/home/dev");
///
/// assert_eq!(expand("$HOME/bin", &environment).unwrap(), "/home/dev/bin");
/// assert_eq!(expand("${MISSING:-/root}", &environment).unwrap(), "/root");
/// ```
pub fn expand(value: &str, environment: &Environment) -> Result<String> {
    let mut options = ExpandOptions::from_environment(environment);
    expand_with_options(value, &mut options)
}

/// Expands `value` using an explicit lookup.
///
/// The lookup covers every form that has no setter; see [`ExpandOptions`] for
/// the full grammar. An unset name is empty. Because no setter is supplied,
/// `${NAME:=default}` returns the default without storing it.
///
/// # Errors
///
/// Returns [`EnvError::Syntax`] when a reference is unterminated or names a
/// variable a shell would reject, and [`EnvError::Required`] for a
/// `${NAME:?message}` whose value is missing or empty.
///
/// ```
/// use neotales_env::expand_with;
///
/// let home = expand_with("$HOME/bin", |name| (name == "HOME").then_some("/home/dev"));
/// assert_eq!(home.unwrap(), "/home/dev/bin");
/// ```
pub fn expand_with<'a>(
    value: &str,
    mut lookup: impl FnMut(&str) -> Option<&'a str>,
) -> Result<String> {
    let mut options = ExpandOptions::new(move |name| lookup(name).map(str::to_owned));
    expand_with_options(value, &mut options)
}

/// Expands `value` using `options`, consuming them for the duration of the call.
///
/// See [`ExpandOptions`] for the accepted forms and their defaults.
///
/// # Errors
///
/// Returns [`EnvError::Syntax`] when a reference is unterminated or names a
/// variable a shell would reject, [`EnvError::Required`] for a
/// `${NAME:?message}` whose value is missing or empty, [`EnvError::SetFailed`]
/// when the supplied setter fails, and [`EnvError::CommandFailed`] when a
/// substituted command cannot run or exits non-zero.
pub fn expand_with_options(value: &str, options: &mut ExpandOptions<'_>) -> Result<String> {
    let characters: Vec<char> = value.chars().collect();
    expand_characters(&characters, options)
}

fn expand_characters(characters: &[char], options: &mut ExpandOptions<'_>) -> Result<String> {
    let mut output = String::with_capacity(characters.len());
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if character == '\\' && next == Some('$') {
            output.push('$');
            index += 2;
            continue;
        }

        if character == '$' {
            if next == Some('$') {
                output.push('$');
                index += 2;
                continue;
            }

            if next == Some('(') && options.command_substitution {
                let (expression, after) = read_command(characters, index + 2)?;
                output.push_str(&run_command(&expression, options)?);
                index = after;
                continue;
            }

            if next == Some('{') {
                let (token, after) = read_braced(characters, index + 2)?;
                output.push_str(&interpolate(&token, options)?);
                index = after;
                continue;
            }

            if next.is_some_and(|name| name.is_alphanumeric() || name == '_') {
                let start = index + 1;
                let mut end = start;
                while end < characters.len()
                    && (characters[end].is_alphanumeric() || characters[end] == '_')
                {
                    end += 1;
                }
                let name: String = characters[start..end].iter().collect();
                output.push_str(&interpolate(&name, options)?);
                index = end;
                continue;
            }

            output.push('$');
            index += 1;
            continue;
        }

        if character == '%' && options.windows_variables {
            let start = index + 1;
            let mut end = start;
            while end < characters.len() && characters[end] != '%' {
                end += 1;
            }
            if end == characters.len() {
                return Err(syntax(
                    "invalid windows variable syntax: missing closing percent sign",
                ));
            }
            if end == start {
                return Err(syntax(
                    "invalid windows variable syntax: empty variable name",
                ));
            }
            let name: String = characters[start..end].iter().collect();
            output.push_str(&(options.lookup)(&name).unwrap_or_default());
            index = end + 1;
            continue;
        }

        output.push(character);
        index += 1;
    }
    Ok(output)
}

/// Reads the contents of a `${…}` reference, returning the token and the index
/// after the closing brace.
fn read_braced(characters: &[char], start: usize) -> Result<(String, usize)> {
    let mut depth = 0_usize;
    let mut token = String::new();
    for (offset, &character) in characters[start..].iter().enumerate() {
        match character {
            '{' => {
                depth += 1;
                token.push(character);
            }
            '}' if depth == 0 => {
                if token.is_empty() {
                    return Err(syntax("invalid bash variable syntax: empty variable name"));
                }
                return Ok((token, start + offset + 1));
            }
            '}' => {
                depth -= 1;
                token.push(character);
            }
            _ => token.push(character),
        }
    }
    Err(syntax(
        "invalid bash variable syntax: missing closing brace",
    ))
}

/// Reads the contents of a `$(…)` reference, returning the expression and the
/// index after the closing parenthesis.
fn read_command(characters: &[char], start: usize) -> Result<(String, usize)> {
    let mut depth = 0_usize;
    let mut token = String::new();
    for (offset, &character) in characters[start..].iter().enumerate() {
        match character {
            '(' => {
                depth += 1;
                token.push(character);
            }
            ')' if depth == 0 => return Ok((token, start + offset + 1)),
            ')' => {
                depth -= 1;
                token.push(character);
            }
            _ => token.push(character),
        }
    }
    Err(syntax(
        "invalid command substitution: missing closing parenthesis",
    ))
}

/// What a `${…}` token asks for when its variable has no value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fallback {
    /// No fallback at all: the token is just a name.
    None,
    /// `${NAME:-default}` and `${NAME:default}`.
    Default,
    /// `${NAME:=default}`.
    Assign,
    /// `${NAME:?message}`.
    Required,
}

/// Splits a `${…}` token into its name, its fallback operand and the operator.
fn parse_token(token: &str) -> (String, String, Fallback) {
    for (operator, fallback) in [
        (":-", Fallback::Default),
        (":=", Fallback::Assign),
        (":?", Fallback::Required),
        (":", Fallback::Default),
    ] {
        if let Some(index) = token.find(operator) {
            let first = &token[..index];
            let second = &token[index + operator.len()..];
            return if first.is_empty() {
                (second.to_owned(), String::new(), fallback)
            } else {
                (first.to_owned(), second.to_owned(), fallback)
            };
        }
    }
    (token.to_owned(), String::new(), Fallback::None)
}

/// Resolves a name, or a `${…}` token, against the options.
fn interpolate(token: &str, options: &mut ExpandOptions<'_>) -> Result<String> {
    let (name, operand, fallback) = parse_token(token);

    if name.is_empty() {
        return Err(syntax("invalid bash variable syntax: empty variable name"));
    }

    if let Some(arguments) = &options.unix_args
        && let Ok(index) = name.parse::<usize>()
    {
        return Ok(arguments.get(index).cloned().unwrap_or_default());
    }

    if !valid_name(&name) {
        return Err(syntax(
            "invalid bash variable syntax: invalid variable name",
        ));
    }

    let value = (options.lookup)(&name).unwrap_or_default();
    if !value.is_empty() {
        return Ok(value);
    }

    match fallback {
        Fallback::Required => Err(EnvError::Required { message: operand }),
        Fallback::Assign => {
            if let Some(set) = options.set.as_mut() {
                set(&name, &operand).map_err(|message| EnvError::SetFailed {
                    key: name.clone(),
                    message,
                })?;
            }
            if !options.set_keys.iter().any(|key| key == &name) {
                options.set_keys.push(name.clone());
            }
            // The setter may write into the same store the lookup reads, so a
            // second lookup is what turns a successful store into the value.
            let stored = (options.lookup)(&name).unwrap_or_default();
            if stored.is_empty() {
                expand_operand(&operand, options)
            } else {
                Ok(stored)
            }
        }
        Fallback::Default | Fallback::None => expand_operand(&operand, options),
    }
}

/// Expands a fallback operand, which is returned as written when it holds no
/// reference of its own.
fn expand_operand(operand: &str, options: &mut ExpandOptions<'_>) -> Result<String> {
    if operand.contains('$') {
        expand_with_options(operand, options)
    } else {
        Ok(operand.to_owned())
    }
}

/// Reports whether `name` is a variable a shell accepts: a letter or an
/// underscore, then letters, digits or underscores.
fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    characters.all(|character| character.is_alphanumeric() || character == '_')
}

fn syntax(message: &str) -> EnvError {
    EnvError::Syntax {
        message: message.to_owned(),
    }
}

/// One piece of a command word: text to copy, or text to expand first.
enum Segment {
    Literal(String),
    Expandable(String),
}

/// Splits a command expression into words, keeping the quoting distinction
/// between text that may be expanded and text that may not.
fn command_words(expression: &str) -> Vec<Vec<Segment>> {
    let mut words = Vec::new();
    let mut word: Vec<Segment> = Vec::new();
    let mut in_word = false;
    let mut quote = None;
    let mut characters = expression.chars();
    while let Some(character) = characters.next() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    push_literal(&mut word, character);
                }
            }
            Some('"') => {
                if character == '"' {
                    quote = None;
                } else {
                    push_expandable(&mut word, character);
                }
            }
            Some(_) => unreachable!("only the two quote characters are recorded"),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    in_word = true;
                }
                '\\' => {
                    if let Some(escaped) = characters.next() {
                        push_literal(&mut word, escaped);
                    }
                    in_word = true;
                }
                character if character.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                character => {
                    push_expandable(&mut word, character);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

fn push_literal(word: &mut Vec<Segment>, character: char) {
    match word.last_mut() {
        Some(Segment::Literal(text)) => text.push(character),
        _ => word.push(Segment::Literal(character.to_string())),
    }
}

fn push_expandable(word: &mut Vec<Segment>, character: char) {
    match word.last_mut() {
        Some(Segment::Expandable(text)) => text.push(character),
        _ => word.push(Segment::Expandable(character.to_string())),
    }
}

/// Runs a `$(…)` expression and returns its trimmed standard output.
fn run_command(expression: &str, options: &mut ExpandOptions<'_>) -> Result<String> {
    if expression.trim().is_empty() {
        return Err(syntax("invalid command substitution: empty expression"));
    }

    let mut arguments = Vec::new();
    for word in command_words(expression) {
        let mut text = String::new();
        for segment in word {
            match segment {
                Segment::Literal(value) => text.push_str(&value),
                Segment::Expandable(value) => {
                    text.push_str(&expand_with_options(&value, options)?);
                }
            }
        }
        arguments.push(text);
    }

    let Some((program, rest)) = arguments.split_first() else {
        return Err(syntax("invalid command substitution: empty command"));
    };

    let mut command = Command::new(program);
    command.args(rest);
    // A value stored by `${NAME:=default}` is exposed to the command, matching
    // what a caller would see for a variable the expansion just created.
    for key in options.set_keys.clone() {
        command.env(&key, (options.lookup)(&key).unwrap_or_default());
    }

    let output = command.output().map_err(|error| EnvError::CommandFailed {
        message: format!("{:?}", error.kind()),
    })?;
    if !output.status.success() {
        return Err(EnvError::CommandFailed {
            message: match output.status.code() {
                Some(code) => format!("exit status {code}"),
                None => "terminated by a signal".to_owned(),
            },
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use super::{ExpandOptions, expand, expand_with, expand_with_options, parse_token, valid_name};
    use crate::{EnvError, Environment, Platform};

    /// Expands against a fixed set of values, so a test says what it expects
    /// rather than which variable it happened to look up.
    fn expand_fixture(value: &str) -> crate::Result<String> {
        expand_with(value, |name| match name {
            "HOME" => Some("/home/dev"),
            "PATH" => Some("/usr/bin"),
            "FOO" => Some("bar"),
            _ => None,
        })
    }

    #[test]
    fn a_braced_name_is_looked_up() {
        assert_eq!(expand_fixture("Value: ${FOO}").unwrap(), "Value: bar");
        assert_eq!(expand_fixture("${HOME}/bin").unwrap(), "/home/dev/bin");
        assert_eq!(expand_fixture("${MISSING}/bin").unwrap(), "/bin");
    }

    #[test]
    fn an_unbraced_name_is_looked_up() {
        assert_eq!(expand_fixture("Value: $FOO").unwrap(), "Value: bar");
        assert_eq!(expand_fixture("$HOME/bin").unwrap(), "/home/dev/bin");
        assert_eq!(
            expand_fixture("$PATH:/opt/bin").unwrap(),
            "/usr/bin:/opt/bin"
        );
    }

    #[test]
    fn unbraced_name_ends_at_any_character_a_value_cannot_contain() {
        for terminator in [':', '/', '-', '.', ' ', '=', '\n'] {
            let value = format!("$HOME{terminator}bin");
            assert_eq!(
                expand_fixture(&value).unwrap(),
                format!("/home/dev{terminator}bin"),
                "terminator {terminator:?} was swallowed"
            );
        }
    }

    #[test]
    fn a_unicode_letter_is_a_name_character() {
        // Go's name rules are Unicode letters and digits, so `é` continues a
        // name rather than ending it. The fixture has no such name, so the
        // whole reference is empty.
        assert_eq!(expand_fixture("$HOME\u{e9}bin").unwrap(), "");
        let result = expand_with("$caf\u{e9}", |name| {
            (name == "caf\u{e9}").then_some("value")
        });
        assert_eq!(result.unwrap(), "value");
    }

    #[test]
    fn an_unset_name_is_empty() {
        assert_eq!(expand_fixture("$MISSING/bin").unwrap(), "/bin");
        assert_eq!(expand_fixture("x${MISSING}y").unwrap(), "xy");
    }

    #[test]
    fn a_dollar_with_no_name_is_literal() {
        assert_eq!(expand_fixture("$").unwrap(), "$");
        assert_eq!(expand_fixture("a$/b").unwrap(), "a$/b");
    }

    #[test]
    fn a_default_is_used_when_the_value_is_missing() {
        assert_eq!(
            expand_fixture("Value: ${MISSING:-default}").unwrap(),
            "Value: default"
        );
        assert_eq!(
            expand_fixture("Value: ${MISSING:default}").unwrap(),
            "Value: default"
        );
        assert_eq!(expand_fixture("${MISSING:-}").unwrap(), "");
    }

    #[test]
    fn a_default_is_used_when_the_value_is_empty() {
        let result = expand_with("${EMPTY:-fallback}", |name| (name == "EMPTY").then_some(""));
        assert_eq!(result.unwrap(), "fallback");
    }

    #[test]
    fn a_default_with_a_reference_is_expanded() {
        assert_eq!(
            expand_fixture("${MISSING:-$HOME/x}").unwrap(),
            "/home/dev/x"
        );
        assert_eq!(
            expand_fixture("Value: ${MISSING:-${BAR:-default}}").unwrap(),
            "Value: default"
        );
        assert_eq!(
            expand_fixture("${MISSING:-${HOME}/bin}").unwrap(),
            "/home/dev/bin"
        );
    }

    #[test]
    fn an_existing_value_beats_the_default() {
        assert_eq!(expand_fixture("${FOO:-default}").unwrap(), "bar");
        assert_eq!(expand_fixture("${FOO:-${HOME}}").unwrap(), "bar");
    }

    #[test]
    fn empty_braced_name_is_an_error() {
        let error = expand_fixture("Value: ${}").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn an_invalid_name_is_an_error() {
        for value in ["Value: ${1FOO}", "Value: ${MY-VAR}", "Value: ${A B}"] {
            let error = expand_fixture(value).unwrap_err();
            assert!(
                matches!(error, EnvError::Syntax { .. }),
                "{value}: {error:?}"
            );
        }
    }

    #[test]
    fn a_valid_name_is_a_unicode_letter_then_name_characters() {
        assert!(valid_name("FOO"));
        assert!(valid_name("_internal2"));
        assert!(valid_name("é"));
        assert!(!valid_name(""));
        assert!(!valid_name("1FOO"));
        assert!(!valid_name("MY-VAR"));
        assert!(!valid_name("MY VAR"));
    }

    #[test]
    fn a_required_value_reports_its_message() {
        let error = expand_fixture("Value: ${MISSING:?missing}").unwrap_err();
        assert_eq!(error.to_string(), "missing");
        assert_eq!(
            error,
            EnvError::Required {
                message: "missing".to_owned()
            }
        );
    }

    #[test]
    fn a_required_value_that_exists_is_returned() {
        assert_eq!(expand_fixture("${FOO:?missing}").unwrap(), "bar");
    }

    #[test]
    fn an_assign_returns_the_existing_value_before_the_default() {
        let stored = shared_store();
        let mut options = options_over_shared_store(&stored);
        assert_eq!(options.expand("${FOO:=fallback}").unwrap(), "fallback");
        assert_eq!(options.expand("${FOO:=other}").unwrap(), "fallback");
        assert_eq!(
            stored.borrow().get("FOO").map(String::as_str),
            Some("fallback")
        );
    }

    #[test]
    fn an_assign_stores_the_default_and_returns_it() {
        let stored = shared_store();
        let mut options = options_over_shared_store(&stored);
        assert_eq!(options.expand("Value: ${FOO:=bar}").unwrap(), "Value: bar");
        assert_eq!(stored.borrow().get("FOO").map(String::as_str), Some("bar"));
    }

    /// A store both closures can reach, the way a caller sharing one map
    /// between a lookup and a setter has to.
    type SharedStore = Rc<RefCell<BTreeMap<String, String>>>;

    fn shared_store() -> SharedStore {
        Rc::new(RefCell::new(BTreeMap::new()))
    }

    fn options_over_shared_store(stored: &SharedStore) -> ExpandOptions<'static> {
        let lookup = Rc::clone(stored);
        let set = Rc::clone(stored);
        ExpandOptions::new(move |name| lookup.borrow().get(name).cloned()).with_set(
            move |name, value| {
                set.borrow_mut().insert(name.to_owned(), value.to_owned());
                Ok(())
            },
        )
    }

    #[test]
    fn an_assign_without_a_setter_returns_the_default() {
        assert_eq!(expand_fixture("${MISSING:=bar}").unwrap(), "bar");
    }

    #[test]
    fn a_failing_setter_is_reported_with_its_key() {
        let mut options = ExpandOptions::new(|_: &str| None)
            .with_set(|_, _| Err("store is read-only".to_owned()));
        let error = options.expand("${FOO:=bar}").unwrap_err();
        assert_eq!(
            error,
            EnvError::SetFailed {
                key: "FOO".to_owned(),
                message: "store is read-only".to_owned()
            }
        );
    }

    #[test]
    fn an_escaped_dollar_is_literal_and_is_not_looked_up() {
        let result = expand_with("Price: \\$100", |_| panic!("looked up a variable"));
        assert_eq!(result.unwrap(), "Price: $100");
    }

    #[test]
    fn a_double_dollar_is_one_dollar_and_is_not_looked_up() {
        let result = expand_with("PID: $$", |_| panic!("looked up a variable"));
        assert_eq!(result.unwrap(), "PID: $");
    }

    #[test]
    fn a_colon_after_an_unbraced_name_stays_literal() {
        assert_eq!(
            expand_fixture("Value: $MISSING:-default").unwrap(),
            "Value: :-default"
        );
        assert_eq!(
            expand_fixture("Value: $FOO:-default").unwrap(),
            "Value: bar:-default"
        );
    }

    #[test]
    fn windows_references_are_off_by_default() {
        assert_eq!(expand_fixture("%FOO%").unwrap(), "%FOO%");
    }

    #[test]
    fn windows_references_are_expanded_when_enabled() {
        let mut options =
            ExpandOptions::new(|name: &str| (name == "FOO").then(|| "bar".to_owned()))
                .with_windows_variables(true);
        assert_eq!(options.expand("Value: %FOO%").unwrap(), "Value: bar");
        assert_eq!(options.expand("%MISSING%").unwrap(), "");
    }

    #[test]
    fn an_empty_windows_name_is_an_error() {
        let mut options = ExpandOptions::new(|_: &str| None).with_windows_variables(true);
        let error = options.expand("Value: %%").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn an_unterminated_windows_reference_is_an_error() {
        let mut options = ExpandOptions::new(|_: &str| None).with_windows_variables(true);
        let error = options.expand("Value: %FOO").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn command_substitution_is_literal_until_enabled() {
        assert_eq!(
            expand_fixture("Echo: $(echo hi)").unwrap(),
            "Echo: $(echo hi)"
        );
        // A missing `)` is only a syntax error when substitution is parsed.
        assert_eq!(expand_fixture("$(").unwrap(), "$(");
    }

    #[cfg(unix)]
    #[test]
    fn command_substitution_runs_the_command_and_trims_its_output() {
        let mut options = ExpandOptions::new(|_: &str| None).with_command_substitution(true);
        assert_eq!(options.expand("Echo: $(echo hi)").unwrap(), "Echo: hi");
    }

    #[cfg(unix)]
    #[test]
    fn command_substitution_reports_a_non_zero_exit() {
        let mut options = ExpandOptions::new(|_: &str| None).with_command_substitution(true);
        let error = options.expand("$(sh -c 'exit 7')").unwrap_err();
        assert_eq!(
            error,
            EnvError::CommandFailed {
                message: "exit status 7".to_owned()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_substitution_expands_its_arguments() {
        let mut options =
            ExpandOptions::new(|name: &str| (name == "WORD").then(|| "hi".to_owned()))
                .with_command_substitution(true);
        assert_eq!(options.expand("$(echo $WORD)").unwrap(), "hi");
        // Single quotes suppress expansion, as they do in a shell.
        assert_eq!(options.expand("$(echo '$WORD')").unwrap(), "$WORD");
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_command_expression_is_an_error() {
        let mut options = ExpandOptions::new(|_: &str| None).with_command_substitution(true);
        let error = options.expand("$()").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn a_missing_closing_brace_is_an_error() {
        for value in ["${HOME", "${HOME:-/x", "${FOO:-${BAR}"] {
            let error = expand_fixture(value).unwrap_err();
            assert!(
                matches!(error, EnvError::Syntax { .. }),
                "{value}: {error:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_closing_parenthesis_is_an_error() {
        let mut options = ExpandOptions::new(|_: &str| None).with_command_substitution(true);
        let error = options.expand("$(echo hi").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn positional_arguments_are_supplied_explicitly() {
        let mut options =
            ExpandOptions::new(|_: &str| None).with_unix_args(["cmd", "first", "second"]);
        assert_eq!(options.expand("Arg1: ${1}").unwrap(), "Arg1: first");
        assert_eq!(options.expand("Arg1: $1").unwrap(), "Arg1: first");
        assert_eq!(options.expand("Arg9: ${9}").unwrap(), "Arg9: ");
    }

    #[test]
    fn a_positional_argument_needs_the_option() {
        let error = expand_fixture("Arg1: ${1}").unwrap_err();
        assert!(matches!(error, EnvError::Syntax { .. }), "{error:?}");
    }

    #[test]
    fn tokens_split_at_the_first_operator() {
        assert_eq!(
            parse_token("FOO:-a:-b"),
            (
                "FOO".to_owned(),
                "a:-b".to_owned(),
                super::Fallback::Default
            )
        );
        assert_eq!(
            parse_token("FOO"),
            ("FOO".to_owned(), String::new(), super::Fallback::None)
        );
        assert_eq!(
            parse_token("FOO:?"),
            ("FOO".to_owned(), String::new(), super::Fallback::Required)
        );
    }

    #[test]
    fn expanding_through_an_environment_uses_its_platform_rules() {
        let mut environment = Environment::new(Platform::Windows);
        environment.set("Path", "/usr/bin");
        environment.set("HOME", "/home/dev");
        assert_eq!(expand("$HOME/bin", &environment).unwrap(), "/home/dev/bin");
        assert_eq!(expand("${MISSING:-/root}", &environment).unwrap(), "/root");
        assert_eq!(
            expand_with_options(
                "${MISSING:=/root}",
                &mut ExpandOptions::from_environment(&environment)
            )
            .unwrap(),
            "/root"
        );
        assert_eq!(environment.get("MISSING"), None);
    }
}
