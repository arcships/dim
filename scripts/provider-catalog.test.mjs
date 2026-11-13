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
const modelOverrides = join(resources, "model-overrides.json");

async function checkout(t) {
  const cwd = await mkdtemp(join(tmpdir(), "dim-provider-catalog-"));
  t.after(() => rm(cwd, { recursive: true, force: true }));
  const upstream = join(cwd, "upstream");
  await Promise.all([
    mkdir(join(cwd, "scripts")),
    mkdir(join(cwd, resources), { recursive: true }),
    mkdir(upstream),
  ]);
  await Promise.all([script, bundled, overrides, policy, modelOverrides]
    .map((path) => cp(join(root, path), join(cwd, path))));
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
  const kimiPro = old.models.find((model) => model.modelId === "k3.1-pro").metadata.raw;
  await writeFile(upstreamFile, JSON.stringify({ models: [{ id: "wrong-model" }, kimiPro] }));
  const failed = spawnSync(process.execPath, sync, { cwd, env, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /local defaultModelId .* missing/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

  await writeFile(upstreamFile, JSON.stringify({
    models: [{ id: defaultId, limit: { context: 1.5 } }, kimiPro],
  }));
  const invalidLimit = spawnSync(process.execPath, sync, { cwd, env, encoding: "utf8" });
  assert.equal(invalidLimit.status, 1);
  assert.match(invalidLimit.stderr, /contextWindow must be a positive safe integer/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

  await writeFile(join(upstream, "openrouter.json"), JSON.stringify({ models: [] }));
  const empty = spawnSync(process.execPath, [script, "--sync", "openrouter"], { cwd, env, encoding: "utf8" });
  assert.equal(empty.status, 1);
  assert.match(empty.stderr, /sync would remove every upstream model/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);

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
    ["default-catalog.json", "model-overrides.json", "provider-sync.json", "reasoning-off-overrides.json"]);
  assert.deepEqual(await readdir(join(cwd, "scripts")), ["update-provider-catalog.ts"]);

  execFileSync(process.execPath, sync, { cwd, env });
  assert.equal(await readFile(path, "utf8"), generated);
});

test("a media entry the upstream dropped keeps its bundled canonical id and limits", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const google = previous.providers.find((provider) => provider.providerId === "google");
  const entry = google.models.find((model) => model.modelId === "veo-3.1-generate-preview");
  assert.equal(entry.metadata.raw.canonical_model_id, "google/veo-3.1-generate-preview");
  assert.deepEqual(entry.metadata.raw.limit, { context: 480, output: 8192 });
  await writeFile(join(upstream, "google.json"), JSON.stringify({
    api: google.metadata.raw.api,
    models: google.models
      .filter((model) => model.modelId !== "veo-3.1-generate-preview")
      .map((model) => model.metadata.raw),
  }));
  execFileSync(process.execPath, [script, "--sync", "google"], { cwd, env });

  const updated = JSON.parse(await readFile(path, "utf8"))
    .providers.find((provider) => provider.providerId === "google");
  const kept = updated.models.find((model) => model.modelId === "veo-3.1-generate-preview");
  assert.deepEqual(kept, entry);
  assert.equal(kept.capabilities.contextWindow, 480);
  assert.equal(kept.capabilities.maxOutputTokens, 8192);
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

test("an override merges into the upstream row without losing its other keys", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const kimi = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  const defaultId = kimi.metadata.defaultModelId;
  await writeFile(join(upstream, "kimi-for-coding.json"), JSON.stringify({
    api: kimi.metadata.raw.api,
    models: [
      { id: defaultId },
      { id: "k3.1-pro", limit: { context: 1048576, output: 262144 }, tool_call: false },
    ],
  }));
  execFileSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env });

  const updated = JSON.parse(await readFile(path, "utf8"))
    .providers.find((provider) => provider.providerId === "kimi-for-coding");
  const pro = updated.models.find((model) => model.modelId === "k3.1-pro");
  assert.equal(pro.capabilities.contextWindow, 262144);
  assert.equal(pro.capabilities.maxOutputTokens, 262144);
  assert.equal(pro.metadata.raw.limit.output, 262144);
  assert.equal(pro.metadata.raw.tool_call, false);
});

test("an override that repeats upstream is reported for retirement", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const kimi = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  const defaultId = kimi.metadata.defaultModelId;
  await writeFile(join(upstream, "kimi-for-coding.json"), JSON.stringify({
    api: kimi.metadata.raw.api,
    models: [
      { id: defaultId },
      { id: "k3.1-pro", limit: { context: 262144, output: 131072 } },
    ],
  }));
  const summary = execFileSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env, encoding: "utf8" });
  assert.match(summary, /k3\.1-pro +override repeats upstream; retire it/);
});

test("an override for a model the sync does not produce fails the run", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const initialCatalog = await readFile(path, "utf8");
  const previous = JSON.parse(initialCatalog);
  const kimi = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  await writeFile(join(upstream, "kimi-for-coding.json"), JSON.stringify({
    api: kimi.metadata.raw.api,
    models: [{ id: kimi.metadata.defaultModelId }],
  }));
  const failed = spawnSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /Model overrides target missing models for provider kimi-for-coding: k3\.1-pro/);
  assert.equal(await readFile(path, "utf8"), initialCatalog);
});

test("a keepBundledModels provider keeps its bundle and lands new upstream models fail-closed", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const stepfun = previous.providers.find((provider) => provider.providerId === "stepfun");
  const bundledChat = stepfun.models.find((model) => model.modelId === "step-3.7-flash");
  assert.equal(bundledChat.capabilities.toolCalling, true);
  await writeFile(join(upstream, "stepfun.json"), JSON.stringify({
    api: stepfun.metadata.raw.api,
    models: [
      { id: "step-3.7-flash", limit: { context: 256000, output: 256000 }, reasoning: { supported: true } },
      { id: "step-new-chat", limit: { context: 256000, output: 256000 }, tool_call: true, reasoning: { supported: true } },
    ],
  }));
  execFileSync(process.execPath, [script, "--sync", "stepfun"], { cwd, env });

  const updated = JSON.parse(await readFile(path, "utf8"))
    .providers.find((provider) => provider.providerId === "stepfun");
  const models = new Map(updated.models.map((model) => [model.modelId, model]));
  assert.deepEqual(models.get("step-3.7-flash"), bundledChat);
  const fresh = models.get("step-new-chat");
  assert.equal(fresh.metadata.raw.type, "other");
  assert.equal(fresh.metadata.raw.chat, false);
  assert.equal(fresh.metadata.raw.tool_call, false);
  assert.deepEqual(fresh.metadata.raw.modalities, { input: ["text"], output: ["text"] });
  assert.equal(fresh.capabilities.toolCalling, false);
});

test("a keepBundledModels provider keeps a bundled entry the upstream no longer lists", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const stepfun = previous.providers.find((provider) => provider.providerId === "stepfun");
  const dropped = ["step-2x-large", "step-image-edit-2", "step-tts-mini"];
  const entries = dropped.map((id) => {
    const entry = stepfun.models.find((model) => model.modelId === id);
    assert.notEqual(entry, undefined, `${id} must be bundled for this test`);
    return entry;
  });
  await writeFile(join(upstream, "stepfun.json"), JSON.stringify({
    api: stepfun.metadata.raw.api,
    models: stepfun.models
      .filter((model) => !dropped.includes(model.modelId))
      .map((model) => model.metadata.raw),
  }));
  execFileSync(process.execPath, [script, "--sync", "stepfun"], { cwd, env });

  const updated = JSON.parse(await readFile(path, "utf8"))
    .providers.find((provider) => provider.providerId === "stepfun");
  const models = new Map(updated.models.map((model) => [model.modelId, model]));
  for (const entry of entries) {
    assert.deepEqual(models.get(entry.modelId), entry);
  }
});

test("every model override the table states is applied by a sync", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const declared = JSON.parse(await readFile(join(cwd, modelOverrides), "utf8")).providers;
  const before = JSON.parse(await readFile(path, "utf8"));
  const entries = Object.entries(declared).flatMap(([providerId, config]) =>
    Object.entries(config.models).map(([modelId, patch]) => ({ providerId, modelId, patch })));
  assert.deepEqual(entries.map(({ providerId, modelId }) => `${providerId}/${modelId}`).sort(), [
    "aihubmix/glm-5.3-flashx",
    "aihubmix/laguna-s-2.1",
    "aihubmix/laguna-xs-2.1",
    "aihubmix/ling-3.0-flash",
    "custom-provider/MiniMax-M2.7",
    "kimi-for-coding/k3.1-pro",
    "siliconflow/Qwen/Qwen3-235B-A22B-Thinking-2507",
    "siliconflow/Qwen/Qwen3-Coder-480B-A35B-Instruct",
  ]);

  for (const { providerId } of entries) {
    const provider = before.providers.find((entry) => entry.providerId === providerId);
    const policy = (await policies(cwd))[providerId];
    const source = policy.upstreamId ?? providerId;
    await writeFile(join(upstream, `${source}.json`), JSON.stringify({
      api: provider.metadata.raw.api,
      models: provider.models.map((model) => model.metadata.raw),
    }));
    execFileSync(process.execPath, [script, "--sync", providerId], { cwd, env });
  }

  const after = new Map(JSON.parse(await readFile(path, "utf8")).providers
    .map((provider) => [provider.providerId, provider]));
  for (const { providerId, modelId, patch } of entries) {
    const model = after.get(providerId).models.find((entry) => entry.modelId === modelId);
    assert.notEqual(model, undefined, `${providerId}/${modelId} must survive its sync`);
    for (const [key, value] of Object.entries(patch)) {
      if (key === "limit") {
        assert.equal(model.metadata.raw.limit.context, value.context, `${modelId} context`);
        assert.equal(model.capabilities.contextWindow, value.context, `${modelId} window`);
        if (value.output !== undefined) {
          assert.equal(model.capabilities.maxOutputTokens, value.output, `${modelId} output`);
        }
        continue;
      }
      assert.deepEqual(
        intersect(model.metadata.raw[key] ?? {}, value),
        value,
        `${providerId}/${modelId} ${key}`,
      );
    }
  }
});

/// The part of `actual` a patch claims, at the depth the patch states, so an
/// upstream object that carries more keys still matches.
function intersect(actual, patch) {
  if (actual === null || typeof actual !== "object" || Array.isArray(actual)
    || typeof patch !== "object" || Array.isArray(patch)) {
    return actual;
  }
  return Object.fromEntries(Object.entries(patch).map(([key, value]) => [key, intersect(actual[key], value)]));
}

test("the summary prints an override beside upstream's value", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const kimi = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  await writeFile(join(upstream, "kimi-for-coding.json"), JSON.stringify({
    api: kimi.metadata.raw.api,
    models: [
      { id: kimi.metadata.defaultModelId },
      { id: "k3.1-pro", limit: { context: 1048576, output: 262144 } },
    ],
  }));
  const summary = execFileSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env, encoding: "utf8" });
  assert.match(summary, /\* k3\.1-pro +limit\.context 262144 \(upstream 1048576\)/);
  assert.match(summary, /~ k3\.1-pro .*\(override: limit\.context\)/);
  for (const line of summary.split("\n")) {
    if (line.includes("  ~ ") && !line.includes("k3.1-pro") && line.includes("(override")) {
      assert.fail(`only the k3.1-pro change line may carry an override marker: ${line}`);
    }
  }
  const context = summary.indexOf("limit.context 262144 (upstream 1048576)");
  const local = summary.indexOf("providers kept as written");
  assert.ok(context < local, "the override report comes before the local block");
});

test("an up-to-date run still reports a redundant override", async (t) => {
  const { cwd, env, upstream } = await checkout(t);
  const path = join(cwd, bundled);
  const previous = JSON.parse(await readFile(path, "utf8"));
  const kimi = previous.providers.find((provider) => provider.providerId === "kimi-for-coding");
  await writeFile(join(upstream, "kimi-for-coding.json"), JSON.stringify({
    api: kimi.metadata.raw.api,
    models: [
      { id: kimi.metadata.defaultModelId },
      { id: "k3.1-pro", limit: { context: 262144, output: 262144 } },
    ],
  }));
  const first = execFileSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env, encoding: "utf8" });
  const settled = await readFile(path, "utf8");
  const changed = first.split("\n").find((line) => line.includes("  ~ k3.1-pro"));
  assert.match(changed, /^  ~ k3\.1-pro /);
  assert.doesNotMatch(changed, /\(override/);
  const second = execFileSync(process.execPath, [script, "--sync", "kimi-for-coding"], { cwd, env, encoding: "utf8" });
  assert.match(second, /Catalog is up to date/);
  assert.match(second, /= k3\.1-pro +override repeats upstream; retire it/);
  assert.equal(await readFile(path, "utf8"), settled);
});

test("an override for a local or unknown provider fails the offline run", async (t) => {
  const { cwd, env } = await checkout(t);
  const path = join(cwd, modelOverrides);
  const table = JSON.parse(await readFile(path, "utf8"));
  const declared = await policies(cwd);
  assert.equal(declared["ollama"].source, "local");

  table.providers["ollama"] = { models: { "some-model": { display_name: "Some Model" } } };
  await writeFile(path, JSON.stringify(table, null, 2));
  const local = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(local.status, 1);
  assert.match(local.stderr, /Model override provider ollama has no remote sync policy/);

  delete table.providers["ollama"];
  table.providers["not-a-provider"] = { models: { "some-model": { display_name: "Some Model" } } };
  await writeFile(path, JSON.stringify(table, null, 2));
  const unknown = spawnSync(process.execPath, [script], { cwd, env, encoding: "utf8" });
  assert.equal(unknown.status, 1);
  assert.match(unknown.stderr, /Model override provider not-a-provider is not a bundled provider/);
});
