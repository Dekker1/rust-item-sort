# rust-item-sort

An opinionated tool (library + CLIs) that sorts/reorders items in a Rust source file in the following order:

1. `mod <name>`
3. `use`
4. sorted `const`/`static`
5. sorted `struct`/`enum`/`union`/`type`/`trait`
6. `fn`
7. `impl`
8. `mod <name> { ... }`

## Commands

### `rust-item-sort`

Sort one or more *root* files, following `mod foo;` declarations to visit module files (similar to rustfmt).

```sh
rust-item-sort path/to/lib.rs
rust-item-sort --check path/to/lib.rs
rust-item-sort --write path/to/lib.rs
```

### `cargo item-sort`

Mirrors `cargo fmt` target selection, but runs `rust-item-sort` first and then invokes `rustfmt`:

```sh
cargo item-sort
cargo item-sort --check
cargo item-sort -p my_package
cargo item-sort --all
cargo item-sort -- --config-path rustfmt.toml
```

## GitHub Action

`rust-item-sort` is available as a GitHub Action that installs the tool and runs `cargo item-sort --check` in your CI pipeline.

```yaml
- uses: Dekker1/rust-item-sort@v0
```

### Inputs

| Input | Description | Default |
|---|---|---|
| `version` | Version of `rust-item-sort` to install (e.g. `"0.1.0"`). | `"latest"` |
| `args` | Extra arguments forwarded to `cargo item-sort --check` (e.g. `"--all"` or `"-p my_crate"`). | `""` |
| `working-directory` | Directory in which to run the check. | `"."` |

### Examples

Check all packages in a workspace:

```yaml
- uses: Dekker1/rust-item-sort@v0
  with:
    args: --all
```

Pin to a specific version and check a single package:

```yaml
- uses: Dekker1/rust-item-sort@v0
  with:
    version: "0.1.1"
    args: -p my_crate
```
