# Contributing to Korvo Relay

Thank you for contributing to Korvo Relay.

## Developer Certificate of Origin

All contributions require sign-off under the [Developer Certificate of Origin v1.1](https://developercertificate.org/). By signing off a commit, you certify that you have the right to submit the contribution under this repository's license.

Create signed-off commits with:

```bash
git commit -s
```

Git adds a trailer using your configured name and email:

```text
Signed-off-by: Your Name <your.email@example.com>
```

Use your real name and an email address associated with the contribution. Every commit in a pull request must contain a valid `Signed-off-by` trailer. If needed, sign off the latest commit with:

```bash
git commit --amend --signoff --no-edit
```

DCO sign-off is required for every contribution and will be enforced by the repository's DCO GitHub App check. Maintainers must install and require that check before accepting external contributions. Relay uses DCO only; contributors are not required to sign a Contributor License Agreement.

## Before submitting

Run the local quality checks:

```bash
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Keep changes within the frozen release scope documented in [`ARCHITECTURE.md`](ARCHITECTURE.md), and include tests for changed behavior.


