# Rev. 5 wiki update — Authorization & Trust Model + related section edits

This replaces the rev. 4 markdown snippet with the full rev. 5 content, following a
second round of reviewer feedback. Two parts:

1. **Full replacement** for the "Authorization & Trust Model" section (paste over
   the rev. 4 version of that section).
2. **Targeted edits** to five other existing sections that had a real gap
   (object key path always leaked identity regardless of `privacy_mode`) or
   needed a one-line cross-reference added.

---

## Part 1 — Authorization & Trust Model (full replacement)

## Authorization & Trust Model

### Current trust boundary (Phase 1)

Write access to the analytics bucket is gated by exactly two things, both already implied by the transport decision in this document, made explicit here per review feedback:

1. **Network-layer trust.** The MinIO endpoint is reachable only from inside the corporate network — either because a client is on VPN/private network, or, where that isn't available, because the client's source IP is on an allowlist enforced in front of MinIO (load balancer / firewall rule). No authorization decision is made by ChunkHound or MinIO based on which developer, machine, or repository is making the request — only whether this network path is allowed to reach the endpoint at all. The endpoint **must require TLS/HTTPS** — network-layer trust alone doesn't protect the shared credential in transit; plain-HTTP exposure would turn any on-path observer into a credential thief.
2. **A single shared write-only credential.** Every ChunkHound install is configured with the same `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` pair, granted `PutObject` on the `analytics/*` prefix and nothing else — no `GetObject`, `ListBucket`, or `DeleteObject`. Distributed as plaintext environment variables; never persisted in `.chunkhound.json`.

Critically, the `<repository>` and `<privacy_id>` segments of the object key are **client-asserted, not authenticated** — nothing in the credential or the bucket policy ties a given caller to a specific `<repository>`/`<privacy_id>` value. The key scheme is a convenient partitioning for downstream consumers, not an access-control boundary.

### What this credential can and cannot do

| Capability | Granted? |
|---|---|
| Write a new object under any `analytics/<repository>/<privacy_id>/...` prefix | Yes — for every repository and every user, not just the caller's own |
| Overwrite an existing analytics object | Not via key guessing — the `uuid4` suffix makes key collisions effectively impossible. This is **collision-resistance, not an IAM guarantee**: enable bucket versioning and, if MinIO's object-lock/`If-None-Match` support is available, use it as a defense-in-depth backstop rather than relying on the key format alone. |
| Read any analytics object (exfiltrate another user's/repo's usage data) | No — credential is write-only |
| List bucket contents / enumerate other users' uploads | No — no `ListBucket` grant |
| Delete any analytics object | No — no `DeleteObject` grant |

**Implementation note:** Phase 1 should pin the client to a single `PutObject` call per flush — no `ListBucket`, no multipart upload (`UploadPart`/`AbortMultipartUpload`). Some S3 SDKs default to multipart above a size threshold or probe bucket state before writing; test the chosen client against the actual write-only IAM policy so "write-only" doesn't get silently widened by a library default. A size/quota cap on the bucket (or per-prefix) is also recommended, bounding the volumetric-abuse case below regardless of client behavior.

### Blast radius of a single compromised credential

Because the credential is shared across every client and not scoped per repository or per user, **compromise of one copy is equivalent to compromising all of them**. If the credential leaks (a captured env var from a compromised laptop, a misconfigured CI log, a shared build image) or the endpoint is reached from an allowlisted IP without a legitimate developer's machine (a compromised jump host, shared NAT egress, another tenant on the same allowlisted range), an attacker can:

- **Inject fabricated analytics events attributed to any user or repository** — since `<repository>`/`<privacy_id>` are self-reported key-path segments, downstream tooling will attribute forged events to whoever the attacker names.
- **Perform volumetric abuse** — flood the bucket with objects under arbitrary prefixes, driving up storage cost and stressing whatever pipeline consumes these objects.

The attacker **cannot** read, enumerate, or delete any analytics data — the write-only grant meaningfully limits the damage to injection and volumetric abuse, not exfiltration or destruction. That said, the blast radius for the write-side risk is the **entire org's analytics bucket across all repositories and all users**, not just the compromised client's own data, because there is no per-caller scoping today. Note that "write-only" bounds the damage from a *leaked client credential* — it says nothing about who can *read* the bucket; see Reader/Reporting Credential below for that separate boundary.

### Uploader identity is weak; bucket contents are not low-sensitivity

`action` fields (query text, questions, URLs — see Privacy) are present in every event regardless of `privacy_mode`, because understanding usage patterns is this feature's whole purpose. That means the bucket holds real search/question content, not just counters — it should be treated like telemetry containing query text, not like anonymous click counts. The accepted Phase 1 tradeoff is specifically about **who is authorized to write and under what claimed identity** being weakly enforced; it is not a claim that the data itself is low-value to protect.

### Reader/Reporting credential — the actual confidentiality boundary

The client write credential can't read, list, or delete — but something has to read the bucket to produce any reporting/dashboards, and **that** reader role, not the write credential, is where query-content exfiltration risk actually lives. Compromise of a reporting credential is strictly worse than compromise of the write-only client credential: it exposes the accumulated query/question/URL history of the whole org, not just the ability to inject or flood. The reader role must be least-privilege (read/list only, no write/delete), issued separately from the client write credential, and never distributed to developer machines — it belongs only to the reporting/ETL service that needs it.

### Shared-secret distribution & rotation

The write credential should be distributed through the org's normal secret-manager channel (not, e.g., pasted into a wiki page or committed anywhere), with rotation supporting a brief dual-key overlap window so in-flight buffers on developer machines don't fail mid-rotation. CI is deliberately not a default holder of this credential — the CI-log leak path is already called out above as a blast-radius scenario, so unless CI-originated analytics events are an intentional source, the credential should be scoped to developer/install environments only.

### Why this is an accepted Phase 1 tradeoff

This matches the design's existing non-goal: *"No per-user or per-repository S3 credential scoping in Phase 1 — one shared write-only credential for all clients."* That decision is reaffirmed here, not revisited — standing up per-user credential issuance is nontrivial net-new infrastructure (see Phase 2 direction below), and Phase 1's threat model accepts network-layer trust plus a write-only, un-scoped credential as sufficient given the write-only limitation, provided the reader/reporting boundary above is respected. This section exists to make that assumption explicit and reviewable, not to change the Phase 1 plan.

### Relationship to provider API tokens

This section has been about one question: who can write to the analytics bucket, and as whom. That's a different question from **spend attribution** — who spent how much on embedding/LLM calls — which many orgs already solve independently via per-developer provider API tokens (e.g. individually-issued Voyage AI or LLM-gateway keys with their own rotation and billing attribution). That path is complementary to CH analytics and often sufficient on its own for cost tracing.

ChunkHound analytics targets a different question: **what commands ran, against which repositories, with what outcomes**, plus a rolled-up view of provider usage inside CH — not a source of billing truth. Concretely, the `providers[].input_tokens`/`output_tokens` counts in a `command_summary` event are self-reported by the client and **must not** be treated as a billing source of truth — the provider's own per-key usage/billing remains the cost ledger. Orgs with strong per-developer provider keys already may reasonably: rely on provider billing for cost identity and use CH analytics mainly for usage/product insight (`privacy_mode = anonymous` or `hashed` is fine for that purpose), or still enable CH analytics with the shared S3 write path, accepting that upload authorization is network/IAM-based while provider spend remains separately token-attributable. Good provider token creation/rotation policy reduces the pressure on per-user analytics-upload credentials; it does not by itself authorize or authenticate writes to the analytics bucket — that's what the Phase 2 direction below is for.

## Phase 2 direction: per-user scoped credentials

*(Proposal only — not committed work.)*

The reviewer's core point — that today's model reduces to "inside the network perimeter (or on an allowlisted IP) plus the one shared secret grants full write access to everything" — can be made redundant by issuing each client credentials that are both **scoped** to its own `analytics/<repository>/<privacy_id>/` prefix and **short-lived**, with issuance traced centrally — analogous to how per-developer provider tokens let an org trace usage back to an individual. Evaluated below against **self-hosted MinIO** specifically, since some AWS-only mechanisms don't apply here.

| Option | Durable secret on client? | New infra required | Blast radius of one leak | Revocation |
|---|---|---|---|---|
| **C — Per-developer static IAM users** *(Phase 1.5)* | Yes, but prefix-scoped | IAM provisioning/rotation automation (moderate) | Single prefix, unbounded until manually rotated | Manual/scripted, no automatic expiry |
| **B — Presigned URL broker** *(North star)* | **No — client never holds any S3 credential** | Small internal issuance service + its own client-auth mechanism (moderate) | Single object, minutes-bounded | Automatic on URL expiry; broker can also deny at issuance time |
| **A — MinIO STS / AssumeRole** | No (short-lived derived creds) | OIDC IdP or LDAP integration + policy templating (heaviest) | Single prefix, TTL-bounded | Automatic on TTL expiry |

**Option C, reframed as an incremental "Phase 1.5" step** — provision one MinIO user per developer with a prefix-scoped policy (`mc admin user`/`mc admin policy`). This is literally the shape of the per-developer provider-token pattern the original review compared this to, and it's the right recommendation for **orgs that already run per-developer credential issuance** for other systems: no new ChunkHound service required, just IAM automation the org may already operate. It still leaves a durable secret per developer (narrower blast radius than today, not eliminated) and doesn't reduce reliance on network-layer trust for the upload itself, so it's a real improvement, not an end state.

**Option B, as the long-term north star** — a small internal, stateless issuance endpoint authenticates the calling client, generates a presigned PUT URL scoped to exactly one object key with a short expiry, and returns it over HTTPS; the client performs a single HTTP PUT with no AWS SDK and no S3 credential of any kind. This is the only option where the client never holds any durable or short-lived bucket credential at all. It comes with hard requirements the rev. 4 draft left implicit:

- **Identity is broker-derived, never client-supplied.** The broker must set `<privacy_id>` from its own authentication of the caller — a client that authenticates as itself cannot request a presigned URL for someone else's prefix. `<repository>` can remain client-asserted (spoofing a repo name is much lower-stakes than spoofing identity). Without this, Option B reproduces Phase 1's exact spoofing gap behind a narrower secret — it would not actually be an improvement.
- **Presign at flush time, not at process start.** TTL should be minutes-scale, covering one buffer flush — not issued once and held for the whole process lifetime (flush interval defaults to 6 hours).
- **Broker downtime degrades gracefully.** The client already buffers locally before flushing, so a broker outage means a delayed upload, not lost data or a broken command — this should be stated explicitly so "we added a server" isn't read as a new availability risk to the actual product.
- **The broker's own client-authentication mechanism is undesigned.** "A lightweight per-developer/install token" is not yet a real answer — it needs its own issuance, rotation, and offboarding story (an IdP, MDM-issued cert, or the same distribution flow as provider tokens) before Option B actually delivers the traceability the reviewer described; otherwise this is Option C moved one hop rather than a genuine improvement.
- **The broker is a thin signing service, not an ingestion pipeline.** It authorizes and signs one PUT URL per request; it does not receive, store, or process event data. This doesn't reverse the existing "no central ingestion server" non-goal from rev. 2.

**Option A** — MinIO's STS-compatible API (`AssumeRole`, `AssumeRoleWithWebIdentity`, `AssumeRoleWithLDAPIdentity`) returns temporary, policy-scoped credentials, but every flow still requires an existing trusted identity source first — an OIDC provider or LDAP directory MinIO trusts — neither of which exists today. Heaviest prerequisite of the three; only worth it if the org already has OIDC/LDAP wired into MinIO for other reasons.

**Recommendation:** adopt Option C where an org already issues per-developer credentials elsewhere — it's the closest match to the reviewer's own Voyage-token analogy and needs no new ChunkHound service. Treat Option B (with broker-bound identity as a hard requirement, not an implementation detail) as the target state once ChunkHound is willing to own a small broker service. Option A stays last unless OIDC/LDAP already exists on the MinIO deployment. **All of Phase 2 is explicitly out of scope for this revision** — this section documents direction and rationale, not an implementation commitment.

---

## Part 2 — Targeted edits to other existing sections

### Key Design Decisions — "Object key scheme" bullet

Replace with:

> **Object key scheme** *(revised, rev. 5)* — `analytics/<repository>/<privacy_id>/<YYYY>/<MM>/<DD>/<iso8601-timestamp>_<uuid4>.jsonl`. As decided in rev. 2, the key's identity segment was always the raw OS username, independent of `privacy_mode` (added in rev. 3) — so `hashed`/`anonymous` mode hid identity in the JSON payload but still leaked it via the S3 path to anyone with list/read access. `<privacy_id>` now reuses the exact value already computed for the payload's `user` field, so the two can never disagree. See the Authorization & Trust Model section for the full rationale.

### Concurrency & Collision Prevention — two mentions

Both `analytics/<repository>/<user>/...` key-template mentions in this section (the "S3-side collision" paragraph and the closing note about the reporting layer) become `analytics/<repository>/<privacy_id>/...`.

### Data Model — "Object key format" line

Replace with:

> **Object key format** *(revised, rev. 5)*: `analytics/<repository>/<privacy_id>/<YYYY>/<MM>/<DD>/<iso8601-timestamp>_<uuid4>.jsonl`. `<privacy_id>` is derived the same way as the payload's `user` field shown in the examples above (OS username for `full`, the same salted hash for `hashed`, a fixed shared literal `anonymous` for `anonymous` mode — see Privacy below) — key and payload identity can never disagree. One file per flush; a file may contain multiple `command_summary` lines.

### Privacy — new subsection, inserted right after the existing mode table

Add:

> ### `privacy_mode` also governs the object key *(revised, rev. 5)*
>
> Prior to rev. 5, `privacy_mode` only affected the JSON payload's `user` field — the S3 object key's identity segment (see Data Model) was always the raw OS username, regardless of mode. That meant `hashed`/`anonymous` mode hid identity in the event body while still leaking it via the bucket path to anyone with list/read access on the bucket (see the Authorization & Trust Model section's Reader/Reporting Credential note for why that's a real boundary, not a hypothetical one). This is now fixed: the key's `<privacy_id>` segment is computed once and reused for both the payload's `user` field and the key path.
>
> | Mode | Object key `<privacy_id>` segment |
> |---|---|
> | `full` | OS username, same as the payload's `user` field. |
> | `hashed` | The identical `sha256(local_salt + os_username)` value used in the payload — computed once, used in both places. |
> | `anonymous` | A fixed literal segment, `anonymous`, shared by every anonymous-mode install. S3 keys can't have a null path component, and a per-install random ID would just reintroduce pseudonymous tracking under a different name — collapsing all anonymous-mode uploads under one shared prefix is the correct analogue to `user: null`. |

### Configuration — after the credential/env-var sentence

Append: "Adopters must place that credential and endpoint behind their org's normal private-network/IAM controls — see Authorization & Trust Model."

### Non-Goals — "No per-user or per-repository S3 credential scoping..." bullet

Replace with:

> No per-user or per-repository S3 credential scoping in Phase 1 — one shared, write-only credential, with authorization left to network isolation + bucket IAM (see Authorization & Trust Model). Provider-side per-user API tokens remain the recommended way to attribute embedding/LLM spend when that is the primary goal.

### Header meta / revision history

Bump `rev. 4` → `rev. 5` in the page header, and append to the revision-history paragraph: rev. 5 fixes the object-key/`privacy_mode` gap, hardens the Phase 1 write-up (reader/reporting boundary, TLS, secret rotation, sensitivity framing), adds the provider-API-tokens relationship subsection, and re-ranks Phase 2 (per-developer static credentials as an incremental step, presigned-URL broker as the longer-term target with broker-bound identity now a stated requirement).
