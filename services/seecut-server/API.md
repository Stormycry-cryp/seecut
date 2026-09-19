# SeeCut Server API Contract v0.1

All API responses use JSON except signed media downloads and the Alipay asynchronous-notification response. Protected routes require `Authorization: Bearer <session-token>`. Timestamps are Unix seconds.

Errors use:

```json
{"error":{"code":"STABLE_CODE","message":"面向用户的说明"}}
```

## Accounts

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/auth/register` | Create a personal account and send a 6-digit email-verification code. |
| `POST` | `/api/auth/verify-email` | Consume `{email, token}` for email verification. |
| `POST` | `/api/auth/resend-verification` | Resend verification without revealing account existence. |
| `POST` | `/api/auth/login` | Create a bearer session after email verification. |
| `POST` | `/api/auth/logout` | Revoke the current session. |
| `GET` | `/api/auth/me` | Read the current personal account. |
| `POST` | `/api/auth/forgot-password` | Send a reset token without revealing account existence. |
| `POST` | `/api/auth/reset-password` | Consume `{email, token, password}`, set a new password and revoke existing sessions. |

Development and test environments may return tokens in responses only when `SECUT_EXPOSE_TEST_TOKENS=true`. Production configuration cannot expose them.

Email codes expire after 30 minutes, allow at most five incorrect attempts, and are bound to both the normalized email address and purpose. Resend requests for an existing account have a 60-second cooldown and may return `EMAIL_CODE_COOLDOWN` with `retry_after`. Unknown addresses still receive `202 {"accepted":true}` from resend and forgot-password. A configured-provider or delivery failure returns `503 EMAIL_DELIVERY_UNAVAILABLE`; the API does not report an email as sent in that case. Password reset does not change email-verification state.

## Teams and invitations

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/teams` | Create a shared team asset space owned by the current person. |
| `GET` | `/api/teams` | List teams joined by the current person. |
| `POST` | `/api/teams/{teamId}/invites` | Owner creates a single-use expiring invite. |
| `DELETE` | `/api/teams/{teamId}/invites/{inviteId}` | Owner revokes an unused invite. |
| `POST` | `/api/team-invites/accept` | Current person consumes an invite token once. |
| `GET` | `/api/teams/{teamId}/members` | List members of a joined team. |
| `DELETE` | `/api/teams/{teamId}/members/{userId}` | Owner removes a member. |

Invitations grant team asset membership only. They never share accounts, sessions or credit balances.

## Uploads and team assets

Request an upload with `POST /api/uploads`:

```json
{
  "purpose": "team_asset",
  "team_id": "team_...",
  "filename": "scene.png",
  "content_type": "image/png",
  "size_bytes": 12345,
  "sha256": "optional-lowercase-sha256"
}
```

The server returns `{"upload_id":"upl_...","method":"PUT","upload_url":"https://...","expires_at":1700003600,"required_headers":{"Content-Type":"image/png"}}`. Use `purpose=team_asset` only after the user explicitly chooses to upload to the team. Use `purpose=generation_input` for local media temporarily relayed to a provider and omit `team_id`. Generation inputs accept PNG, JPG, WebP, MP4, MOV, MP3 and WAV. The completed upload is inspected with `ffprobe`; its bytes must match the declared media family. Video and audio references must each be 2-15 seconds.

After a team upload completes, call `POST /api/teams/{teamId}/assets` with `{"upload_id":"upl_..."}`. This explicit second step creates the cloud asset metadata. Listing and downloading use:

| Method | Path |
| --- | --- |
| `GET` | `/api/teams/{teamId}/assets` |
| `POST` | `/api/teams/{teamId}/assets/{assetId}/download` |
| `DELETE` | `/api/teams/{teamId}/assets/{assetId}` (owner moves metadata to the recycle state) |
| `GET` | `/api/teams/{teamId}/assets/{assetId}/content` (authenticated stream) |
| `POST` | `/api/teams/{teamId}/assets/{assetId}/restore` |

The download action returns an authenticated relative content URL. The content route rechecks the session and current membership, so removing a member immediately revokes future downloads.

## Wallet and Alipay

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/wallet` | Personal available credits, held credits and ledger. |
| `GET` | `/api/credit-plans` | Active personal recharge plans. |
| `POST` | `/api/orders` | Create an Alipay order from `plan_id`. |
| `GET` | `/api/orders` | List the current person's most recent 100 orders. |
| `GET` | `/api/orders/{orderId}` | Query the current person's order. |
| `POST` | `/api/orders/{orderId}/refresh` | Signed Alipay trade-query compensation. |
| `POST` | `/api/payments/alipay/notify` | Alipay form-encoded asynchronous notification. |

Order creation returns `ALIPAY_NOT_CONFIGURED` until all merchant fields and key files exist. It returns an RSA2-signed `payment_action.open_url`; the server does not open the URL itself. Notification processing verifies RSA2, `app_id`, `seller_id`, order and amount, then uses a unique provider event before personal wallet crediting.

## Generation

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/generation/capabilities` | Static SeeCut capability and parameter whitelist. `/models` is an alias. |
| `POST` | `/api/generation/quote` | Freeze a canonical parameter set into a ten-minute quote. |
| `POST` | `/api/generation/assets` | Turn an uploaded temporary input into a user-owned generation asset. |
| `POST` | `/api/generation/images` | Submit image generation. |
| `POST` | `/api/generation/videos` | Submit video generation. |
| `GET` | `/api/generation/tasks` | List the person's recent tasks. |
| `GET` | `/api/generation/tasks/{taskId}` | Read durable worker state. |
| `GET` | `/api/generation/tasks/{taskId}/outputs/{outputId}/content` | Authenticated result stream. |

Generation submissions require an `Idempotency-Key` header and the unmodified `quote_id` plus quoted request fields. The Image2 model is configured by `SECUT_IMAGE2_MODEL` (default `gpt-image-2.5-flare`); video uses fixed Xiangxin model `sd_2.0_mini_special`. Video duration accepts every integer from 4 through 15 seconds and `generate_audio` is a strict boolean that defaults to `true`. A video request accepts at most nine ordered references: up to nine images, three videos and three audio clips. Reference video and audio totals are independently limited to 15 seconds, and audio cannot be the only reference type. The gateway registers each asset as `Image`, `Video` or `Audio` and sends only the matching `reference_images`, `reference_videos` and `reference_audios` arrays. HTTP submission only queues the task. The durable worker submits and recovers polling after restart. Definite failure releases the hold. A verified local result captures it. Submission uncertainty produces `pending_reconcile`, keeps the hold and is not blindly resubmitted. An asynchronous Image2 response preserves its upstream task ID in this state; the current worker does not poll an Image2 task automatically.

Task status is one of `queued`, `submitting`, `provider_accepted`, `processing`, `validating`, `succeeded`, `failed`, `pending_reconcile`, or `expired`. A task enters `pending_reconcile` when submission or polling cannot be resolved safely; its credit hold remains in place and a submission with an unknown result is never repeated automatically. Successful outputs expire after the configured local TTL and then move the task to `expired`.

Before a task succeeds, the server downloads the provider result, checks its host and content type, and uses `ffprobe` to verify readable image dimensions or a positive video duration. Outputs contain `id`, `content_type`, `size_bytes`, `sha256`, `expires_at`, and an authenticated relative `download_url`; provider URLs are never returned. Cleanup removes expired personal outputs and unused staging files while preserving team assets and inputs referenced by active tasks.

Production quotes require a configured full billing key for the complete normalized parameter set. The shorter `kind:model` price key is accepted only in development and test environments.

Account endpoints are limited per source IP using `SECUT_AUTH_RATE_LIMIT_PER_MINUTE` and `SECUT_AUTH_RATE_LIMIT_WINDOW_SECONDS`. Passwords must be 10 to 128 characters. Provider and result requests reject redirects; result hosts are checked against `SECUT_PROVIDER_RESULT_HOSTS` before downloading. Provider JSON responses are bounded by `SECUT_MAX_PROVIDER_RESPONSE_BYTES`. Image reference files are bounded by `SECUT_MAX_REFERENCE_IMAGE_BYTES`; video and audio reference files are bounded at 200 MiB and 15 MiB respectively.

Successful quote response:

```json
{"quote_id":"quote_...","credits":12,"currency":"credits","expires_at":1700000600,"billing_key":"image2:gpt-image-2.5-flare:operation=generate:size=auto:quality=high","request":{"model":"gpt-image-2.5-flare","operation":"generate","prompt":"...","size":"auto","quality":"high","n":1,"output_format":"png"}}
```

Task response:

```json
{"id":"gen_...","kind":"image","model":"gpt-image-2.5-flare","operation":"generate","prompt":"...","quoted_credits":12,"status":"succeeded","error":null,"outputs":[{"id":"out_...","content_type":"image/png","size_bytes":12345,"sha256":"...","created_at":1700000010,"download_url":"/api/generation/tasks/gen_.../outputs/out_.../content"}],"created_at":1700000000,"updated_at":1700000010}
```

## Team asset lifecycle

`GET /api/teams/{teamId}/assets?trash=true` lists the recycle state. Owners use `DELETE /api/teams/{teamId}/assets/{assetId}` and `POST /api/teams/{teamId}/assets/{assetId}/restore`. Team content streams from `GET /api/teams/{teamId}/assets/{assetId}/content`, which rechecks the bearer session and current membership on every request.

## Alipay order action

With complete merchant configuration, `POST /api/orders` returns:

```json
{"id":"ord_...","status":"pending","provider":"alipay","amount_fen":100,"credits":100,"payment_action":{"type":"open_url","url":"https://openapi.alipay.com/gateway.do?...","expires_at":1700001800}}
```

The URL is an RSA2-signed `alipay.trade.page.pay` request. `POST /api/orders/{orderId}/refresh` performs signed `alipay.trade.query` compensation and requires a valid RSA2 signature on the response before checking status, amount, order number and seller when returned. Asynchronous notification settlement remains authoritative and idempotent, and validates `app_id`, `seller_id`, amount, order and RSA2 signature.
