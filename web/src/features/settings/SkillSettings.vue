<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readPlugins, readSkills, type PluginEntry, type SkillEntry, type SkillsView } from "../../rpc/general-settings"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const catalog = ref<SkillsView>()
const cwd = ref("")
const loadedCwd = ref("")
const search = ref("")
const busy = ref(false)
const error = ref("")
const packages = ref<PluginEntry[]>([])
const sourceType = ref<"path" | "git">("path")
const source = ref("")
const gitRef = ref("")
const installId = ref("")
const dirty = computed(() => !!source.value || !!gitRef.value || !!installId.value)
bindSettingsDirty(dirty)
const entries = computed(() => catalog.value?.entries.filter((skill) => `${skill.name}\n${skill.description}\n${skill.filePath}`.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
function scope(): Record<string, unknown> { return loadedCwd.value ? { cwd: loadedCwd.value } : {} }
async function readPackages(): Promise<void> {
  if (connection.supports("plugin.list")) packages.value = (await connection.client.call("plugin.list", {}, readPlugins)).filter((plugin) => plugin.candidates.some((candidate) => candidate.kinds.includes("skills")))
}
async function load(refresh = false): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  try {
    if (!connection.supports("skills.catalog")) throw new Error(s('Skills management is not available on this server.', '此服务器不支持技能管理。'))
    const nextCwd = cwd.value.trim()
    const params = nextCwd ? { cwd: nextCwd } : {}
    if (refresh) await connection.client.call("skills.refresh", params, readAck)
    const next = await connection.client.call("skills.catalog", params, readSkills)
    catalog.value = next
    loadedCwd.value = nextCwd
    await readPackages()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function toggle(skill: SkillEntry): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call("skills.set_enabled", { ...scope(), id: skill.id, enabled: !skill.enabled }, readAck)
    catalog.value = await connection.client.call("skills.catalog", scope(), readSkills)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function packageAction(method: string, params: Record<string, unknown>): Promise<void> {
  if (busy.value) return
  if (["plugin.enable", "plugin.reload", "plugin.update"].includes(method) && !window.confirm(s('Skill packages may also contain executable plugin code and configured commands. Continue?', '技能包也可能包含可执行插件代码和配置命令。是否继续？'))) return
  if (method === "plugin.remove" && !window.confirm(s('Remove this installed package and all its skills? The original source is not deleted.', '移除此安装包及其全部技能？原始来源不会被删除。'))) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call(method, params, readAck, 180_000)
    if (method === "plugin.install") { source.value = ""; gitRef.value = ""; installId.value = "" }
    await readPackages()
    await connection.client.call("skills.refresh", scope(), readAck)
    catalog.value = await connection.client.call("skills.catalog", scope(), readSkills)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function install(): void {
  if (!source.value.trim()) return
  void packageAction("plugin.install", { source: sourceType.value === "path" ? { path: source.value.trim() } : { git: source.value.trim(), ...(gitRef.value.trim() ? { ref: gitRef.value.trim() } : {}) }, ...(installId.value.trim() ? { id: installId.value.trim() } : {}), enable: false })
}
reloadSettingsOnReconnect(connection, () => load(), dirty)
onMounted(() => load())
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Skills', '技能') }}</h2><p>{{ s('Discover installed skills and choose which the agent may use. Skill files are never edited here.', '发现已安装的技能并选择代理可使用的技能。此处不会编辑技能文件。') }}</p></header>
    <form class="settings-card settings-form" @submit.prevent="load()"><label><span>{{ s('Project directory (optional, absolute path)', '项目目录（可选，绝对路径）') }}</span><input v-model="cwd" class="ui-field" data-size="md" placeholder="/path/to/project" :disabled="busy" /></label><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Load', '加载') }}</button><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy || !connection.supports('skills.refresh')" @click="load(true)">{{ s('Rescan skills', '重新扫描技能') }}</button></div></form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <form class="settings-card settings-form" @submit.prevent="install"><h3>{{ s('Install skill package', '安装技能包') }}</h3><p>{{ s('Install a Dim package from a local directory or Git repository. Its dim-plugin.json declares the skills directory. Installation is disabled by default; review the package, then enable it below.', '从本地目录或 Git 仓库安装 Dim 包。其 dim-plugin.json 声明技能目录。安装后默认禁用；请检查包内容，再在下方启用。') }}</p><fieldset :disabled="busy || !connection.supports('plugin.install')"><label><span>{{ s('Source type', '来源类型') }}</span><select v-model="sourceType" class="ui-field" data-size="md"><option value="path">{{ s('Local directory', '本地目录') }}</option><option value="git">Git</option></select></label><label><span>{{ s('Source', '来源') }}</span><input v-model="source" class="ui-field" data-size="md" required :placeholder="sourceType === 'path' ? '/path/to/skill-package' : 'https://github.com/owner/skills.git'" /></label><label v-if="sourceType === 'git'"><span>{{ s('Git ref (optional)', 'Git 引用（可选）') }}</span><input v-model="gitRef" class="ui-field" data-size="md" /></label><label><span>{{ s('Installation ID (optional)', '安装 ID（可选）') }}</span><input v-model="installId" class="ui-field" data-size="md" /></label><button class="ui-button" data-size="md" data-variant="primary">{{ s('Install package', '安装包') }}</button></fieldset></form>
    <section v-for="plugin in packages" :key="plugin.name" class="settings-card"><h3>{{ plugin.name }}</h3><p>{{ plugin.status ?? (plugin.off ? s('Disabled', '已禁用') : s('Content package', '内容包')) }}</p><p v-if="plugin.error" class="ui-banner" data-tone="danger">{{ plugin.error }}</p><div v-for="candidate in plugin.candidates.filter((item) => item.kinds.includes('skills'))" :key="candidate.id" class="settings-actions"><code>{{ candidate.id }}</code><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || plugin.locked || !connection.supports('plugin.enable')" @click="packageAction('plugin.enable', { name: plugin.name, use: candidate.id })">{{ s('Enable / use', '启用 / 使用') }}</button><button v-if="candidate.git" class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || !connection.supports('plugin.update')" @click="packageAction('plugin.update', { id: candidate.id })">{{ s('Update', '更新') }}</button><button v-if="candidate.id !== 'builtin' && candidate.id !== 'default'" class="ui-button" data-size="sm" data-variant="danger-ghost" :disabled="busy || plugin.locked || !connection.supports('plugin.remove')" @click="packageAction('plugin.remove', { id: candidate.id })">{{ s('Remove package', '移除包') }}</button></div><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || plugin.locked || plugin.off || !connection.supports('plugin.disable')" @click="packageAction('plugin.disable', { name: plugin.name })">{{ s('Disable package', '禁用包') }}</button></section>
    <label class="skill-search"><span>{{ s('Search skills', '搜索技能') }}</span><input v-model="search" class="ui-field" data-size="md" type="search" /></label>
    <p v-for="issue in catalog?.issues" :key="`${issue.path}:${issue.message}`" class="ui-banner settings-banner" data-tone="warning"><code>{{ issue.path }}</code> — {{ issue.message }}</p>
    <section v-for="skill in entries" :key="skill.filePath" class="settings-card"><div class="skill-heading"><h3>{{ skill.name }}</h3><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy || !!skill.shadowedBy || !connection.supports('skills.set_enabled')" @click="toggle(skill)">{{ skill.enabled ? s('Disable', '禁用') : s('Enable', '启用') }}</button></div><p>{{ skill.description }}</p><p><code>{{ skill.filePath }}</code></p><small v-if="skill.allowedTools.length">{{ s('Declared tools', '声明的工具') }}: {{ skill.allowedTools.join(', ') }}</small><p v-if="skill.shadowedBy" class="ui-banner" data-tone="warning">{{ s('Shadowed by', '被以下技能覆盖') }}: {{ skill.shadowedBy }}</p></section>
    <p v-if="catalog && !entries.length" role="status">{{ s('No matching skills. Install a skill package through Plugins or add a skill to a configured skill root, then rescan.', '没有匹配的技能。请通过插件安装技能包或将技能添加到已配置的技能目录，然后重新扫描。') }}</p>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
fieldset label { display: grid; gap: var(--space-2); }
.skill-heading { display: flex; justify-content: space-between; align-items: center; gap: var(--space-3); }
.skill-heading h3 { margin: 0; }
.skill-search { display: grid; gap: var(--space-2); margin-block: var(--space-4); }
code { overflow-wrap: anywhere; }
</style>
