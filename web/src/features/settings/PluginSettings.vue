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
const sourceTypeItems = computed(() => [
  { value: "path", label: s('Local directory', '本地目录') },
  { value: "git", label: "Git" },
])
function candidateItems(plugin: PluginEntry) {
  return plugin.candidates.map((candidate) => ({
    value: candidate.id,
    label: `${candidate.id} (${candidate.kinds.join(', ')})`,
  }))
}
/** Dim ships it but did not install it: the page offers Install, not Enable. */
const installable = (plugin: PluginEntry): boolean => !!plugin.optional && !plugin.candidates.length
// An optional install waits for Node and npm, which the 180 s of a path or
// Git install does not cover (PluginManager §3.2).
const OPTIONAL_INSTALL_TIMEOUT_MS = 15 * 60_000
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
async function action(method: string, params: Record<string, unknown>, timeoutMs = 180_000): Promise<void> {
  if (busy.value) return
  if (["plugin.install", "plugin.enable", "plugin.update", "plugin.reload"].includes(method) && !window.confirm(s('Plugins execute trusted code and configured commands with your user privileges. Continue?', '插件会以您的用户权限执行受信任代码和配置命令。是否继续？'))) return
  if (method === "plugin.remove" && !window.confirm(s('Remove this installed plugin? Its content will be withdrawn.', '移除此已安装插件？其提供的内容也将撤回。'))) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call(method, params, readAck, timeoutMs)
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
    <form class="settings-card settings-form" @submit.prevent="install">
      <h3>{{ s('Install plugin', '安装插件') }}</h3>
      <fieldset :disabled="busy || !connection.supports('plugin.install')">
        <UFormField :label="s('Source type', '来源类型')"><USelect v-model="sourceType" :items="sourceTypeItems" /></UFormField>
        <UFormField :label="s('Source', '来源')"><UInput v-model="source" :placeholder="sourceType === 'path' ? '/path/to/plugin' : 'https://github.com/owner/plugin.git'" required /></UFormField>
        <UFormField v-if="sourceType === 'git'" :label="s('Git ref (optional)', 'Git 引用（可选）')"><UInput v-model="gitRef" /></UFormField>
        <UFormField :label="s('Installation ID (optional)', '安装 ID（可选）')"><UInput v-model="installId" /></UFormField>
        <UCheckbox v-model="enable" :label="s('Enable after installation (executes plugin code)', '安装后启用（执行插件代码）')" />
        <div><UButton type="submit" color="primary">{{ s('Install', '安装') }}</UButton></div>
      </fieldset>
    </form>
    <UFormField class="search-label" :label="s('Search plugins', '搜索插件')"><UInput v-model="search" type="search" /></UFormField>
    <section v-for="plugin in entries" :key="plugin.name" class="settings-card">
      <h3>{{ plugin.name }} <small v-if="plugin.locked">{{ s('Locked', '已锁定') }}</small></h3>
      <p>{{ plugin.status ?? (plugin.off ? 'off' : 'content') }}<template v-if="plugin.use"> · {{ plugin.use }}</template></p>
      <UAlert v-if="plugin.error" class="settings-banner" color="error" variant="soft" :description="plugin.error" />
      <UAlert v-for="refusal in plugin.refused" :key="refusal.kind" class="settings-banner" color="error" variant="soft" :description="`${refusal.kind}: ${refusal.error}`" />
      <div class="settings-actions">
        <USelect v-if="plugin.candidates.length" v-model="choices[plugin.name]" :items="candidateItems(plugin)" :aria-label="s('Implementation', '实现')" :disabled="busy || plugin.locked" />
        <UButton v-if="installable(plugin)" color="neutral" variant="soft" :disabled="busy || !connection.supports('plugin.install')" @click="action('plugin.install', { source: { optional: plugin.name } }, OPTIONAL_INSTALL_TIMEOUT_MS)">{{ s('Install', '安装') }}</UButton>
        <UButton v-else color="neutral" variant="soft" :disabled="busy || plugin.locked || !choices[plugin.name] || !connection.supports('plugin.enable')" @click="action('plugin.enable', { name: plugin.name, use: choices[plugin.name] })">{{ s('Enable / use', '启用 / 使用') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy || plugin.locked || plugin.off || !connection.supports('plugin.disable')" @click="action('plugin.disable', { name: plugin.name })">{{ s('Disable', '禁用') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy || plugin.off || !connection.supports('plugin.reload')" @click="action('plugin.reload', { name: plugin.name })">{{ s('Reload', '重新加载') }}</UButton>
      </div>
      <div v-for="candidate in plugin.candidates.filter((item) => item.id !== 'builtin' && item.id !== 'default')" :key="candidate.id" class="candidate-row">
        <code>{{ candidate.id }}</code>
        <UButton v-if="candidate.git" size="sm" color="neutral" variant="soft" :disabled="busy || !connection.supports('plugin.update')" @click="action('plugin.update', { id: candidate.id })">{{ s('Update', '更新') }}</UButton>
        <UButton size="sm" color="error" variant="ghost" :disabled="busy || plugin.locked || !connection.supports('plugin.remove')" @click="action('plugin.remove', { id: candidate.id })">{{ s('Remove', '移除') }}</UButton>
      </div>
    </section>
    <p v-if="plugins && !entries.length" role="status">{{ s('No matching plugins.', '没有匹配的插件。') }}</p>
    <UButton color="neutral" variant="soft" :disabled="busy" @click="load">{{ s('Refresh', '刷新') }}</UButton>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
.candidate-row { display: flex; align-items: center; gap: var(--space-3); margin-top: var(--space-3); flex-wrap: wrap; }
.search-label { margin-block: var(--space-4); }
</style>
