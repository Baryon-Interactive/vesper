# Vesper paid-game key gateway

The gateway is a Cloudflare Worker/D1 service for paid-game authentication, grants and key delivery.
See the [project overview](../README.md), [architecture](../docs/architecture.md)
and [testing guide](../docs/testing.md).

It serves only paid games. It stores opaque session token hashes, email verification code hashes, redeem-code hashes, and AES-256-GCM encrypted DepotKeys in D1. The Worker never returns a private signing key and the developer API accepts only short-lived Ed25519 signed requests.

## Local setup

```bash
npm ci
npx wrangler d1 create vesper-gateway
npx wrangler d1 migrations apply vesper-gateway --local
npm test
npx tsc --noEmit
```

`npm test` runs the Worker through `@cloudflare/vitest-pool-workers` with the
`migrations/` D1 schema applied to a local Miniflare D1 database. The integration
tests call the exported Worker fetch handler and generate all signing keys at
runtime; no test key or secret is committed.

Set `MASTER_KEY`, `INDEX_BASE_URL`, and (optionally) `INDEX_ROOT_KEY`, GitHub OAuth variables, and `DEV_MODE=true` for local email-code responses. Production email delivery uses the replaceable `EMAIL_PROVIDER_URL`/`EMAIL_PROVIDER_TOKEN` adapter; with neither configured, email login returns a clear 503 instead of logging a code. `wrangler d1 create` supplies the production database ID to `wrangler.toml`; deploy with `npx wrangler deploy` after configuring the route. Deployment requires operator-managed credentials and service configuration.

Email delivery is an interface boundary: the default implementation logs a code in development. A production adapter can call a provider with a free tier such as Resend, Brevo, Mailgun trial, or an SMTP relay; none is connected here.

## Interfaces

`GET /auth/github/start` and `/auth/github/callback` implement OAuth state checking. `POST /auth/email/start` accepts `{email}`, and `POST /auth/email/verify` accepts `{email,code}`. A successful login sets an HttpOnly session cookie; `GET /me/grants` lists active and revoked grants. `POST /apps/<app_id>/redeem` accepts `{code}` once. `POST /apps/<app_id>/key` optionally accepts `{key_id}` and returns `{app_id,key_id,depot_key_hex}` only for an active grant.

Developer requests carry `x-vesper-timestamp`, `x-vesper-request-id`, and a base64 Ed25519 `x-vesper-signature` over `METHOD\nPATH\nTIMESTAMP\nSHA256(body)`. The Worker fetches and verifies the root-signed `routes.json` before looking up the app key. The endpoints are `PUT /dev/apps/<app_id>/depot-key`, `POST /dev/apps/<app_id>/codes`, `POST /dev/apps/<app_id>/grants/revoke`, and `PUT /dev/apps/<app_id>/webhook`.

Stripe uses `Stripe-Signature` HMAC-SHA256 with a five-minute timestamp window. Event IDs are deduplicated before `checkout.session.completed` grants access and `charge.refunded` revokes it. The schema is in `migrations/0001_initial.sql`.

Approximate free-quota use per request: login start/callback 1–3 D1 writes plus external OAuth calls; email start 1 write, verify 2 reads/writes; grants 1 read; redeem 2 reads + 2 writes; key 3 reads + 1 decrypt; developer key 2–3 writes; code generation one write per code; webhook 2 reads + 1–2 writes. Keep the gateway behind the index and avoid polling.
