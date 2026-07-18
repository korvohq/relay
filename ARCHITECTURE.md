# Korvo Relay Architecture

**Status:** Draft for implementation  
**Scope:** Frozen product plan for v0.1 through v0.3  
**Implementation language:** Rust 2024 edition

This document turns the Relay product specification into implementation boundaries and safety rules. It describes planned behavior; the current crate is still a minimal scaffold.

## 1. Goals and constraints

Relay is a local control plane for AI coding requests. It must:

- expose the token and dollar cost of supported provider calls;
- refuse a request before network I/O when its conservative maximum cost would exceed a configured cap;
- isolate provider-specific behavior behind one stable adapter contract;
- keep credentials, usage records, indexes, and embeddings on the local machine;
- evolve from direct provider selection to local-first deterministic routing without rewriting the money path.

The following are permanent architectural constraints:

1. Unknown or invalid pricing fails closed.
2. Cap enforcement precedes provider dispatch.
3. Provider-reported usage determines final remote-call cost.
4. A new provider is implemented as an adapter plus price data.
5. Relay never wraps or reverse-engineers GitHub Copilot clients or private endpoints.
6. Medha is a reserved adapter namespace and capability, not an implementation commitment before a later release.

## 2. System context

```text
User / shell
     |
     v
relay CLI
     |
     v
Request pipeline ----------------------------------------------------+
  CapEnforcer -> ContextBuilder -> Router -> ProviderAdapter         |
       ^              (v0.2+)      |       |                         |
       |                           |       +-> OpenAI    (v0.1)      |
       |                           |       +-> Anthropic (v0.1)      |
       |                           |       +-> Local      (v0.3)      |
       |                           |       +-> Medha      (reserved)  |
       |                           v                                 |
       +-------------------- PriceCatalog                            |
                                                                   |
Provider response -> Meter -> Ledger (SQLite) -> CLI output <-------+

Read-only side channel (v0.2):
GitHub documented billing API -> CopilotSpendMonitor -> usage output

Local context side channel (v0.3):
Repository -> Indexer -> local embeddings/index -> ContextBuilder
```

The logical order above places cap enforcement first. Before checking a cap, the application may perform local-only work required to resolve configuration, pricing, model limits, token estimates, and context size. **No provider network request may occur before cap approval.** In v0.3, routing that can select a paid tier must complete before the final paid-call cap check.

## 3. Component responsibilities

### 3.1 CLI

The CLI parses commands and renders results. It does not contain provider, pricing, or cap logic.

Planned commands by release:

| Command | Release | Responsibility |
| --- | --- | --- |
| `relay ask` | v0.1 | Submit a chat request and print response, usage, and cost |
| `relay usage` | v0.1 | Summarize today, month, and model usage |
| `relay cap set` | v0.1 | Explicitly update configured caps |
| `relay models` | v0.1 | List aliases, models, prices, and availability |
| `relay index build` | v0.3 | Build or rebuild the local repository index |
| `relay index status` | v0.3 | Report index location, freshness, and document counts |
| `relay usage --savings` | v0.3 | Report deflection and estimated avoided spend |

### 3.2 Configuration

Configuration resolves defaults, aliases, provider credential environment-variable names, caps, and context budgets. Secrets are read from the environment only when an adapter needs them; resolved secret values must not be serialized or included in diagnostics.

Proposed locations:

```text
~/.relay/relay.toml       user configuration
~/.relay/prices.json      optional user-managed price catalog
~/.relay/ledger.db        usage ledger
~/.relay/index/           repository indexes and embeddings (v0.3)
```

An explicit command or file edit may change a cap. Environment variables and per-command flags may not weaken caps.

### 3.3 PriceCatalog

`PriceCatalog` loads model metadata from JSON and validates it before use. Pricing is data, not Rust source code.

Each canonical model key has the form `<adapter>:<model>` and includes:

```json
{
  "input_per_mtok": 0.0,
  "output_per_mtok": 0.0,
  "max_context": 0
}
```

Production entries must contain verified, non-negative finite prices, a positive context limit, source provenance, and a freshness timestamp. Example prices in planning documents are placeholders. Missing, stale according to the configured policy, malformed, or non-finite paid-model prices cause request refusal rather than a zero-cost assumption.

Cost is calculated with decimal arithmetic, never binary floating-point:

```text
cost = input_tokens  * input_per_mtok  / 1,000,000
     + output_tokens * output_per_mtok / 1,000,000
```

The database stores `cost_microusd INTEGER` as the authoritative monetary value from migration 1. Cap comparisons and aggregates use integer microdollars; the CLI converts them to decimal dollars for display. Rounding must be conservative during pre-flight estimation. A floating-point dollar value must never be used for accounting or cap decisions.

### 3.4 ProviderAdapter

The adapter contract is the main stability boundary. Relay normalizes chat requests to an OpenAI-style sequence of role/content messages, while adapters translate to and from provider wire formats.

Conceptual Rust contract:

```rust,ignore
trait ProviderAdapter {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> CapabilitySet;
    fn estimate_tokens(&self, request: &RelayRequest) -> Result<TokenEstimate>;
    async fn complete(&self, request: &RelayRequest) -> Result<RelayResponse>;
}

struct RelayRequest {
    messages: Vec<Message>,
    model: String,
    max_output_tokens: Option<u32>,
    stream: bool,
    metadata: Metadata,
}

struct RelayResponse {
    text: String,
    usage: Usage,
    model: String,
    latency_ms: u64,
    raw: RedactedRawResponse,
}
```

`CapabilitySet` initially contains `chat`; v0.3 adds `embed` where applicable, and `consensus` is reserved. Core code selects adapters through a registry rather than matching concrete provider types.

Adapter rules:

- use bounded connect and request timeouts;
- never log credentials or authorization headers;
- distinguish transport, authentication, rate-limit, provider, malformed-response, and cancellation errors;
- return provider-reported input/output usage for remote calls;
- count tokens with the actual tokenizer for local calls;
- preserve only a redacted raw response needed for diagnostics;
- do not silently substitute models;
- support streaming only after usage accounting and interrupted-stream behavior are specified and tested.

Although the request shape reserves `stream`, v0.1 may reject `stream = true` rather than risk unmetered partial responses.

### 3.5 CapEnforcer

`CapEnforcer` is the money-path boundary. For a selected paid model it:

1. obtains an adapter token estimate for the complete outbound request;
2. derives a bounded maximum output-token count from the request or a validated model/config default;
3. verifies the request fits the model context window;
4. calculates a conservatively rounded worst-case cost;
5. atomically compares current spend plus active reservations plus that cost against daily and monthly caps;
6. refuses with `CapExceededError` or creates a short-lived reservation;
7. permits dispatch only after the reservation is durable.

Reservations prevent two concurrent Relay processes from independently passing a cap check and exceeding the cap together. A reservation is finalized with actual reported cost after a response, or released on a confirmed pre-dispatch failure. Ambiguous transport failures require a recorded reconciliation state because a provider may have processed the request even when Relay did not receive usage.

At 80% utilization, Relay emits a warning but does not alter routing. The warning threshold is computed against both periods. A cap refusal must leave the adapter request counter at zero in tests.

### 3.6 Router

In v0.1, the router only resolves a model alias to a canonical adapter/model pair. It must not infer task complexity.

In v0.3, routing becomes deterministic and ordered:

1. explicit `--model` or `--think` override;
2. context-window feasibility;
3. task-class heuristics;
4. local answer validation;
5. immediate repeat/failure escalation.

The tiers are:

| Tier | Name | Typical target |
| --- | --- | --- |
| 0 | `local` | Quantized local coding model; free |
| 1 | `cheap` | Low-cost remote model |
| 2 | `reasoning` | Higher-cost reasoning model |
| 3 | `truth` | Reserved `medha:consensus`; later release |

Every decision includes a machine-readable reason and is written to the ledger. Any route to a paid adapter re-enters cap enforcement with the exact selected model and context. Automatic escalation is never allowed to bypass a cap.

### 3.7 Meter and Ledger

`Meter` combines actual usage with the price snapshot used for the request. `Ledger` provides durable storage and aggregate queries. The initial call record follows the product schema:

```sql
CREATE TABLE calls (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    tokens_in INTEGER,
    tokens_out INTEGER,
    cost_microusd INTEGER NOT NULL CHECK (cost_microusd >= 0),
    latency_ms INTEGER,
    session_id TEXT,
    route_tier TEXT DEFAULT 'direct',
    deflected INTEGER DEFAULT 0
);
```

The implementation should strengthen this baseline with migrations and fields/tables for request status, price snapshot, routing reason, context tokens saved, idempotency/correlation IDs, and cap reservations. SQLite write-ahead logging, busy timeouts, foreign keys, and explicit transactions are required for safe multi-process use.

Time semantics must be explicit: timestamps are stored in UTC; daily/monthly cap windows are calculated in a persisted configured timezone so changing the host timezone cannot unexpectedly reset a budget.

The ledger is local-only. File permissions should restrict access to the current user where the platform supports it. Raw prompts and responses are not required for accounting and should not be stored in the ledger.

### 3.8 ContextBuilder (v0.2)

`ContextBuilder` constructs context under explicit component budgets:

```text
total = repo map + selected file content + conversation history
```

Inputs include explicit `--files`, tree-sitter symbol/signature maps, staged and modified files, recent edit signals, and history. Explicit files outrank inferred files. Selection is deterministic for identical repository state and input.

When over budget, truncation occurs in this order:

1. oldest/lowest-priority history;
2. lowest-ranked repo-map entries;
3. lowest-ranked inferred file content.

Explicitly selected content has priority but is still subject to the hard model context limit; Relay reports truncation instead of silently sending an oversized request. The builder records estimated tokens before and after minimization so the ledger can report tokens saved.

### 3.9 CopilotSpendMonitor (v0.2)

This component is an isolated, read-only integration with documented GitHub billing/usage APIs. As verified against GitHub's published REST description, premium-request usage endpoints exist for both users and organizations. Actual availability still depends on the account, plan, billing platform, and token permissions; Relay reports unsupported access explicitly and never fills gaps by scraping or estimation. It reports available premium-request usage alongside, but separately from, Relay's API ledger.

Endpoints verified on July 18, 2026:

- `/users/{username}/settings/billing/premium_request/usage`
- `/organizations/{org}/settings/billing/premium_request/usage`

GitHub APIs and billing eligibility can change. Revalidate these contracts, required permissions, plan coverage, and response semantics against GitHub's official documentation before implementing or releasing v0.2.

It is not a `ProviderAdapter`, cannot submit Copilot prompts, and has no path into request routing. GitHub API failures must not disable Relay's provider usage report.

### 3.10 Local model and index (v0.3)

The local adapter targets a llama.cpp-compatible runtime and implements the same adapter contract as remote providers. Setup detects available RAM and supported acceleration, recommends a viable quantized model, or disables Tier 0 gracefully.

The local index contains:

- semantic file chunks;
- structural repo-map symbols;
- git-recency weights;
- local embedding vectors;
- source fingerprints and index-version metadata.

Indexes are namespaced by a stable repository identity to prevent cross-project retrieval. Symlinks, ignored files, binary files, generated content, file-size limits, and secret-like files require explicit indexing policy. Incremental updates use content fingerprints and a pre-query freshness check; watch mode is optional.

Retrieval selects candidates, while `ContextBuilder` remains responsible for hard budgets. Neither source text nor embeddings are sent to a hosted Relay service.

## 4. Request lifecycle

### 4.1 v0.1 remote request

```text
1. Parse CLI input.
2. Load and validate configuration and price catalog.
3. Resolve alias to an exact adapter/model.
4. Normalize RelayRequest and reject unsupported options.
5. Estimate input tokens and bounded maximum output.
6. Atomically reserve worst-case daily/monthly budget.
7. Resolve provider credential and dispatch exactly one request.
8. Normalize response and validate reported usage.
9. Calculate actual cost from the request's price snapshot.
10. Transactionally record the call and settle the reservation.
11. Print response, tokens, latency, cost, and cap warning if applicable.
```

Steps 1–6 perform no provider network I/O. If any of them fail, step 7 is unreachable.

### 4.2 v0.3 local-first request

```text
1. Build budgeted context using local repository data.
2. Apply explicit overrides and deterministic routing rules.
3. Run locally when Tier 0 is selected; record zero cost and deflected = 1.
4. Validate applicable local code output.
5. If validation requires escalation, select Tier 1 and perform paid-call pre-flight.
6. Refuse escalation if the cap cannot reserve its worst-case cost.
7. Record each attempt and its routing reason without double-counting one user query.
```

## 5. Failure behavior

Relay favors explicit refusal over uncertain spending.

| Failure | Required behavior |
| --- | --- |
| Missing/invalid price | Refuse before provider dispatch |
| Estimated context over model limit | Refuse or select a valid higher tier under routing rules; never truncate invisibly |
| Daily or monthly cap exceeded | Refuse before provider dispatch |
| Missing credential | Refuse before provider dispatch and name only the expected environment variable |
| Provider timeout before confirmed send | Release reservation and record failure |
| Ambiguous timeout after send | Preserve a conservative pending/reconciliation record |
| Missing/malformed provider usage | Record an accounting error and conservatively retain reservation; never report zero cost |
| Ledger unavailable | Do not make a paid request |
| Local runtime unavailable | Disable local tier or escalate only after normal paid pre-flight |
| Copilot monitor unavailable | Report monitor error separately; retain Relay usage output |

## 6. Planned Rust organization

The project should begin as one binary/library package and split into workspace crates only when compile boundaries or reusable APIs justify it.

```text
src/
  main.rs                 thin process entry point
  lib.rs                  application composition
  cli.rs                  command definitions and rendering
  config.rs               typed config and paths
  error.rs                stable error taxonomy
  request.rs              Message, RelayRequest, RelayResponse, Usage
  pricing.rs              catalog loading, validation, decimal cost math
  caps.rs                 pre-flight checks and reservations
  meter.rs                actual usage accounting
  ledger/
    mod.rs                repository interface and aggregates
    sqlite.rs             SQLite implementation and migrations
  router.rs               aliases; deterministic tiers in v0.3
  adapters/
    mod.rs                ProviderAdapter and registry
    openai.rs
    anthropic.rs
    local.rs              v0.3
  context/                v0.2+
    mod.rs
    repo_map.rs
    relevance.rs
    budget.rs
  copilot.rs              v0.2 read-only monitor
  index/                  v0.3 local index
    mod.rs
    chunk.rs
    embeddings.rs
migrations/
prices.json
tests/
  adapter_conformance.rs
  cap_refusal.rs
  money_math.rs
```

Core policy depends on traits for adapters, clocks, price catalogs, and ledgers. Tests use fakes to prove that denied requests cannot invoke transport. Provider HTTP clients stay inside adapter modules.

## 7. Testing strategy

### 7.1 Adapter conformance

Every adapter runs the same behavioral suite:

- reports a stable name and declared capabilities;
- accepts normalized role/content messages;
- estimates tokens deterministically for fixed input;
- honors model and output limits;
- normalizes successful text, model, latency, and usage;
- distinguishes authentication, rate-limit, transport, timeout, and malformed responses;
- redacts credentials and sensitive headers;
- does not silently change the requested model;
- handles unsupported streaming explicitly.

Wire-level tests use local mock servers and never consume paid API credits. Optional credential-gated smoke tests are separate.

### 7.2 Money-path tests

Required cases include:

- exact input/output cost vectors and conservative rounding boundaries;
- zero-token, maximum-token, and overflow inputs;
- NaN, infinity, negative, missing, and stale prices rejected;
- daily cap refusal, monthly cap refusal, and exact-boundary behavior;
- 80% warning behavior;
- simulated at-cap state followed by **zero transport calls**;
- concurrent requests cannot over-reserve the same remaining budget;
- ledger failure prevents dispatch;
- reported usage settles an estimate correctly;
- malformed/missing usage never becomes a zero-cost success;
- local calls record tokens and zero monetary cost.

Property-based testing is appropriate for cost monotonicity: increasing token counts or prices must never reduce estimated cost.

### 7.3 Context and routing tests

From v0.2 onward, golden fixtures verify deterministic ranking and truncation. From v0.3 onward, table-driven tests cover every routing signal, override precedence, local validation retry, repeated-query escalation, and cap enforcement after escalation.

## 8. Observability and privacy

Operational logs and user-facing diagnostics are separate from the accounting ledger. Logs may contain request IDs, adapter/model names, durations, token counts, route decisions, and error categories. By default they must not contain prompt text, response text, source content, environment values, credentials, or raw HTTP headers.

Metrics and update checks, if ever introduced, are opt-in and outside v0.1–v0.3. Relay has no hosted telemetry requirement.

## 9. Version evolution

### v0.1 — See + Stop

Build in order: adapter contract → price data → meter/ledger → cap enforcer → config/CLI. The release is complete only when OpenAI and Anthropic pass conformance, the cap-refusal test proves no network call, and a fresh user can reach a first metered call in under five minutes.

Multi-process-safe reservations and reconciliation are part of v0.1, not a post-launch optimization. Correct cap enforcement takes priority over the original three-week estimate; the release schedule may extend rather than ship a documented concurrency hole in the core promise.

### v0.2 — Shrink + Watch

Add `ContextBuilder` and the isolated Copilot monitor. No routing heuristics or local model runtime are introduced. Token reduction is measured against a minimizer-disabled baseline and recorded for reporting.

### v0.3 — Deflect

Add the local adapter, local index, deterministic tier router, validation escalation, and savings report. Existing cap and meter components remain unchanged except for schema-compatible route metadata and local zero-cost accounting. This release proves the adapter boundary by adding local inference without special-casing it in core policy.

### Later — Medha readiness

The protocol permits adapter name `medha`, capability `consensus`, route tier `truth`, and model alias `medha:consensus`. There is no Medha runtime, network integration, or consensus policy in the frozen v0.1–v0.3 scope.

## 10. Architecture decision rules

Until dedicated decision records are added, contributors should use these rules:

- Money-path correctness outranks availability and convenience.
- Prefer explicit typed states over booleans for request/accounting lifecycles.
- Keep provider SDK types out of core domain modules.
- Keep prices and model limits out of executable logic.
- Keep CLI formatting out of domain services.
- Introduce no network service when a local implementation satisfies the requirement.
- Reject scope that appears in a version's non-goals, even if the code change seems small.
- Record material architectural changes as an ADR before implementation.

## 11. Explicit non-goals

Across the frozen plan, Relay does not provide a hosted proxy, team control plane, IDE integration, editing-agent loop, learned router, fine-tuning system, or hosted source index. Copilot request routing and private endpoint access are permanently excluded. `medha:consensus` remains reserved until a later version.
