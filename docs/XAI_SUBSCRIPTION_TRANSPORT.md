# Subscription transports and grants

This reference describes the earlier Rust planner's provider transports and
grant receipts. The planner used application-owned grants, required an explicit
model binding, and charged inference against its own reservations.
[Configure the pi agent](AGENT.md#prepare-and-verify)

| Transport | Fixed inference origin | Recorded use |
| --- | --- | --- |
| `openai_codex` | `https://chatgpt.com/backend-api/codex/responses` | Exact `gpt-6-luna` digest, separate OpenAI login/grant |
| `direct_oauth` | `https://api.x.ai/v1/responses` | Exact `grok-4.7` decision and reviews, shared own xAI grant |
| `subscription_proxy` | `https://cli-chat-proxy.grok.com/v1/responses` | Compatible legacy/diagnostic route; explicit proxy client version required |

The proxy adds `X-XAI-Token-Auth`, configured `x-grok-client-version` and exact
`x-grok-model-override`; direct OAuth never inherits them. All requests use the
application User-Agent. Proxy client version is operator-configured dotted numeric
metadata, not automatically the Cargo version. There is no automatic model rewrite.

## Explicit limit fallback

The reviewed explicit limit fallback
([Operations](OPERATIONS.md#historical-explicit-limit-fallback)) is the only fallback and is
not part of a subscription transport: it is policy-configured, uses its own
application-owned key, switches at most one call for the explicit reason codes,
and never rewrites a role's transport, model or origin. A 401/403 at the catalog
or inference request and an opaque HTTP 200 response-validation rejection are
eligible; timeouts, HTTP 5xx, cancellation, malformed output from a real
response and local refusals are not.

## Grant ownership

Each transport used the application's private grant. Grok roles could share
one grant config and one refresh owner.

Refresh is durably fenced before network access. Success verifies account identity,
expiry and response, then atomically fsyncs/renames the private grant. Omitted rotated
refresh credentials preserve the original. Ambiguous failure leaves the fence and
failed artifacts for explicit review; there is no automatic ambiguous refresh retry.
Re-login uses a new explicit application grant path, not a foreign cache import.

Bounded strict Responses/SSE requires a valid terminal completion. Incomplete,
truncated, duplicate-terminal, mismatched-model or malformed output fails. Errors
retain safe codes; credentials/raw auth/account payloads are not printed. A catalog
row or boolean tool marker does not certify entitlement, billing or tool quality.

## Grant and catalog admission

Direct/OpenAI inference needs an authenticated application-owned grant for the
configured transport and an exact model ID that appears in the live provider
catalog. The runtime does not read, write or renew a separate approval file.
`grok-4.7-build-fast` is refused even if listed. A catalog row or boolean tool
marker does not certify entitlement, billing or tool quality.
See [Operations](OPERATIONS.md#grants-and-model-admission).

## Dated protocol evidence

Public source investigations used
[OpenClaw `1c7aced`](https://github.com/openclaw/openclaw/tree/1c7aced3b063bb0ee9adcd727eb2a6db1dabcf90/extensions/xai)
and [Hermes `18c525b`](https://github.com/NousResearch/hermes-agent/tree/18c525b669b6ea4ba62e6967d228af21d89aa9d3).
They established request shapes, not permission to use their credentials.
Old catalog/login observations must not be repeated as current blockers.
See [harness](MODEL_HARNESS.md) for roles.
