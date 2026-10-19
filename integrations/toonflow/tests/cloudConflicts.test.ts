import { test, expect, beforeAll, afterAll } from "bun:test";
import { createRequire } from "node:module";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { createSeecutProjects } from "../../../external/toonflow/apps/server/src/seecutProjects";
const express = createRequire(new URL("../../../external/toonflow/apps/server/package.json", import.meta.url))("express");
let service: any, base: string;
const digest = (value: Uint8Array) => createHash("sha256").update(value).digest("hex");
beforeAll(async () => {
  service = Bun.spawn(["python3", "integrations/toonflow/tests/cloudConflictFixture.py"], { stdin: "pipe", stdout: "pipe", stderr: "inherit", env: { ...process.env, PYTHONPATH: resolve("services/seecut-server/src") } });
  const reader = service.stdout.getReader();
  let line = "";
  while (!line.includes("\n")) line += new TextDecoder().decode((await reader.read()).value);
  base = `http://127.0.0.1:${JSON.parse(line.split("\n")[0]).port}`;
});
afterAll(async () => { service.stdin.write("stop\n"); service.stdin.end(); await service.exited; });
async function remote(path: string, init: RequestInit = {}) {
  const r = await fetch(base + path, { ...init, headers: { Authorization: "Bearer token-one", ...init.headers } });
  if (!r.ok) { const body: any = await r.json(); throw Object.assign(new Error(body.error?.message || body.message), { code: body.error?.code || body.code, status: r.status }); }
  return r;
}
async function device() {
  const root = await mkdtemp(join(tmpdir(), "seecut-r3-"));
  const calls: any[] = [];
  let epoch = 1, hook: ((method: string, params: any, result: any) => Promise<void>) | undefined;
  const native = async (method: string, params: any = {}) => {
    calls.push({ method, ...params });
    if (method === "account.status") return { signedIn: true, scopeKey: "test-one", epoch };
    expect(params.expectedScopeKey).toBe("test-one");
    let result: any;
    if (method === "projects.cloud.list") result = await (await remote("/api/flow/projects")).json();
    else if (method === "projects.cloud.upload") {
      const bytes = await readFile(params.path);
      result = await (await remote(`/api/flow/projects/${params.projectId}/snapshot?base_revision=${params.baseRevision}&name=${encodeURIComponent(params.name)}`, { method: "PUT", body: bytes, headers: { "Content-Type": "application/zip", "X-Content-SHA256": params.sha256, "Idempotency-Key": params.requestId } })).json();
    } else if (method === "projects.cloud.download") {
      const metadata: any = await (await remote(`/api/flow/projects/${params.projectId}?revision=${params.revision}`)).json();
      const bytes = new Uint8Array(await (await remote(metadata.download_url)).arrayBuffer());
      await writeFile(params.path, bytes);
      result = { metadata, verifiedSha256: digest(bytes) };
    } else if (method === "generation.exportProject") {
      const folder = join(root, "workspaces", params.projectId, ".seecut"); await mkdir(folder, { recursive: true });
      await writeFile(join(folder, "native-generation.json"), JSON.stringify({ format: 1, project_id: params.projectId, scope: "test-one", ledgers: [{ id: "historical-paid-attempt" }] }));
      result = {};
    } else if (method === "generation.importProject") result = {};
    else throw new Error(method);
    await hook?.(method, params, result);
    return result;
  };
  const projects = await createSeecutProjects(root, native as any);
  const app = express(); app.use(projects.router);
  const server: any = await new Promise(resolve => { const s = app.listen(0, "127.0.0.1", () => resolve(s)); });
  const url = `http://127.0.0.1:${server.address().port}`;
  const request = async (path: string, body?: object) => {
    const response = await fetch(url + "/projects" + path, body ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) } : undefined);
    return { status: response.status, ...await response.json() as any };
  };
  return { root, calls, projects, request, hook(value: typeof hook) { hook = value; }, changeAccount() { epoch++; },
    async edit(id: string, value: string) {
      const folder = await projects.directory(id);
      await projects.withProjectWrite(folder, async () => {
        await writeFile(join(folder, "canvas.json"), JSON.stringify({ nodes: [{ id: "image", text: value }], edges: [{ source: "text", target: "image" }], viewport: { x: 14, y: 28, zoom: .9 } }));
        await writeFile(join(folder, "document.md"), value);
        await mkdir(join(folder, "assets"), { recursive: true }); await writeFile(join(folder, "assets/image.png"), new Uint8Array([1, 2, 3, 4]));
      });
      await projects.touch(id);
    },
    async close() { await projects.close(); await new Promise<void>(resolve => server.close(() => resolve())); await rm(root, { recursive: true, force: true }); },
  };
}
async function pair() {
  const a = await device(), b = await device();
  const id = (await a.request("", { name: "R3" })).project.id;
  await a.edit(id, "A1"); expect((await a.request(`/${id}/sync`, {})).sync.state).toBe("synced");
  await b.request("?refresh=1");
  // refresh endpoint explicitly awaits the same remote list rather than polling local state
  await b.request(`/${id}/sync?refresh=1`);
  expect((await b.request(`/${id}/open`, {})).status).toBe(200);
  await b.edit(id, "B2"); expect((await b.request(`/${id}/sync`, {})).sync.state).toBe("synced");
  await a.edit(id, "A2"); expect((await a.request(`/${id}/sync`, {})).sync.state).toBe("conflict");
  const opened = await a.request(`/${id}/open`, {});
  const state = await a.request(`/${id}/sync?refresh=1`);
  return { a, b, id, input: { ...state, requestId: randomUUID(), session: opened.session }, async close() { await a.close(); await b.close(); } };
}
for (const choice of ["local", "cloud"] as const) test(`A/B HTTP round trip: ${choice}, full recovery and continued sync`, async () => {
  const p = await pair();
  try {
    const result = await p.a.request(`/${p.id}/resolve`, { ...p.input, choice });
    expect(result.status).toBe(200);
    const folder = await p.a.projects.directory(p.id), copy = await p.a.projects.directory(result.recovery.id);
    expect(await readFile(join(folder, "document.md"), "utf8")).toBe(choice === "local" ? "A2" : "B2");
    expect(await readFile(join(copy, "document.md"), "utf8")).toBe("A2");
    expect(await readFile(join(copy, "canvas.json"))).toEqual(await readFile(join(p.a.root, "workspaces", choice === "local" ? p.id : result.recovery.id, "canvas.json")));
    expect(digest(await readFile(join(copy, "assets/image.png")))).toBe(digest(new Uint8Array([1, 2, 3, 4])));
    const index = JSON.parse(await readFile(join(p.a.root, "seecutProjects.json"), "utf8"));
    expect(index.projects.find((x: any) => x.id === result.recovery.id).cloud).toBeUndefined();
    expect(await readFile(join(copy, ".seecut/recovery-native-generation.json"), "utf8")).toContain("historical-paid-attempt");
    expect((await p.a.request(`/${result.recovery.id}/open`, {})).status).toBe(200);
    await p.a.edit(p.id, "A3"); expect((await p.a.request(`/${p.id}/sync`, {})).sync.state).toBe("synced");
    expect(p.a.calls.some(x => /generation\.(prepare|submit|task|register)/.test(x.method))).toBe(false);
  } finally { await p.close(); }
}, 30_000);
for (const boundary of ["edit", "account", "close", "remote", "downloadFailure"] as const) test(`late download does not replace current content: ${boundary}`, async () => {
  const p = await pair();
  try {
    let once = false;
    p.a.hook(async method => {
      if (method !== "projects.cloud.download" || once) return; once = true;
      if (boundary === "edit") await p.a.edit(p.id, "NEW LOCAL");
      if (boundary === "account") p.a.changeAccount();
      if (boundary === "close") await p.a.request(`/${p.id}/close`, { session: p.input.session });
      if (boundary === "remote") { await p.b.edit(p.id, "B3"); await p.b.request(`/${p.id}/sync`, {}); }
      if (boundary === "downloadFailure") throw new Error("synthetic download failure");
    });
    const result = await p.a.request(`/${p.id}/resolve`, { ...p.input, choice: "cloud" });
    expect(result.status).toBeGreaterThanOrEqual(400);
    expect(await readFile(join(await p.a.projects.directory(p.id), "document.md"), "utf8")).toBe(boundary === "edit" ? "NEW LOCAL" : "A2");
    const index = JSON.parse(await readFile(join(p.a.root, "seecutProjects.json"), "utf8"));
    expect(index.projects.filter((x: any) => x.recoveryOf === p.id)).toHaveLength(1);
  } finally { await p.close(); }
}, 30_000);
test("duplicate clicks share one adopted recovery; replay returns the same receipt", async () => {
  const p = await pair();
  let release!: () => void, reached!: () => void;
  const gate = new Promise<void>(r => { release = r; }), ready = new Promise<void>(r => { reached = r; });
  try {
    p.a.hook(async method => { if (method === "projects.cloud.download") { reached(); await gate; } });
    const body = { ...p.input, choice: "cloud" };
    const first = p.a.request(`/${p.id}/resolve`, body);
    await ready;
    expect((await p.a.request(`/${p.id}/resolve`, body)).error.code).toBe("FLOW_BUSY");
    release(); const done = await first;
    expect(done.status).toBe(200);
    const replay = await p.a.request(`/${p.id}/resolve`, body);
    expect(replay.recovery.id).toBe(done.recovery.id);
    expect(p.a.calls.filter(x => x.method === "projects.cloud.download")).toHaveLength(1);
  } finally { release(); await p.close(); }
}, 30_000);
test("account change after extraction stops adoption", async () => {
  const p = await pair(); let downloaded = false;
  try {
    p.a.hook(async method => {
      if (method === "projects.cloud.download") downloaded = true;
      else if (downloaded && method === "projects.cloud.list") { p.a.changeAccount(); downloaded = false; }
    });
    expect((await p.a.request(`/${p.id}/resolve`, { ...p.input, choice: "cloud" })).status).toBeGreaterThanOrEqual(400);
    expect(await readFile(join(await p.a.projects.directory(p.id), "document.md"), "utf8")).toBe("A2");
  } finally { await p.close(); }
}, 30_000);
test("cancelled session and stale confirmation do not create copies or download", async () => {
  const p = await pair();
  try {
    await p.a.request(`/${p.id}/close`, { session: p.input.session });
    expect((await p.a.request(`/${p.id}/resolve`, { ...p.input, choice: "cloud" })).status).toBe(409);
    expect(p.a.calls.filter(x => x.method === "projects.cloud.download")).toHaveLength(0);
    expect((await p.a.request("")).projects.filter((x: any) => x.name.includes("恢复副本"))).toHaveLength(0);
  } finally { await p.close(); }
}, 30_000);
test("unknown upload replays original immutable request before resolving a newer cloud", async () => {
  const a = await device(), b = await device();
  try {
    const id = (await a.request("", { name: "lost receipt" })).project.id;
    await a.edit(id, "A1"); let lost = false;
    a.hook(async method => { if (method === "projects.cloud.upload" && !lost) { lost = true; throw Object.assign(new Error("lost response"), { code: "HOST_UNKNOWN" }); } });
    expect((await a.request(`/${id}/sync`, {})).sync.state).toBe("error");
    await b.request("?refresh=1"); await b.request(`/${id}/sync?refresh=1`); await b.request(`/${id}/open`, {});
    await b.edit(id, "B2"); await b.request(`/${id}/sync`, {});
    // Opening replays the original pending operation; it never invents a new key.
    const opened = await a.request(`/${id}/open`, {});
    await a.request(`/${id}/sync`, {});
    await a.edit(id, "A2");
    const state = await a.request(`/${id}/sync?refresh=1`);
    const done = await a.request(`/${id}/resolve`, { ...state, session: opened.session, requestId: randomUUID(), choice: "local" });
    expect(done.status).toBe(200);
    const uploads = a.calls.filter(x => x.method === "projects.cloud.upload");
    expect(uploads.length).toBe(3);
    expect(uploads[0].requestId).toBe(uploads[1].requestId);
    expect(uploads[0].sha256).toBe(uploads[1].sha256);
    expect(uploads[0].baseRevision).toBe(uploads[1].baseRevision);
    expect(uploads[2].requestId).not.toBe(uploads[0].requestId);
    expect(uploads[2].baseRevision).toBe(2);
    expect((await (await remote(`/api/flow/projects/${id}`)).json() as any).revision).toBe(3);
  } finally { await a.close(); await b.close(); }
}, 30_000);
test("unconfirmed original upload is retained, with no replacement key", async () => {
  const p = await pair();
  try {
    // Accept one resolution upload, lose its response; the next recovery attempt
    // must retain that operation when the exact replay is unavailable.
    let uploads = 0;
    p.a.hook(async method => { if (method === "projects.cloud.upload") { uploads++; throw Object.assign(new Error("lost response"), { code: "HOST_UNKNOWN" }); } });
    expect((await p.a.request(`/${p.id}/resolve`, { ...p.input, choice: "local" })).status).toBe(500);
    const before = JSON.parse(await readFile(join(p.a.root, "seecutProjects.json"), "utf8")).projects.find((x: any) => x.id === p.id).cloud.pending;
    expect((await p.a.request(`/${p.id}/resolve`, { ...p.input, requestId: randomUUID(), choice: "cloud" })).status).toBe(500);
    const after = JSON.parse(await readFile(join(p.a.root, "seecutProjects.json"), "utf8")).projects.find((x: any) => x.id === p.id).cloud.pending;
    expect(after).toEqual(before); expect(uploads).toBe(2);
  } finally { await p.close(); }
}, 30_000);
test("corrupted download never replaces either local or recovery content", async () => {
  const p = await pair();
  try {
    p.a.hook(async (method, params) => { if (method === "projects.cloud.download") await writeFile(params.path, "corrupt"); });
    expect((await p.a.request(`/${p.id}/resolve`, { ...p.input, choice: "cloud" })).status).toBe(400);
    expect(await readFile(join(await p.a.projects.directory(p.id), "document.md"), "utf8")).toBe("A2");
  } finally { await p.close(); }
}, 30_000);
