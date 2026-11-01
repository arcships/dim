<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readEditedModalityCatalog, readModalityCatalog, type ModalityCatalog, type ModalityModel } from "../../rpc/general-settings"
import { readProviderList } from "../../rpc/read"
import type { ProviderSummary } from "../../rpc/protocol"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const providers = ref<ProviderSummary[]>([])
const providerId = ref("")
const loadedProvider = ref("")
const catalog = ref<ModalityCatalog>()
const drafts = ref<ModalityModel[]>([])
const busy = ref(false)
const error = ref("")
const saved = ref(false)
const dirty = computed(() => catalog.value !== undefined && JSON.stringify(catalog.value.models) !== JSON.stringify(drafts.value))
bindSettingsDirty(dirty)
async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved model capabilities?', '放弃未保存的模型能力？'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    if (!connection.supports("provider.list") || !connection.supports("provider.catalog")) throw new Error(s('Model catalogs are not available on this server.', '此服务器不支持模型目录。'))
    providers.value = await connection.client.call("provider.list", {}, readProviderList)
    const id = providerId.value || providers.value[0]?.providerId
    if (!id) { catalog.value = undefined; drafts.value = []; return }
    providerId.value = id
    catalog.value = await connection.client.call("provider.catalog", { providerId: id }, readModalityCatalog)
    loadedProvider.value = id
    drafts.value = catalog.value?.models.map((model) => ({ ...model, modalities: [...model.modalities] })) ?? []
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function save(): Promise<void> {
  if (!catalog.value || busy.value || !dirty.value) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    let version = catalog.value.version
    for (const model of drafts.value) {
      const original = catalog.value.models.find((item) => item.id === model.id)
      if (JSON.stringify(model) === JSON.stringify(original)) continue
      const next = await connection.client.call("provider.models.edit", { providerId: loadedProvider.value, expectedVersion: version, modelId: model.id, edit: { vision: model.vision, toolCalling: model.toolCalling, modalityCapabilities: model.modalities } }, readEditedModalityCatalog)
      catalog.value = next
      version = next.version
    }
    drafts.value = catalog.value.models.map((model) => ({ ...model, modalities: [...model.modalities] }))
    saved.value = true
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Modality', '模态') }}</h2><p>{{ s('Review and override static model capabilities. Capability declarations do not add abilities the upstream model does not have.', '查看并覆盖静态模型能力声明。声明不会为上游模型增加它不具备的能力。') }}</p></header>
    <p class="ui-banner settings-banner">{{ s('The runtime has no separate image/audio task routing service yet. These settings update provider model declarations, not an unused default-model record.', '当前运行时尚无独立图像 / 音频任务路由服务。此处更改的是提供商模型声明，而非未被使用的默认模型记录。') }}</p>
    <form class="settings-card settings-form" @submit.prevent="load"><label><span>{{ s('Provider', '提供商') }}</span><select v-model="providerId" class="ui-field" data-size="md" :disabled="busy || dirty"><option v-for="provider in providers" :key="provider.providerId" :value="provider.providerId">{{ provider.displayName }}</option></select></label><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Load catalog', '加载目录') }}</button></form>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load" />
    <form v-if="catalog" class="settings-form" @submit.prevent="save"><fieldset :disabled="busy || !connection.supports('provider.models.edit')"><section v-for="model in drafts" :key="model.id" class="settings-card"><h3>{{ model.name }}</h3><p><code>{{ model.id }}</code></p><label><input v-model="model.vision" type="checkbox" />{{ s('Vision input', '视觉输入') }}</label><label><input v-model="model.toolCalling" type="checkbox" />{{ s('Tool calling', '工具调用') }}</label><label><input v-model="model.modalities" type="checkbox" value="image.generate" />{{ s('Image generation', '图像生成') }}</label><p v-if="model.modalities.some((id) => id !== 'image.generate')">{{ s('Other declarations (preserved)', '其他声明（保留）') }}: {{ model.modalities.filter((id) => id !== 'image.generate').join(', ') }}</p></section></fieldset><footer class="settings-actions"><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !dirty || !connection.supports('provider.models.edit')">{{ s('Save capabilities', '保存能力声明') }}</button></footer></form>
    <p v-if="!busy && !error && !drafts.length" role="status">{{ s('No static models. Add a provider and models first. Managed-provider catalogs are controlled by their provider.', '没有静态模型。请先添加提供商和模型。托管提供商目录由其提供商管理。') }}</p>
  </article>
</template>

<style scoped>
fieldset { padding: 0; border: 0; min-width: 0; }
section label { display: flex; align-items: center; gap: var(--space-2); margin-block: var(--space-3); }
</style>
