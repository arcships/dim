<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readModelRecording, readPing, type ModelRecording } from "../../rpc/general-settings"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const ping = ref<ReturnType<typeof readPing>>()
const latency = ref<number>()
const busy = ref(false)
const error = ref("")
const search = ref("")
const recording = ref<ModelRecording>()
const enabled = ref(false)
const saved = ref(false)
const dirty = computed(() => recording.value !== undefined && enabled.value !== recording.value.enabled)
bindSettingsDirty(dirty)
const features = computed(() => connection.hello?.features.filter((method) => method.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())).sort() ?? [])
async function refresh(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved recording settings?', '放弃未保存的录制设置？'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    const start = performance.now()
    ping.value = await connection.client.call("daemon.ping", {}, readPing)
    latency.value = Math.round(performance.now() - start)
    if (connection.supports("settings.read")) {
      recording.value = await connection.client.call("settings.read", { source: "model-log", scope: "user" }, readModelRecording)
      enabled.value = recording.value.enabled
    }
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function saveRecording(): Promise<void> {
  if (!recording.value || !dirty.value || busy.value) return
  if (enabled.value && !window.confirm(s('Record model requests and responses? Recordings include prompts, source code and tool output. Custom headers may contain secrets. The file has no automatic rotation.', '录制模型请求和响应？录制包含提示词、源代码和工具输出。自定义请求头可能包含机密。文件不会自动轮换。'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    recording.value = await connection.client.call("settings.update", { source: "model-log", scope: "user", expectedRevision: recording.value.revision, document: { enabled: enabled.value } }, readModelRecording)
    enabled.value = recording.value.enabled
    saved.value = true
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, refresh, dirty)
onMounted(refresh)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Diagnostics', '诊断') }}</h2><p>{{ s('Live connection and server information. This page never displays credentials or bootstrap tokens.', '实时连接和服务器信息。此页面不会显示凭据或引导令牌。') }}</p></header>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="refresh" />
    <dl class="settings-card"><div><dt>{{ s('Server version', '服务器版本') }}</dt><dd>{{ connection.hello?.serverVersion ?? '—' }}</dd></div><div><dt>{{ s('Protocol', '协议') }}</dt><dd>{{ connection.hello?.protocolVersion ?? '—' }}</dd></div><div><dt>{{ s('Instance', '实例') }}</dt><dd><code>{{ connection.hello?.instanceId ?? '—' }}</code></dd></div><div><dt>{{ s('Connection', '连接') }}</dt><dd>{{ connection.phase }}</dd></div><template v-if="ping"><div><dt>{{ s('Sessions', '会话') }}</dt><dd>{{ ping.sessions }}</dd></div><div><dt>{{ s('Connections', '连接数') }}</dt><dd>{{ ping.connections }}</dd></div><div><dt>{{ s('Round trip', '往返延迟') }}</dt><dd>{{ latency }} ms</dd></div></template></dl>
    <button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy" @click="refresh">{{ s('Refresh diagnostics', '刷新诊断') }}</button>
    <form v-if="recording" class="settings-card settings-form" @submit.prevent="saveRecording"><h3>{{ s('Model request recording', '模型请求录制') }}</h3><p>{{ s('Changes apply to new model requests without restarting. In-flight requests finish with their existing recorder. Records are written to model-log/recordings.jsonl in the daemon execution home.', '更改适用于新模型请求，无需重启。正在进行的请求继续使用原录制器。记录写入守护进程执行目录中的 model-log/recordings.jsonl。') }}</p><label class="recording-toggle"><input v-model="enabled" type="checkbox" :disabled="busy || !connection.supports('settings.update')" />{{ s('Record model requests and responses', '录制模型请求和响应') }}</label><p class="ui-banner settings-banner" data-tone="warning">{{ s('Recordings contain prompts, code and tool output. Custom headers may contain secrets. Files are private but have no automatic size limit or rotation.', '录制包含提示词、代码和工具输出。自定义请求头可能包含机密。文件为私有文件，但没有自动大小限制或轮换。') }}</p><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !dirty || !connection.supports('settings.update')">{{ s('Save recording settings', '保存录制设置') }}</button></div></form>
    <section class="settings-card"><h3>{{ s('Process logs', '进程日志') }}</h3><p>{{ s('The daemon writes runtime output to the private daemon.log in its native home when the CLI starts it, and to stderr when it is started directly; the file appends across starts. RUST_LOG sets the filter and is read when the daemon starts, and with it unset the daemon keeps errors only.', 'CLI 启动守护进程时，运行时输出写入其本机目录中的私有 daemon.log；直接启动时写入 stderr，该文件跨多次启动追加。RUST_LOG 设置过滤条件，在守护进程启动时读取；未设置时只保留错误。') }}</p><p>{{ s('This page has no log-mode control. To raise logging, run dimcode daemon stop, then start the daemon from a terminal with the filter set, for example RUST_LOG=debug dimcode.', '本页没有日志模式开关。要提高日志级别，先运行 dimcode daemon stop，再从终端带过滤条件启动守护进程，例如 RUST_LOG=debug dimcode。') }}</p><p>{{ s('The complete record of what a run committed is dimcode export SESSION_ID; read it with daemon.log. Model request recording above is a separate opt-in record of model wire exchanges, not a process-log switch.', '一次运行提交内容的完整记录是 dimcode export SESSION_ID，与 daemon.log 一起阅读。上方的模型请求录制是独立的、需主动启用的模型请求线路记录，不是进程日志开关。') }}</p></section>
    <section class="settings-card"><h3>{{ s('Advertised methods', '已声明的方法') }} ({{ connection.hello?.features.length ?? 0 }})</h3><label><span>{{ s('Filter methods', '筛选方法') }}</span><input v-model="search" class="ui-field" data-size="md" type="search" /></label><ul class="feature-list"><li v-for="method in features" :key="method"><code>{{ method }}</code></li></ul><p v-if="!features.length">{{ s('No matching methods.', '没有匹配的方法。') }}</p></section>
  </article>
</template>

<style scoped>
dl div { display: grid; grid-template-columns: minmax(120px, 1fr) 2fr; gap: var(--space-3); margin-block: var(--space-3); }
dd { margin: 0; overflow-wrap: anywhere; }
dt { color: var(--muted-foreground); }
.recording-toggle { display: flex; align-items: center; gap: var(--space-2); }
.feature-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(230px, 1fr)); gap: var(--space-2); padding: 0; list-style: none; font-size: var(--text-sm); }
</style>
