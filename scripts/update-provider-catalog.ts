#!/usr/bin/env node
import { randomUUID } from "node:crypto"
import { readFile, rename, unlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { isDeepStrictEqual } from "node:util"

const resources = join(import.meta.dirname, "../crates/dim-app-server/resources")
const catalogOutput = join(resources, "default-catalog.json")
const reasoningOffOverridesFile = join(resources, "reasoning-off-overrides.json")
const syncPolicyFile = join(resources, "provider-sync.json")
const modelOverridesFile = join(resources, "model-overrides.json")
const remoteBase = process.env.DIM_PROVIDER_REMOTE_BASE ?? "https://raw.githubusercontent.com/ThinkInAIXYZ/PublicProviderConf/refs/heads/dev/dist"

type ProviderPolicy = {
  source: "remote" | "local"
  reason?: string
  upstreamId?: string
  keepBaseUrl?: boolean
  keepDefaultModel?: boolean
  localModels?: string[]
  mediaModels?: string[]
  keepBundledModels?: boolean
  failClosedUpstreamModels?: boolean
}

/// Every bundled provider has a policy: `remote` ones are refreshed from
/// PublicProviderConf, `local` ones are maintained in this repository and a
/// sync never touches them. The reasons are facts read by a reviewer, and the
/// coverage check below refuses a catalog and a table that disagree.
async function loadPolicies(catalog): Promise<Record<string, ProviderPolicy>> {
  const file = await readJson(syncPolicyFile)
  const providers = isRecord(file?.providers) ? file.providers : undefined
  if (providers === undefined) {
    throw new Error(`${syncPolicyFile}: expected a providers object`)
  }
  const listed = new Set(catalog.providers.map((provider) => provider.providerId))
  for (const id of Object.keys(providers)) {
    if (!listed.has(id)) throw new Error(`Provider ${id} has a sync policy but the catalog does not list it`)
  }
  for (const id of listed) {
    if (!Object.hasOwn(providers, id)) throw new Error(`Provider ${id} has no sync policy`)
    validatePolicy(id, providers[id])
  }
  for (const provider of catalog.providers) {
    const expected = sourceFor(providers[provider.providerId].source)
    if (provider.metadata.source !== expected) {
      throw new Error(`Provider ${provider.providerId}: metadata.source is ${provider.metadata.source ?? "absent"}, and its policy makes it ${expected}`)
    }
  }
  return providers
}

/// The `metadata.source` a policy authors. A provider is refreshed from the
/// public upstream or maintained in this repository, and the column says which.
function sourceFor(source) {
  return source === "remote" ? "public-provider-conf" : "local"
}

function validatePolicy(id, policy) {
  if (!isRecord(policy)) throw new Error(`Provider ${id}: sync policy must be an object`)
  const unknown = Object.keys(policy).filter((key) => !POLICY_FIELDS.includes(key))
  if (unknown.length > 0) {
    throw new Error(`Provider ${id}: unknown sync policy field ${unknown.join(", ")}`)
  }
  if (policy.source !== "remote" && policy.source !== "local") {
    throw new Error(`Provider ${id}: sync policy source must be remote or local`)
  }
  if (policy.source === "local") {
    if (normalizeString(policy.reason) === undefined) {
      throw new Error(`Provider ${id}: a local provider needs a reason`)
    }
  } else if (policy.reason !== undefined) {
    throw new Error(`Provider ${id}: only a local provider carries a reason`)
  }
  for (const key of ["keepBaseUrl", "keepDefaultModel", "keepBundledModels", "failClosedUpstreamModels"]) {
    if (policy[key] !== undefined && policy[key] !== true) {
      throw new Error(`Provider ${id}: ${key} is either absent or true`)
    }
  }
  if (policy.upstreamId !== undefined && normalizeString(policy.upstreamId) === undefined) {
    throw new Error(`Provider ${id}: upstreamId must be a non-empty string`)
  }
  for (const key of ["localModels", "mediaModels"]) {
    if (policy[key] !== undefined && stringArray(policy[key]) === undefined) {
      throw new Error(`Provider ${id}: ${key} must be a non-empty string array`)
    }
  }
}

const POLICY_FIELDS = [
  "source",
  "reason",
  "upstreamId",
  "keepBaseUrl",
  "keepDefaultModel",
  "localModels",
  "mediaModels",
  "keepBundledModels",
  "failClosedUpstreamModels",
]

async function main() {
  const [command, providerId, ...extra] = process.argv.slice(2)
  if (extra.length || (command !== undefined && command !== "--sync")) {
    throw new Error("Usage: node scripts/update-provider-catalog.ts [--sync [provider-id]]")
  }

  const previous = await readFile(catalogOutput, "utf8")
  const catalog = JSON.parse(previous)
  validateCatalog(catalog)
  const policies = await loadPolicies(catalog)
  const remote = Object.keys(policies).filter((id) => policies[id].source === "remote")
  const overrides = await loadModelOverrides(policies)
  if (command === undefined) {
    console.log(`Validated catalog: ${catalogOutput}`)
    return
  }

  const ids = new Set(catalog.providers.map((provider) => provider.providerId))
  const requested = providerId === undefined ? remote : [providerId]
  for (const id of requested) {
    if (!ids.has(id)) throw new Error(`Unknown provider: ${id}`)
    if (policies[id]?.source !== "remote") throw new Error(`Provider ${id} has no remote catalog source`)
  }

  const selected = new Set(requested)
  const reasoningOff = createReasoningOffOverrideState(await readJson(reasoningOffOverridesFile), policies)
  const applied = loadSelectedOverrides(overrides, selected)
  const notes = []
  const kept = []
  const providers = []
  for (const provider of catalog.providers) {
    providers.push(selected.has(provider.providerId)
      ? await syncProvider(provider, policies[provider.providerId], reasoningOff.get(provider.providerId), applied, notes, kept)
      : provider)
  }
  for (const [providerId, pending] of applied) {
    if (pending.size > 0) {
      throw new Error(`Model overrides target missing models for provider ${providerId}: ${[...pending.keys()].join(", ")}`)
    }
  }
  const report = [...overrideReport(notes), ...keptReport(kept)]
  if (isDeepStrictEqual(providers, catalog.providers)) {
    console.log("Catalog is up to date")
    if (report.length > 0) console.log(report.join("\n"))
    return
  }
  const candidate = { ...catalog, updatedAt: new Date().toISOString(), providers }
  validateCatalog(candidate)
  console.log(summarize(catalog.providers, providers, policies, selected, notes, report))
  await writeCatalog(previous, candidate)
  console.log(`Updated catalog: ${requested.join(", ")}`)
}

async function syncProvider(provider, policy: ProviderPolicy, reasoningOff, patches, notes, kept) {
  const id = provider.providerId
  const raw = { ...provider.metadata.raw }
  const sourceId = policy.upstreamId ?? id
  const remote = await readRemoteJson(`${remoteBase}/${encodeURIComponent(sourceId)}.json`)
  if (!Array.isArray(remote?.models)) throw new Error(`${id}: expected a remote models array`)
  assertUniqueModelIds(remote.models, id, "id")

  const previous = new Map(provider.models.map((model) => [model.modelId, model.metadata.raw]))
  const local = (modelId: string) => {
    const model = previous.get(modelId)
    if (model === undefined) throw new Error(`Provider ${id}: missing local model ${modelId}`)
    return model
  }
  const upstream = failClosed(policy, applyModelOverrides(id, fetchUpstream(policy, remote.models, previous), patchFor(id, patches), notes), previous)
  if (upstream.length === 0 && provider.models.some(({ modelId }) =>
    !policy.localModels?.includes(modelId) && !policy.mediaModels?.includes(modelId)
  )) {
    throw new Error(`Provider ${id}: sync would remove every upstream model`)
  }
  const models = new Map(upstream.map((model) => [model.id.trim(), model]))
  for (const modelId of policy.localModels ?? []) models.set(modelId, local(modelId))
  let defaultModelId = provider.metadata.defaultModelId
  if (policy.keepDefaultModel) {
    if (!models.has(defaultModelId)) {
      throw new Error(`Provider ${id}: local defaultModelId ${defaultModelId} is missing from merged models`)
    }
  } else {
    defaultModelId = resolveDefaultModelId([...models.values()]) || undefined
    if (defaultModelId === undefined) delete raw.defaultModelId
    else raw.defaultModelId = defaultModelId
  }

  for (const modelId of policy.mediaModels ?? []) {
    models.set(modelId, mediaOverlay(id, local(modelId), models.get(modelId)))
  }
  const metadata = { ...provider.metadata, defaultModelId, raw }
  if (defaultModelId === undefined) delete metadata.defaultModelId
  const rows = carryCanonicalIds(id, [...models.values()], previous, kept)
  const result = { ...provider, models: applyReasoningOffOverrides(id, rows, reasoningOff).map(toCatalogModel), metadata }
  if (!policy.keepBaseUrl) {
    const api = normalizeProviderApi(remote.api, metadata.adapter)
    if (api !== undefined) {
      raw.api = api
      result.defaultBaseUrl = api
    }
  }
  return result
}

/// The upstream rows a sync starts from, with the filtered entries removed.
/// `keepBundledModels` keeps the bundled row wherever one exists, and adds an
/// upstream row the bundle lacks.
function fetchUpstream(policy, models, previous) {
  const usable = models.filter((model) => !isFilteredModel(model))
  if (!policy.keepBundledModels) return usable
  return usable.map((model) => previous.get(model.id.trim()) ?? model)
}

/// Where a `keepBundledModels` provider gets a model the bundle lacks, its
/// upstream row is kept but declared unusable: no chat, no tools, text only.
function failClosed(policy, models, previous) {
  if (!policy.failClosedUpstreamModels) return models
  return models.map((model) => previous.has(model.id.trim()) ? model : {
    ...model,
    type: "other",
    chat: false,
    tool_call: false,
    structured_output: false,
    modalities: { input: ["text"], output: ["text"] },
  })
}

function patchFor(providerId, patches) {
  const providerPatches = patches.get(providerId)
  return providerPatches ?? new Map()
}

/// A patch merges into the upstream row field by field. Objects merge key by
/// key, so a `limit` patch keeps the output cap upstream states; scalars and
/// arrays replace the upstream value. Every leaf the patch sets is recorded
/// with upstream's own value, so a reviewer sees what the table overrides and
/// when upstream has caught up with it.
function applyModelOverrides(providerId, models, patches, notes) {
  if (patches.size === 0) return models
  return models.map((model) => {
    const id = model.id.trim()
    const patch = patches.get(id)
    if (patch === undefined) return model
    patches.delete(id)
    const differences = []
    collectDifferences(model, patch, [], differences)
    notes.push({ providerId, modelId: id, differences })
    return mergePatch(model, patch)
  })
}

/// Every scalar or array the patch sets that upstream states differently, as a
/// dotted path with both values. A key upstream lacks counts as differing.
function collectDifferences(upstream, patch, path, differences) {
  for (const [key, value] of Object.entries(patch)) {
    const next = [...path, key]
    const current = isRecord(upstream) ? upstream[key] : undefined
    if (isRecord(value)) {
      collectDifferences(current, value, next, differences)
    } else if (!isDeepStrictEqual(current, value)) {
      differences.push({ path: next.join("."), patched: value, upstream: current })
    }
  }
}

function mergePatch(current, patch) {
  if (!isRecord(current) || !isRecord(patch)) return patch
  const merged = { ...current }
  for (const [key, value] of Object.entries(patch)) {
    merged[key] = isRecord(merged[key]) && isRecord(value)
      ? mergePatch(merged[key], value)
      : value
  }
  return merged
}

/// Every model override, checked on every run, offline included: a patch for a
/// provider the catalog lacks or one whose policy is `local` would never be
/// applied by a sync, so it fails here by name instead of being ignored.
async function loadModelOverrides(policies) {
  const file = await readJson(modelOverridesFile)
  const providers = isRecord(file?.providers) ? file.providers : {}
  const state = new Map()
  for (const [providerId, providerConfig] of Object.entries(providers)) {
    if (!isRecord(providerConfig)) {
      throw new Error(`Model override provider ${providerId} must be an object`)
    }
    if (policies[providerId] === undefined) {
      throw new Error(`Model override provider ${providerId} is not a bundled provider`)
    }
    if (policies[providerId].source !== "remote") {
      throw new Error(`Model override provider ${providerId} has no remote sync policy`)
    }
    const models = isRecord(providerConfig.models) ? providerConfig.models : undefined
    if (models === undefined) {
      throw new Error(`Model override provider ${providerId} models must be an object`)
    }
    const providerModels = new Map()
    for (const [modelId, patch] of Object.entries(models)) {
      if (!isRecord(patch) || Object.keys(patch).length === 0) {
        throw new Error(`Model override ${providerId}/${modelId} must be a non-empty object`)
      }
      providerModels.set(modelId, patch)
    }
    state.set(providerId, providerModels)
  }
  return state
}

/// The copy a run applies its patches to, so the table a later report reads is
/// the file, not what one sync consumed.
function loadSelectedOverrides(overrides, synced) {
  const state = new Map()
  for (const [providerId, providerModels] of overrides) {
    if (synced.has(providerId)) state.set(providerId, new Map(providerModels))
  }
  return state
}

const SUMMARY_FIELDS = [
  ["contextWindow", "context"],
  ["maxOutputTokens", "output"],
]

/// What a reviewer reads instead of the catalog diff. The changes a reviewer
/// must judge come first: the model counts, the curated capability lines, and
/// then every other changed leaf. Then every override with upstream's own value
/// beside it, and the local providers last, since nothing there changed. A
/// provider counts as unchanged only when its whole entry is equal, so a change
/// outside the curated fields cannot hide in the count.
function summarize(before, after, policies, synced, notes, report) {
  const changed = []
  let changedProviders = 0
  for (const provider of after) {
    if (!synced.has(provider.providerId)) continue
    const old = before.find((entry) => entry.providerId === provider.providerId)
    if (isDeepStrictEqual(old, provider)) continue
    const oldModels = new Map(old.models.map((model) => [model.modelId, model]))
    const newModels = new Map(provider.models.map((model) => [model.modelId, model]))
    const added = [...newModels.keys()].filter((id) => !oldModels.has(id))
    const removed = [...oldModels.keys()].filter((id) => !newModels.has(id))
    const details = []
    let changedModels = 0
    for (const [id, model] of newModels) {
      const previous = oldModels.get(id)
      if (previous === undefined) continue
      const fields = changedFields(previous, model)
      if (fields.length === 0) continue
      changedModels += 1
      const note = notes.find((entry) => entry.providerId === provider.providerId && entry.modelId === id)
      const leaves = note?.differences.length
        ? `  (override: ${note.differences.map((difference) => difference.path).join(", ")})`
        : ""
      details.push(`  ~ ${pad(id)} ${fields.join(", ")}${leaves}`)
    }
    const oldDefault = old.metadata.defaultModelId
    const newDefault = provider.metadata.defaultModelId
    if (oldDefault !== newDefault) {
      details.push(`  ~ defaultModelId ${oldDefault ?? "none"} -> ${newDefault ?? "none"}`)
    }
    changedProviders += 1
    changed.push(`${pad(provider.providerId, 24)} +${added.length} -${removed.length} ~${changedModels}`)
    if (added.length > 0) changed.push(`  + ${listPreview(added)}`)
    if (removed.length > 0) changed.push(`  - ${listPreview(removed)}`)
    changed.push(...details)
    for (const line of leafLines(old, provider, oldModels, newModels)) changed.push(line)
  }
  const locals = Object.entries(policies).filter(([, policy]) => policy.source === "local")
  const head = [`Synced ${synced.size} providers (${synced.size - changedProviders} unchanged, ${changedProviders} changed)`]
  const tail = [`local: ${locals.length} providers kept as written`]
  for (const [id, policy] of locals) tail.push(`  ${pad(id)} ${policy.reason}`)
  return [...head, "", ...changed, "", ...report, "", ...tail].join("\n")
}

/// Every leaf the curated lines above did not print, so the summary carries a
/// change wherever it happens. A model only in the new or the old entry is
/// already named by its `+` or `-` line, and its entry is not walked.
function leafLines(before, after, oldModels, newModels) {
  const lines = []
  for (const leaf of changedLeaves(entryWithoutModels(before), entryWithoutModels(after), PROVIDER_REPORTED_LEAVES)) {
    lines.push(`  ~ ${leaf.path} ${leafValue(leaf.before)} -> ${leafValue(leaf.after)}`)
  }
  for (const id of [...newModels.keys()].filter((modelId) => oldModels.has(modelId)).sort()) {
    for (const leaf of changedLeaves(oldModels.get(id), newModels.get(id), MODEL_REPORTED_LEAVES)) {
      lines.push(`  ~ ${pad(id)} ${leaf.path} ${leafValue(leaf.before)} -> ${leafValue(leaf.after)}`)
    }
  }
  return lines
}

/// The leaves the lines above already report, so the leaf list adds what is
/// left rather than repeating them. This is not the unchanged test: that one
/// compares the whole entry.
const MODEL_REPORTED_LEAVES = new Set([
  ...SUMMARY_FIELDS.map(([key]) => `capabilities.${key}`),
  "status",
  ...["reasoning", "toolCalling", "vision"].map((key) => `capabilities.${key}`),
  "metadata.reasoning.effortOptions",
])

const PROVIDER_REPORTED_LEAVES = new Set(["metadata.defaultModelId", "metadata.raw.defaultModelId"])

function entryWithoutModels(provider) {
  const entry = { ...provider }
  delete entry.models
  return entry
}

/// The leaves of two values, by path. An array is one leaf: its indices are not
/// a stable path, and a reviewer reads the whole list either way.
function changedLeaves(before, after, reported) {
  const olds = leafValues(before, [], new Map())
  const news = leafValues(after, [], new Map())
  const leaves = []
  for (const key of new Set([...olds.keys(), ...news.keys()])) {
    const path = olds.get(key)?.path ?? news.get(key).path
    if (reported.has(path.join("."))) continue
    if (isDeepStrictEqual(olds.get(key)?.value, news.get(key)?.value)) continue
    leaves.push({ path: path.join("."), before: olds.get(key)?.value, after: news.get(key)?.value })
  }
  return leaves.sort((a, b) => a.path.localeCompare(b.path))
}

/// A key of its own is not a safe path segment, since a catalog key may contain
/// the separator; the JSON encoding of the whole path is.
function leafValues(value, path, leaves) {
  if (isRecord(value)) {
    for (const [key, item] of Object.entries(value)) leafValues(item, [...path, key], leaves)
    return leaves
  }
  leaves.set(JSON.stringify(path), { path, value })
  return leaves
}

const LEAF_VALUE_WIDTH = 80

function leafValue(value) {
  if (value === undefined) return "(absent)"
  const encoded = JSON.stringify(value)
  return encoded.length > LEAF_VALUE_WIDTH ? `${encoded.slice(0, LEAF_VALUE_WIDTH - 1)}…` : encoded
}

/// Every override the run applied, with the value upstream states beside the
/// patched one. A patch whose leaves all equal upstream says so, so a reviewer
/// can retire it with evidence. This prints on an up-to-date run too.
function overrideReport(notes) {
  if (notes.length === 0) return []
  const lines = []
  let providerId
  for (const note of notes) {
    if (note.providerId !== providerId) {
      providerId = note.providerId
      lines.push(`${pad(providerId, 24)} overrides`)
    }
    if (note.differences.length === 0) {
      lines.push(`  = ${pad(note.modelId)} override repeats upstream; retire it`)
      continue
    }
    for (const difference of note.differences) {
      lines.push(`  * ${pad(note.modelId)} ${difference.path} ${show(difference.patched)} (upstream ${show(difference.upstream)})`)
    }
  }
  return lines
}

function show(value) {
  if (value === undefined) return "absent"
  return JSON.stringify(value)
}

function changedFields(before, after) {
  const fields = []
  for (const [key, label] of SUMMARY_FIELDS) {
    const a = before.capabilities?.[key]
    const b = after.capabilities?.[key]
    if (a !== b) fields.push(`${label} ${a ?? "none"} -> ${b ?? "none"}`)
  }
  if (before.status !== after.status) fields.push(`status ${before.status} -> ${after.status}`)
  for (const key of ["reasoning", "toolCalling", "vision"]) {
    const a = before.capabilities?.[key]
    const b = after.capabilities?.[key]
    if (a !== b) fields.push(`${key} ${a} -> ${b}`)
  }
  const a = before.metadata?.reasoning?.effortOptions
  const b = after.metadata?.reasoning?.effortOptions
  if (JSON.stringify(a) !== JSON.stringify(b)) fields.push(`effortOptions ${showOptions(a)} -> ${showOptions(b)}`)
  return fields
}

function showOptions(value) {
  return Array.isArray(value) ? value.join(",") : "none"
}

function listPreview(ids) {
  return ids.length <= 6 ? ids.join(", ") : `${ids.slice(0, 6).join(", ")}, ... (${ids.length})`
}

function pad(text, width = 32) {
  return text.length >= width ? text : `${text}${" ".repeat(width - text.length)}`
}

/// The bundled entry's media declaration laid over the upstream row. When the
/// upstream no longer lists the model, the bundled row is the only source, so
/// the merge starts from it and takes the fields the overlay overrides; that
/// keeps `canonical_model_id` and the entry's limits instead of falling back to
/// the assumed pair.
function mediaOverlay(providerId, bundled, upstream) {
  const fields = ["id", "display_name", "type", "modalities", "modalityCapabilities"]
  if (providerId !== "google" || bundled.id !== "gemini-3.5-flash") {
    fields.push("tool_call", "structured_output")
  }
  const declared = Object.fromEntries(fields.filter((key) => Object.hasOwn(bundled, key)).map((key) => [key, bundled[key]]))
  const base = isRecord(upstream) ? upstream : bundled
  return mergePatch(base, declared)
}

/// `canonical_model_id` is not upstream's to omit: the matching step reads it,
/// so a surviving row keeps the bundled value when upstream states none. An
/// upstream value wins, and a kept one is reported so a reviewer can retire it
/// by removing it from the bundled entry by hand.
function carryCanonicalIds(providerId, models, previous, kept) {
  return models.map((model) => {
    const id = model.id.trim()
    const bundled = previous.get(id)?.canonical_model_id
    if (bundled === undefined) return model
    const value = model.canonical_model_id
    if (value !== undefined) return model
    kept.push({ providerId, modelId: id, canonicalId: bundled })
    return { ...model, canonical_model_id: bundled }
  })
}

/// Every canonical id a sync carried over because upstream omitted it, so the
/// summary says which entry a reviewer has to edit to let go of one.
function keptReport(kept) {
  if (kept.length === 0) return []
  const lines = []
  let providerId
  for (const note of kept) {
    if (note.providerId !== providerId) {
      providerId = note.providerId
      lines.push(`${pad(providerId, 24)} carried`)
    }
    lines.push(`  = ${pad(note.modelId)} canonical_model_id ${note.canonicalId} kept; upstream omits it`)
  }
  return lines
}

function createReasoningOffOverrideState(overrides, policies) {
  const providers = isRecord(overrides?.providers) ? overrides.providers : {}
  const pending = new Map()
  for (const [providerId, providerConfig] of Object.entries(providers)) {
    if (!isRecord(providerConfig)) {
      throw new Error(`Reasoning off override provider ${providerId} must be an object`)
    }
    const models = isRecord(providerConfig.models) ? providerConfig.models : undefined
    if (models === undefined) {
      throw new Error(`Reasoning off override provider ${providerId} models must be an object`)
    }
    const providerModels = new Map()
    for (const [modelId, modelConfig] of Object.entries(models)) {
      providerModels.set(modelId, validateReasoningOffOverride(providerId, modelId, modelConfig))
    }
    if (policies[providerId]?.source === "remote") pending.set(providerId, providerModels)
  }
  return pending
}

function applyReasoningOffOverrides(providerId, models, overrides) {
  if (overrides === undefined) return models
  const patched = models.map((model) => {
    const id = model.id.trim()
    const override = overrides.get(id)
    if (override === undefined) return model
    overrides.delete(id)
    return applyReasoningOffOverride(model, override)
  })
  if (overrides.size > 0) {
    throw new Error(`Reasoning off overrides target missing models for provider ${providerId}: ${[...overrides.keys()].join(", ")}`)
  }
  return patched
}

function applyReasoningOffOverride(model, override) {
  const currentReasoning = model.reasoning === true || model.reasoning === false
    ? { supported: model.reasoning }
    : isRecord(model.reasoning) ? model.reasoning : {}
  const currentExtraReasoning = isRecord(model.extra_capabilities?.reasoning)
    ? model.extra_capabilities.reasoning
    : {}
  const reasoning = { ...currentReasoning }
  const extraReasoning = { ...currentExtraReasoning }
  if (override.effortOptions !== undefined) {
    const currentOptions = stringArray(reasoning.effortOptions ?? reasoning.effort_options) ?? []
    const currentExtraOptions = stringArray(extraReasoning.effortOptions ?? extraReasoning.effort_options) ?? []
    reasoning.effortOptions = mergeReasoningEffortOptions(currentOptions, override.effortOptions)
    extraReasoning.effort_options = mergeReasoningEffortOptions(currentExtraOptions, override.effortOptions)
  }
  if (override.off !== undefined) {
    reasoning.off = override.off
    extraReasoning.off = override.off
  }
  return {
    ...model,
    reasoning,
    extra_capabilities: {
      ...(model.extra_capabilities ?? {}),
      reasoning: extraReasoning,
    },
  }
}

function mergeReasoningEffortOptions(currentOptions, patchOptions) {
  const result = [...patchOptions]
  for (const option of currentOptions) {
    if (!result.includes(option)) result.push(option)
  }
  return result
}

function validateReasoningOffOverride(providerId, modelId, value) {
  if (!isRecord(value)) {
    throw new Error(`Reasoning off override ${providerId}/${modelId} must be an object`)
  }
  if (!isRecord(value.reasoning)) {
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning must be an object`)
  }
  const keys = Object.keys(value.reasoning)
  for (const key of keys) {
    if (key !== "off" && key !== "effortOptions") {
      throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.${key} is not allowed`)
    }
  }
  const effortOptions = stringArray(value.reasoning.effortOptions)
  if (value.reasoning.effortOptions !== undefined && effortOptions === undefined) {
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.effortOptions must be a non-empty string array`)
  }
  if (effortOptions !== undefined && value.reasoning.off === undefined && !effortOptions.includes("none")) {
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.effortOptions must include none`)
  }
  return {
    effortOptions,
    off: validateReasoningOffDescriptor(providerId, modelId, value.reasoning.off),
  }
}

function validateReasoningOffDescriptor(providerId, modelId, value) {
  if (value === undefined) return undefined
  if (!isRecord(value)) {
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off must be an object`)
  }
  const kind = normalizeString(value.kind)
  if (kind === "thinking_type") {
    if (value.value !== "disabled") throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.value must be disabled`)
    return { kind, value: "disabled" }
  }
  if (kind === "thinking_budget") {
    if (value.value !== 0) throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.value must be 0`)
    return { kind, value: 0 }
  }
  if (kind === "enable_thinking") {
    if (value.value !== false) throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.value must be false`)
    return { kind, value: false }
  }
  if (kind === "chat_template_kwargs") {
    return { kind, keys: validateChatTemplateOffKeys(providerId, modelId, value.keys) }
  }
  throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.kind is unsupported`)
}

function validateChatTemplateOffKeys(providerId, modelId, value) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.keys must be a non-empty array`)
  }
  return value.map((key) => {
    if (key === "thinking" || key === "enable_thinking") return key
    throw new Error(`Reasoning off override ${providerId}/${modelId}.reasoning.off.keys contains unsupported key`)
  })
}

/// A provider with bundled models states whether it serves its own vendor's
/// models or resells other vendors'. The question has no answer for a provider
/// whose model list comes from the user or the endpoint, so those carry none.
function validateRole(id, metadata, models) {
  const role = metadata.role
  if (role !== undefined && role !== "vendor" && role !== "aggregator") {
    throw new Error(`Provider ${id}: role must be vendor or aggregator`)
  }
  if (models.length > 0 && role === undefined) {
    throw new Error(`Provider ${id}: a provider with bundled models needs a role`)
  }
  if (models.length === 0 && role !== undefined) {
    throw new Error(`Provider ${id}: a provider without bundled models carries no role`)
  }
}

function validateCatalog(catalog) {
  if (!Array.isArray(catalog?.providers) || typeof catalog.updatedAt !== "string" || !Number.isFinite(Date.parse(catalog.updatedAt))) {
    throw new Error("Catalog requires an updatedAt timestamp and a providers array")
  }
  const ids = new Set()
  for (const { providerId, metadata, models } of catalog.providers) {
    const id = normalizeRequiredString(providerId, "provider id")
    if (ids.has(id)) throw new Error(`Duplicate provider: ${id}`)
    ids.add(id)
    if (id === "dimcode-api-oauth") throw new Error("The managed provider owns its catalog")
    if (!isRecord(metadata?.raw) || !Array.isArray(models)) throw new Error(`Provider ${id} requires raw metadata and a models array`)
    if (metadata.lookupOnly && metadata.customModels) throw new Error(`Provider ${id}: model ownership is ambiguous`)
    validateRole(id, metadata, models)
    assertUniqueModelIds(models, id, "modelId")
    for (const { modelId, capabilities } of models) {
      for (const key of ["contextWindow", "maxOutputTokens"]) {
        const limit = capabilities?.[key]
        if (limit !== undefined && (!Number.isSafeInteger(limit) || limit < 1)) {
          throw new Error(`Provider ${id}: model ${modelId} ${key} must be a positive safe integer`)
        }
      }
    }
  }
}

function assertUniqueModelIds(models, providerId, idField) {
  const ids = new Set()
  for (const model of models) {
    const id = normalizeRequiredString(model?.[idField], `provider ${providerId} model id`)
    if (ids.has(id)) throw new Error(`Provider ${providerId}: duplicate model ${id}`)
    ids.add(id)
  }
}

function toCatalogModel(raw) {
  const modelId = normalizeRequiredString(raw?.id, "model id")
  const limit = isRecord(raw?.limit) ? raw.limit : {}
  const contextWindow = positiveNumber(limit.context) ?? positiveNumber(raw?.contextWindow) ?? 128000
  const maxOutputTokens = positiveNumber(limit.output) ?? positiveNumber(raw?.maxOutputTokens) ?? 16384
  const inputModalities = Array.isArray(raw?.modalities?.input) ? raw.modalities.input.map((item) => String(item)) : []
  const vision = raw?.vision === true || inputModalities.includes("image")
  const audio = inputModalities.includes("audio")
  const modalities = ["text"]
  if (vision) modalities.push("image")
  if (audio) modalities.push("audio")
  if (inputModalities.includes("video")) modalities.push("video")
  const reasoningMetadata = reasoningMetadataFromModel(raw)
  const serviceTierMetadata = serviceTierMetadataFromModel(raw)
  const reasoning = reasoningMetadata?.supported === true || reasoningMetadata?.interleaved === true

  return {
    modelId,
    displayName: normalizeString(raw?.display_name) ?? normalizeString(raw?.name) ?? modelId,
    status: catalogModelStatus(raw),
    capabilities: {
      modalities: [...new Set(modalities)],
      toolCalling: raw?.tool_call !== false,
      streaming: true,
      structuredOutput: raw?.structured_output !== false,
      vision,
      audio,
      reasoning,
      contextWindow,
      maxOutputTokens,
    },
    metadata: compactRecord({
      description: normalizeString(raw?.name),
      releaseDate: normalizeString(raw?.release_date),
      pricing: pricingFromCost(raw?.cost),
      reasoning: reasoningMetadata,
      serviceTier: serviceTierMetadata,
      raw,
    }),
  }
}

function catalogModelStatus(raw) {
  // Upstream carries retirement on `status`; a few providers only set
  // `lifecycle`. Read both so a deprecated model never ships as active.
  const value = normalizeString(raw?.status) ?? normalizeString(raw?.lifecycle)
  if (value === "beta" || value === "preview") return "beta"
  if (value === "deprecated" || value === "disabled") return "deprecated"
  return "active"
}

function reasoningMetadataFromModel(raw) {
  const sourceReasoning = typeof raw?.reasoning === "boolean"
    ? { supported: raw.reasoning }
    : isRecord(raw?.reasoning) ? raw.reasoning : undefined
  const extraReasoning = isRecord(raw?.extra_capabilities?.reasoning)
    ? raw.extra_capabilities.reasoning
    : undefined
  const reasoningOptions = reasoningOptionsMetadata(raw?.reasoning_options)
  if (sourceReasoning === undefined && extraReasoning === undefined && reasoningOptions === undefined) return undefined

  const effortOptions = stringArray(reasoningField(sourceReasoning, extraReasoning, "effort_options", "effortOptions"))
    ?? reasoningOptions?.effortOptions
  const levelOptions = stringArray(reasoningField(sourceReasoning, extraReasoning, "level_options", "levelOptions"))
  const verbosityOptions = stringArray(reasoningField(sourceReasoning, extraReasoning, "verbosity_options", "verbosityOptions"))
  const interleaved = booleanValue(reasoningField(sourceReasoning, extraReasoning, "interleaved"))
  const off = reasoningOffMetadata(reasoningField(sourceReasoning, extraReasoning, "off"))
  const supported = booleanValue(extraReasoning?.supported)
    ?? booleanValue(sourceReasoning?.supported)
    ?? (interleaved === true || effortOptions || levelOptions || off ? true : undefined)
  const reasoning = compactRecord({
    supported,
    defaultEnabled: booleanValue(reasoningField(sourceReasoning, extraReasoning, "default_enabled", "defaultEnabled"))
      ?? booleanValue(reasoningField(sourceReasoning, extraReasoning, "default")),
    mode: reasoningMode(reasoningField(sourceReasoning, extraReasoning, "mode")),
    effort: normalizeString(reasoningField(sourceReasoning, extraReasoning, "effort")),
    effortOptions,
    level: normalizeString(reasoningField(sourceReasoning, extraReasoning, "level")),
    levelOptions,
    verbosity: normalizeString(reasoningField(sourceReasoning, extraReasoning, "verbosity")),
    verbosityOptions,
    interleaved,
    summaries: booleanValue(reasoningField(sourceReasoning, extraReasoning, "summaries")),
    visibility: reasoningVisibility(reasoningField(sourceReasoning, extraReasoning, "visibility")),
    continuation: stringArray(reasoningField(sourceReasoning, extraReasoning, "continuation")),
    budget: reasoningBudget(reasoningField(sourceReasoning, extraReasoning, "budget")),
    notes: stringArray(reasoningField(sourceReasoning, extraReasoning, "notes")),
    off,
  })
  if (reasoning.supported === undefined && interleaved === true) {
    reasoning.supported = true
  }
  return reasoning.supported === undefined ? undefined : reasoning
}

function reasoningOptionsMetadata(value) {
  if (!Array.isArray(value)) return undefined
  const effort = value.find((item) => isRecord(item) && item.type === "effort")
  const effortOptions = stringArray(effort?.values)
  return effortOptions === undefined ? undefined : { effortOptions }
}

function reasoningField(sourceReasoning, extraReasoning, snakeKey, camelKey = snakeKey) {
  if (extraReasoning?.[snakeKey] !== undefined) return extraReasoning[snakeKey]
  if (extraReasoning?.[camelKey] !== undefined) return extraReasoning[camelKey]
  if (sourceReasoning?.[snakeKey] !== undefined) return sourceReasoning[snakeKey]
  return sourceReasoning?.[camelKey]
}

function reasoningMode(value) {
  const normalized = normalizeString(value)
  if (
    normalized === "effort"
    || normalized === "fixed"
    || normalized === "mixed"
    || normalized === "budget"
    || normalized === "level"
  ) {
    return normalized
  }
  return undefined
}

function reasoningVisibility(value) {
  const normalized = normalizeString(value)
  if (
    normalized === "hidden"
    || normalized === "summary"
    || normalized === "full"
    || normalized === "omitted"
  ) {
    return normalized
  }
  return undefined
}

function reasoningBudget(value) {
  if (!isRecord(value)) return undefined
  const budget = compactRecord({
    min: numberValue(value.min),
    max: numberValue(value.max),
    default: numberValue(value.default),
    unit: normalizeString(value.unit),
  })
  return Object.keys(budget).length === 0 ? undefined : budget
}

function reasoningOffMetadata(value) {
  if (value === undefined) return undefined
  if (!isRecord(value)) {
    throw new Error("reasoning.off must be an object")
  }
  const kind = normalizeString(value.kind)
  if (kind === "thinking_type") {
    if (value.value !== "disabled") throw new Error("reasoning.off.value must be disabled")
    return { kind, value: "disabled" }
  }
  if (kind === "thinking_budget") {
    if (value.value !== 0) throw new Error("reasoning.off.value must be 0")
    return { kind, value: 0 }
  }
  if (kind === "enable_thinking") {
    if (value.value !== false) throw new Error("reasoning.off.value must be false")
    return { kind, value: false }
  }
  if (kind === "chat_template_kwargs") {
    return { kind, keys: chatTemplateOffKeys(value.keys) }
  }
  throw new Error(`Unsupported reasoning.off.kind: ${String(value.kind)}`)
}

function chatTemplateOffKeys(value) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error("reasoning.off.keys must be a non-empty array")
  }
  return value.map((key) => {
    if (key === "thinking" || key === "enable_thinking") return key
    throw new Error(`Unsupported reasoning.off key: ${String(key)}`)
  })
}

function pricingFromCost(cost) {
  if (!isRecord(cost)) return undefined
  const inputTokenPrice = numberValue(cost.input)
  const outputTokenPrice = numberValue(cost.output)
  if (inputTokenPrice === undefined && outputTokenPrice === undefined) return undefined
  return {
    ...(inputTokenPrice === undefined ? {} : { inputTokenPrice }),
    ...(outputTokenPrice === undefined ? {} : { outputTokenPrice }),
    currency: "USD",
  }
}

function serviceTierMetadataFromModel(raw) {
  const direct = serviceTierValue(raw?.serviceTier ?? raw?.service_tier)
  const extra = isRecord(raw?.extra_capabilities)
    ? serviceTierValue(raw.extra_capabilities.serviceTier ?? raw.extra_capabilities.service_tier)
    : undefined
  const value = extra ?? direct
  if (value === undefined) return undefined
  const options = stringArray(value.options)
  const defaultValue = normalizeString(value.default)
  const supported = booleanValue(value.supported)
    ?? (defaultValue !== undefined || options !== undefined ? true : undefined)
  if (supported === undefined) return undefined
  return compactRecord({
    supported,
    default: defaultValue,
    options,
  })
}

function serviceTierValue(value) {
  if (value === true) return { supported: true }
  if (value === false) return { supported: false }
  if (!isRecord(value)) return undefined
  return value
}

function resolveDefaultModelId(models) {
  const list = Array.isArray(models) ? models : []
  if (!list.length) return ""
  const chatCandidates = list.filter((model) => {
    const type = normalizeString(model?.type)
    return type === undefined || type === "chat"
  })
  const candidates = chatCandidates.length ? chatCandidates : list
  // Never default to a retired alias: prefer the first active reasoning default,
  // then the first active model, and fall back only when everything is retired.
  const active = candidates.filter((model) => catalogModelStatus(model) !== "deprecated")
  const pool = active.length ? active : candidates
  const reasoningDefault = pool.find((model) => model?.reasoning?.default)
  return normalizeString(reasoningDefault?.id) ?? normalizeString(pool[0]?.id) ?? ""
}

function isFilteredModel(model) {
  const matches = (value) => /embedding|rerank|reranker/i.test(String(value ?? ""))
  if (matches(model?.type)) return true
  if (matches(model?.id)) return true
  if (matches(model?.name)) return true
  if (matches(model?.family)) return true
  const hints = model?.typeHints
  return Array.isArray(hints) && hints.some((hint) => matches(hint))
}

async function readRemoteJson(url) {
  if (url.startsWith("file://")) return readJson(fileURLToPath(url))
  const response = await fetch(url, { signal: AbortSignal.timeout(30_000) })
  if (!response.ok) throw new Error(`Fetch failed for ${url}: HTTP ${response.status}`)
  return response.json()
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, "utf8"))
}

function renderCatalog(catalog) {
  const providers = catalog.providers.map((provider) => {
    const fields = Object.entries(provider).map(([key, value]) => {
      if (key === "models" && provider.models.length > 0) {
        const models = provider.models.map((model) => `        ${JSON.stringify(model)}`)
        return `      "models": [\n${models.join(",\n")}\n      ]`
      }
      return `      ${JSON.stringify(key)}: ${JSON.stringify(value)}`
    })
    return `    {\n${fields.join(",\n")}\n    }`
  })
  const fields = Object.entries(catalog).map(([key, value]) => key === "providers" && providers.length > 0
    ? `  "providers": [\n${providers.join(",\n")}\n  ]`
    : `  ${JSON.stringify(key)}: ${JSON.stringify(value)}`)
  return `{\n${fields.join(",\n")}\n}\n`
}

async function writeCatalog(previous: string, candidate) {
  const temporary = `${catalogOutput}.${randomUUID()}.tmp`
  try {
    await writeFile(temporary, renderCatalog(candidate), { flag: "wx" })
    if (await readFile(catalogOutput, "utf8") !== previous) {
      throw new Error("The catalog changed during sync. Run the command again.")
    }
    await rename(temporary, catalogOutput)
  } catch (error) {
    await unlink(temporary).catch((cleanup) => {
      if (cleanup.code !== "ENOENT") throw cleanup
    })
    throw error
  }
}

function normalizeProviderApi(value, adapter) {
  const api = normalizeString(value)
  return api === undefined || adapter !== "anthropic" ? api : api.replace(/\/v1\/?$/i, "")
}

function compactRecord(value) {
  const entries = Object.entries(value).filter(([, item]) => item !== undefined)
  return Object.fromEntries(entries)
}

function normalizeRequiredString(value, fieldName) {
  const normalized = normalizeString(value)
  if (normalized === undefined) throw new Error(`${fieldName} is required`)
  return normalized
}

function normalizeString(value) {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined
}

function positiveNumber(value) {
  const number = numberValue(value)
  return number !== undefined && number > 0 ? number : undefined
}

function numberValue(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function booleanValue(value) {
  return typeof value === "boolean" ? value : undefined
}

function stringArray(value) {
  if (!Array.isArray(value)) return undefined
  const strings = value.map((item) => normalizeString(item)).filter((item) => item !== undefined)
  return strings.length === 0 ? undefined : strings
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

main().catch((error) => {
  console.error(error.message)
  process.exitCode = 1
})
