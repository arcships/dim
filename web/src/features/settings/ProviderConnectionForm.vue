<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { settingsMessages } from "../../i18n/settings"
import { readManagementProvider, readProbe, type ManagementProvider, type ManagementView, type ModelEntry } from "../../rpc/management"
import { RpcError } from "../../rpc/client"
import { useConnectionStore } from "../../stores/connection"
import { messageFrom } from "./context"

const props = defineProps<{ provider?: ManagementProvider; drivers: ManagementView["drivers"] }>()
const emit = defineEmits<{ saved: [provider: ManagementProvider]; reload: []; cancel: [] }>()
const dirty = defineModel<boolean>("dirty", { default: false })
const { t } = useI18n({ messages: settingsMessages })
const connection = useConnectionStore()
const id = ref("")
const name = ref("")
const driver = ref("openai-compatible")
const auth = ref("bearer")
const authHeader = ref("Authorization")
const prefix = ref("Bearer ")
const baseUrl = ref("")
const key = ref("")
const clearKey = ref(false)
const accessKey = ref("")
const sessionToken = ref("")
const proxy = ref("")
const timeout = ref("")
const modelIdPrefix = ref("")
const enabled = ref(true)
const headers = ref<{ name: string; value: string; remove: boolean; existing: boolean }[]>([])
const saving = ref(false)
const error = ref("")
const conflict = ref(false)
const probed = ref<ModelEntry[]>()
const baseline = ref("")
const version = ref<number | null>(null)
const accountOwned = computed(() => !!props.provider && props.provider.credentialAuthority !== "static")
const authTypes = computed(() => props.drivers.find((d) => d.driverKind === driver.value)?.authTypes ?? [])
const driverItems = computed(() => props.drivers.map((d) => ({ label: d.driverKind, value: d.driverKind })))
const authItems = computed(() => authTypes.value.map((kind) => ({ label: t(`providerAuth.${kind}`), value: kind })))
const usesStoredKeyAtNewUrl = computed(() => {
  const saved = props.provider?.connection
  return saved?.credentialConfigured && baseUrl.value !== saved.baseUrl && !credential()
})
function snapshot(): string { return JSON.stringify([id.value, name.value, driver.value, auth.value, authHeader.value, prefix.value, baseUrl.value, key.value, clearKey.value, accessKey.value, sessionToken.value, proxy.value, timeout.value, enabled.value, headers.value, modelIdPrefix.value]) }
function adopt(): void {
  const p = props.provider, c = p?.connection
  id.value = p?.providerId ?? crypto.randomUUID()
  name.value = p?.displayName ?? ""
  driver.value = c?.driverKind ?? "openai-compatible"
  auth.value = c?.auth.type ?? "bearer"
  authHeader.value = c?.auth.headerName ?? "Authorization"
  prefix.value = c?.auth.prefix ?? "Bearer "
  baseUrl.value = c?.baseUrl ?? ""
  key.value = ""
  clearKey.value = false
  accessKey.value = ""
  sessionToken.value = ""
  proxy.value = c?.proxyUrl ?? ""
  timeout.value = c?.timeoutMs?.toString() ?? ""
  modelIdPrefix.value = c?.gluedPrefix ?? ""
  enabled.value = p?.enabled ?? true
  probed.value = undefined
  headers.value = (c?.headerNames ?? []).map((name) => ({ name, value: "", remove: false, existing: true }))
  version.value = p?.version ?? null
  baseline.value = snapshot()
  dirty.value = false
  conflict.value = false
}
watch(() => props.provider, () => { if (!dirty.value) adopt() }, { immediate: true })
watch(snapshot, (current) => {
  dirty.value = current !== baseline.value
  probed.value = undefined
})
watch(driver, () => { if (!props.provider && !authTypes.value.includes(auth.value)) auth.value = authTypes.value[0] ?? "anonymous" })
function credential(): Record<string, unknown> | undefined {
  if (clearKey.value) return { action: "clear" }
  if (auth.value === "aws_bedrock" && (accessKey.value || key.value || sessionToken.value)) return { action: "set_aws", value: { accessKeyId: accessKey.value, secretAccessKey: key.value, ...(sessionToken.value ? { sessionToken: sessionToken.value } : {}) } }
  return key.value ? { action: "set", value: key.value } : undefined
}
function patch(): Record<string, unknown> {
  const p = props.provider!, c = p.connection
  const changes: Record<string, unknown> = {}
  if (p.source === "custom" && name.value !== p.displayName) changes.displayName = name.value
  if (!accountOwned.value) {
    if (baseUrl.value !== c?.baseUrl) changes.baseUrl = baseUrl.value
    const secret = credential()
    if (secret) changes.credential = secret
    const updates = headers.value.flatMap((h) => h.remove && h.existing ? [{ action: "remove", name: h.name }] : !h.remove && h.value ? [{ action: "set", name: h.name, value: h.value }] : [])
    if (updates.length) changes.headers = updates
  }
  // An emptied prefix is the patch's "clear it"; an untouched field is not in
  // the patch at all, so an omitted statement keeps the stored one.
  if (modelIdPrefix.value !== (c?.gluedPrefix ?? "")) changes.gluedPrefix = modelIdPrefix.value ? modelIdPrefix.value : null
  if (enabled.value !== p.enabled) changes.enabled = enabled.value
  const network: Record<string, unknown> = {}
  if (proxy.value !== (c?.proxyUrl ?? "")) network.proxy = proxy.value ? { action: "set", value: proxy.value } : { action: "clear" }
  if (timeout.value !== (c?.timeoutMs?.toString() ?? "")) network.timeoutMs = timeout.value ? { action: "set", value: Number(timeout.value) } : { action: "clear" }
  if (Object.keys(network).length) changes.network = network
  return changes
}
function newConnection(): Record<string, unknown> {
  return {
    driverKind: driver.value, baseUrl: baseUrl.value,
    auth: { type: auth.value, ...(["api_key", "custom_header"].includes(auth.value) ? { headerName: authHeader.value } : {}), ...(auth.value === "api_key" ? { prefix: prefix.value } : {}) },
    ...(credential() ? { credential: credential() } : {}),
    headers: Object.fromEntries(headers.value.filter((h) => !h.remove && h.name && h.value).map((h) => [h.name, h.value])),
    ...(proxy.value || timeout.value ? { network: { ...(proxy.value ? { proxy: proxy.value } : {}), ...(timeout.value ? { timeoutMs: Number(timeout.value) } : {}) } } : {}),
  }
}
async function save(): Promise<void> {
  if (saving.value || connection.phase !== "connected" || conflict.value) return
  saving.value = true
  error.value = ""
  try {
    const p = props.provider
    const result = await connection.client.call(p ? "provider.update" : "provider.create", p ? { providerId: p.providerId, expectedVersion: version.value, patch: patch() } : { providerId: id.value, displayName: name.value, enabled: enabled.value, connection: newConnection(), models: probed.value ?? [] }, readManagementProvider)
    key.value = ""
    accessKey.value = ""
    sessionToken.value = ""
    headers.value.forEach((h) => { h.value = "" })
    dirty.value = false
    baseline.value = snapshot()
    emit("saved", result)
  } catch (cause) {
    conflict.value = cause instanceof RpcError && cause.appError === "provider_conflict"
    error.value = conflict.value ? t("conflict") : messageFrom(cause)
  } finally { saving.value = false }
}
async function probe(): Promise<void> {
  saving.value = true
  error.value = ""
  probed.value = undefined
  try {
    probed.value = await connection.client.call("provider.probe", { providerId: props.provider?.providerId ?? id.value, connection: props.provider ? patch() : newConnection() }, readProbe)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { saving.value = false }
}
onBeforeUnmount(() => { key.value = ""; accessKey.value = ""; sessionToken.value = ""; headers.value = [] })
</script>

<template>
  <form class="settings-form" @submit.prevent="save">
    <fieldset class="settings-form" :disabled="saving || connection.phase !== 'connected'" style="border: 0; padding: 0; margin: 0; min-width: 0">
      <UFormField v-if="!provider || provider.source === 'custom'" :label="t('providerName')">
        <UInput v-model="name" required class="w-full" />
      </UFormField>
      <UFormField v-if="!accountOwned" :label="t('baseUrl')">
        <UInput v-model="baseUrl" type="url" placeholder="https://api.example.com/v1" required autocomplete="off" class="w-full" />
      </UFormField>
      <template v-if="!provider">
        <div class="settings-grid">
          <UFormField :label="t('apiProtocol')">
            <USelect v-model="driver" :items="driverItems" class="w-full" />
          </UFormField>
          <UFormField :label="t('authType')">
            <USelect v-model="auth" :items="authItems" class="w-full" />
          </UFormField>
        </div>
        <div v-if="['api_key', 'custom_header'].includes(auth)" class="settings-grid">
          <UFormField :label="t('headerName')">
            <UInput v-model="authHeader" required class="w-full" />
          </UFormField>
          <UFormField v-if="auth === 'api_key'" :label="t('headerPrefix')">
            <UInput v-model="prefix" class="w-full" />
          </UFormField>
        </div>
      </template>
      <template v-if="!accountOwned">
        <UFormField v-if="auth === 'aws_bedrock'" label="AWS Access Key ID">
          <UInput v-model="accessKey" type="password" autocomplete="new-password" class="w-full" />
        </UFormField>
        <UFormField v-if="auth !== 'anonymous'" :label="auth === 'aws_bedrock' ? 'AWS Secret Access Key' : t('apiKey')">
          <UInput v-model="key" type="password" autocomplete="new-password" :placeholder="provider?.connection?.credentialConfigured ? t('keepSecret') : ''" :disabled="clearKey" class="w-full" />
        </UFormField>
        <UFormField v-if="auth === 'aws_bedrock'" label="AWS Session Token">
          <UInput v-model="sessionToken" type="password" autocomplete="new-password" class="w-full" />
        </UFormField>
        <UCheckbox v-if="provider?.connection?.credentialConfigured" v-model="clearKey" :label="t('clearSecret')" />
        <UCollapsible>
          <template #default="{ open }">
            <UButton color="neutral" variant="ghost" size="sm" :trailing-icon="open ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'" :label="t('headers')" />
          </template>
          <template #content>
            <div class="settings-form" style="margin-top: 12px">
              <div v-for="(header, index) in headers" :key="index" class="settings-header-field">
                <UInput v-model="header.name" :readonly="header.existing" :aria-label="t('headerName')" :placeholder="t('headerName')" :disabled="header.remove" />
                <UInput v-model="header.value" type="password" autocomplete="new-password" :aria-label="t('headerValue')" :placeholder="header.existing ? t('keepSecret') : t('headerValue')" :disabled="header.remove" />
                <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-x" :aria-label="t('remove')" :aria-pressed="header.remove" @click="header.existing ? header.remove = !header.remove : headers.splice(index, 1)" />
              </div>
              <UButton color="neutral" variant="outline" icon="i-lucide-plus" class="justify-self-start" @click="headers.push({ name: '', value: '', existing: false, remove: false })">{{ t("addHeader") }}</UButton>
            </div>
          </template>
        </UCollapsible>
      </template>
      <UCollapsible>
        <template #default="{ open }">
          <UButton color="neutral" variant="ghost" size="sm" :trailing-icon="open ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'" :label="t('advanced')" />
        </template>
        <template #content>
          <div class="settings-grid" style="margin-top: 12px">
            <UFormField :label="t('proxy')">
              <UInput v-model="proxy" autocomplete="off" type="url" class="w-full" />
            </UFormField>
            <UFormField :label="t('timeout')">
              <UInput v-model="timeout" type="number" min="1" step="1" :placeholder="t('default')" class="w-full" />
            </UFormField>
            <UFormField :label="t('modelIdPrefix')" :help="t('modelIdPrefixHint')">
              <UInput v-model="modelIdPrefix" autocomplete="off" class="w-full" />
            </UFormField>
          </div>
        </template>
      </UCollapsible>
      <UCheckbox v-model="enabled" :label="t('enableProvider')" />
    </fieldset>
    <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="error" role="alert">
      <template v-if="conflict" #actions>
        <UButton size="xs" color="neutral" variant="ghost" @click="$emit('reload')">{{ t("reload") }}</UButton>
      </template>
    </UAlert>
    <p v-if="usesStoredKeyAtNewUrl" class="settings-help">{{ t("storedKeyAtNewUrl") }}</p>
    <p v-if="probed" class="settings-help" role="status">{{ t("providerProbeResult", { count: probed.length }) }}</p>
    <div class="settings-actions">
      <UButton color="neutral" variant="ghost" :disabled="saving" @click="$emit('cancel')">{{ t("cancel") }}</UButton>
      <UButton v-if="!accountOwned" color="neutral" variant="outline" :disabled="saving || !baseUrl || connection.phase !== 'connected'" @click="probe">{{ t("probe") }}</UButton>
      <UButton color="primary" type="submit" :disabled="saving || conflict || connection.phase !== 'connected' || (provider && !dirty)">{{ saving ? t("saving") : t(provider ? "save" : "saveProvider") }}</UButton>
    </div>
  </form>
</template>
