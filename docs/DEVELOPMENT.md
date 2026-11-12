# Development and release verification

Use Rust 1.99.1 and the checked-in Cargo.lock. Linux requires libheif >=1.23.7
and pkg-config. The release advertises IPC protocol 1 independently of its
package version. Increment that protocol for incompatible semantics and update
the native guard and behavioral fixtures together.

Run `cargo test --locked --workspace`, `cargo fmt --check`, and
`cargo clippy --locked --workspace --all-targets -- -D warnings`.
On actual Omarchy run `omarchy plugin validate .` and `python scripts/test-qml`.
The Python harness is a development tool, not a production Linux dependency.
Use a separate source checkout for Python development, or put UV_PROJECT_ENVIRONMENT outside the installed plugin checkout: Omarchy validation rejects symlinks, including those inside a virtual environment. Fixtures do not establish real Apple authentication or visible UI acceptance.

The optional Mac helper uses uv-managed Python >=3.13.18,<3.14 and pinned OSXPhotos
0.77.2 on Darwin. `uv run --frozen --extra bridge --extra dev pytest -q` runs
its fixtures. The source tree intentionally excludes the historical Python
Linux client and its command entry point. Helper pairing has no dependency on it.

CI uses a digest-pinned Arch container, a dated signed Arch package snapshot,
immutable GitHub action commits and locked Cargo/uv dependencies. It records
compiler/runtime identities. It uploads build artifacts without publishing
releases. Repeating an archive with the same executable/source identity and
SOURCE_DATE_EPOCH yields normalized tar/gzip metadata; this does not promise
identical compiler output across changed native toolchains/system libraries.

`scripts/package-release --binary target/release/omarchy-photo-wallpapers
--output NEW_DIRECTORY` packages a tested x86_64 Arch executable, notices,
checksums and BUILD.json. CI passes its exact checked commit using --source-commit and the original commit epoch, avoiding an unrelated synthetic export commit. The destination must not already exist. Shared
library dependencies remain external and are recorded. Retain the complete
license directory when redistributing. `scripts/collect-licenses` collects
texts from the exact resolved local Cargo packages; the inventory marks the
Linux normal/build graph. Optional target/development entries are a superset.

`cargo run --locked --example promo_prepare -- INPUT_DIRECTORY NEW_PROFILE [WIDTH HEIGHT]`
prepares the four documented synthetic wallpapers for the supplied actual output dimensions (default 2880×1800) using the real pipeline in
an isolated profile. It does not authenticate, download, mutate a personal
library or apply a desktop background. Running a daemon with that profile and
`--no-network` demonstrates cached pipeline behavior, not iCloud integration.
See assets/promo for capture/composition provenance. Actual desktop acceptance
requires separately controlled UI/theme activation and restoration.

Marketplace publication remains an explicit owner/maintainer action. GitHub
private vulnerability reporting should be enabled on the public repository.
Never execute remote source from an unreviewed mutable branch through an
installer, and never introduce download-to-shell setup paths.
