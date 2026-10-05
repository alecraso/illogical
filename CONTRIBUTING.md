# Contributing

Thanks for wanting to help.

- **Questions and ideas:** [Discussions](https://github.com/arugula-salad/illogical/discussions).
- **Bugs:** open an issue with the bug template. Say what you ran, what you
  expected and what happened, plus `illogical --version` and your OS.
- **Security problems:** see [SECURITY.md](SECURITY.md). Please don't
  file those as issues.

## Changes

[docs/development.md](docs/development.md) covers building and the code's
layout, and [docs/testing.md](docs/testing.md) the tests. In short:

```sh
just bootstrap   # once: toolchains and the web client's packages
just check       # what CI runs: format, lints, the Rust tests, interop and control
just e2e         # the browser tests (not in CI yet; run them if you touched web/)
```

- Open an issue first for anything bigger than a small fix, so we can agree
  on the shape before you spend time on it.
- Keep a pull request to one change, with tests where it changes behaviour,
  and docs updated if they'd become wrong.
- CI runs on our own machines, so a maintainer starts it for pull requests
  from forks after a look at the diff.

By contributing you agree your work is licensed under the project's terms:
MIT OR Apache-2.0, at the user's option.
