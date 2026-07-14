# Releasing Mission

Mission is the source of truth for the parser. A release means: bump the version, tag it, let CI
build the binaries, then publish to crates.io. It takes a few minutes.

> **Versions are permanent.** Once `X.Y.Z` is published to crates.io it can never be re-published
> (only *yanked*, which hides it but keeps it forever). Get the pre-flight green before you tag.

## Pre-flight (local)

```sh
cargo fmt --all --check          # formatting (CI enforces this)
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
cargo publish --dry-run          # packages + compiles from the packaged tarball
```

All green? Continue.

## 1. Bump the version

Edit `Cargo.toml` → `version = "X.Y.Z"` (semver: patch = fixes, minor = additive, major = breaking).
If the public API changed, update the README examples too.

```sh
git add Cargo.toml Cargo.lock
git commit -m "chore: release X.Y.Z — <one-line summary>"
git push origin main            # CI runs on main
```

## 2. Tag → triggers the binary release

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The `release` workflow builds 6 targets (linux gnu/musl x86_64+aarch64, macOS x86_64+aarch64,
windows msvc) and attaches 12 assets (binary + `.sha256`) to the GitHub release. `install.sh`
tracks `releases/latest`, so it picks up the new version automatically. Watch it:

```sh
gh run watch "$(gh run list --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
gh release view vX.Y.Z --json assets --jq '.assets | length'   # expect 12
```

## 3. Publish to crates.io

Create a short-lived token at <https://crates.io/settings/tokens> (scope: `publish-update`), then:

```sh
CARGO_REGISTRY_TOKEN=<token> cargo publish
```

Using the env var (not `cargo login`) keeps the token off disk. **Revoke the token afterward** —
it has done its job. Verify:

```sh
curl -s -H "User-Agent: mission (you@example.com)" https://crates.io/api/v1/crates/mission \
  | python -c "import sys,json;d=json.load(sys.stdin)['crate'];print(d['max_version'],'—',d['description'])"
```

## 4. Update the private workspace (Model A)

The private `mission-dev` workspace consumes the published crate. If the member crates pin a version
that needs to move, bump `mission = "X.Y"` in `bridge/aatp-slicer/Cargo.toml` and
`driver/orchestrator/Cargo.toml`, then `cargo update -p mission`.

For **live** parser work before a release, uncomment the `[patch.crates-io]` block in the private
workspace root (`Cargo.toml`) to build the whole workspace against your local `mission` checkout;
comment it back out once the change is released.

---

### Checklist

- [ ] Pre-flight all green (fmt, clippy, test, doctest, dry-run)
- [ ] Version bumped in `Cargo.toml`; README examples still accurate
- [ ] Commit + push `main` → CI green
- [ ] Tag `vX.Y.Z` pushed → release has 12 assets
- [ ] `cargo publish` → new version live on crates.io
- [ ] **Token revoked**
- [ ] Private workspace bumped / re-locked if needed
