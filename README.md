# neotales-env

Expand environment-variable references, compose `PATH` values and build reversible shell changes without changing the process environment.

By [x0craft](https://github.com/x0craft).

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-env)

<!-- badges: crates.io, docs.rs, CI. Add once the crate name and first version exist. -->

## Install 📦

```sh
cargo add neotales-env
```

## What you can do 🔧

**Expand a value from an explicit environment.** `$NAME` and `${NAME}` use the values you provide. An unset name becomes an empty string.

```rust
use neotales_env::{Environment, Platform, expand};

let mut environment = Environment::new(Platform::Unix);
environment.insert("HOME", "/home/dev");

assert_eq!(expand("$HOME/bin", &environment).unwrap(), "/home/dev/bin");
```

**Use a default, require a value, or set one.** `${NAME:-fallback}` supplies a default when the name is missing or empty, `${NAME:?message}` turns a missing value into that message, and `${NAME:=fallback}` stores the default through a setter you provide.

```rust
use neotales_env::{Environment, Platform, expand};

let environment = Environment::new(Platform::Unix);

assert_eq!(
    expand("${MISSING:-/usr/local}", &environment).unwrap(),
    "/usr/local"
);

let error = expand("${MISSING:?MISSING is required}", &environment).unwrap_err();
assert_eq!(error.to_string(), "MISSING is required");
```

**Turn on the risky forms deliberately.** Windows `%NAME%` references and `$(command)` substitution are off by default. The options type enables either one for the call that needs it.

```rust
use neotales_env::ExpandOptions;

let mut options = ExpandOptions::new(|name| {
    (name == "HOME").then(|| "/home/dev".to_owned())
})
.with_windows_variables(true);

assert_eq!(options.expand("%HOME%/bin").unwrap(), "/home/dev/bin");
```

**Supply your own lookup.** Use `expand_with` when values come from another source or when you want to define the lookup inline.

```rust
use neotales_env::expand_with;

let path = expand_with("$HOME/bin", |name| (name == "HOME").then_some("/home/dev"));
assert_eq!(path.unwrap(), "/home/dev/bin");
```

**Compose a `PATH` without guessing the separator.** `Platform` supplies the separator and the case rules, and `Environment` reads, writes and splits the value.

```rust
use neotales_env::{Environment, Platform};

let mut environment = Environment::new(Platform::Unix);
environment.set_path("/usr/bin");
environment.prepend_path("/opt/bin");

assert_eq!(environment.path(), "/opt/bin:/usr/bin");
assert!(environment.has_path("/usr/bin"));
```

![One does not simply expand variables](https://media.giphy.com/media/smW5FBep69d3q/giphy.gif)

## Licence 📜

MIT. See [LICENSE.md](LICENSE.md).
