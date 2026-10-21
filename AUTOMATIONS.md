# Automations

This document describes the project's automations, where they live, and what they enforce.

## Local automations (Git hooks)

Git hooks are configured through `core.hooksPath`, pointing to `.githooks/`. Run `./setup-hooks.sh` once from the repository root to enable them.

| Hook file | Trigger | What it does |
|---|---|---|
| `.githooks/commit-msg` | `git commit` | Validates that the first line of the commit message follows [Conventional Commits](https://www.conventionalcommits.org/), using the format `type[(scope)][!]: description`. |

The local hook is a convenience for immediate feedback. The CI workflow also checks pull request titles so that the convention is validated for contributions made without the local hooks.

## Development environment

The repository includes two complementary ways to obtain the Rust toolchain:

| File | Tool | What it does |
|---|---|---|
| `mise.toml` | Mise | Pins the Rust version for local development and CI. |
| `flake.nix` | Nix | Provides a reproducible development shell; it reads the Rust version from `mise.toml`. |
| `.envrc` | direnv | Enters the Nix development shell automatically after `direnv allow`. |

The Nix shell prepends the Mise shims to `PATH`, so both tools use the same pinned version when both are installed. It also provides the native dependencies required to build `open62541`.

## Continuous integration

All GitHub Actions workflows live in `.github/workflows/`. Rust is pinned to the version declared in `mise.toml`, and the `dtolnay/rust-toolchain` pins in the workflows match it.

### `ci.yml` — Core CI

**Trigger:** every push to `main` and every pull request. New commits to a branch cancel the previous run. The workflow can also be dispatched manually.

| Job | Tool | What it checks |
|---|---|---|
| PR title | shell | Pull request titles follow Conventional Commits. |
| Format | `cargo fmt --all -- --check` | Rust code is formatted with rustfmt. |
| Clippy | `cargo clippy --all-targets --all-features -- -D warnings` | No Clippy warnings are present. |
| Test | `cargo test --all` | Unit, integration, and documentation tests pass. |
| Docs | `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps` | Documentation builds without warnings. |
| Unused dependencies | `cargo machete` | No unused dependencies are declared. |

### `security.yml` — Security and license audit

**Trigger:** pushes and pull requests to `main`, plus a weekly scheduled run. The workflow can also be dispatched manually.

| Job | Tool | What it checks |
|---|---|---|
| cargo-deny | `cargo-deny` | Advisories, license compatibility, dependency bans, and registry/source policy. |

The policy is configured in `deny.toml`, including the allowed licenses and the clarifications for the workspace crates and native dependencies.

### `binary-baseline.yml` — Baseline recording

**Trigger:** pushes to `main`.

The workflow builds the release profile, records executable sizes, and stores the result in the GitHub Actions cache. This baseline is made available to `binary-check.yml` for pull requests.

### `binary-check.yml` — Binary size report

**Trigger:** pull requests targeting `main`. It can also be dispatched manually with a `pr-number` input.

The workflow builds the release profile, records executable sizes and a `cargo-bloat` breakdown, compares the sizes with the latest baseline, and posts a report to the pull request. It fails when a matching binary grows by more than 10%. New binaries and missing baselines are reported as warnings rather than failures.

The threshold is configured as `SIZE_THRESHOLD` in the workflow and is currently 10%.

Both workflows operate on the executable targets produced by `cargo build --release`.

## Releases

The release workflows use Conventional Commits and a release pull request to separate version changes from publishing.

### `release-pr.yml` — Release pull request

**Trigger:** pushes to `main`.

The workflow:

1. Resolves the package to release. The `PACKAGE` environment variable selects it explicitly and is set to `runtime`, because the repository is a virtual Cargo workspace without a root package.
2. Computes the next version with `git cliff --bumped-version`, which follows Conventional Commits and includes `BREAKING CHANGE` footers.
3. Skips the release when the computed version is not newer than the current version. `chore`, `docs`, `ci`, `test`, `style`, and `build` commits do not trigger a release.
4. Bumps every workspace crate with `cargo set-version --workspace` so the workspace stays in lockstep.
5. Regenerates `CHANGELOG.md` with `git-cliff` using `cliff.toml`.
6. Opens or updates a `release/auto` pull request with the commit message `chore: release`.
7. Dispatches `ci.yml`, `binary-check.yml`, and `security.yml` on the `release/auto` branch so the release pull request receives the same checks as a normal pull request.

The release pull request is created with `GITHUB_TOKEN`, which does not trigger `pull_request` workflows on its own. The dispatch in step 7 therefore runs the checks against the PR's head commit; it requires the `actions: write` permission.

### `release.yml` — GitHub release and container

**Trigger:** every push to `main`.

The workflow publishes the version currently in `Cargo.toml` when `CHANGELOG.md` contains an entry for it and no `v<version>` tag exists. It is independent of the merge style and idempotent: once the tag exists, later pushes do nothing, and the tag is only created when the release is published.

The workflow:

1. Resolves the package name and version from `cargo metadata`.
2. Runs the test suite with `cargo test --workspace --locked`.
3. Builds the selected package's release binaries with `cargo build --release --locked`.
4. Extracts the latest changelog section as release notes.
5. Creates the GitHub release and attaches the package's binaries. The release action creates the tag at the pushed commit.
6. Builds the `Dockerfile` image and pushes the versioned and `latest` tags to GitHub Container Registry.

## Configuration files

| File | Purpose |
|---|---|
| `deny.toml` | `cargo-deny` advisory, license, dependency, and source policy. |
| `cliff.toml` | `git-cliff` changelog format, commit groups, bump rules, and tag pattern. |
| `.githooks/commit-msg` | Local Conventional Commits validation. |
| `setup-hooks.sh` | Enables the repository's Git hooks. |
| `Cargo.toml` | Virtual workspace and shared package metadata. |
| `Cargo.lock` | Reproducible dependency resolution for the application workspace. |
| `Dockerfile` | Multi-stage container image used by the release workflow and the E2E tests. |
| `flake.nix` | Nix development shell and native build tooling. |
| `mise.toml` | Pinned Rust toolchain version. |
