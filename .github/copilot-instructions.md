# Copilot Instructions

## After every code change

Run the following checks in order and fix any issues before considering the task done:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

- `cargo fmt` – enforces consistent formatting; fix by running `cargo fmt --all`
- `cargo clippy` – must produce zero warnings (all warnings are errors via `-D warnings`)
- `cargo test` – all tests must pass
