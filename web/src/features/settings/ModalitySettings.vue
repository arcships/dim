<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readModalitySnapshot, type ModalityOperation, type ModalitySnapshot } from "../../rpc/general-settings"
import type { ModelId } from "../../rpc/protocol"
import { RpcError } from "../../rpc/client"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const models = useModelsStore()
const snapshot = ref<ModalitySnapshot>()
const drafts = ref<Record<string, string>>({})
const busy = ref(false)
const error = ref("")
const conflict = ref(false)
const saved = ref(false)
const rows = computed(() => snapshot.value?.categories.find((category) => category.category === "image")?.operations ?? [])
const dirty = computed(() => rows.value.some((row) => row.key !== undefined && (drafts.value[row.key] ?? "") !== baseline(row)))
bindSettingsDirty(dirty)
function keyOf(model: ModelId): string { return JSON.stringify([model.providerId, model.modelId]) }
function baseline(row: ModalityOperation): string {
  const latest = rows.value.find((item) => item.key === row.key)
  return latest?.default ? keyOf(latest.default) : ""
}
function label(model: ModelId): string {
  const provider = models.providers?.find((item) => item.providerId === model.providerId)
  if (!provider) return `${model.providerId} / ${model.modelId}`
  return `${provider.name} / ${provider.models.find((item) => item.id === model.modelId)?.name ?? model.modelId}`
}
function title(operation: string): string {
  switch (operation) {
    case "generate": return s('Image generation', '图像生成')
    case "edit": return s('Image editing', '图像编辑')
    case "enhance": return s('Image enhancement', '图像增强')
    case "understand": return s('Image understanding', '图像理解')
    default: return operation
  }
}
function reasons(operation: ModalityOperation): string {
  return operation.reasons.map((reason) => reason.detail).join("; ")
}
function effective(operation: ModalityOperation): string {
  if (!operation.effective) {
    return operation.default
      ? s('None. The configured default is unavailable and is never replaced by another model.', '无。已配置的默认值不可用，且不会被其他模型替代。')
      : s('None. No model serves this operation now.', '无。当前没有模型服务此操作。')
  }
  const name = label(operation.effective)
  return operation.effective.source === "automatic" ? `${name} (${s('automatic', '自动')})` : `${name} (${s('configured', '已配置')})`
}
function modelFrom(draft: string): ModelId | null {
  if (draft === "") return null
  const [providerId, modelId] = JSON.parse(draft) as [string, string]
  return { providerId, modelId }
}
function choose(key: string, event: Event): void {
  drafts.value[key] = (event.target as HTMLSelectElement).value
}
function fill(next: ModalitySnapshot): void {
  const operations = next.categories.find((category) => category.category === "image")?.operations ?? []
  drafts.value = Object.fromEntries(operations.filter((operation) => operation.key !== undefined).map((operation) => [operation.key!, operation.default ? keyOf(operation.default) : ""]))
}
async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved defaults?', '放弃未保存的默认设置？'))) return
  busy.value = true
  error.value = ""
  conflict.value = false
  saved.value = false
  try {
    if (!connection.supports("modality.capabilities")) throw new Error(s('Modality settings are not available on this server.', '此服务器不支持多模态设置。'))
    const [next] = await Promise.all([connection.client.call("modality.capabilities", {}, readModalitySnapshot), models.load()])
    if (!next.categories.some((category) => category.category === "image")) throw new Error(s('The server reports no image category for Modality.', '服务器未返回多模态的图像类别。'))
    snapshot.value = next
    fill(next)
    if (models.error) error.value = models.error
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function save(): Promise<void> {
  if (!snapshot.value || busy.value || !dirty.value || conflict.value) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    for (const row of rows.value) {
      if (row.key === undefined || (drafts.value[row.key] ?? "") === baseline(row)) continue
      snapshot.value = await connection.client.call("modality.configure", { expectedRevision: snapshot.value.revision, category: "image", operation: row.operation, model: modelFrom(drafts.value[row.key] ?? "") }, readModalitySnapshot)
    }
    saved.value = true
  } catch (cause) {
    conflict.value = cause instanceof RpcError && cause.appError === "modality_conflict"
    error.value = conflict.value ? s('Modality defaults changed. Reload before saving again; your draft has been preserved.', '多模态默认设置已变更。请重新加载后再保存，当前草稿已保留。') : messageFrom(cause)
  } finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Modality', '多模态') }}</h2><p>{{ s('Choose the default model for each image operation. Clearing a default restores automatic resolution; an unavailable default is never replaced by another model.', '为每个图像操作选择默认模型。清除默认值即恢复自动解析；不可用的默认值不会被其他模型替代。') }}</p></header>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load" />
    <form v-if="snapshot" class="settings-form" @submit.prevent="save">
      <section v-for="row in rows" :key="row.operation" class="settings-card">
        <h3>{{ title(row.operation) }}</h3>
        <p v-if="row.reasons.length" class="ui-banner settings-banner" data-tone="warning">{{ reasons(row) }}</p>
        <label>
          <span>{{ s('Default model', '默认模型') }}</span>
          <select :value="drafts[row.key!] ?? ''" class="ui-field" data-size="md" :disabled="busy || conflict || !connection.supports('modality.configure')" @change="choose(row.key!, $event)">
            <option value="">{{ s('Automatic', '自动') }}</option>
            <option v-for="option in row.options" :key="keyOf(option)" :value="keyOf(option)" :disabled="!option.available">{{ label(option) }}{{ option.available ? '' : ` (${s('unavailable', '不可用')})` }}</option>
          </select>
        </label>
        <p>{{ s('Currently effective', '当前生效') }}: {{ effective(row) }}</p>
        <p v-if="row.default && !row.default.available" class="ui-field-hint" data-tone="danger">{{ row.default.reason?.detail }}</p>
        <p v-if="row.operation === 'understand'">{{ s('Selection only for now: the Session image-understanding path does not read this default yet, so it does not change the model a Session uses.', '目前仅用于选择：会话的图像理解路径尚未读取此默认值，因此不会改变会话使用的模型。') }}</p>
      </section>
      <footer class="settings-actions"><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !dirty || conflict || !connection.supports('modality.configure')">{{ s('Save defaults', '保存默认设置') }}</button></footer>
    </form>
  </article>
</template>
