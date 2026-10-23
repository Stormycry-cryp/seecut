# Bounded CI diagnostics repair

The job log for [attempt 2, job 118107281225](https://github.com/Stormycry-cryp/seecut/actions/runs/39257512199/job/118107281225) explicitly records a runner shutdown signal and cancellation at 15:47:23 UTC. Its aggregate API status continued to say `in_progress`; this does not override the termination log. No finished test build or assertion result was observed before shutdown. The earlier attempt likewise ended during compilation.

The configured ordinary job limit was 360 minutes; shutdown occurred about 97 minutes after start. Ordinary CI uses `ci-${github.ref}` with `cancel-in-progress: true`. No newer same-branch CI appeared in the pre-repair run listing. Available run/check metadata does not identify the shutdown origin. OOM, a manual cancellation and concurrency cancellation are not established causes.

## Repair and limits

The engine job consistently sets dev/test debug information to zero and Cargo build jobs to one, including the gRPC steps. Product source, tests, features, profiles, concurrency and other job definitions remain unchanged. Workspace and gRPC tests each have a no-run compilation step followed by the original unfiltered test scope, including doctests. Cargo can still compile doctests in the execution step.

Each phase streams its normal output, samples only its own process group's CPU/RSS/state every minute, and retains started/result JSON, raw command output and resource samples. It records elapsed time, child CPU time, maximum child RSS (not a sum of all simultaneous RSS), free disk, Linux memory/swap and load. It does not collect process argv or the full environment. A deadline sends TERM to the command group, then KILL after 15 seconds, preserves failure exit codes and records timeout as 124. A runner TERM records 143 while cleanup is still possible.

| Phase | Inner deadline | Outer step limit |
| --- | ---: | ---: |
| Workspace Clippy | 60 min | 65 min |
| Workspace test compilation | 90 min | 95 min |
| Workspace tests/doctests | 30 min | 35 min |
| gRPC Clippy | 30 min | 35 min |
| gRPC test compilation | 45 min | 50 min |
| gRPC tests/doctests | 20 min | 25 min |

The Clippy budget leaves margin over the observed 39–40 minute cold checks. The other limits are conservative diagnostic ceilings, not measured expected runtimes. The existing 360-minute job budget remains. Artifact upload runs with `always()` after normal failures or phase timeouts and retains evidence for seven days. A destroyed runner cannot execute artifact upload; output also streams to GitHub logs, and this repair does not claim to eliminate the unknown infrastructure shutdown cause.

## Validation and delivery boundary

Four small subprocess tests pass: success/output/resource capture, preserving command failure, timing out an ignoring process and its descendant, and retaining runner-cancellation evidence. They run locally without Cargo or the App and are included as an early CI check. YAML and command-scope validation confirms that all workspace/gRPC coverage remains and that wasm, macOS and existing window diagnostic steps are unchanged. See the adjacent logs and JSON.

The next verification is one new run on this repair, not an unchanged third attempt. No new App has been built, installed or launched; the existing `11c7d8e` candidate retains its old source identity. The prepared `4a2d3d8` packaging inputs are not a deliverable and must be replaced with the eventual green source identity before building. Shared desktop input, production, credentials and paid calls remain untouched. Fresh official weekly quota must remain above 50% before continued work or candidate construction.
