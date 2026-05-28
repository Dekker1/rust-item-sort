# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1](https://github.com/Dekker1/rust-item-sort/compare/v0.2.0...v0.2.1) - 2026-05-28

### Fixed

- handle `foreign_mod_item`, `tuple_type`, and `unit_type`

### Other

- fix automatically updating the floating tags

## [0.2.0](https://github.com/Dekker1/rust-item-sort/compare/v0.1.1...v0.2.0) - 2026-05-28

### Added

- sort identifiers using Rust style guide version sorting

### Fixed

- use stable sort to preserve order of equal items
- correct three rendering bugs

### Other

- maintain floating tag for GitHub action

## [0.1.1](https://github.com/Dekker1/rust-item-sort/compare/v0.1.0...v0.1.1) - 2026-05-27

### Added

- GitHub action

### Fixed

- preserve indentation and fix leading blank line in item_sort_str
