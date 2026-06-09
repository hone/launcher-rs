# Cloud Native Buildpacks Launcher

[![Build Status](https://github.com/buildpacks/launcher/workflows/build/badge.svg)](https://github.com/buildpacks/launcher/actions)

A fast, secure Rust port of the Cloud Native Buildpacks [lifecycle](https://github.com/buildpacks/lifecycle) launcher.

## Supported APIs
| Lifecycle Version | Platform APIs                                                                                                                                    | Buildpack APIs                                                                                                                 |
|-------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|
| 0.21.x            | [0.7][p/0.7], [0.8][p/0.8], [0.9][p/0.9], [0.10][p/0.10], [0.11][p/0.11], [0.12][p/0.12], [0.13][p/0.13], [0.14][p/0.14], [0.15][p/0.15]        | [0.7][b/0.7], [0.8][b/0.8], [0.9][b/0.9], [0.10][b/0.10], [0.11][b/0.11], [0.12][b/0.12]                                       |
| 0.20.x            | [0.7][p/0.7], [0.8][p/0.8], [0.9][p/0.9], [0.10][p/0.10], [0.11][p/0.11], [0.12][p/0.12], [0.13][p/0.13], [0.14][p/0.14]                         | [0.7][b/0.7], [0.8][b/0.8], [0.9][b/0.9], [0.10][b/0.10], [0.11][b/0.11]                                                       |

[b/0.7]: https://github.com/buildpacks/spec/tree/buildpack/v0.7/buildpack.md
[b/0.8]: https://github.com/buildpacks/spec/tree/buildpack/v0.8/buildpack.md
[b/0.9]: https://github.com/buildpacks/spec/tree/buildpack/v0.9/buildpack.md
[b/0.10]: https://github.com/buildpacks/spec/tree/buildpack/v0.10/buildpack.md
[b/0.11]: https://github.com/buildpacks/spec/tree/buildpack/v0.11/buildpack.md
[b/0.12]: https://github.com/buildpacks/spec/tree/buildpack/0.12/buildpack.md
[p/0.7]: https://github.com/buildpacks/spec/blob/platform/v0.7/platform.md
[p/0.8]: https://github.com/buildpacks/spec/blob/platform/v0.8/platform.md
[p/0.9]: https://github.com/buildpacks/spec/blob/platform/v0.9/platform.md
[p/0.10]: https://github.com/buildpacks/spec/blob/platform/v0.10/platform.md
[p/0.11]: https://github.com/buildpacks/spec/blob/platform/v0.11/platform.md
[p/0.12]: https://github.com/buildpacks/spec/blob/platform/v0.12/platform.md
[p/0.13]: https://github.com/buildpacks/spec/blob/platform/v0.13/platform.md
[p/0.14]: https://github.com/buildpacks/spec/blob/platform/v0.14/platform.md
[p/0.15]: https://github.com/buildpacks/spec/blob/platform/0.15/platform.md

## Usage

### Run

* `launcher` - Invokes a chosen process.

## Development

To run the test suite and verify changes:
```bash
cargo check
cargo test
```
