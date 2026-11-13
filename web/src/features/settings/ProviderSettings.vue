<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
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
const models = useModelsStore()
const view = ref<ManagementView>()
const detail = ref<ManagementProvider>()
const loading = ref(true)
const busy = ref(false)
const error = ref("")
const detailError = ref("")
const modelRefreshError = ref("")
const savedProvider = ref("")
const search = ref("")
const pickerSearch = ref("")
const custom = ref(false)
const tab = ref("connection")
const connectionDirty = ref(false)
const modelDirty = ref(false)
const formKey = ref(0)
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
const dirty = computed(() => connectionDirty.value || modelDirty.value)
bindSettingsDirty(dirty)
const isNew = computed(() => route.name === "settings-provider-new")
const providerId = computed(() => typeof route.params.providerId === "string" ? route.params.providerId : "")
const opened = computed(() => isNew.value || providerId.value !== "")
const tabItems = computed(() => [
  { label: t("connection"), value: "connection" },
  { label: `${t("models")} (${detail.value?.models?.length ?? 0})`, value: "models" },
])
function configured(provider: ManagementProvider): boolean {
  return provider.source === "custom" || provider.account?.state.authenticated === true
    || provider.connection?.credentialConfigured === true || (!provider.account && provider.enabled === true)
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
      const provider = result.providers.find((p) => p.providerId === id)
      if (provider) updated(provider)
      else {
        detail.value = undefined
        detailError.value = result.errors.find((failure) => failure.providerId === id)?.message ?? t("unavailable")
      }
    }
  } catch (cause) { if (providerId.value === id) detailError.value = messageFrom(cause) }
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
  savedProvider.value = provider.displayName
  await router.replace({ name: "settings-providers" })
  await models.load()
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
  if (!current?.actions.includes("refresh") || busy.value || dirty.value || connection.phase !== "connected") return
  busy.value = true
  modelRefreshError.value = ""
  try {
    await models.refresh(current.providerId)
    if (providerId.value === current.providerId) await loadDetail()
  } catch (cause) {
    if (providerId.value === current.providerId) modelRefreshError.value = messageFrom(cause)
  }
  finally { busy.value = false }
}
async function signedInFromAuth(refreshError?: string): Promise<void> {
  const id = providerId.value
  await loadDetail()
  if (providerId.value === id) modelRefreshError.value = refreshError ?? ""
}
async function remove(logout = false): Promise<void> {
  const current = detail.value
  if (!current || busy.value || !await confirm.value?.open(t(logout ? "signOutConfirm" : "deleteConfirm"), { confirmLabel: t(logout ? "signOut" : "remove") })) return
  busy.value = true
  detailError.value = ""
  modelRefreshError.value = ""
  try {
    await connection.client.call(logout ? `${current.account!.namespace}.logout` : "provider.delete", logout ? (current.account!.namespace === "account" ? { providerId: current.providerId } : {}) : { providerId: current.providerId, expectedVersion: current.version }, readAck)
    if (logout) await loadDetail()
    else close()
    await Promise.all([load(), models.load()])
  } catch (cause) { detailError.value = messageFrom(cause) }
  finally { busy.value = false }
}
watch([providerId, isNew], async () => {
  custom.value = false
  pickerSearch.value = ""
  tab.value = "connection"
  connectionDirty.value = false
  modelDirty.value = false
  modelRefreshError.value = ""
  if (opened.value) savedProvider.value = ""
  ++formKey.value
  await loadDetail()
})
watch(() => connection.phase, (phase, before) => { if (phase === "connected" && before !== "connected" && !dirty.value) void load() })
void load()
</script>

<template>
  <article class="settings-page">
    <ConfirmDialog ref="confirm" />
    <header><h2 data-route-heading tabindex="-1">{{ t("providers") }}</h2><p>{{ t("providerDescription") }}</p></header>
    <UAlert v-if="savedProvider" color="success" variant="subtle" icon="i-lucide-check-circle" :description="t('providerSaved', { name: savedProvider })" role="status" class="settings-banner" />
    <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="error" role="alert" class="settings-banner">
      <template #actions>
        <UButton size="xs" color="neutral" variant="ghost" :disabled="loading || connection.phase !== 'connected'" @click="load">{{ t("retry") }}</UButton>
      </template>
    </UAlert>
    <UAlert v-for="failure in view?.errors" :key="failure.providerId" color="warning" variant="subtle" icon="i-lucide-alert-triangle" :description="`${failure.providerId}: ${failure.message}`" class="settings-banner" />
    <div class="settings-list-toolbar">
      <UInput v-model="search" type="search" icon="i-lucide-search" :placeholder="t('search')" class="max-w-xs flex-1" />
      <UButton color="primary" icon="i-lucide-plus" :disabled="loading || connection.phase !== 'connected'" @click="router.push({ name: 'settings-provider-new' })">{{ t("add") }}</UButton>
    </div>
    <UAlert v-if="loading && !view" color="neutral" variant="subtle" :description="t('loading')" role="status" />
    <div class="settings-list">
      <UButton v-for="provider in configuredProviders" :key="provider.providerId" color="neutral" variant="ghost" block class="settings-row" @click="open(provider.providerId)">
        <span class="settings-status" :data-connected="provider.enabled !== false" aria-hidden="true" />
        <span class="settings-row-main">
          <strong>{{ provider.displayName }} <UBadge v-if="provider.account" color="neutral" variant="subtle" size="sm" label="OAuth" /></strong>
          <small v-if="provider.connection?.baseUrl">{{ provider.connection.baseUrl }}</small>
        </span>
        <span class="settings-meta">{{ t(provider.enabled === false ? "disabled" : provider.account ? "signedIn" : "enabled") }} · {{ t("providerModelCount", { count: provider.models?.length ?? 0 }) }}</span>
        <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-muted" aria-hidden="true" />
      </UButton>
      <p v-if="view && !configuredProviders.length" class="settings-empty">{{ t(search ? 'noResults' : 'noProviders') }}</p>
    </div>
    <UModal
      :open="opened"
      :title="isNew ? t(custom ? 'custom' : 'add') : detail?.displayName ?? providerId"
      :description="detail?.enabled === false && !isNew ? t('disabled') : undefined"
      :ui="{ content: 'sm:max-w-2xl' }"
      @update:open="!$event && close()"
    >
      <template #body>
        <UAlert v-if="connection.phase === 'reconnecting'" color="neutral" variant="subtle" :description="t('page.reconnecting')" role="status" class="settings-banner" />
        <UAlert v-if="loading || busy" color="neutral" variant="subtle" :description="t(busy ? 'working' : 'loading')" role="status" class="settings-banner" />
        <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="error" role="alert" class="settings-banner">
          <template #actions>
            <UButton size="xs" color="neutral" variant="ghost" :disabled="loading || connection.phase !== 'connected'" @click="load">{{ t("retry") }}</UButton>
          </template>
        </UAlert>
        <UAlert v-if="detailError" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="detailError" role="alert" class="settings-banner">
          <template #actions>
            <UButton size="xs" color="neutral" variant="ghost" :disabled="connection.phase !== 'connected'" @click="loadDetail">{{ t("retry") }}</UButton>
          </template>
        </UAlert>
        <UAlert v-if="modelRefreshError" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="t('modelRefreshFailed', { reason: modelRefreshError })" role="alert" class="settings-banner">
          <template v-if="detail?.actions.includes('refresh')" #actions>
            <UButton size="xs" color="neutral" variant="ghost" :disabled="busy || dirty || connection.phase !== 'connected'" @click="refresh">{{ t("retry") }}</UButton>
          </template>
        </UAlert>
        <template v-if="isNew">
          <template v-if="custom && view">
            <div class="settings-list-toolbar">
              <UButton size="sm" color="neutral" variant="ghost" icon="i-lucide-arrow-left" @click="backToProviders">{{ t("back") }}</UButton>
            </div>
            <ProviderConnectionForm :key="`new-${formKey}`" v-model:dirty="connectionDirty" :drivers="view.drivers" @saved="saved" @cancel="close" />
          </template>
          <template v-else-if="view">
            <div class="settings-list-toolbar">
              <UInput v-model="pickerSearch" type="search" icon="i-lucide-search" :placeholder="t('searchProvider')" class="max-w-xs flex-1" />
              <UButton color="neutral" variant="outline" @click="custom = true">{{ t("addCustomProvider") }}</UButton>
            </div>
            <div class="settings-list">
              <UButton v-for="provider in availableProviders" :key="provider.providerId" color="neutral" variant="ghost" block class="settings-row" @click="open(provider.providerId)">
                <span class="settings-row-main"><strong>{{ provider.displayName }} <UBadge v-if="provider.account" color="neutral" variant="subtle" size="sm" label="OAuth" /></strong></span>
                <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-muted" aria-hidden="true" />
              </UButton>
              <p v-if="!availableProviders.length" class="settings-empty">{{ t("noResults") }}</p>
            </div>
          </template>
        </template>
        <template v-else-if="detail">
          <div class="provider-tabs">
            <UTabs :model-value="tab" :items="tabItems" :content="false" size="sm" @update:model-value="changeTab(String($event))" />
            <UButton v-if="detail.actions.includes('refresh')" color="neutral" variant="outline" size="sm" icon="i-lucide-refresh-cw" :disabled="busy || dirty || connection.phase !== 'connected'" @click="refresh">{{ t("refreshModelList") }}</UButton>
          </div>
          <template v-if="tab === 'connection'">
            <section v-if="detail.account" class="settings-card">
              <div v-if="detail.account.state.authenticated" class="settings-list-toolbar">
                <span>{{ detail.account.state.identity?.email ?? detail.account.state.preview ?? t("connected") }}</span>
                <UButton color="neutral" variant="outline" :disabled="busy || dirty || connection.phase !== 'connected'" @click="remove(true)">{{ t("signOut") }}</UButton>
              </div>
              <ProviderAuthFlow v-else :key="detail.providerId" :provider="detail" @finished="signedInFromAuth" />
            </section>
            <ProviderConnectionForm v-if="detail.connection && view" :key="`${detail.providerId}-${formKey}`" v-model:dirty="connectionDirty" :provider="detail" :drivers="view.drivers" @saved="saved" @reload="reloadDetail" @cancel="close" />
          </template>
          <ProviderModels v-else :key="detail.providerId" v-model:dirty="modelDirty" :provider="detail" @updated="updated" />
          <footer v-if="detail.actions.includes('delete')" class="provider-footer">
            <UButton color="error" variant="ghost" :disabled="busy || dirty || connection.phase !== 'connected'" @click="remove()">{{ t("remove") }}</UButton>
          </footer>
        </template>
        <p v-else-if="!error && !detailError && !loading" class="settings-help" role="status">{{ t("loading") }}</p>
      </template>
    </UModal>
  </article>
</template>

<style scoped>
.provider-tabs { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-2); margin-bottom: var(--space-5); padding-bottom: var(--space-3); border-bottom: 1px solid var(--border); }
.provider-footer { display: flex; flex-wrap: wrap; justify-content: space-between; gap: var(--space-3); margin-top: var(--space-6); padding-top: var(--space-4); border-top: 1px solid var(--border); }
</style>
