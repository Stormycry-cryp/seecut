# R2 native Agent and embedded Flow — local verification

R1 remains fixed at `298ef67bf5d195bd764bfaa593250e8175bce547` in Draft PR 9. R2 work is isolated on `codex/seecut-flow-r2`; old mixed C03 worktrees and all R1 app candidates remain untouched.

The new named Flow tools operate through the actual mounted editor and its node tools. Calls bind the native project UUID, active canvas, opening/session, account login epoch, observed revision and original operation identity. Writes require the native edit lease; native Agent writes also require the exact named approval. Durable receipts are outside exported projects and scoped by account, client, project and operation. Replay returns the original receipt. Accepted/unknown operations are not dispatched again.

Generation uses the existing managed provider and automatic native asset registration. The node's recorded task exposes the original status, actual accumulated credits, outputs and asset/version identity. Reading that task never launches generation. Retired `flow_read` and `flow_write` remain unavailable. There is one asset library.

macOS uses a transparent native wrapper and mask for the Agent footprint, preserving the Flow viewport size. Empty wrapper areas pass input through to native controls. Windows keeps its previous visibility behavior and is not verified in this macOS run.

## Verification so far

- Flow gate: 8 tests, including stale revision, canvas/account change, replay, managed-provider enforcement and save failure without duplicate dispatch.
- Durable receipt router: 1 test / 8 assertions, including recreation, ownership and changed intent.
- Private Agent runtime regression: 43 tests passed.
- Private Flow recovery: 2 tests passed; same-process and reopened-session recovery query the original operation without another write.
- Server explicit tool contract: 4 tests passed.
- Complete embedded web/server build passed; original-source patch verification passed.
- Native MCP protocol: 23 tests passed (18 library, 5 named-tool/adapter). Final native overlay input/focus/mask build passed (`native-build-final-2.log`). Actual UI opened the existing synthetic project and native Agent together. The first UI run found a missing canvas-host `getAgentState` forwarding method; it was fixed, rebuilt and verified in `01-canvas-host-fixed.png`.

`flow-ui-fixture.py` and `flow_synthetic_provider.py` are explicitly synthetic local QA. The provider reads actual native tool results and drives the formal tool chain; it does not call GLM or any paid media service. Synthetic task bytes and credits are not evidence of real supplier pricing or model quality. No real credential values were read or configured.

R1 CI boundary from parent review (2026-10-15 11:43 UTC): server contracts and wasm succeeded. Android run 38725858438 / job 116391593318 ended after runner shutdown, exit 143; infrastructure interruption, not established as a code regression or OOM. iOS/Engine were still running. No speculative code change or full CI rerun was made for that event.

## Actual UI checkpoint (2026-10-15)

The formal Agent tools read the actual project/canvas and created one text node, receiving `status=completed` and `saved=true`, then read its actual node tools. The next update was cancelled before dispatch while the UI acceptance was interrupted; no generation was submitted in this R2 pass. The complete create/configure/connect/generate/reopen chain remains **unverified**, and no real GLM or paid provider call was made. R1 automatic library registration remains unchanged.

Project: `9e776455-1743-42b2-8ded-a022c47219b9`; text node: `5f2a67eb-9c9d-4a6a-968f-1d372181b958`. Native QA package: `/tmp/SeeCut-Flow-R2-host-fix.app`. Sequential finalization verified the app signature before restoring its synthetic portable runtime data. A packaging/finalization overlap initially caused signature failure; both writers finished before sequential finalization. This is a local QA artifact, not a release candidate.

The user's subsequent Jev recommendation request is tracked independently. This R2 checkpoint preserves the code and evidence without claiming full acceptance or merging PR 9.
