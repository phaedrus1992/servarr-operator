# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Added

- Release automation: `cargo-release` + Keep a Changelog, with the multi-arch
  container image and Helm charts published to GHCR on each `v*` tag.

### Changed

- Make `image.repository` optional on the ServarrApp CR. Any omitted image
  sub-field inherits the per-`app` default, so you can pin only `image.tag`
  (e.g. a `develop` build) without repeating the default repository. The same
  inheritance now applies to `DEFAULT_IMAGE_<APP>_*` operator overrides.

<!-- next-url -->
[Unreleased]: https://github.com/phaedrus1992/servarr-operator/compare/v0.1.0...HEAD
