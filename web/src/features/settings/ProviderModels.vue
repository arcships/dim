<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { Plus, Search, X } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import { settingsMessages } from "../../i18n/settings"
import { readManagement, type ManagementModel, type ManagementProvider } from "../../rpc/management"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import { messageFrom } from "./context"

const props = defineProps<{ provider: ManagementProvider }>()
const emit = defineEmits<{ updated: [provider: ManagementProvider] }>()
const dirty = defineModel<boolean>("dirty", { default: false })
const { t } = useI18n({ messages: settingsMessages })
const connection = useConnectionStore()
const query = ref("")
const editing = ref(false)
const original = ref<ManagementModel>()
const modelId = ref("")
const name = ref("")
const context = ref("")
const output = ref("")
const tools = ref(true)
const vision = ref(false)
const reasoning = ref(false)
const efforts = ref("")
const effort = ref("")
const tier = ref("")
const endpoint = ref("")
const busy = ref(false)
const error = ref("")
const expectedVersion = ref<number | null>(null)
const baseline = ref("")
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
const models = computed(() => (props.provider.models ?? []).filter((m) => `${m.model.modelId} ${m.model.displayName}`.toLowerCase().includes(query.value.toLowerCase())))
const allowedEfforts = computed(() => original.value?.model.metadata?.reasoning?.effortOptions ?? [])
const tiers = computed(() => original.value?.model.metadata?.serviceTier?.options ?? [])
function snapshot(): string { return JSON.stringify([modelId.value, name.value, context.value, output.value, tools.value, vision.value, reasoning.value, efforts.value, effort.value, tier.value, endpoint.value]) }
watch(() => editing.value && snapshot() !== baseline.value, (value) => { dirty.value = value }, { flush: "sync" })
function open(model?: ManagementModel): void {
  original.value = model
  modelId.value = model?.model.modelId ?? ""
  name.value = model?.model.displayName ?? ""
  context.value = model?.model.capabilities.contextWindow?.toString() ?? ""
  output.value = model?.model.capabilities.maxOutputTokens?.toString() ?? ""
  tools.value = model?.model.capabilities.toolCalling ?? true
  vision.value = model?.model.capabilities.vision ?? false
  reasoning.value = model?.model.capabilities.reasoning ?? false
  efforts.value = (model?.model.metadata?.reasoning?.effortOptions ?? []).join(", ")
  const preferences = model && props.provider.preferences[model.model.modelId]
  effort.value = preferences?.reasoningEffort ?? ""
  tier.value = preferences?.serviceTier ?? ""
  endpoint.value = model?.endpointOverride ?? ""
  expectedVersion.value = props.provider.version
  error.value = ""
  baseline.value = snapshot()
  editing.value = true
}
async function close(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !await confirm.value?.open(t("discard"))) return
  editing.value = false
  dirty.value = false
}
async function mutate(method: string, params: Record<string, unknown>, version = props.provider.version): Promise<ManagementProvider> {
  const providerId = props.provider.providerId
  await connection.client.call(method, { providerId, expectedVersion: version, ...params }, readAck)
  const view = await connection.client.call("provider.management", { providerId }, readManagement)
  const updated = view.providers.find((p) => p.providerId === providerId)
  if (!updated) throw new Error(view.errors[0]?.message ?? t("unavailable"))
  emit("updated", updated)
  await useModelsStore().load()
  return updated
}
async function toggle(model: ManagementModel): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  try {
    const enabledIds = (props.provider.models ?? []).filter((m) => m.enabled !== (m.model.modelId === model.model.modelId)).map((m) => m.model.modelId)
    await mutate("provider.models.set_enabled", { modelIds: enabledIds })
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function remove(model: ManagementModel, reset: boolean): Promise<void> {
  if (busy.value || !await confirm.value?.open(t(reset ? "resetConfirm" : "modelDeleteConfirm"), { confirmLabel: t(reset ? "reset" : "remove") })) return
  busy.value = true
  error.value = ""
  try { await mutate(reset ? "provider.models.reset" : "provider.models.delete", { modelId: model.model.modelId }) }
  catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function save(): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  try {
    const existing = original.value
    const c = existing?.model.capabilities
    const edit: Record<string, unknown> = {}
    if (name.value !== existing?.model.displayName) edit.displayName = name.value
    if (context.value !== (c?.contextWindow?.toString() ?? "") && context.value) edit.contextWindow = Number(context.value)
    if (output.value !== (c?.maxOutputTokens?.toString() ?? "") && output.value) edit.maxOutputTokens = Number(output.value)
    if (tools.value !== c?.toolCalling) edit.toolCalling = tools.value
    if (vision.value !== c?.vision) edit.vision = vision.value
    if (reasoning.value !== c?.reasoning || efforts.value !== (existing?.model.metadata?.reasoning?.effortOptions ?? []).join(", ")) edit.reasoning = { supported: reasoning.value, effortOptions: efforts.value.split(",").map((s) => s.trim()).filter(Boolean) }
    if (endpoint.value !== (existing?.endpointOverride ?? "")) edit.endpoint = endpoint.value ? { action: "set", value: endpoint.value } : { action: "clear" }
    const model = { modelId: modelId.value, displayName: name.value, status: "active", capabilities: { modalities: ["text"], toolCalling: tools.value, streaming: true, structuredOutput: true, vision: vision.value, audio: false, reasoning: reasoning.value, ...(context.value ? { contextWindow: Number(context.value) } : {}), ...(output.value ? { maxOutputTokens: Number(output.value) } : {}) }, ...(reasoning.value ? { metadata: { reasoning: { supported: true, effortOptions: efforts.value.split(",").map((s) => s.trim()).filter(Boolean) } } } : {}) }
    const currentPreferences = props.provider.preferences[modelId.value]
    if (!existing || Object.keys(edit).length) {
      const updated = await mutate(existing ? "provider.models.edit" : "provider.models.create", existing ? { modelId: modelId.value, edit } : { model }, expectedVersion.value)
      expectedVersion.value = updated.version
    }
    if (existing && props.provider.actions.includes("preferences.update")) {
      const patch: Record<string, unknown> = {}
      if (effort.value !== (currentPreferences?.reasoningEffort ?? "")) patch.reasoningEffort = effort.value ? { action: "set", value: effort.value } : { action: "clear" }
      if (tier.value !== (currentPreferences?.serviceTier ?? "")) patch.serviceTier = tier.value ? { action: "set", value: tier.value } : { action: "clear" }
      if (Object.keys(patch).length) {
        const updated = await mutate("provider.preferences.update", { modelId: modelId.value, patch }, expectedVersion.value)
        expectedVersion.value = updated.version
      }
    }
    editing.value = false
    dirty.value = false
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
</script>

<template>
  <section>
    <ConfirmDialog ref="confirm" />
    <div class="settings-list-toolbar"><h3>{{ t("models") }}</h3><button v-if="provider.actions.includes('models.create')" class="ui-button" data-variant="secondary" data-size="sm" :disabled="busy || editing || provider.version === null" @click="open()"><Plus aria-hidden="true" />{{ t("addModel") }}</button></div>
    <p class="settings-help" style="margin-bottom: 16px">{{ t("modelDescription") }}</p>
    <p v-if="error" class="ui-banner settings-banner" data-tone="danger" role="alert">{{ error }}</p>
    <form v-if="editing" class="settings-card settings-form" @submit.prevent="save">
      <div class="settings-list-toolbar"><h3>{{ t(original ? "edit" : "addModel") }}</h3><button class="ui-icon-button" data-variant="close" type="button" :disabled="busy" :aria-label="t('close')" @click="close"><X aria-hidden="true" /></button></div>
      <fieldset class="settings-form" :disabled="busy || connection.phase !== 'connected'" style="margin: 0; padding: 0; border: 0; min-width: 0">
        <label><span>{{ t("modelId") }}</span><input v-model="modelId" class="ui-field" required :readonly="!!original" /></label>
        <label><span>{{ t("name") }}</span><input v-model="name" class="ui-field" required /></label>
        <div class="settings-grid"><label><span>{{ t("context") }}</span><input v-model="context" class="ui-field" type="number" min="1" step="1" :required="original?.model.capabilities.contextWindow !== undefined" :placeholder="t('unknownLimits')" /></label><label><span>{{ t("output") }}</span><input v-model="output" class="ui-field" type="number" min="1" step="1" :required="original?.model.capabilities.maxOutputTokens !== undefined" :placeholder="t('unknownLimits')" /></label></div>
        <div class="settings-inline-actions"><label class="settings-check"><input v-model="tools" type="checkbox" />{{ t("tools") }}</label><label class="settings-check"><input v-model="vision" type="checkbox" />{{ t("vision") }}</label><label class="settings-check"><input v-model="reasoning" type="checkbox" />{{ t("reasoning") }}</label></div>
        <label v-if="reasoning"><span>{{ t("effort") }}</span><input v-model="efforts" class="ui-field" placeholder="low, medium, high" /></label>
        <label v-if="original && provider.connection?.driverKind === 'new-api-router'"><span>{{ t("endpoint") }}</span><select v-model="endpoint" class="ui-field"><option value="">{{ t("inherit") }}</option><option v-for="kind in ['openai', 'openai-response', 'openai-response-compact', 'anthropic', 'gemini']" :key="kind" :value="kind">{{ kind }}</option></select></label>
      </fieldset>
      <template v-if="original && provider.actions.includes('preferences.update') && (allowedEfforts.length || tiers.length)">
        <h3>{{ t("preferences") }}</h3><div class="settings-grid"><label v-if="allowedEfforts.length"><span>{{ t("effort") }}</span><select v-model="effort" class="ui-field" :disabled="busy || connection.phase !== 'connected'"><option value="">{{ t("inherit") }}</option><option v-for="value in allowedEfforts" :key="value">{{ value }}</option></select></label><label v-if="tiers.length"><span>{{ t("tier") }}</span><select v-model="tier" class="ui-field" :disabled="busy || connection.phase !== 'connected'"><option value="">{{ t("inherit") }}</option><option v-for="value in tiers" :key="value">{{ value }}</option></select></label></div>
      </template>
      <div class="settings-actions"><button class="ui-button" data-variant="secondary" type="button" :disabled="busy" @click="close">{{ t("cancel") }}</button><button class="ui-button" data-variant="primary" type="submit" :disabled="busy || !dirty || connection.phase !== 'connected'">{{ busy ? t("saving") : t("save") }}</button></div>
    </form>
    <template v-else>
      <label class="ui-field-wrap" style="margin-bottom: 12px"><Search aria-hidden="true" /><input v-model="query" class="ui-field" type="search" :placeholder="t('search')" /></label>
      <div class="provider-models">
        <div v-for="entry in models" :key="entry.model.modelId" class="settings-row">
          <input v-if="entry.actions.includes('set_enabled')" type="checkbox" :checked="entry.enabled" :aria-label="`${t('enabled')} ${entry.model.displayName}`" :disabled="busy || connection.phase !== 'connected'" @change="toggle(entry)" />
          <div class="settings-row-main"><strong>{{ entry.model.displayName }}</strong><small>{{ entry.model.modelId }}</small><small v-if="entry.unavailableReason">{{ entry.unavailableReason }}</small><small v-else>{{ entry.model.capabilities.contextWindow?.toLocaleString() ?? t('unknownLimits') }} · {{ entry.source }}</small></div>
          <div class="settings-inline-actions"><button v-if="entry.actions.includes('edit')" class="ui-button" data-variant="ghost" data-size="sm" :disabled="busy" @click="open(entry)">{{ t("edit") }}</button><button v-if="entry.actions.includes('reset')" class="ui-button" data-variant="ghost" data-size="sm" :disabled="busy || connection.phase !== 'connected'" @click="remove(entry, true)">{{ t("reset") }}</button><button v-if="entry.actions.includes('delete')" class="ui-button" data-variant="danger-ghost" data-size="sm" :disabled="busy || connection.phase !== 'connected'" @click="remove(entry, false)">{{ t("remove") }}</button></div>
        </div>
      </div>
      <p v-if="!models.length" class="settings-empty">{{ t(query ? 'noResults' : 'noModels') }}</p>
    </template>
  </section>
</template>
