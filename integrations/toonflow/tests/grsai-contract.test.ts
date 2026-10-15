import { test, expect, mock } from "bun:test";

const model = { id: "nano-banana-2.1", kind: "image", available: true, parameters: {
  size: { values: ["1K", "2K", "4K"], default: "1K" },
  aspect_ratio: { values: ["auto", "16:9", "9:16"], default: "auto" },
  reference_asset_ids: { max_items: 16 },
} };
const policy = (id: string) => ({ id, label: id, policy_revision: "v1", generation_available: true,
  generation: { image_model_ids: [model.id], video_model_ids: [] } });
mock.module("../../../external/toonflow/apps/server/src/seecutNative", () => ({ nativeCall: async (method: string) => {
  if (method === "account.status") return { signedIn: true, scopeKey: "fixture" };
  if (method === "generation.catalog") return { scopeKey: "fixture", models: [model], efforts: [policy("low"), policy("high")] };
  throw new Error("Unexpected native operation: " + method);
} }));
const { actualCredits, listSeecutMediaModels } = await import("../../../external/toonflow/apps/server/src/seecutGeneration");

test("one canonical model with explicit policies retains both stable route IDs", async () => {
  const rows = await listSeecutMediaModels() as any[];
  expect(rows).toHaveLength(1);
  expect(rows[0].label).toBe(model.id);
  expect(rows[0].seecutPolicies.map((p: any) => p.label)).toEqual(["low", "high"]);
  expect(new Set(rows[0].seecutPolicies.map((p: any) => p.modelId)).size).toBe(2);
  expect(rows[0].seecutIndependentRatio).toBe(true);
  expect(rows[0].imageRatios).toEqual(["auto", "16:9", "9:16"]);
  expect(rows[0].imageSizes).toEqual(["1K", "2K", "4K"]);
});

test("actual credits use parent total once, never quote/reservation or duplicate children", () => {
  expect(actualCredits([])).toBe(0);
  expect(actualCredits([
    { id: "a", captured_credits: 1, billing_id: "batch", billing_captured_credits: 2 },
    { id: "b", captured_credits: 1, billing_id: "batch", billing_captured_credits: 2 },
    { id: "legacy", captured_credits: 3 }, { id: "legacy", captured_credits: 3 },
    { id: "unknown" },
  ])).toBe(5);
  expect(actualCredits([{ id: "a", captured_credits: -1 }, { id: "b", captured_credits: Number.NaN }])).toBe(0);
});
