# Jev prompt recommendations — implementation and acceptance

This increment is separate from R2 checkpoint `8101e4e` (private runtime `84c020b`).
Branch: `codex/seecut-prompt-recommendations`. R1 PR9 is unchanged.

User source: “搭建灵感成款模板推荐流程”, thread
`01a13eb8-5699-7033-bc68-da683d8a12bd`. User clarified that Jev selects from the
provided prompt pool; GLM 5.3 Flash rewrites after selection. This implementation
uses SeeCut's six original curated recipes, not FD production/private templates.
Local personal templates have no account ownership metadata and are not uploaded
as if they belonged to the current signed-in account.

The generation page and native canvas use the same Slint form and recommendation
state; native canvas adds a floating “灵感推荐” panel. Flow image/video node editors
share a Vue controller. Both call authenticated server routes via existing native
account transport. The server enforces the common account interval, and supplies
recent history from current-account generation records. Model credentials never
enter the native application, embedded webpage or project export.

Validation:
- Server: 14 tests passed (10 prompt-specific plus 4 existing editor contracts), including supplied-pool ranking, persistent replay, no resend after uncertain results, authoritative account history, GLM alias support, text-only rewrite and failure rollback.
- Web scheduler: 7 tests / 18 assertions passed, including debounce/shared timing, newest input, stale account responses, explicit rewrite, references, Retry-After and clearing the waiting label after apply.
- Native bridge router: 1 test / 12 assertions passed (scope, login epoch, supplier Retry-After).
- Embedded full web/server build passed after integration of R2 review fixes. Initial final-source native build passed in 10m54s; final integrated native build passed in 9m04s (native-build-verified.log). An obsolete earlier debug build was deliberately stopped; it is not acceptance evidence.
- Actual native UI: generation page and native canvas each showed three supplied recipes and applied the synthetic GLM rewrite. Typing v/b/m in the canvas prompt did not change the canvas tool. Added one known synthetic image from the asset library; Jev evidence shows imageCount 1 and six candidates; rewriting retained that reference.
- Final signed candidate `/tmp/SeeCut-Prompt-Recommendations-Final.app` was launched with a fresh synthetic login. Editing and immediately applying an existing recipe produced the rewrite with status “为你推荐”; the stale waiting-label issue is fixed. Signing and all 5,135 Agent manifest files verified before launch.
- Actual embedded UI: Flow image node showed three recommendations, applied the rewrite, and completed exactly one synthetic image task. Without clicking the manual add button, “Flow 图片” appeared in the existing personal asset library.
- Actual appearance: light/dark at 1400×900 and generation form at 1100×750; native canvas floating panel retained its own scroll region at the narrower size. Screenshots are included. Flow video uses the same recommendation component but was not separately exercised in UI.

R2 corrections are separately committed: public 85ef7ea + 8c82ae7, private aaaeae4. The recommendation branch merges them without rewriting PR9. The full Agent-driven R2 UI chain remains pending; the Jev UI smoke test is not a substitute for it.

The local fixture at 127.0.0.1:18743 returns synthetic Jev decisions and a synthetic
GLM rewrite. It performs no external model calls and is not real latency, cost or
quality evidence. R2's separate fixture is not repurposed.

Credential/deployment boundary: production's checked-in service definition reads
`/etc/seecut/server.env` for `/opt/seecut/server`. No file at that target has been
read or changed. Before real Jev activation, the user must safely supply
`BEATAPI_API_KEY` in that server environment; existing `SECUT_ASSISTANT_*` values
remain server-owned. Do not copy the other thread's Keychain credential, publish
keys, invoke real paid GLM, restart production or deploy as part of this fixture QA.

R2 review links: public Draft https://github.com/Stormycry-cryp/seecut/pull/10 ; private Draft https://github.com/Stormycry-cryp/seecut-agent/pull/3 . The private draft uses frozen review base 7dd038b; neither PR is a merge or production instruction.
