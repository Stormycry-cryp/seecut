# SeeCut Server

SeeCut Server is the first modular-monolith backend for the Concat-based SeeCut application. It keeps user identity, team membership, shared team asset metadata, personal credit accounting, generation task metadata and payment records in SQLite. Personal media stays on the device unless the client explicitly requests a `team_asset` upload.

The first version uses Python's standard library, so it can run without downloading application dependencies. It binds to `127.0.0.1` by default and is intended to sit behind an HTTPS reverse proxy in production.

## Run locally

Python 3.12 or newer and `ffprobe` are required. The worker uses `ffprobe` to reject corrupt or empty provider media before capturing credits. Provider responses are bounded by `SECUT_MAX_PROVIDER_RESPONSE_BYTES`; image reference uploads are limited per file by `SECUT_MAX_REFERENCE_IMAGE_BYTES` (20 MiB by default).

```bash
cd services/seecut-server
cp .env.example .env
set -a
source .env
set +a
PYTHONPATH=src python3 -m seecut_server.app
```

For production, set a random `SECUT_SIGNING_SECRET` containing at least 32 characters. Keep Xiangxin, SMTP and Alipay credentials only in the server environment. Do not commit `.env`.

The local health check is:

```bash
curl http://127.0.0.1:8787/health
```

## Test

```bash
cd services/seecut-server
PYTHONPATH=src python3 -m unittest discover -s tests -v
```

## Current scope

- Email registration, email verification, login, logout and password reset tokens.
- Provider-neutral email delivery with SMTP and Tencent Cloud SES template API implementations.
- Personal sessions and personal wallets.
- Teams created by a user and single-use, expiring invitation links.
- Team asset metadata and signed upload/download URLs backed by local private storage.
- Separate expiring staging uploads for generation inputs; staging uploads never become team assets implicitly.
- Configured Image2 adapter (`SECUT_IMAGE2_MODEL`, default `gpt-image-2.5-flare`) and Xiangxin Seedance 2.0 Mini video adapter. Image2 responses that are asynchronous remain `pending_reconcile` with the upstream task ID and keep their credit hold.
- Static model capability whitelist and parameter-bound credit quotes.
- Durable generation worker with recovery, authenticated local result streaming, credit hold/capture/release, expiring local outputs and `pending_reconcile` states.
- RSA2-signed Alipay web-payment URL, asynchronous notifications and signed order-query compensation. Merchant configuration comes only from environment variables and missing configuration returns `ALIPAY_NOT_CONFIGURED`.
- Account endpoints use a bounded per-source-IP rate limiter. Passwords must be 10 to 128 characters.

The local signed storage implementation is an interface-compatible first step. Production deployment can replace it with private COS upload/download signatures without changing the client workflow.

### Tencent Cloud SES

Use `.env.tencent.example` as the production template. It targets Tencent Cloud SES in `ap-guangzhou`, where `mail.stormycry.cloud` is already verified, and uses `seecut@mail.stormycry.cloud` as the SeeCut sender. Create separate approved templates for email verification and password reset; each template must contain the `{{code}}` variable. Configure a dedicated SecretId and SecretKey only in the protected server environment, then verify a real registration and password-reset delivery through the candidate environment. The API sends both messages as trigger emails. Generic SMTP configuration remains available in `.env.example` for compatible providers.

Alipay remains deferred. The Tencent template intentionally leaves all Alipay settings empty, so credit orders remain unavailable until merchant configuration is approved and supplied.

## Configuration

All supported variables are documented in `.env.example`. `GET /api/capabilities` reports whether email, Xiangxin and Alipay configuration is complete without revealing credential values.

Model pricing is an explicit server setting. Production requires the complete billing key returned by the capability catalog for every accepted parameter combination. Development and test environments may use a `kind:model` fallback. For example:

```bash
export SECUT_MODEL_PRICES_JSON='{"image2:gpt-image-2.5-flare:operation=generate:size=auto:quality=high":12}'
```

No production generation task is accepted until its full billing key has a non-negative integer credit price.

Generated outputs expire after `SECUT_GENERATION_OUTPUT_TTL_SECONDS`. Periodic cleanup deletes expired personal outputs and unused generation staging files. It preserves team assets and staging inputs referenced by active tasks.

Credit plans must be created by an administrative migration once product pricing is approved. The server does not seed invented prices. A development-only example is:

```sql
INSERT INTO plans(id,name,price_fen,credits,active,created_at)
VALUES('plan_dev_100','100 积分',100,100,1,unixepoch());
```

## Deployment boundary

This directory contains no Nginx, systemd or production deployment mutation. A production deployment should use an independent service account and database backup, bind this process to a loopback address, and expose only the intended HTTPS API host through Nginx. The API key and merchant private key must never be sent to the desktop client or written to request logs.
