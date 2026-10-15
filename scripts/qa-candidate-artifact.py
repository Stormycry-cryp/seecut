#!/usr/bin/env python3
"""Keep or restore one normal Linux App on GitHub Actions; never run the App.

create --app ABSOLUTE --output-dir FRESH_ABSOLUTE --expected-head FULL_SHA
restore --source-run-id ID --source-artifact-id ID --output-dir FRESH_ABSOLUTE
        --expected-head WORKFLOW_SHA --source-head APP_SOURCE_SHA

Both commands require a Linux x86_64 GitHub Actions runner, the exact checked-out
HEAD and GH_TOKEN/GITHUB_TOKEN. For restore, the App source HEAD may differ from
the checked-out workflow HEAD. Only candidate.tar belongs in the ordinary
one-day artifact; private QA evidence must be uploaded separately. Stdout and
outputs.json contain paths and identity, never credentials. All limits are hard
limits, including wrapper overhead; create reserves 1 MiB for the ZIP wrapper. An App
close to 512 MiB can exceed the combined archive limit.
"""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

REPOSITORY = "Stormycry-cryp/seecut"
PLATFORM = "Linux x86_64"
JOB_KEY = "engine"
JOB_NAME = "Linux independent black-box QA"
BUILD_STEP = "Build the normal window candidate"
RETAIN_STEPS = ("Prepare the immutable normal App candidate",
                "Retain the immutable normal App candidate")
LIMIT = 512 * 1024 * 1024
TAR_LIMIT = LIMIT - 1024 * 1024  # Reserve room for GitHub's ZIP wrapper.
MANIFEST_LIMIT = 16 * 1024
JSON_LIMIT = 2 * 1024 * 1024
BUILD_READY_SECONDS = 60
BUILD_READY_OBSERVATIONS = 8
BUILD_READY_INTERVAL = 2
CHUNK = 1024 * 1024
MANIFEST_KEYS = {"schema", "repository", "source_head", "platform", "app_sha256",
                 "app_bytes", "build_run_id", "build_job", "build_job_id",
                 "build_job_name", "build_step_name"}


def positive(value):
    if not re.fullmatch(r"[1-9][0-9]{0,19}", str(value)):
        raise ValueError("IDs must be explicit positive decimal integers")
    return int(value)


def full_head(value):
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("expected HEAD must be 40 lowercase hex characters")
    return value


def absolute_path(value):
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("paths must be absolute without parent traversal")
    for component in (path, *path.parents):
        if component.is_symlink():
            raise ValueError("paths must not contain symlinks")
    return path


def file_snapshot(info):
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("App must be a regular file")
    if not 0 < info.st_size <= LIMIT:
        raise ValueError("App exceeds the 512 MiB limit or is empty")
    return (info.st_dev, info.st_ino, info.st_size, info.st_mode, info.st_nlink,
            info.st_mtime_ns, info.st_ctime_ns)


def new_file(path, mode=0o600):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                             os.O_NOFOLLOW, mode), "wb")


def elf_header(data):
    if (len(data) < 20 or data[:6] != b"\x7fELF\x02\x01" or
            int.from_bytes(data[16:18], "little") not in (2, 3) or
            int.from_bytes(data[18:20], "little") != 62):
        raise ValueError("App is not an ELF64 little-endian x86_64 executable")


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate JSON keys are forbidden")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=pairs)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def remaining_time(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise ValueError("API response exceeded its deadline")
    return remaining


def bounded_read(response, maximum, deadline=None):
    length = response.headers.get("Content-Length")
    if length is not None and (not length.isdecimal() or int(length) > maximum):
        raise ValueError("HTTP declared size exceeds the limit")
    chunks, total = [], 0
    shared_deadline = deadline is not None
    deadline = time.monotonic() + 60 if deadline is None else deadline
    while True:
        if shared_deadline:
            remaining_time(deadline)
        elif time.monotonic() > deadline:
            raise ValueError("API response exceeded its deadline")
        data = response.read1(min(CHUNK, maximum - total + 1))
        if shared_deadline:
            remaining_time(deadline)
        if not data:
            break
        total += len(data)
        if total > maximum:
            raise ValueError("HTTP response exceeds the limit")
        chunks.append(data)
    if length is not None and total != int(length):
        raise ValueError("HTTP response length differs from declared size")
    return b"".join(chunks)


class GitHub:
    def __init__(self):
        self.token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        if not self.token:
            raise ValueError("GH_TOKEN or GITHUB_TOKEN is required")
        if len(self.token) > 4096 or any(ord(c) < 33 or ord(c) > 126 for c in self.token):
            raise ValueError("GitHub token has invalid header characters or length")

    def request(self, suffix):
        return urllib.request.Request("https://api.github.com/repos/" + REPOSITORY + suffix,
                                      headers={"Authorization": "Bearer " + self.token,
                                               "Accept": "application/vnd.github+json",
                                               "X-GitHub-Api-Version": "2022-11-28",
                                               "User-Agent": "seecut-qa-candidate"})

    def get(self, suffix, deadline=None):
        if deadline is None:
            with OPENER.open(self.request(suffix), timeout=30) as response:
                return strict_json(bounded_read(response, JSON_LIMIT))
        timeout = min(30, remaining_time(deadline))
        with OPENER.open(self.request(suffix), timeout=timeout) as response:
            return strict_json(bounded_read(response, JSON_LIMIT, deadline))

    def archive_url(self, artifact_id):
        try:
            with OPENER.open(self.request(f"/actions/artifacts/{artifact_id}/zip"), timeout=30):
                raise ValueError("artifact API must return a signed download redirect")
        except urllib.error.HTTPError as error:
            if error.code != 302:
                raise ValueError(f"artifact download API failed with HTTP {error.code}") from None
            location = error.headers.get("Location", "")
            error.close()
            return download_url(location)


def download_url(value):
    parsed = urllib.parse.urlsplit(value)
    host = parsed.hostname or ""
    suffixes = (".blob.core.windows.net", ".githubusercontent.com",
                ".actions.githubusercontent.com")
    if (parsed.scheme != "https" or parsed.username is not None or
            parsed.password is not None or parsed.port not in (None, 443) or
            parsed.fragment or not any(host.endswith(s) and host != s[1:] for s in suffixes)):
        raise ValueError("artifact download host or URL is not allowed")
    return value


def download(api, artifact_id, destination, expected_digest):
    url = api.archive_url(artifact_id)
    deadline = time.monotonic() + 600
    for redirects in range(4):
        try:
            response = OPENER.open(urllib.request.Request(url, headers={
                "User-Agent": "seecut-qa-candidate"}), timeout=30)
        except urllib.error.HTTPError as error:
            if error.code not in (301, 302, 303, 307, 308) or redirects == 3:
                raise ValueError(f"signed artifact download failed with HTTP {error.code}") from None
            url = download_url(urllib.parse.urljoin(url, error.headers.get("Location", "")))
            error.close()
            continue
        with response, new_file(destination) as output:
            length = response.headers.get("Content-Length")
            if length is not None and (not length.isdecimal() or int(length) > LIMIT):
                raise ValueError("artifact HTTP declared size exceeds 512 MiB")
            digest, total = hashlib.sha256(), 0
            while True:
                if time.monotonic() > deadline:
                    raise ValueError("artifact download exceeded its deadline")
                data = response.read1(min(CHUNK, LIMIT - total + 1))
                if not data:
                    break
                total += len(data)
                if total > LIMIT:
                    raise ValueError("artifact download exceeds 512 MiB")
                output.write(data)
                digest.update(data)
            if length is not None and total != int(length):
                raise ValueError("artifact HTTP length mismatch")
            if expected_digest is not None and digest.hexdigest() != expected_digest:
                raise ValueError("artifact REST SHA256 digest mismatch")
            return
    raise ValueError("artifact redirect limit reached")


@contextmanager
def create_source_deadline():
    # A Linux wall-clock watchdog covers the wait, including API I/O and sleeps.
    if any(signal.getitimer(signal.ITIMER_REAL)):
        raise ValueError("create source check requires an unused wall-clock timer")
    previous = signal.getsignal(signal.SIGALRM)
    deadline = time.monotonic() + BUILD_READY_SECONDS
    def expired(_signum, _frame):
        raise ValueError("create source Build readiness exceeded its 60 second deadline")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, remaining_time(deadline))
    try:
        yield deadline
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def wait_for_build(api, job, run_id, deadline):
    pinned_id = job["id"]
    for observation in range(BUILD_READY_OBSERVATIONS):
        if (not isinstance(job, dict) or type(job.get("id")) is not int or job["id"] != pinned_id or
                type(job.get("run_id")) is not int or job["run_id"] != run_id or
                job.get("name") != JOB_NAME):
            raise ValueError("pinned source build job identity changed")
        steps = job.get("steps", [])
        if not isinstance(steps, list) or any(not isinstance(step, dict) for step in steps):
            raise ValueError("invalid source build steps response")
        matching = [step for step in steps if step.get("name") == BUILD_STEP]
        if len(matching) > 1:
            raise ValueError("source must have exactly one matching Build step")
        step = matching[0] if matching else {}
        status = step.get("status") if matching else "missing"
        conclusion = step.get("conclusion")
        def label(value):
            return str(value) if value in (None, "missing", "queued", "in_progress", "completed",
                                           "success", "failure", "skipped", "cancelled", "timed_out",
                                           "action_required", "neutral", "startup_failure") else "invalid"
        print(f"candidate build observation {observation + 1}/{BUILD_READY_OBSERVATIONS}: "
              f"job_id={pinned_id} run_id={run_id} job_status={label(job.get('status'))} "
              f"job_conclusion={label(job.get('conclusion'))} "
              f"step_status={label(status)} step_conclusion={label(conclusion)}", file=sys.stderr)
        job_status, job_conclusion = job.get("status"), job.get("conclusion")
        if (job_status not in ("queued", "in_progress", "completed") or
                (job_status == "completed" and job_conclusion != "success") or
                (job_status != "completed" and job_conclusion is not None)):
            raise ValueError("source build job status or conclusion is invalid")
        if status == "completed" and conclusion == "success":
            return pinned_id
        if (status not in ("missing", "queued", "in_progress") or
                conclusion is not None or job_status == "completed"):
            raise ValueError(f"source step was not successfully completed: {BUILD_STEP}")
        if observation + 1 == BUILD_READY_OBSERVATIONS:
            raise ValueError("source Build readiness observation limit exhausted")
        time.sleep(min(BUILD_READY_INTERVAL, remaining_time(deadline)))
        remaining_time(deadline)
        job = api.get(f"/actions/jobs/{pinned_id}", deadline=deadline)
    raise ValueError("source Build readiness observation limit exhausted")


def source_job(api, run_id, head, restoring):
    if restoring:
        return checked_source_job(api, run_id, head, True)
    with create_source_deadline() as deadline:
        return checked_source_job(api, run_id, head, False, deadline)


def checked_source_job(api, run_id, head, restoring, deadline=None):
    def get(suffix):
        return api.get(suffix) if restoring else api.get(suffix, deadline=deadline)
    run = get(f"/actions/runs/{run_id}")
    if (type(run.get("id")) is not int or run.get("id") != run_id or run.get("repository", {}).get("full_name") != REPOSITORY or
            run.get("head_repository", {}).get("full_name") != REPOSITORY or
            run.get("head_sha") != head or run.get("event") != "workflow_dispatch" or
            run.get("path") != ".github/workflows/ci.yml"):
        raise ValueError("source run repository, HEAD, event or workflow does not match")
    if restoring and run.get("status") != "completed":
        raise ValueError("source run must be completed before restore")
    jobs = []
    for page in range(1, 6):
        data = get(f"/actions/runs/{run_id}/jobs?filter=all&per_page=100&page={page}")
        current = data.get("jobs")
        if not isinstance(current, list):
            raise ValueError("invalid source jobs response")
        jobs.extend(current)
        if len(current) < 100:
            break
    else:
        raise ValueError("source job list exceeds the bounded page limit")
    # Reruns can create ambiguous repeated job names; reject instead of guessing.
    matching = [job for job in jobs if job.get("name") == JOB_NAME]
    if len(matching) != 1:
        raise ValueError("source must have exactly one matching build job")
    job = matching[0]
    if type(job.get("id")) is not int:
        raise ValueError("source job ID must be an integer")
    positive(job["id"])
    if type(job.get("run_id")) is not int or job.get("run_id") != run_id or (restoring and (job.get("status") != "completed" or
                                      job.get("conclusion") not in ("success", "failure"))):
        raise ValueError("source build job identity or completion is invalid")
    if not restoring:
        return wait_for_build(api, job, run_id, deadline)
    for name in (BUILD_STEP, *RETAIN_STEPS):
        steps = [step for step in job.get("steps", []) if step.get("name") == name]
        if len(steps) != 1 or steps[0].get("status") != "completed" or steps[0].get("conclusion") != "success":
            raise ValueError(f"source step was not successfully completed: {name}")
    return job["id"]


def validate_manifest(manifest, head, run_id, job_id):
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise ValueError("manifest fields do not match schema 1")
    expected = {"schema": 1, "repository": REPOSITORY, "source_head": head,
                "platform": PLATFORM, "build_run_id": run_id, "build_job": JOB_KEY,
                "build_job_id": job_id, "build_job_name": JOB_NAME, "build_step_name": BUILD_STEP}
    for key, value in expected.items():
        if type(manifest[key]) is not type(value) or manifest[key] != value:
            raise ValueError(f"manifest source mismatch: {key}")
    if (type(manifest["app_bytes"]) is not int or not 0 < manifest["app_bytes"] <= LIMIT or
            not isinstance(manifest["app_sha256"], str) or
            not re.fullmatch(r"[0-9a-f]{64}", manifest["app_sha256"])):
        raise ValueError("invalid manifest App size or SHA256")
    return manifest


def artifact_name(head, run_id):
    return f"seecut-linux-candidate-{head}-{run_id}"


def create(app, output, head, api, run_id):
    if os.environ.get("GITHUB_JOB") != JOB_KEY:
        raise ValueError("create must run in the engine job")
    job_id = source_job(api, run_id, head, False)
    app = absolute_path(app)
    with os.fdopen(os.open(app, os.O_RDONLY | os.O_NOFOLLOW), "rb") as source:
        snapshot = file_snapshot(os.fstat(source.fileno()))
        elf_header(source.read(20))
        source.seek(0)
        digest, total = hashlib.sha256(), 0
        with tempfile.TemporaryFile(dir=output) as copy:
            while True:
                data = source.read(min(CHUNK, LIMIT - total + 1))
                if not data:
                    break
                total += len(data)
                if total > LIMIT or total > snapshot[2]:
                    raise ValueError("source App grew during bounded copy")
                copy.write(data)
                digest.update(data)
            if total != snapshot[2] or file_snapshot(os.fstat(source.fileno())) != snapshot or file_snapshot(app.lstat()) != snapshot:
                raise ValueError("source App changed during copy")
            manifest = {"schema": 1, "repository": REPOSITORY, "source_head": head,
                        "platform": PLATFORM, "app_sha256": digest.hexdigest(), "app_bytes": total,
                        "build_run_id": run_id, "build_job": JOB_KEY, "build_job_id": job_id,
                        "build_job_name": JOB_NAME, "build_step_name": BUILD_STEP}
            encoded = json_bytes(manifest)
            if len(encoded) > MANIFEST_LIMIT:
                raise ValueError("manifest exceeds 16 KiB")
            # Account for two headers, block padding and tarfile's record padding.
            estimated = ((512 + ((total + 511) // 512) * 512 + 512 +
                          ((len(encoded) + 511) // 512) * 512 + 1024 + 10239) // 10240) * 10240
            if estimated > TAR_LIMIT:
                raise ValueError("candidate.tar exceeds the 512 MiB budget with 1 MiB ZIP reserve")
            archive = output / "candidate.tar"
            with new_file(archive) as archive_file, tarfile.open(fileobj=archive_file, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                info = tarfile.TarInfo("concat")
                info.size, info.mode = total, 0o700
                copy.seek(0)
                tar.addfile(info, copy)
                info = tarfile.TarInfo("manifest.json")
                info.size, info.mode = len(encoded), 0o600
                import io
                tar.addfile(info, io.BytesIO(encoded))
            if archive.stat().st_size > TAR_LIMIT:
                raise ValueError("candidate.tar exceeds the 512 MiB budget with 1 MiB ZIP reserve")
            # Re-hash archive content before publishing outputs, including copied App.
            check_archive(archive, head, run_id, job_id, None)
            if file_snapshot(os.fstat(source.fileno())) != snapshot or file_snapshot(app.lstat()) != snapshot:
                raise ValueError("source App changed before archive completion")
    with new_file(output / "manifest.json") as target:
        target.write(encoded)
    return outputs(output, app, manifest, None, head)


def unwrap_zip(zip_path, archive):
    with zipfile.ZipFile(zip_path) as zipped:
        items = zipped.infolist()
        if len(items) != 1 or items[0].filename != "candidate.tar":
            raise ValueError("artifact ZIP must contain only candidate.tar")
        item = items[0]
        mode = item.external_attr >> 16
        if (item.is_dir() or (stat.S_IFMT(mode) not in (0, stat.S_IFREG)) or
                item.flag_bits & 1 or item.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED) or
                not 0 < item.file_size <= LIMIT or
                not 0 < item.compress_size <= LIMIT):
            raise ValueError("invalid artifact ZIP member type, encryption or size")
        total = 0
        with zipped.open(item) as source, new_file(archive) as target:
            while True:
                data = source.read(min(CHUNK, LIMIT - total + 1))
                if not data:
                    break
                total += len(data)
                if total > LIMIT or total > item.file_size:
                    raise ValueError("ZIP expanded candidate.tar exceeds its limit")
                target.write(data)
        if total != item.file_size:
            raise ValueError("ZIP member size mismatch")


def check_archive(archive, head, run_id, job_id, restored_app):
    size = archive.stat().st_size
    if not 0 < size <= LIMIT or size % 512:
        raise ValueError("candidate.tar has invalid size")
    manifest, seen, app_digest, app_size = None, set(), None, None
    # Temporary App remains unnamed until every manifest/API/hash check passes.
    with archive.open("rb") as source, tempfile.TemporaryFile(dir=archive.parent) as copy:
        while True:
            header = source.read(512)
            if header == b"\0" * 512:
                trailer_size = 512
                while True:
                    trailer = source.read(CHUNK)
                    if not trailer:
                        break
                    trailer_size += len(trailer)
                    if any(trailer) or trailer_size > 10240:
                        raise ValueError("candidate.tar has nonzero or excessive trailing data")
                if trailer_size < 1024:
                    raise ValueError("candidate.tar is missing its terminator")
                break
            if len(header) != 512 or header[257:265] != b"ustar\x0000":
                raise ValueError("candidate.tar requires ordinary USTAR headers")
            item = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
            if (item.name not in {"concat", "manifest.json"} or item.name in seen or
                    item.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or item.linkname or
                    item.sparse is not None or item.pax_headers or item.size <= 0):
                raise ValueError("candidate.tar contains an extra, duplicate or nonregular entry")
            seen.add(item.name)
            maximum = MANIFEST_LIMIT if item.name == "manifest.json" else LIMIT
            if item.size > maximum or item.size > size - source.tell():
                raise ValueError("candidate.tar member exceeds its limit")
            remaining, data_parts, digest = item.size, [], hashlib.sha256()
            while remaining:
                data = source.read(min(CHUNK, remaining))
                if not data:
                    raise ValueError("candidate.tar member is truncated")
                if item.name == "concat":
                    if remaining == item.size:
                        elf_header(data)
                    digest.update(data)
                    if restored_app is not None:
                        copy.write(data)
                else:
                    data_parts.append(data)
                remaining -= len(data)
            if item.name == "concat":
                app_digest, app_size = digest.hexdigest(), item.size
            else:
                manifest = validate_manifest(strict_json(b"".join(data_parts)), head, run_id, job_id)
            padding = (-item.size) % 512
            if source.read(padding) != b"\0" * padding:
                raise ValueError("candidate.tar has invalid member padding")
        if (seen != {"concat", "manifest.json"} or manifest is None or
                manifest["app_sha256"] != app_digest or manifest["app_bytes"] != app_size):
            raise ValueError("candidate App SHA256 or manifest size mismatch")
        if restored_app is not None:
            copy.seek(0)
            with new_file(restored_app, 0o700) as target:
                while True:
                    data = copy.read(CHUNK)
                    if not data:
                        break
                    target.write(data)
    return manifest


def restore(output, head, api, run_id, artifact_id, workflow_head):
    job_id = source_job(api, run_id, head, True)
    artifact = api.get(f"/actions/artifacts/{artifact_id}")
    origin = artifact.get("workflow_run", {})
    declared = artifact.get("size_in_bytes")
    if (type(artifact.get("id")) is not int or artifact.get("id") != artifact_id or artifact.get("name") != artifact_name(head, run_id) or
            artifact.get("expired") is not False or type(declared) is not int or
            not 0 < declared <= LIMIT or type(origin.get("id")) is not int or
            origin.get("id") != run_id or origin.get("head_sha") != head):
        raise ValueError("artifact identity, source, expiry or declared size is invalid")
    digest = artifact.get("digest")
    if digest is not None:
        if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
            raise ValueError("artifact REST digest format is invalid")
        digest = digest[7:]
    zip_path, archive = output / ".candidate.zip", output / "candidate.tar"
    download(api, artifact_id, zip_path, digest)
    if zip_path.stat().st_size != declared:
        raise ValueError("downloaded ZIP size differs from artifact REST size_in_bytes")
    unwrap_zip(zip_path, archive)
    manifest = check_archive(archive, head, run_id, job_id, output / "concat")
    with new_file(output / "manifest.json") as target:
        target.write(json_bytes(manifest))
    zip_path.unlink()
    return outputs(output, output / "concat", manifest, artifact_id, workflow_head)


def outputs(output, app, manifest, artifact_id, workflow_head):
    result = {"binary_path": str(app), "manifest_path": str(output / "manifest.json"),
              "archive_path": str(output / "candidate.tar"), "source_head": manifest["source_head"],
              "workflow_head": workflow_head,
              "app_sha256": manifest["app_sha256"], "app_bytes": manifest["app_bytes"],
              "source_run_id": manifest["build_run_id"], "source_artifact_id": artifact_id,
              "artifact_name": artifact_name(manifest["source_head"], manifest["build_run_id"])}
    with new_file(output / "outputs.json") as target:
        target.write(json_bytes(result))
    return result


def runner_head(expected):
    if (os.environ.get("GITHUB_ACTIONS") != "true" or platform.system() != "Linux" or
            platform.machine() != "x86_64" or os.environ.get("GITHUB_REPOSITORY") != REPOSITORY):
        raise ValueError("candidate commands require this repository's Linux x86_64 Actions runner")
    actual = subprocess.check_output(["git", "rev-parse", "--verify", "HEAD"], text=True).strip()
    if actual != expected:
        raise ValueError("checked-out git HEAD differs from expected HEAD")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    for name in ("create", "restore"):
        command = sub.add_parser(name)
        command.add_argument("--output-dir", required=True)
        command.add_argument("--expected-head", required=True)
        if name == "create":
            command.add_argument("--app", required=True)
        else:
            command.add_argument("--source-run-id", required=True)
            command.add_argument("--source-artifact-id", required=True)
            command.add_argument("--source-head", required=True)
    args = parser.parse_args()
    try:
        workflow_head = full_head(args.expected_head)
        runner_head(workflow_head)
        head = full_head(args.source_head) if args.mode == "restore" else workflow_head
        run_id = positive(os.environ.get("GITHUB_RUN_ID", "")) if args.mode == "create" else positive(args.source_run_id)
        artifact_id = positive(args.source_artifact_id) if args.mode == "restore" else None
        output = absolute_path(args.output_dir)
        output.mkdir(mode=0o700, parents=False, exist_ok=False)
        api = GitHub()
        result = create(args.app, output, head, api, run_id) if args.mode == "create" else restore(output, head, api, run_id, artifact_id, workflow_head)
        print(json_bytes(result).decode(), end="")
        return 0
    except (ValueError, OSError, KeyError, TypeError, tarfile.TarError, zipfile.BadZipFile,
            subprocess.SubprocessError) as error:
        # Avoid displaying URLs (signed downloads) or request headers on failures.
        message = str(error) if isinstance(error, ValueError) else type(error).__name__
        print("candidate error: " + message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
