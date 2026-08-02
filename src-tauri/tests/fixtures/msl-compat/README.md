# mSL compatibility corpus

These scripts represent common, real-world mIRC scripting patterns rather than
isolated parser examples. The Rust test `real_world_msl_compatibility_corpus`
loads them together and verifies aliases, event handlers, channel-state
identifiers, commands, and script-defined dialogs.

Run the corpus with:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml real_world_msl_compatibility_corpus
```

Add regressions as small `.msl` fixtures with deterministic inputs and expected
actions in the corpus test. Fixtures use `.msl` because repository-wide `.mrc`
files are ignored to avoid accidentally committing users' scripts or secrets.
