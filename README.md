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
