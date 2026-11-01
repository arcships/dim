<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue"
import { useI18n } from "vue-i18n"
import { Copy, ExternalLink } from "lucide-vue-next"
import { copyText } from "../../clipboard"
import { settingsMessages } from "../../i18n/settings"
import { readAccountDescriptors, readLoginAttempt, readLoginStatus, type LoginAttempt, type ManagementProvider } from "../../rpc/management"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import { messageFrom, reloadSettingsOnReconnect } from "./context"

const props = defineProps<{ provider: ManagementProvider }>()
const emit = defineEmits<{ finished: [] }>()
const { t } = useI18n({ messages: settingsMessages })
const connection = useConnectionStore()
const mode = ref(props.provider.account?.loginMethods.includes("browser") ? "browser" : "device")
const directory = ref("")
const enterprise = ref(false)
const email = ref("")
const callback = ref("")
const attempt = ref<LoginAttempt>()
const busy = ref(false)
const polling = ref(false)
const error = ref("")
let timer: ReturnType<typeof setTimeout> | undefined
let generation = 0
let mounted = true
const namespace = props.provider.account?.namespace ?? "account"
const target = namespace === "account" ? { providerId: props.provider.providerId } : {}
const url = computed(() => attempt.value?.authorizationUrl ?? attempt.value?.verificationUriComplete ?? attempt.value?.verificationUri)
const methods = props.provider.account?.loginMethods ?? []
onMounted(async () => {
  if (namespace !== "account") return
  try {
    const providers = await connection.client.call("account.providers", {}, readAccountDescriptors)
    if (mounted && !directory.value) directory.value = providers.find((p) => p.providerId === props.provider.providerId)?.defaultDirectory ?? ""
  } catch (cause) { if (mounted) error.value = messageFrom(cause) }
})

async function cancelAttempt(current: LoginAttempt): Promise<void> {
  await connection.client.call(`${namespace}.login.cancel`, { ...target, attemptId: current.attemptId }, readAck)
}
async function poll(): Promise<void> {
  const current = attempt.value
  const epoch = generation
  if (!current || busy.value || polling.value || connection.phase !== "connected") return
  clearTimeout(timer)
  polling.value = true
  error.value = ""
  try {
    const status = await connection.client.call(`${namespace}.login.status`, { ...target, attemptId: current.attemptId }, readLoginStatus)
    if (!mounted || generation !== epoch) return
    if (status === "pending") {
      timer = setTimeout(() => void poll(), 1500)
    } else {
      attempt.value = undefined
      callback.value = ""
      if (status === "completed") {
        email.value = ""
        await useModelsStore().load()
        emit("finished")
      } else error.value = t(status)
    }
  } catch (cause) {
    if (mounted && generation === epoch) error.value = messageFrom(cause)
  } finally {
    if (mounted && generation === epoch) polling.value = false
  }
}
async function start(): Promise<void> {
  if (busy.value || attempt.value) return
  busy.value = true
  polling.value = false
  error.value = ""
  const epoch = ++generation
  try {
    const method = `${namespace}.login.${mode.value === "browser" ? "browser.begin" : "begin"}`
    if (!connection.supports(method)) throw new Error(t("missingMethod"))
    const current = await connection.client.call(method, {
      ...target,
      ...(namespace === "account" ? { directory: directory.value, ...(enterprise.value ? { email: email.value } : {}), ...(mode.value === "browser" ? { redirectUri: "http://localhost:54321/auth/callback" } : {}) } : {}),
    }, readLoginAttempt)
    if (!mounted || epoch !== generation) { await cancelAttempt(current); return }
    attempt.value = current
    timer = setTimeout(() => void poll(), 100)
  } catch (cause) {
    if (mounted && epoch === generation) error.value = messageFrom(cause)
  } finally { if (mounted && epoch === generation) busy.value = false }
}
async function finish(): Promise<void> {
  const current = attempt.value
  if (!current || busy.value || !callback.value.trim()) return
  clearTimeout(timer)
  busy.value = true
  polling.value = false
  error.value = ""
  const epoch = ++generation
  try {
    await connection.client.call(`${namespace}.login.browser.complete`, { ...target, attemptId: current.attemptId, callbackUrl: callback.value.trim() }, readAck)
    if (mounted && epoch === generation) timer = setTimeout(() => void poll(), 100)
  } catch (cause) {
    if (mounted && epoch === generation) error.value = messageFrom(cause)
  } finally {
    callback.value = ""
    if (mounted && epoch === generation) busy.value = false
  }
}
async function cancel(): Promise<void> {
  const current = attempt.value
  ++generation
  clearTimeout(timer)
  attempt.value = undefined
  callback.value = ""
  busy.value = false
  polling.value = false
  if (!current) return
  try { await cancelAttempt(current) } catch (cause) { error.value = messageFrom(cause) }
}
function open(): void { if (url.value) window.open(url.value, "_blank", "noopener,noreferrer") }
async function copy(): Promise<void> {
  if (!url.value) return
  try { await copyText(url.value) } catch (cause) { error.value = messageFrom(cause) }
}
reloadSettingsOnReconnect(connection, poll)
onBeforeUnmount(() => {
  mounted = false
  ++generation
  clearTimeout(timer)
  callback.value = ""
  email.value = ""
  if (attempt.value) void cancelAttempt(attempt.value).catch(() => undefined)
})
</script>

<template>
  <form class="settings-auth settings-form" @submit.prevent="attempt ? finish() : start()">
    <template v-if="!attempt">
      <div v-if="namespace === 'account'" class="settings-grid">
        <label><span>{{ t("region") }}</span><select v-model="directory" class="ui-field" :disabled="busy" required><option value="" disabled>{{ t("region") }}</option><option value="https://dimagent.cn">{{ t("cn") }}</option><option value="https://dimagent.com">{{ t("global") }}</option></select></label>
        <label v-if="methods.length > 1"><span>{{ t("authType") }}</span><select v-model="mode" class="ui-field" :disabled="busy"><option v-for="method in methods" :key="method" :value="method">{{ t(method) }}</option></select></label>
      </div>
      <label v-if="enterprise" ><span>{{ t("email") }}</span><input v-model="email" class="ui-field" type="email" autocomplete="email" required :disabled="busy" /></label>
      <div class="settings-inline-actions">
        <button class="ui-button" data-variant="primary" type="submit" :disabled="busy || connection.phase !== 'connected'">{{ busy ? t("working") : t("login") }}</button>
        <button v-if="namespace === 'account'" class="ui-button" data-variant="ghost" type="button" :disabled="busy" @click="enterprise = !enterprise">{{ t("enterprise") }}</button>
      </div>
    </template>
    <template v-else>
      <p class="settings-help" role="status">{{ t("waiting") }}</p>
      <code v-if="attempt.userCode" class="settings-auth-code">{{ attempt.userCode }}</code>
      <div class="settings-inline-actions">
        <button class="ui-button" data-variant="primary" type="button" @click="open"><ExternalLink aria-hidden="true" />{{ t("openAuth") }}</button>
        <button class="ui-button" data-variant="secondary" type="button" @click="copy"><Copy aria-hidden="true" />{{ t("copy") }}</button>
      </div>
      <p class="settings-meta">{{ t("expires") }}: {{ new Date(attempt.expiresAt).toLocaleTimeString() }}</p>
      <template v-if="mode === 'browser'">
        <label><span>{{ t("callback") }}</span><textarea v-model="callback" class="ui-field" rows="3" autocomplete="off" spellcheck="false" /></label>
        <p class="settings-help">{{ t("callbackHint") }}</p>
      </template>
      <div class="settings-inline-actions">
        <button v-if="mode === 'browser'" class="ui-button" data-variant="primary" type="submit" :disabled="busy || !callback.trim() || connection.phase !== 'connected'">{{ t("finish") }}</button>
        <button class="ui-button" data-variant="secondary" type="button" :disabled="busy" @click="cancel">{{ t("cancel") }}</button>
      </div>
    </template>
    <p v-if="error" class="ui-banner" data-tone="danger" role="alert">{{ error }} <button v-if="attempt && !busy" class="ui-button" data-variant="ghost" type="button" :disabled="polling || connection.phase !== 'connected'" @click="poll">{{ t("retry") }}</button></p>
  </form>
</template>
