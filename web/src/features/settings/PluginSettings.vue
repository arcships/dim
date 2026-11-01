<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readPlugins, type PluginEntry } from "../../rpc/general-settings"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const plugins = ref<PluginEntry[]>()
const choices = ref<Record<string, string>>({})
const sourceType = ref<"path" | "git">("path")
const source = ref("")
const gitRef = ref("")
const installId = ref("")
const enable = ref(false)
const search = ref("")
const busy = ref(false)
const error = ref("")
const dirty = computed(() => !!source.value || !!gitRef.value || !!installId.value)
bindSettingsDirty(dirty)
const entries = computed(() => plugins.value?.filter((plugin) => plugin.name.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
async function read(): Promise<void> {
  plugins.value = await connection.client.call("plugin.list", {}, readPlugins)
  choices.value = Object.fromEntries(plugins.value.map((plugin) => [plugin.name, plugin.use ?? plugin.candidates[0]?.id ?? ""]))
}
async function load(): Promise<void> {
  busy.value = true
  error.value = ""
  try {
    if (!connection.supports("plugin.list")) throw new Error(s('Plugin management is not available on this server.', '此服务器不支持插件管理。'))
    await read()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function action(method: string, params: Record<string, unknown>): Promise<void> {
  if (busy.value) return
  if (["plugin.install", "plugin.enable", "plugin.update", "plugin.reload"].includes(method) && !window.confirm(s('Plugins execute trusted code and configured commands with your user privileges. Continue?', '插件会以您的用户权限执行受信任代码和配置命令。是否继续？'))) return
  if (method === "plugin.remove" && !window.confirm(s('Remove this installed plugin? Its content will be withdrawn.', '移除此已安装插件？其提供的内容也将撤回。'))) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call(method, params, readAck, 180_000)
    if (method === "plugin.install") { source.value = ""; gitRef.value = ""; installId.value = "" }
    await read()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function install(): void {
  const location = source.value.trim()
  if (!location) return
  void action("plugin.install", { source: sourceType.value === "path" ? { path: location } : { git: location, ...(gitRef.value.trim() ? { ref: gitRef.value.trim() } : {}) }, ...(installId.value.trim() ? { id: installId.value.trim() } : {}), enable: enable.value })
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Plugins', '插件') }}</h2><p>{{ s('Install local or Git packages, select implementations and manage their lifecycle. Locked system plugins cannot be disabled or replaced.', '安装本地或 Git 包、选择实现并管理生命周期。锁定的系统插件不可禁用或替换。') }}</p></header>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load" />
    <form class="settings-card settings-form" @submit.prevent="install"><h3>{{ s('Install plugin', '安装插件') }}</h3><fieldset :disabled="busy || !connection.supports('plugin.install')"><label><span>{{ s('Source type', '来源类型') }}</span><select v-model="sourceType" class="ui-field" data-size="md"><option value="path">{{ s('Local directory', '本地目录') }}</option><option value="git">Git</option></select></label><label><span>{{ s('Source', '来源') }}</span><input v-model="source" class="ui-field" data-size="md" :placeholder="sourceType === 'path' ? '/path/to/plugin' : 'https://github.com/owner/plugin.git'" required /></label><label v-if="sourceType === 'git'"><span>{{ s('Git ref (optional)', 'Git 引用（可选）') }}</span><input v-model="gitRef" class="ui-field" data-size="md" /></label><label><span>{{ s('Installation ID (optional)', '安装 ID（可选）') }}</span><input v-model="installId" class="ui-field" data-size="md" /></label><label class="checkbox-label"><input v-model="enable" type="checkbox" />{{ s('Enable after installation (executes plugin code)', '安装后启用（执行插件代码）') }}</label><button class="ui-button" data-size="md" data-variant="primary">{{ s('Install', '安装') }}</button></fieldset></form>
    <label class="search-label"><span>{{ s('Search plugins', '搜索插件') }}</span><input v-model="search" class="ui-field" data-size="md" type="search" /></label>
    <section v-for="plugin in entries" :key="plugin.name" class="settings-card"><h3>{{ plugin.name }} <small v-if="plugin.locked">{{ s('Locked', '已锁定') }}</small></h3><p>{{ plugin.status ?? (plugin.off ? 'off' : 'content') }}<template v-if="plugin.use"> · {{ plugin.use }}</template></p><p v-if="plugin.error" class="ui-banner" data-tone="danger">{{ plugin.error }}</p><p v-for="refusal in plugin.refused" :key="refusal.kind" class="ui-banner" data-tone="danger">{{ refusal.kind }}: {{ refusal.error }}</p><div class="settings-actions"><select v-model="choices[plugin.name]" class="ui-field" data-size="md" :aria-label="s('Implementation', '实现')" :disabled="busy || plugin.locked"><option v-for="candidate in plugin.candidates" :key="candidate.id" :value="candidate.id">{{ candidate.id }} ({{ candidate.kinds.join(', ') }})</option></select><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || plugin.locked || !choices[plugin.name] || !connection.supports('plugin.enable')" @click="action('plugin.enable', { name: plugin.name, use: choices[plugin.name] })">{{ s('Enable / use', '启用 / 使用') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || plugin.locked || plugin.off || !connection.supports('plugin.disable')" @click="action('plugin.disable', { name: plugin.name })">{{ s('Disable', '禁用') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || plugin.off || !connection.supports('plugin.reload')" @click="action('plugin.reload', { name: plugin.name })">{{ s('Reload', '重新加载') }}</button></div><div v-for="candidate in plugin.candidates.filter((item) => item.id !== 'builtin' && item.id !== 'default')" :key="candidate.id" class="candidate-row"><code>{{ candidate.id }}</code><button v-if="candidate.git" class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || !connection.supports('plugin.update')" @click="action('plugin.update', { id: candidate.id })">{{ s('Update', '更新') }}</button><button class="ui-button" data-size="sm" data-variant="danger-ghost" :disabled="busy || plugin.locked || !connection.supports('plugin.remove')" @click="action('plugin.remove', { id: candidate.id })">{{ s('Remove', '移除') }}</button></div></section>
    <p v-if="plugins && !entries.length" role="status">{{ s('No matching plugins.', '没有匹配的插件。') }}</p>
    <button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy" @click="load">{{ s('Refresh', '刷新') }}</button>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
label { display: grid; gap: var(--space-2); }
.checkbox-label, .candidate-row { display: flex; align-items: center; gap: var(--space-3); }
.candidate-row { margin-top: var(--space-3); flex-wrap: wrap; }
.search-label { margin-block: var(--space-4); }
</style>
