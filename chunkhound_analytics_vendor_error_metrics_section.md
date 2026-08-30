# Rev. 6 wiki update — Per-vendor call/error metrics

This replaces the rev. 5 markdown snippet with rev. 6 content. New requirement:
get the number of calls made to each vendor (embedding / reranking / LLM), the
number of those calls that failed, and a spread of failure types — **per
vendor**, not just a flat command-level count as in rev. 5. Four parts:

1. **Full replacement** for the `providers`/`errors` portion of the Data Model
   section (new JSON shape + updated examples).
2. **Full replacement** for the `AnalyticsRecorder` API table in Architecture
   Detail (`record_provider_call` signature change, `record_error` →
   `record_internal_error`).
3. **New subsection** — Instrumentation placement per vendor.
4. **Targeted edits** to the Testing section and the revision header.

---

## Part 1 — Data Model: `providers` / `errors` (full replacement)

### Why this changed

Rev. 5's `errors: {count, types}` lives at the command level only — it can't
tell you *which* vendor call failed. A command that calls both an embedding
provider and an LLM and ends with `errors.types == ["TimeoutError"]` gives no
way to attribute that timeout to either one. This rev. moves failure
accounting into the same per-`(kind, provider, model)` grouping already used
for `calls`/token totals, so cost and failure data share one source of truth.

### New shape

> **`providers[<kind>]` entry** *(revised, rev. 6)*: `{provider, model, calls,
> fails, error_types, input_tokens, output_tokens}`. `calls` is now **total
> attempts, success + fail** (previously success-only) — consistent with how
> retries already inflated `calls` in the rev. 5 examples below. `fails` is
> the subset of `calls` that raised. `error_types` is `{ExceptionTypeName:
> count}`, scoped to this `(kind, provider, model)` group — same "type name
> only, no message text" privacy rule as rev. 5's command-level `errors`.
>
> The command-level `errors: {count, types}` block is **removed** — a
> command-level failure total can always be derived by summing `fails` across
> every `providers[*][*]` entry. In its place, one narrow fallback field:
>
> **`internal_error_type`** *(new, rev. 6)*: `string | null`, command level.
> Set only when `success: false` **and** no `providers[*][*]` entry recorded a
> fail — i.e. the failure wasn't caused by any vendor call (an internal bug, a
> bad user query, a local file error). `null` in every other case, including
> all `success: true` events. Exception type name only, never message text —
> same rule as `error_types`.

### Updated examples

```jsonc
// MCP tool call: search (privacy_mode = "full", the default) — all vendor calls succeeded
{"type": "command_summary", "user": "jsmith", "ts": "<iso8601>", "repository": "chunkhound",
 "os": "Linux", "chunkhound_version": "5.2.2",
 "command": "search", "source": "mcp", "duration_ms": 842, "success": true,
 "action": {"query": "explain indexing"},
 "providers": {
   "llm":       [{"provider": "anthropic", "model": "claude-...", "calls": 1, "fails": 0, "error_types": {}, "input_tokens": 900, "output_tokens": 210}],
   "embedding": [{"provider": "voyageai", "model": "voyage-3", "calls": 1, "fails": 0, "error_types": {}, "input_tokens": 32}],
   "reranker":  [{"provider": "voyageai", "model": "rerank-2", "calls": 1, "fails": 0, "error_types": {}}]
 },
 "internal_error_type": null}

// CLI command: index — one embedding batch attempt failed and was retried, then succeeded
{"type": "command_summary", "user": "jsmith", "ts": "<iso8601>", "repository": "chunkhound",
 "os": "Linux", "chunkhound_version": "5.2.2",
 "command": "index", "source": "cli", "duration_ms": 612000, "success": true,
 "action": {"mode": "initial", "file_count": 8500, "total_chunks": 340000},
 "providers": {
   "embedding": [{"provider": "voyageai", "model": "voyage-3", "calls": 1135, "fails": 1,
                   "error_types": {"RateLimitError": 1}, "input_tokens": 41200000}]
 },
 "internal_error_type": null}

// A command with a transient LLM failure that was retried (app-visible chokepoint invocations),
// and one that ultimately failed
{"type": "command_summary", "user": "jsmith", "ts": "<iso8601>", "repository": "chunkhound",
 "os": "Darwin", "chunkhound_version": "5.2.2",
 "command": "code_research", "source": "mcp", "duration_ms": 15300, "success": false,
 "action": {"question": "how does the compaction protocol work"},
 "providers": {
   "llm": [{"provider": "openai", "model": "gpt-4o", "calls": 4, "fails": 2,
            "error_types": {"TimeoutError": 1, "ValueError": 1},
            "input_tokens": 6200, "output_tokens": 1800}]
 },
 "internal_error_type": null}

// A command that failed for a reason unrelated to any vendor call
{"type": "command_summary", "user": "jsmith", "ts": "<iso8601>", "repository": "chunkhound",
 "os": "Linux", "chunkhound_version": "5.2.2",
 "command": "search", "source": "cli", "duration_ms": 12, "success": false,
 "action": {"query": "explain indexing"},
 "providers": {},
 "internal_error_type": "KeyError"}

// search, git-history-scoped (--last-n / --commit-range / --commit-hash)
{"type": "command_summary", "user": "jsmith", "ts": "<iso8601>", "repository": "chunkhound",
 "os": "Linux", "chunkhound_version": "5.2.2",
 "command": "search", "source": "cli", "duration_ms": 610, "success": true,
 "action": {"query": "explain indexing", "last_n_commits": 10},
 "providers": {"embedding": [{"provider": "voyageai", "model": "voyage-3", "calls": 1, "fails": 0, "error_types": {}, "input_tokens": 14}]},
 "internal_error_type": null}
```

`privacy_mode = "hashed"`/`"anonymous"` behave exactly as in rev. 5 — only the
`user` field (and object key `<privacy_id>` segment) changes; none of the
fields introduced in this revision are privacy-mode-dependent.

---

## Part 2 — Architecture Detail: `AnalyticsRecorder` API (full replacement)

Replace the two rows below in the `AnalyticsRecorder` method table:

| Old (rev. 5) | New (rev. 6) |
|---|---|
| `record_provider_call(kind, provider, model, input_tokens=None, output_tokens=None, request_count=1) -> None` | `record_provider_call(kind, provider, model, success, error=None, input_tokens=None, output_tokens=None) -> None` |
| `record_error(exc: Exception) -> None` — adds the exception's type name to the current accumulator's error tally, without capturing the message text. | `record_internal_error(exc: Exception) -> None` — sets the current accumulator's `internal_error_type` to the exception's type name, without capturing message text. Only meaningful when no provider entry has recorded a fail; if one has, this is a no-op (a vendor-caused failure already explains it). |

`record_provider_call` semantics *(revised, rev. 6)*: called **once per
application-visible attempt** — every retry iteration in an app-level retry
loop is its own call; a single LLM chokepoint invocation (whose SDK-internal
retries, if any, are already resolved by the time it returns or raises) is
also its own call. On the `(kind, provider, model)` accumulator group:
`calls` increments on every invocation regardless of `success`; `fails`
increments only when `success=False`; on failure, `error_types[type(error).__name__]`
increments by 1 (`error` is required when `success=False`, ignored otherwise).
Token args are only meaningful on success and are ignored (left at their
prior value) on failure. As before, if no accumulator is open the call is
silently dropped rather than raised — every provider call is always triggered
by some command.

`end_command(success: bool) -> None` is unchanged in shape, but now also
computes `internal_error_type`: `null` unless `success=False` and every
`providers[*][*].fails == 0`, in which case it takes whatever
`record_internal_error` set (or stays `null` if nothing called it, e.g. an
unclassified failure — an edge case this rev. doesn't attempt to close further).

---

## Part 3 — New subsection: Instrumentation placement per vendor

*(Insert after the `AnalyticsRecorder` method table in Architecture Detail.)*

There is no single repo-wide chokepoint all three vendor kinds funnel
through — each provider class has its own narrow per-attempt call site, and
retry visibility differs by vendor kind:

| Vendor | Chokepoint(s) | Retry visibility |
|---|---|---|
| Embedding | `chunkhound/providers/embeddings/openai_provider.py: _embed_batch_internal()`; `chunkhound/providers/embeddings/voyageai_provider.py: _embed_single_batch_locked()` | **App-level.** Each provider wraps its call in its own `for attempt in range(self._retry_attempts)` loop with rate-limit-aware backoff. `record_provider_call` fires once per loop iteration — every attempt, success or fail, is individually visible. |
| Reranker | `openai_provider.py: _rerank_single_batch()`; `voyageai_provider.py: _rerank_via_sdk()` (SDK path) / `_rerank_http_batch()` (HTTP path, no retry at this layer — retries happen one level up via batch-splitting, which does not retry, only splits) | **App-level** for the SDK path, matching embedding. The HTTP path's single-attempt chokepoint should still call `record_provider_call` per attempt; batch-splitting above it is a different concern (chunking work, not retrying a failed attempt) and isn't itself an instrumentation point. |
| LLM | `chunkhound/providers/llm/anthropic_llm_provider.py: _create_message()`; `chunkhound/providers/llm/openai_compatible_provider.py`'s `chat.completions.create()` / `responses.create()` call sites | **SDK-internal.** `max_retries` is passed directly into the SDK client constructor (`AsyncAnthropic(max_retries=...)`, `AsyncOpenAI(max_retries=...)`) — individual HTTP retry attempts happen inside the SDK/httpx transport and are invisible to application code. One `record_provider_call` fires per chokepoint invocation, reflecting an already-resolved result. This is coarser than embedding/reranker **by design** (see Part 2) — not a gap this rev. closes. A `calls` value like `4` for an LLM entry (as in the Part 1 example) reflects the app calling the chokepoint multiple times (e.g. a multi-hop research loop), not raw HTTP attempts. |

---

## Part 4 — Targeted edits

### Testing section — add three contract tests

> - **Per-attempt fail accounting**: `record_provider_call(success=False,
>   error=exc)` increments `fails` and `error_types[type(exc).__name__]` on the
>   correct `(kind, provider, model)` group without double-counting `calls`
>   (i.e. `calls` increments exactly once per invocation regardless of
>   `success`).
> - **Internal-error fallback exclusivity**: a command ending `success=False`
>   with at least one `providers[*][*].fails > 0` leaves `internal_error_type
>   == null`; a command ending `success=False` with **no** provider fails and
>   a `record_internal_error(exc)` call sets `internal_error_type` to that
>   exception's type name.
> - **Retry-loop integration test**: point an embedding provider at an invalid
>   endpoint for a bounded number of attempts; confirm the resulting
>   `providers.embedding[0]` entry has `calls == fails == retry_attempts`,
>   `error_types` populated, and no message text anywhere in the emitted
>   event.

### Revision header

Bump `rev. 5` → `rev. 6` in the page header; append to the revision-history
paragraph: rev. 6 adds per-vendor call/fail counts and a failure-type spread
(`fails`, `error_types` per `providers[kind]` entry), replacing the
command-level `errors` block with a narrow `internal_error_type` fallback for
non-vendor-caused failures; documents the differing retry-visibility
granularity between app-level-retrying providers (embedding, reranker) and
SDK-internal-retrying providers (LLM).
