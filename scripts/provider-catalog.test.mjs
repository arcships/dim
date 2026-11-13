import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

const root = join(import.meta.dirname, "..");
const resources = "crates/dim-app-server/resources";
const bundled = join(resources, "default-catalog.json");
const script = "scripts/update-provider-catalog.ts";
const overrides = join(resources, "reasoning-off-overrides.json");
const policy = join(resources, "provider-sync.json");

async function checkout(t) {
  const cwd = await mkdtemp(join(tmpdir(), "dim-provider-catalog-"));
  t.after(() => rm(cwd, { recursive: true, force: true }));
  const upstream = join(cwd, "upstream");
  await Promise.all([
    mkdir(join(cwd, "scripts")),
    mkdir(join(cwd, resources), { recursive: true }),
    mkdir(upstream),
  ]);
  await Promise.all([script, bundled, overrides, policy].map((path) => cp(join(root, path), join(cwd, path))));
  const env = { ...process.env, DIM_PROVIDER_REMOTE_BASE: pathToFileURL(upstream).href };
  return { cwd, env, upstream };
}

async function policies(cwd) {
  return JSON.parse(await readFile(join(cwd, policy), "utf8")).providers;
}

/// Writes one upstream file per `remote` provider, from the bundled descriptors,
/// so a full sync of an unchanged catalog must reproduce it byte for byte.
async function writeSelfUpstream(cwd, upstream, providers) {
  const declared = await policies(cwd);
  const written = new Map();
  await Promise.all(providers.map(({ providerId, metadata, models }) => {
    const entry = declared[providerId];
    if (entry.source !== "remote") return undefined;
    const source = entry.upstreamId ?? providerId;
    assert.equal(written.has(source), false, `${source} is the upstream id of two providers`);
    written.set(source, providerId);
    return writeFile(join(upstream, `${source}.json`), JSON.stringify({
      api: metadata.raw.api,
      models: models.map((model) => model.metadata.raw),
    }));
  }));
}

test("the maintenance command validates the bundled catalog offline", async (t) => {
  const { cwd, env } = await checkout(t);
  execFileSync(process.execPath, [script], { cwd, env });
  assert.equal(await readFile(join(cwd, bundled), "utf8"), await readFile(join(root, bundled), "utf8"));
});

test("a full sync of the committed model descriptors is idempotent", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const original = await readFile(path, "utf8");
  const { providers } = JSON.parse(original);
  await writeSelfUpstream(cwd, upstream, providers);
  execFileSync(process.execPath, [script, "--sync"], { cwd, env });
  // A `remote` provider with no committed default gets the one derived from its
  // models. That is the release step's own change, so the first sync may write
  // it; a second sync must then find nothing to do.
  const first = await readFile(path, "utf8");
  const settled = JSON.parse(first).providers
    .filter(({ providerId, metadata }) =>
      providers.find((old) => old.providerId === providerId).metadata.defaultModelId === undefined
      && metadata.defaultModelId !== undefined)
    .map(({ providerId }) => providerId);
  assert.deepEqual(settled.sort(),
    ["qwencloud", "qwencloud-coding-plan", "qwencloud-token-plan", "step-plan-intl", "tencent-hunyuan", "xiaomi-mimo"]);
  const strip = (list) => list.map(({ providerId, metadata, ...rest }) => ({
    providerId,
    ...rest,
    metadata: {
      ...Object.fromEntries(Object.entries(metadata).filter(([key]) => key !== "defaultModelId")),
      raw: Object.fromEntries(Object.entries(metadata.raw).filter(([key]) => key !== "defaultModelId")),
    },
  }));
  assert.deepEqual(strip(JSON.parse(first).providers), strip(providers));

  execFileSync(process.execPath, [script, "--sync"], { cwd, env });
  assert.equal(await readFile(path, "utf8"), first);
});

test("a provider without a policy fails the run and names it", async (t) => {
  const { cwd, env } = await checkout(t);
  const path = join(cwd, policy);
  const declared = await policies(cwd);
  assert.equal(declared["zenmux"].source, "remote");
  delete declared["zenmux"];
  await writeFile(path, JSON.stringify({ providers: declared }, null, 2));
  const defined = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(defined.status, 1);
  assert.match(defined.stderr, /Provider zenmux has no sync policy/);
});

test("a policy for a provider the catalog lacks fails the run and names it", async (t) => {
  const { cwd, env } = await checkout(t);
  const path = join(cwd, policy);
  const declared = await policies(cwd);
  declared["retired-provider"] = { source: "local", reason: "a provider the catalog dropped" };
  await writeFile(path, JSON.stringify({ providers: declared }, null, 2));
  const defined = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(defined.status, 1);
  assert.match(defined.stderr, /Provider retired-provider has a sync policy but the catalog does not list it/);
});

test("a local provider needs a reason and a remote one does not carry it", async (t) => {
  const { cwd, env } = await checkout(t);
  const path = join(cwd, policy);
  const declared = await policies(cwd);

  delete declared["new-api"].reason;
  await writeFile(path, JSON.stringify({ providers: declared }, null, 2));
  const missing = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /Provider new-api: a local provider needs a reason/);

  declared["new-api"].reason = "bundle is empty on purpose";
  declared["zenmux"].reason = "a remote provider has no reason of its own";
  await writeFile(path, JSON.stringify({ providers: declared }, null, 2));
  const carried = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(carried.status, 1);
  assert.match(carried.stderr, /Provider zenmux: only a local provider carries a reason/);
});

test("a scoped sync validates its candidate and publishes only the complete catalog", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const initialCatalog = await readFile(path, "utf8");
  const previous = JSON.parse(initialCatalog);
  const old = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  const defaultId = old.metadata.defaultModelId;
  const upstreamFile = join(upstream, "kimi-for-coding.json");
  const sync = [script, "--sync", old.providerId];
  await writeFile(upstreamFile, JSON.stringify({ models: [{ id: "wrong-model" }] }));
  const failed = spawnSync(process.execPath, sync, { cwd, env, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /local defaultModelId .* missing/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

  await writeFile(upstreamFile, JSON.stringify({ models: [{ id: defaultId, limit: { context: 1.5 } }] }));
  const invalidLimit = spawnSync(process.execPath, sync, { cwd, env, encoding: "utf8" });
  assert.equal(invalidLimit.status, 1);
  assert.match(invalidLimit.stderr, /contextWindow must be a positive safe integer/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

  await writeFile(join(upstream, "openrouter.json"), JSON.stringify({ models: [] }));
  const empty = spawnSync(process.execPath, [script, "--sync", "openrouter"], { cwd, env, encoding: "utf8" });
  assert.equal(empty.status, 1);
  assert.match(empty.stderr, /sync would remove every upstream model/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

  const kimiPro = old.models.find((model) => model.modelId === "k3.1-pro").metadata.raw;
  const freshModels = [
    old.models.find((model) => model.modelId === defaultId).metadata.raw,
    { ...kimiPro, limit: { ...kimiPro.limit, context: 1048576 } },
    { id: "new-model" },
  ];
  await writeFile(upstreamFile, JSON.stringify({ api: "https://upstream.invalid/v1", models: freshModels }));
  execFileSync(process.execPath, sync, { cwd, env });
  const generated = await readFile(path, "utf8");
  const { providers, updatedAt } = JSON.parse(generated);
  const refreshed = providers.find((provider) => provider.providerId === old.providerId);
  assert.notEqual(updatedAt, previous.updatedAt);
  assert.match(generated, /\n        {"modelId":"new-model",/);
  assert.deepEqual({ ...refreshed, models: old.models }, old);
  assert.deepEqual(refreshed.models.map((model) => model.modelId), [defaultId, "k3.1-pro", "new-model"]);
  assert.equal(refreshed.models[1].capabilities.contextWindow, 262144);
  assert.deepEqual(providers.filter((provider) => provider !== refreshed), previous.providers.filter((provider) => provider !== old));
  assert.deepEqual((await readdir(join(cwd, resources))).sort(),
    ["default-catalog.json", "provider-sync.json", "reasoning-off-overrides.json"]);
  assert.deepEqual(await readdir(join(cwd, "scripts")), ["update-provider-catalog.ts"]);

  execFileSync(process.execPath, sync, { cwd, env });
  assert.equal(await readFile(path, "utf8"), generated);
});

test("sync refreshes upstream metadata and keeps the catalog's local media declarations", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const openai = previous.providers.find((provider) => provider.providerId === "openai");
  const reasoning = JSON.parse(await readFile(join(cwd, overrides), "utf8"));
  const requiredIds = new Set([openai.metadata.defaultModelId, ...Object.keys(reasoning.providers.openai.models)]);
  const models = openai.models.filter((model) => requiredIds.has(model.modelId)).map((model) => model.metadata.raw);
  models.push({ id: "gpt-image-2", type: "chat", cost: { input: 7 } });
  await writeFile(join(upstream, "openai.json"), JSON.stringify({ models }));
  execFileSync(process.execPath, [script, "--sync", "openai"], { cwd, env });

  const updated = JSON.parse(await readFile(path, "utf8")).providers.find((provider) => provider.providerId === "openai");
  const image = updated.models.find((model) => model.modelId === "gpt-image-2");
  const original = openai.models.find((model) => model.modelId === image.modelId);
  assert.deepEqual(image.metadata.raw.modalityCapabilities, original.metadata.raw.modalityCapabilities);
  assert.equal(image.metadata.raw.type, original.metadata.raw.type);
  assert.equal(image.metadata.pricing.inputTokenPrice, 7);
  assert.ok(updated.models.some((model) => model.modelId === "tts-1"));
});
