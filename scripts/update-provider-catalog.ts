#!/usr/bin/env node
import { randomUUID } from "node:crypto"
import { readFile, rename, unlink, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { isDeepStrictEqual } from "node:util"

const resources = join(import.meta.dirname, "../crates/dim-app-server/resources")
const catalogOutput = join(resources, "default-catalog.json")
const reasoningOffOverridesFile = join(resources, "reasoning-off-overrides.json")
const remoteBase = process.env.DIM_PROVIDER_REMOTE_BASE ?? "https://raw.githubusercontent.com/ThinkInAIXYZ/PublicProviderConf/refs/heads/dev/dist"

type SyncPolicy = {
  keepBaseUrl?: true
  keepDefaultModel?: true
  localModels?: string[]
  mediaModels?: string[]
}

const remoteProviders: Record<string, SyncPolicy> = {
  aihubmix: { mediaModels: ["gpt-image-1", "minimax-h3", "veo-3.1", "tts-1", "whisper-1"] },
  anthropic: { keepDefaultModel: true },
  "custom-provider": { localModels: ["arcship-5.5", "arcship-5.4", "arcship-5.4-mini", "arcship-image"] },
  deepseek: { keepDefaultModel: true },
  fireworks: {},
  google: { mediaModels: ["gemini-3.1-flash-image", "veo-3.1-generate-preview", "gemini-2.5-flash-preview-tts", "gemini-3.5-flash", "lyria-3-pro-preview"] },
  huggingface: {},
  jiekou: {},
  "kimi-for-coding": { keepBaseUrl: true, keepDefaultModel: true },
  longcat: { keepBaseUrl: true },
  "minimax-cn-coding-plan": {},
  "minimax-cn": { keepDefaultModel: true },
  "minimax-coding-plan": {},
  minimax: { keepDefaultModel: true, mediaModels: ["image-01", "video-01", "speech-02-hd", "music-01"] },
  modelscope: { keepDefaultModel: true },
  "moonshot-ai": { keepDefaultModel: true },
  moonshot: {},
  novita: {},
  nvidia: {},
  openai: { keepDefaultModel: true, mediaModels: ["gpt-image-2", "tts-1", "whisper-1"] },
  "opencode-go": { keepBaseUrl: true, keepDefaultModel: true },
  openrouter: { mediaModels: ["openai/gpt-5-image", "google/veo-3.1", "openai/tts-1", "openai/whisper-1"] },
  ppinfra: { keepBaseUrl: true },
  qiniu: {},
  "siliconflow-com": {},
  siliconflow: { mediaModels: ["Kwai-Kolors/Kolors", "Wan-AI/Wan2.2-T2V-A14B", "Wan-AI/Wan2.2-I2V-A14B", "FunAudioLLM/CosyVoice2-0.5B", "FunAudioLLM/SenseVoiceSmall"] },
  stepfun: { mediaModels: ["step-image-edit-2", "step-2x-large", "stepaudio-2.5-tts", "step-tts-2", "step-tts-mini", "step-tts-vivid", "stepaudio-2.5-asr", "stepaudio-2-asr-pro", "step-asr"] },
  "tencent-token-plan": { localModels: ["tc-code-latest"] },
  upstage: {},
  "vercel-ai-gateway": {},
  xai: { keepDefaultModel: true, mediaModels: ["grok-2-image-1212", "grok-video-1"] },
  "xiaomi-token-plan-ams": {},
  "xiaomi-token-plan-cn": {},
  "xiaomi-token-plan-sgp": {},
  "zai-coding-plan": { keepDefaultModel: true },
  zai: { keepDefaultModel: true },
  zenmux: {},
  "zhipuai-coding-plan": { keepDefaultModel: true },
  zhipuai: {},
}

async function main() {
  const [command, providerId, ...extra] = process.argv.slice(2)
  if (extra.length || (command !== undefined && command !== "--sync")) {
    throw new Error("Usage: node scripts/update-provider-catalog.ts [--sync [provider-id]]")
  }

  const previous = await readFile(catalogOutput, "utf8")
  const catalog = JSON.parse(previous)
  validateCatalog(catalog)
  if (command === undefined) {
    console.log(`Validated catalog: ${catalogOutput}`)
    return
  }

  const ids = new Set(catalog.providers.map((provider) => provider.providerId))
  const requested = providerId === undefined ? Object.keys(remoteProviders) : [providerId]
  for (const id of requested) {
    if (!ids.has(id)) throw new Error(`Unknown provider: ${id}`)
    if (!Object.hasOwn(remoteProviders, id)) throw new Error(`Provider ${id} has no remote catalog source`)
  }

  const selected = new Set(requested)
  const overrides = createReasoningOffOverrideState(await readJson(reasoningOffOverridesFile))
  const providers = []
  for (const provider of catalog.providers) {
    providers.push(selected.has(provider.providerId)
      ? await syncProvider(provider, remoteProviders[provider.providerId], overrides.get(provider.providerId))
      : provider)
  }
  if (isDeepStrictEqual(providers, catalog.providers)) {
    console.log("Catalog is up to date")
    return
  }
  const candidate = { ...catalog, updatedAt: new Date().toISOString(), providers }
  validateCatalog(candidate)
  await writeCatalog(previous, candidate)
  console.log(`Updated catalog: ${requested.join(", ")}`)
}

async function syncProvider(provider, policy: SyncPolicy, overrides) {
  const id = provider.providerId
  const raw = { ...provider.metadata.raw }
  const sourceId = normalizeString(raw.providerDbSourceId) ?? id
  const remote = await readRemoteJson(`${remoteBase}/${encodeURIComponent(sourceId)}.json`)
  if (!Array.isArray(remote?.models)) throw new Error(`${id}: expected a remote models array`)
  assertUniqueModelIds(remote.models, id, "id")

  const previous = new Map(provider.models.map((model) => [model.modelId, model.metadata.raw]))
  const local = (modelId: string) => {
    const model = previous.get(modelId)
    if (model === undefined) throw new Error(`Provider ${id}: missing local model ${modelId}`)
    return model
  }
  const upstream = applyLocalModelOverrides(id, remote.models.filter((model) => !isFilteredModel(model)), previous)
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
    models.set(modelId, { ...models.get(modelId), ...mediaOverlay(id, local(modelId)) })
  }
  const metadata = { ...provider.metadata, defaultModelId, raw }
  if (defaultModelId === undefined) delete metadata.defaultModelId
  const result = { ...provider, models: applyReasoningOffOverrides(id, [...models.values()], overrides).map(toCatalogModel), metadata }
  if (!policy.keepBaseUrl) {
    const api = normalizeProviderApi(remote.api, metadata.adapter)
    if (api !== undefined) {
      raw.api = api
      result.defaultBaseUrl = api
    }
  }
  return result
}

function mediaOverlay(providerId, model) {
  if (providerId === "stepfun") return model
  const fields = ["id", "display_name", "type", "modalities", "modalityCapabilities"]
  if (providerId !== "google" || model.id !== "gemini-3.5-flash") {
    fields.push("tool_call", "structured_output")
  }
  return Object.fromEntries(fields.filter((key) => Object.hasOwn(model, key)).map((key) => [key, model[key]]))
}

function createReasoningOffOverrideState(overrides) {
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
    if (Object.hasOwn(remoteProviders, providerId)) pending.set(providerId, providerModels)
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

function applyLocalModelOverrides(providerId, models, previous) {
  if (providerId === "kimi-for-coding") {
    return models.map((model) => {
      const context = model.limit?.context ?? model.contextWindow
      if (model.id.trim() !== "k3.1-pro" || !Number.isSafeInteger(context) || context <= 262144) return model
      return { ...model, limit: { ...model.limit, context: 262144 } }
    })
  }
  if (providerId === "stepfun") {
    const failClosedRemoteModels = models.map((model) => ({
      ...model,
      type: "other",
      chat: false,
      tool_call: false,
      structured_output: false,
      modalities: { input: ["text"], output: ["text"] },
    }))
    return failClosedRemoteModels.map((model) => previous.get(model.id.trim()) ?? model)
  }
  if (providerId === "custom-provider") {
    return models.map((model) => {
      const id = normalizeString(model?.id)?.toLowerCase()
      if (id !== "minimax-m2.7") return model
      return withReasoningCapability(model, { interleaved: true })
    })
  }
  if (providerId === "aihubmix") {
    // AIHubMix omits the feature flags for glm-5.3-flashx, so the upstream
    // pipeline reports it as tool-call/reasoning-incapable and the model drops
    // out of agent model selection. The model supports both — Z.AI's own route
    // advertises them — so patch the capability fields at sync time instead of
    // hand-editing the synced file, which the next sync would overwrite.
    return models.map((model) => {
      if (normalizeString(model?.id) !== "glm-5.3-flashx") return model
      const metadata = isRecord(model.metadata) ? model.metadata : {}
      const extraReasoning = isRecord(model.extra_capabilities?.reasoning)
        ? model.extra_capabilities.reasoning
        : {}
      const features = stringArray(metadata.features) ?? [
        "thinking",
        "tools",
        "function_calling",
        "structured_outputs",
      ]
      return {
        ...model,
        reasoning: { supported: true, default: true },
        tool_call: true,
        metadata: { ...metadata, features },
        extra_capabilities: {
          ...(model.extra_capabilities ?? {}),
          reasoning: { ...extraReasoning, supported: true },
        },
      }
    })
  }
  return models
}

function withReasoningCapability(model, capability) {
  return {
    ...model,
    extra_capabilities: {
      ...(model.extra_capabilities ?? {}),
      reasoning: {
        ...(model.extra_capabilities?.reasoning ?? {}),
        ...capability,
      },
    },
  }
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
