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

assert_eq!(expand("$HOME/bin", &environment), "/home/dev/bin");
```

**Supply your own lookup.** Use `expand_with` when values come from another source or when you want to define the lookup inline.

```rust
use neotales_env::expand_with;

let path = expand_with("$HOME/bin", |name| (name == "HOME").then_some("/home/dev"));
assert_eq!(path, "/home/dev/bin");
```

![One does not simply expand variables](https://media.giphy.com/media/smW5FBep69d3q/giphy.gif)

## Licence 📜

MIT. See [LICENSE.md](LICENSE.md).
