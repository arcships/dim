<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { ArrowLeft, ChevronRight, Plus, RefreshCw, Search, X } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import { settingsMessages } from "../../i18n/settings"
import { readManagement, type ManagementProvider, type ManagementView } from "../../rpc/management"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import ProviderAuthFlow from "./ProviderAuthFlow.vue"
import ProviderConnectionForm from "./ProviderConnectionForm.vue"
import ProviderModels from "./ProviderModels.vue"
import { bindSettingsDirty, messageFrom } from "./context"

const { t } = useI18n({ messages: settingsMessages })
const route = useRoute()
const router = useRouter()
const connection = useConnectionStore()
const view = ref<ManagementView>()
const detail = ref<ManagementProvider>()
const loading = ref(true)
const busy = ref(false)
const error = ref("")
const detailError = ref("")
const search = ref("")
const pickerSearch = ref("")
const custom = ref(false)
const tab = ref("connection")
const connectionDirty = ref(false)
const modelDirty = ref(false)
const formKey = ref(0)
const dialog = ref<HTMLDialogElement>()
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
const dirty = computed(() => connectionDirty.value || modelDirty.value)
bindSettingsDirty(dirty)
const isNew = computed(() => route.name === "settings-provider-new")
const providerId = computed(() => typeof route.params.providerId === "string" ? route.params.providerId : "")
const opened = computed(() => isNew.value || providerId.value !== "")
function configured(provider: ManagementProvider): boolean {
  return provider.account?.state.authenticated === true || provider.connection?.credentialConfigured === true || (provider.connection?.auth.type === "anonymous" && provider.enabled === true)
}
const configuredProviders = computed(() => (view.value?.providers ?? []).filter(configured).filter((p) => `${p.displayName} ${p.providerId}`.toLowerCase().includes(search.value.toLowerCase())))
const availableProviders = computed(() => (view.value?.providers ?? []).filter((p) => !configured(p)).filter((p) => `${p.displayName} ${p.providerId}`.toLowerCase().includes(pickerSearch.value.toLowerCase())))
async function load(): Promise<void> {
  loading.value = true
  error.value = ""
  try {
    view.value = await connection.client.call("provider.management", {}, readManagement)
    if (!dirty.value && providerId.value) await loadDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { loading.value = false }
}
async function loadDetail(): Promise<void> {
  const id = providerId.value
  detailError.value = ""
  detail.value = view.value?.providers.find((p) => p.providerId === id)
  if (!id) return
  try {
    const result = await connection.client.call("provider.management", { providerId: id }, readManagement)
    if (providerId.value === id) {
      detail.value = result.providers.find((p) => p.providerId === id)
      if (!detail.value) detailError.value = result.errors.find((failure) => failure.providerId === id)?.message ?? t("unavailable")
    }
  } catch (cause) { if (providerId.value === id) detailError.value = messageFrom(cause) }
}
async function syncDialog(): Promise<void> {
  await nextTick()
  if (opened.value && !dialog.value?.open) dialog.value?.showModal()
  else if (!opened.value && dialog.value?.open) dialog.value.close()
}
function close(): void { void router.push({ name: "settings-providers" }) }
function open(id: string): void { void router.push({ name: "settings-provider-detail", params: { providerId: id } }) }
async function backToProviders(): Promise<void> {
  if (connectionDirty.value && !await confirm.value?.open(t("discard"))) return
  connectionDirty.value = false
  custom.value = false
}
function updated(provider: ManagementProvider): void {
  detail.value = provider
  if (view.value) {
    const index = view.value.providers.findIndex((p) => p.providerId === provider.providerId)
    if (index >= 0) view.value.providers[index] = provider
    else view.value.providers.push(provider)
  }
}
async function saved(provider: ManagementProvider): Promise<void> {
  connectionDirty.value = false
  updated(provider)
  ++formKey.value
  if (isNew.value) await router.replace({ name: "settings-provider-detail", params: { providerId: provider.providerId } })
}
async function changeTab(next: string): Promise<void> {
  if (tab.value === next) return
  if (dirty.value && !await confirm.value?.open(t("discard"))) return
  connectionDirty.value = false
  modelDirty.value = false
  tab.value = next
}
async function reloadDetail(): Promise<void> {
  if (dirty.value && !await confirm.value?.open(t("discard"))) return
  connectionDirty.value = false
  modelDirty.value = false
  await loadDetail()
  ++formKey.value
}
async function refresh(): Promise<void> {
  const current = detail.value
  if (!current || busy.value || dirty.value) return
  busy.value = true
  detailError.value = ""
  try {
    await connection.client.call("provider.refresh", { providerId: current.providerId }, readAck)
    await Promise.all([loadDetail(), useModelsStore().load()])
  } catch (cause) { detailError.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function remove(logout = false): Promise<void> {
  const current = detail.value
  if (!current || busy.value || !await confirm.value?.open(t(logout ? "signOutConfirm" : "deleteConfirm"), { confirmLabel: t(logout ? "signOut" : "remove") })) return
  busy.value = true
  detailError.value = ""
  try {
    await connection.client.call(logout ? `${current.account!.namespace}.logout` : "provider.delete", logout ? (current.account!.namespace === "account" ? { providerId: current.providerId } : {}) : { providerId: current.providerId, expectedVersion: current.version }, readAck)
    if (logout) await loadDetail()
    else close()
    await Promise.all([load(), useModelsStore().load()])
  } catch (cause) { detailError.value = messageFrom(cause) }
  finally { busy.value = false }
}
watch([providerId, isNew], async () => {
  custom.value = false
  pickerSearch.value = ""
  tab.value = "connection"
  connectionDirty.value = false
  modelDirty.value = false
  ++formKey.value
  await syncDialog()
  await loadDetail()
})
watch(() => connection.phase, (phase, before) => { if (phase === "connected" && before !== "connected" && !dirty.value) void load() })
onMounted(async () => { await syncDialog(); await load() })
</script>

<template>
  <article class="settings-page">
    <ConfirmDialog ref="confirm" />
    <header><h2 data-route-heading tabindex="-1">{{ t("providers") }}</h2><p>{{ t("providerDescription") }}</p></header>
    <p v-if="error" class="ui-banner settings-banner" data-tone="danger" role="alert">{{ error }} <button class="ui-button" data-variant="ghost" :disabled="loading || connection.phase !== 'connected'" @click="load">{{ t("retry") }}</button></p>
    <p v-for="failure in view?.errors" :key="failure.providerId" class="ui-banner settings-banner" data-tone="warning">{{ failure.providerId }}: {{ failure.message }}</p>
    <div class="settings-list-toolbar">
      <label class="ui-field-wrap"><Search aria-hidden="true" /><input v-model="search" class="ui-field" type="search" :placeholder="t('search')" /></label>
      <button class="ui-button" data-variant="primary" :disabled="loading || connection.phase !== 'connected'" @click="router.push({ name: 'settings-provider-new' })"><Plus aria-hidden="true" />{{ t("add") }}</button>
    </div>
    <p v-if="loading && !view" class="ui-banner" role="status">{{ t("loading") }}</p>
    <div class="settings-list">
      <button v-for="provider in configuredProviders" :key="provider.providerId" type="button" class="settings-row" @click="open(provider.providerId)">
        <span class="settings-status" :data-connected="provider.enabled !== false" aria-hidden="true" /><span class="settings-row-main"><strong>{{ provider.displayName }} <span v-if="provider.account" class="ui-badge">OAuth</span></strong><small>{{ provider.connection?.driverKind ?? provider.providerId }}</small></span><span class="settings-meta">{{ provider.enabled === false ? t("disabled") : t("connected") }} · {{ provider.models?.length ?? 0 }} {{ t("models") }}</span><ChevronRight :size="16" aria-hidden="true" />
      </button>
      <p v-if="view && !configuredProviders.length" class="settings-empty">{{ t(search ? 'noResults' : 'noProviders') }}</p>
    </div>
    <dialog ref="dialog" class="ui-dialog provider-dialog" data-size="lg" :aria-label="isNew ? t('add') : detail?.displayName" @cancel.prevent="close">
      <header class="ui-dialog-header"><div><h2 class="ui-dialog-title">{{ isNew ? t("add") : detail?.displayName ?? providerId }}</h2><p v-if="detail && !isNew" class="ui-dialog-description">{{ detail.providerId }}</p></div><button class="ui-icon-button ui-dialog-close" data-size="sm" data-variant="close" type="button" :aria-label="t('close')" @click="close"><X aria-hidden="true" /></button></header>
      <p v-if="connection.phase === 'reconnecting'" class="ui-banner settings-banner" role="status">{{ t("page.reconnecting") }}</p>
      <p v-if="loading || busy" class="ui-banner settings-banner" role="status">{{ t(busy ? "working" : "loading") }}</p>
      <p v-if="error" class="ui-banner settings-banner" data-tone="danger" role="alert">{{ error }} <button class="ui-button" data-variant="ghost" :disabled="loading || connection.phase !== 'connected'" @click="load">{{ t("retry") }}</button></p>
      <p v-if="detailError" class="ui-banner settings-banner" data-tone="danger" role="alert">{{ detailError }} <button class="ui-button" data-variant="ghost" :disabled="connection.phase !== 'connected'" @click="loadDetail">{{ t("retry") }}</button></p>
      <template v-if="isNew">
        <template v-if="custom && view">
          <div class="settings-list-toolbar"><button class="ui-button" data-size="sm" data-variant="ghost" type="button" @click="backToProviders"><ArrowLeft aria-hidden="true" />{{ t("back") }}</button></div>
          <ProviderConnectionForm :key="`new-${formKey}`" v-model:dirty="connectionDirty" :drivers="view.drivers" @saved="saved" />
        </template>
        <template v-else-if="view">
          <label class="ui-field-wrap" style="margin-bottom: 16px"><Search aria-hidden="true" /><input v-model="pickerSearch" class="ui-field" type="search" :placeholder="t('search')" /></label>
          <div class="settings-list"><button v-for="provider in availableProviders" :key="provider.providerId" class="settings-row" @click="open(provider.providerId)"><span class="settings-row-main"><strong>{{ provider.displayName }}</strong><small>{{ provider.connection?.driverKind ?? 'OAuth' }}</small></span><ChevronRight :size="16" aria-hidden="true" /></button><p v-if="!availableProviders.length" class="settings-empty">{{ t("noResults") }}</p></div>
          <footer class="ui-dialog-footer"><button class="ui-button" data-variant="primary" @click="custom = true">{{ t("custom") }}</button></footer>
        </template>
      </template>
      <template v-else-if="detail">
        <nav class="provider-tabs" :aria-label="t('providers')"><button class="ui-button" data-variant="ghost" :aria-pressed="tab === 'connection'" @click="changeTab('connection')">{{ t("connection") }}</button><button class="ui-button" data-variant="ghost" :aria-pressed="tab === 'models'" @click="changeTab('models')">{{ t("models") }} ({{ detail.models?.length ?? 0 }})</button></nav>
        <template v-if="tab === 'connection'">
          <section v-if="detail.account" class="settings-card">
            <div v-if="detail.account.state.authenticated" class="settings-list-toolbar"><span>{{ detail.account.state.identity?.email ?? detail.account.state.preview ?? t("connected") }}</span><button class="ui-button" data-variant="secondary" :disabled="busy || dirty || connection.phase !== 'connected'" @click="remove(true)">{{ t("signOut") }}</button></div>
            <ProviderAuthFlow v-else :key="detail.providerId" :provider="detail" @finished="loadDetail" />
          </section>
          <ProviderConnectionForm v-if="detail.connection && view" :key="`${detail.providerId}-${formKey}`" v-model:dirty="connectionDirty" :provider="detail" :drivers="view.drivers" @saved="saved" @reload="reloadDetail" />
        </template>
        <ProviderModels v-else :key="detail.providerId" v-model:dirty="modelDirty" :provider="detail" @updated="updated" />
        <footer class="provider-footer"><button v-if="detail.actions.includes('delete')" class="ui-button" data-variant="danger-ghost" :disabled="busy || dirty || connection.phase !== 'connected'" @click="remove()">{{ t("remove") }}</button><button v-if="detail.actions.includes('refresh')" class="ui-button" data-variant="secondary" :disabled="busy || dirty || connection.phase !== 'connected'" @click="refresh"><RefreshCw :size="14" aria-hidden="true" />{{ t("refresh") }}</button></footer>
      </template>
      <p v-else-if="!error && !detailError && !loading" class="settings-help" role="status">{{ t("loading") }}</p>
    </dialog>
  </article>
</template>

<style scoped>
.provider-tabs { display: flex; gap: var(--space-2); margin-bottom: var(--space-5); padding-bottom: var(--space-3); border-bottom: 1px solid var(--border); }
.provider-footer { display: flex; flex-wrap: wrap; justify-content: space-between; gap: var(--space-3); margin-top: var(--space-6); padding-top: var(--space-4); border-top: 1px solid var(--border); }
</style>
