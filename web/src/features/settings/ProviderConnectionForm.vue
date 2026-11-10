<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { Plus, X } from "lucide-vue-next"
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
const usesStoredKeyAtNewUrl = computed(() => {
  const saved = props.provider?.connection
  return saved?.credentialConfigured && baseUrl.value !== saved.baseUrl && !credential()
})
function snapshot(): string { return JSON.stringify([id.value, name.value, driver.value, auth.value, authHeader.value, prefix.value, baseUrl.value, key.value, clearKey.value, accessKey.value, sessionToken.value, proxy.value, timeout.value, enabled.value, headers.value]) }
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
      <label v-if="!provider || provider.source === 'custom'"><span>{{ t("providerName") }}</span><input v-model="name" class="ui-field" required /></label>
      <label v-if="!accountOwned"><span>{{ t("baseUrl") }}</span><input v-model="baseUrl" class="ui-field" type="url" placeholder="https://api.example.com/v1" required autocomplete="off" /></label>
      <template v-if="!provider">
        <div class="settings-grid">
          <label><span>{{ t("apiProtocol") }}</span><select v-model="driver" class="ui-field"><option v-for="d in drivers" :key="d.driverKind" :value="d.driverKind">{{ d.driverKind }}</option></select></label>
          <label><span>{{ t("authType") }}</span><select v-model="auth" class="ui-field"><option v-for="kind in authTypes" :key="kind" :value="kind">{{ t(`providerAuth.${kind}`) }}</option></select></label>
        </div>
        <div v-if="['api_key', 'custom_header'].includes(auth)" class="settings-grid"><label><span>{{ t("headerName") }}</span><input v-model="authHeader" class="ui-field" required /></label><label v-if="auth === 'api_key'"><span>{{ t("headerPrefix") }}</span><input v-model="prefix" class="ui-field" /></label></div>
      </template>
      <template v-if="!accountOwned">
        <label v-if="auth === 'aws_bedrock'"><span>AWS Access Key ID</span><input v-model="accessKey" class="ui-field" type="password" autocomplete="new-password" /></label>
        <label v-if="auth !== 'anonymous'"><span>{{ auth === 'aws_bedrock' ? 'AWS Secret Access Key' : t("apiKey") }}</span><input v-model="key" class="ui-field" type="password" autocomplete="new-password" :placeholder="provider?.connection?.credentialConfigured ? t('keepSecret') : ''" :disabled="clearKey" /></label>
        <label v-if="auth === 'aws_bedrock'"><span>AWS Session Token</span><input v-model="sessionToken" class="ui-field" type="password" autocomplete="new-password" /></label>
        <label v-if="provider?.connection?.credentialConfigured" class="settings-check"><input v-model="clearKey" type="checkbox" />{{ t("clearSecret") }}</label>
        <details><summary>{{ t("headers") }}</summary><div class="settings-form" style="margin-top: 12px">
          <div v-for="(header, index) in headers" :key="index" class="settings-header-field">
            <input v-model="header.name" class="ui-field" :readonly="header.existing" :aria-label="t('headerName')" :placeholder="t('headerName')" :disabled="header.remove" />
            <input v-model="header.value" class="ui-field" type="password" autocomplete="new-password" :aria-label="t('headerValue')" :placeholder="header.existing ? t('keepSecret') : t('headerValue')" :disabled="header.remove" />
            <button class="ui-icon-button" data-variant="ghost" type="button" :aria-label="t('remove')" :aria-pressed="header.remove" @click="header.existing ? header.remove = !header.remove : headers.splice(index, 1)"><X aria-hidden="true" /></button>
          </div>
          <button class="ui-button" data-variant="secondary" type="button" @click="headers.push({ name: '', value: '', existing: false, remove: false })"><Plus aria-hidden="true" />{{ t("addHeader") }}</button>
        </div></details>
      </template>
      <details><summary>{{ t("advanced") }}</summary><div class="settings-grid" style="margin-top: 12px"><label><span>{{ t("proxy") }}</span><input v-model="proxy" class="ui-field" autocomplete="off" type="url" /></label><label><span>{{ t("timeout") }}</span><input v-model="timeout" class="ui-field" type="number" min="1" step="1" :placeholder="t('default')" /></label></div></details>
      <label class="settings-check"><input v-model="enabled" type="checkbox" />{{ t("enableProvider") }}</label>
    </fieldset>
    <p v-if="error" class="ui-banner" data-tone="danger" role="alert">{{ error }} <button v-if="conflict" class="ui-button" data-variant="ghost" type="button" @click="$emit('reload')">{{ t("reload") }}</button></p>
    <p v-if="usesStoredKeyAtNewUrl" class="settings-help">{{ t("storedKeyAtNewUrl") }}</p>
    <p v-if="probed" class="settings-help" role="status">{{ t("providerProbeResult", { count: probed.length }) }}</p>
    <div class="settings-actions">
      <button class="ui-button" data-variant="ghost" type="button" :disabled="saving" @click="$emit('cancel')">{{ t("cancel") }}</button>
      <button v-if="!accountOwned" class="ui-button" data-variant="secondary" type="button" :disabled="saving || !baseUrl || connection.phase !== 'connected'" @click="probe">{{ t("probe") }}</button>
      <button class="ui-button" data-variant="primary" type="submit" :disabled="saving || conflict || connection.phase !== 'connected' || (provider && !dirty)">{{ saving ? t("saving") : t(provider ? "save" : "saveProvider") }}</button>
    </div>
  </form>
</template>
