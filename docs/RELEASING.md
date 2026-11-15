# Releasing

The three crates share one version (`[workspace.package] version`) and are
released together. `xtask` is never published (`publish = false`).

## Order

Each crate depends on the one before it, so publish in this order:

1. `zkteco-proto`
2. `zkteco-client`
3. `zkctl`

`cargo publish --workspace` (Cargo 1.90 or later) publishes them in this
order and waits for each to appear in the index before the next.

## Checklist

1. Start from a clean `main` with CI green.
2. Set the new version in the root `Cargo.toml`: `[workspace.package]
   version`, and the `version` of the `zkteco-proto` and `zkteco-client`
   entries in `[workspace.dependencies]`.
3. In `CHANGELOG.md`, move the `Unreleased` entries under a new heading
   with the version and date, and update the comparison links at the
   bottom.
4. Update the status lines and install commands in `README.md`, the crate
   READMEs and `docs/GUIDE.md` if the version appears there.
5. Run the full gate:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo +1.88 clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace
   cargo +1.88 test --workspace
   RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps
   cargo deny check
   cargo publish --workspace --dry-run
   ```

   Repeated dry runs of the same version can verify `zkctl` against a
   stale build of the previous `zkteco-client` tarball, because Cargo
   fingerprints registry crates by version, not content. If the dry run
   fails with an API that does exist, rerun it with a fresh target
   directory (`CARGO_TARGET_DIR=$(mktemp -d)`).

6. Check what each package contains, and that no capture, template or
   personal data is in it:

   ```sh
   cargo package -p zkteco-proto --list
   cargo package -p zkteco-client --list
   cargo package -p zkctl --list
   ```

7. Commit as "Release X.Y.Z", tag `vX.Y.Z`, and push the commit and tag.
8. `cargo publish --workspace`.
9. Create a GitHub release from the tag with the changelog section as its
   notes. Attach man pages (`cargo xtask man`) if wanted.
10. Add a fresh `## [Unreleased]` heading to `CHANGELOG.md`.

A published version cannot be replaced, only yanked
(`cargo yank --version X.Y.Z <crate>`). Yank all three crates together if
the release is broken, and publish a fixed version.

## Pre-releases

While the project is alpha, versions are `0.1.0-alpha.N`. Cargo never
selects a pre-release unless asked for it explicitly, so users install with
`cargo install zkctl --version 0.1.0-alpha.N` or depend on
`"0.1.0-alpha.N"`.
