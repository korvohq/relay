# Korvo Relay

**See, cap, and route every AI coding call.**

Korvo Relay is a local-first command-line tool for making AI coding costs visible and enforceable. Its v0.1 development build meters provider usage, rejects calls that could cross a hard spend cap, and routes requests through compatible adapters. Later releases will reduce paid context and deflect suitable work to local models.

> [!IMPORTANT]
> Relay is under active development and has not been released. The v0.1 request, adapter, pricing, ledger, cap, and CLI path is implemented and tested locally. Bundled paid-model prices intentionally remain unverified, so `relay ask` fails closed until official prices and API model IDs are reviewed for release. See the [implementation tracker](ARCHITECTURE.md#implementation-tracker).

## Why Relay?

Relay grew out of a surprise $3,000 AI coding bill: the cost was visible only after it had already been incurred.

AI coding tools often expose cost only after requests have been made. Relay puts a local control plane in front of supported provider APIs:

- **Visible:** record tokens, latency, route, and cost for every call.
- **Capped:** calculate a conservative worst-case cost before any network request and refuse calls that could exceed a daily or monthly limit.
- **Swappable:** use one request format across provider adapters.
- **Local-first:** keep credentials, usage history, source indexes, and embeddings on the user's machine.
- **Progressively free:** route suitable requests to a local model in v0.3, with explicit escalation to paid models when needed.

Relay does **not** wrap, proxy, or reverse-engineer the GitHub Copilot client or its private endpoints. The planned v0.2 Copilot spend monitor will only read the user's billing and premium-request usage through documented GitHub APIs when the user's account and authorization permit it. Unsupported accounts will be reported honestly rather than estimated or scraped.

## Roadmap

| Version | Theme | Scope |
| --- | --- | --- |
| **v0.1** | See + Stop | OpenAI and Anthropic adapters, external price table, SQLite ledger, hard daily/monthly caps, model aliases, usage CLI |
| **v0.2** | Shrink + Watch | Budgeted repository context, tree-sitter repo map, git relevance, token-savings reporting, read-only Copilot spend monitoring |
| **v0.3** | Deflect | Quantized local model adapter, local RAG index, deterministic three-tier routing, validation-based escalation, savings report |
| **Later** | Think | Reserved `medha:consensus` adapter and Tier 3 routing; not part of v0.1–v0.3 |

Version boundaries are deliberate. In particular, v0.1 will not include local models, RAG, context minimization, Copilot integration, learned routing, a proxy, IDE integrations, or team features.

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for component responsibilities, contracts, data flow, storage, and release boundaries.

## v0.1 development experience

Relay creates this configuration at `~/.relay/relay.toml` on first run:

```toml
# ~/.relay/relay.toml
[caps]
daily_usd = 5.00
monthly_usd = 50.00
timezone = "UTC"

[models]
default = "openai:gpt-4o-mini"
think = "anthropic:claude-sonnet"

[providers.openai]
api_key_env = "OPENAI_API_KEY"

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
```

The implemented v0.1 commands are:

```console
relay ask "Explain this function"
relay ask --model think "Review this design"
relay usage
relay cap show
relay cap set --daily-usd 5.00 --monthly-usd 50.00
relay models
```

Planned v0.3 indexing commands:

```console
relay index build
relay index status
relay usage --savings
```

Provider credentials will be read from configured environment variables and will never be written to Relay's configuration or ledger.

## Core safety guarantees

1. **No unknown prices.** A paid model without a valid price-table entry is rejected before dispatch.
2. **Cap checks happen before network I/O.** Current spend plus the request's conservative maximum cost must fit both configured caps.
3. **No environment-variable cap bypass.** Cap changes require an explicit configuration operation.
4. **Actual cost uses reported usage.** Estimates protect the cap; final ledger cost is calculated from provider-reported token usage. Local adapters count their own tokens.
5. **Adapters are the stability boundary.** Adding a provider should require an adapter and price data, not changes to metering, caps, or routing.
6. **Private data stays local.** The ledger and future source index live under `~/.relay/`; only request content intentionally sent to a selected remote provider leaves the machine.

## Price data

Model prices are stored in user-updatable `~/.relay/prices.json` rather than compiled into executable logic. Prices shown in product examples are placeholders and must not be treated as current provider pricing. The bundled development entries have `verified = false`; paid calls fail before credential lookup or network dispatch until an entry has official source provenance and is explicitly verified. A release must review prices and API model IDs, and CI will eventually check freshness.

## Development

Relay is a Rust project using the 2024 edition. Build and check it with:

```bash
cargo fmt --check
cargo check
cargo test
```

Try the local-only commands without provider credentials or network requests:

```bash
cargo run -- models
cargo run -- cap show
cargo run -- usage
```

The first implementation milestone follows this strict order:

1. Adapter contract and shared conformance suite
2. External model price table and validation
3. Meter and local SQLite ledger
4. Transactional hard-cap enforcement, including a zero-network-request refusal test
5. Configuration and CLI

Before submitting implementation changes, preserve the version non-goals and include tests for money-path behavior. Provider adapters must pass the shared conformance suite.

## Security and privacy

Please do not open a public issue containing API keys, source code from a private repository, billing exports, or ledger contents. Relay must redact secrets from diagnostics and must never log authorization headers. A dedicated private vulnerability-reporting channel will be documented before the first release.

## Project status

The v0.1–v0.3 scope is frozen, but APIs may still change until the first release. The v0.1 core path is implemented; release remains blocked on official price/model verification, reconciliation commands, expanded provider error conformance, and a credential-gated smoke test. The initial target remains a fresh-install-to-first-metered-call experience of under five minutes.

## License

Copyright © 2025–present Snab Limited (trading as Korvo).

Korvo Relay is free and open-source software licensed under the [GNU Affero General Public License v3.0](LICENSE).
