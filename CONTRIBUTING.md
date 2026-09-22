# Contributing

    cargo fmt
    cargo clippy --all-targets -- -D warnings
    cargo test

The same three commands run in CI (`.github/workflows/ci.yml`) on every push and pull request, so a green
run there means these pass locally too. `cargo test` is 24 tests, none of them touch real USB hardware.

Tests live next to the module they test but in a separate file, under `tests/unit/` (e.g.
`src/hub.rs` is tested by `tests/unit/hub_tests.rs`, wired in with `#[path = "..."] mod tests;`).
Add new tests there rather than inline, to keep the source files themselves free of test code.

Adding a protocol (WebRTC, HLS, ...): see the "Adding a protocol" section of `README.md`.

Changing anything under `android/`: the Android workflow (`.github/workflows/android.yml`) builds the Rust
library for Android and the app's debug APK on every push that touches `src/`, `android/`, `Cargo.toml` or
`build.rs`. You can also run it by hand from the Actions tab ("Run workflow").
