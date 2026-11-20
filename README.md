# neotales-env

Expand environment-variable references, compose `PATH` values and build reversible shell changes without hidden process mutation.

By [x0craft](https://github.com/x0craft).

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-env)

<!-- badges: crates.io, docs.rs, CI. Add once the crate name and first version exist. -->

## Install 📦

```sh
cargo add neotales-env
```

## What you can do 🔧

**Expand against the process.** `ExpandOptions::new()` reads through `get_var` and stores `${NAME:=default}` through `set_var`. Windows `%NAME%` references and `$(command)` substitution stay off.

```rust
use neotales_env::{ExpandOptions, expand, set_var};

set_var("NEOTALES_ENV_DEMO", "/opt/demo").unwrap();
assert_eq!(
    expand("$NEOTALES_ENV_DEMO", ExpandOptions::new()).unwrap(),
    "/opt/demo"
);
```

**Override either callback.** `with_get` and `with_set` replace the defaults completely, so the closures below never touch the process.

```rust
use neotales_env::ExpandOptions;

let mut options = ExpandOptions::new()
    .with_get(|name| (name == "HOME").then(|| "/home/dev".to_owned()))
    .with_set(|name, value| {
        assert_eq!((name, value), ("BRAND", "neotales"));
        Ok(())
    });

assert_eq!(options.expand("${HOME}/bin").unwrap(), "/home/dev/bin");
assert_eq!(options.expand("${BRAND:=neotales}").unwrap(), "neotales");
```

**Use a default or require a value without the process.** `expand_with` takes a lookup and never reads the process. Its setter is a no-op, so `${NAME:=default}` returns the default without storing it.

```rust
use neotales_env::expand_with;

assert_eq!(
    expand_with("${MISSING:-/usr/local}", |_| None).unwrap(),
    "/usr/local"
);

let error = expand_with("${MISSING:?MISSING is required}", |_| None).unwrap_err();
assert_eq!(error.to_string(), "MISSING is required");
```

**Turn on the risky forms deliberately.** Windows `%NAME%` references and `$(command)` substitution are off by default. The options type enables either one for the call that needs it.

```rust
use neotales_env::ExpandOptions;

let mut options = ExpandOptions::new()
    .with_get(|name| (name == "HOME").then(|| "/home/dev".to_owned()))
    .with_windows_variables(true);

assert_eq!(options.expand("%HOME%/bin").unwrap(), "/home/dev/bin");
```

**Compose a `PATH` without the process.** `Platform` supplies the separator and the case rules; the helpers return a new string.

```rust
use neotales_env::{Platform, path_append, path_prepend};

let path = path_prepend(Some("/usr/bin"), &["/opt/bin".to_owned()], Platform::Unix);
assert_eq!(path, "/opt/bin:/usr/bin");

let path = path_append(Some(&path), &["/usr/local/bin".to_owned()], Platform::Unix);
assert_eq!(path, "/opt/bin:/usr/bin:/usr/local/bin");
```

**Change the process environment when you mean to.** These crate-root functions edit the process; the helpers above and the custom callbacks stay in memory. The process functions also come as `unset_var`, `set_path`, `append_path`, `has_var` and `vars`.

```rust
use neotales_env::{get_var, has_path, prepend_path, set_var};

set_var("NEOTALES_ENV_DEMO", "/opt/demo").unwrap();
assert_eq!(get_var("NEOTALES_ENV_DEMO").as_deref(), Some("/opt/demo"));

prepend_path("/opt/bin").unwrap();
assert!(has_path("/opt/bin"));
```

![One does not simply expand variables](https://media.giphy.com/media/smW5FBep69d3q/giphy.gif)

## Licence 📜

MIT. See [LICENSE.md](LICENSE.md).
