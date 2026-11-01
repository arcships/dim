<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import {
  readPlugins, readSkillInstall, readSkillInstallations, readSkillPreview, readSkillSourceText, readSkills,
  type PluginEntry, type SkillEntry, type SkillInstallation, type SkillPreview, type SkillsView,
} from "../../rpc/general-settings"
import { RpcError } from "../../rpc/client"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const SOURCE_TIMEOUT_MS = 180_000

const s = useSettingsText()
const connection = useConnectionStore()
const catalog = ref<SkillsView>()
const cwd = ref("")
const loadedCwd = ref("")
const search = ref("")
const busy = ref(false)
const error = ref("")
const status = ref("")
const packages = ref<PluginEntry[]>([])
const installations = ref<SkillInstallation[]>([])
const installationsError = ref("")
const sourceType = ref<"path" | "git">("path")
const source = ref("")
const gitRef = ref("")
const installId = ref("")
const preview = ref<SkillPreview>()
const previewError = ref("")
const selection = ref<string[]>([])
const packageType = ref<"path" | "git">("path")
const packageSource = ref("")
const packageRef = ref("")
const packageId = ref("")
const dirty = computed(() => !!source.value || !!gitRef.value || !!installId.value || !!packageSource.value || !!packageRef.value || !!packageId.value || selection.value.length > 0)
bindSettingsDirty(dirty)
const entries = computed(() => catalog.value?.entries.filter((skill) => `${skill.name}\n${skill.description}\n${skill.filePath}`.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
function scope(): Record<string, unknown> { return loadedCwd.value ? { cwd: loadedCwd.value } : {} }
function sourceParams(): Record<string, unknown> {
  return sourceType.value === "path" ? { path: source.value.trim() } : { git: source.value.trim(), ...(gitRef.value.trim() ? { ref: gitRef.value.trim() } : {}) }
}
watch([source, gitRef, sourceType, installId], () => { preview.value = undefined; selection.value = [] })
async function loadInstallations(): Promise<void> {
  if (!connection.supports("skills.installations")) return
  try { installations.value = await connection.client.call("skills.installations", {}, readSkillInstallations); installationsError.value = "" }
  catch (cause) { installationsError.value = messageFrom(cause) }
}
async function readPackages(): Promise<void> {
  if (connection.supports("plugin.list")) packages.value = (await connection.client.call("plugin.list", {}, readPlugins)).filter((plugin) => plugin.candidates.some((candidate) => candidate.kinds.includes("skills")))
}
async function readCatalog(): Promise<void> {
  catalog.value = await connection.client.call("skills.catalog", scope(), readSkills)
}
async function load(refresh = false): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  status.value = ""
  try {
    if (!connection.supports("skills.catalog")) throw new Error(s('Skills management is not available on this server.', '此服务器不支持技能管理。'))
    const nextCwd = cwd.value.trim()
    const params = nextCwd ? { cwd: nextCwd } : {}
    if (refresh) await connection.client.call("skills.refresh", params, readAck)
    await readCatalog()
    loadedCwd.value = nextCwd
    await readPackages()
    await loadInstallations()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function toggle(skill: SkillEntry): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  status.value = ""
  try {
    await connection.client.call("skills.set_enabled", { ...scope(), id: skill.id, enabled: !skill.enabled }, readAck)
    await readCatalog()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function action(run: () => Promise<void>): Promise<void> {
  busy.value = true
  error.value = ""
  status.value = ""
  try { await run() } catch (cause) { error.value = messageFrom(cause) } finally { busy.value = false }
}
function loadPreview(): void {
  if (busy.value || !source.value.trim() || !connection.supports("skills.preview")) return
  void action(async () => {
    previewError.value = ""
    try {
      const next = await connection.client.call("skills.preview", { source: sourceParams(), ...(installId.value.trim() ? { id: installId.value.trim() } : {}) }, readSkillPreview, SOURCE_TIMEOUT_MS)
      preview.value = next
      const conflicted = new Set(next.conflicts.map((conflict) => conflict.path))
      selection.value = next.skills.filter((skill) => !conflicted.has(skill.path)).map((skill) => skill.path)
    } catch (cause) { preview.value = undefined; selection.value = []; previewError.value = messageFrom(cause) }
  })
}
function toggleSelection(path: string): void {
  selection.value = selection.value.includes(path) ? selection.value.filter((value) => value !== path) : [...selection.value, path]
}
async function install(overwrite: boolean): Promise<void> {
  const picked = preview.value
  if (busy.value || !picked || !selection.value.length) return
  if (overwrite && !window.confirm(s('Replace the skills this installation installed? Files it no longer selects are deleted; the source checkout is not affected.', '替换此安装已安装的技能？不再选中的文件会被删除；源检出不受影响。'))) return
  busy.value = true
  error.value = ""
  status.value = ""
  try {
    const skills = picked.skills.filter((skill) => selection.value.includes(skill.path)).map((skill) => ({ path: skill.path, revision: skill.revision }))
    const result = await connection.client.call("skills.install", { source: sourceParams(), ...(installId.value.trim() ? { id: installId.value.trim() } : {}), skills, overwrite }, readSkillInstall, SOURCE_TIMEOUT_MS)
    status.value = s(`Installed ${result.installation.skills.length} skill(s) as ${result.installation.id}.`, `已将 ${result.installation.skills.length} 个技能安装为 ${result.installation.id}。`)
    preview.value = undefined
    selection.value = []
    source.value = ""
    gitRef.value = ""
    installId.value = ""
    await readCatalog()
    await readPackages()
    await loadInstallations()
  } catch (cause) {
    if (!overwrite && cause instanceof RpcError && cause.appError === "skills_install_exists") {
      busy.value = false
      await install(true)
      return
    }
    if (cause instanceof RpcError && cause.appError === "skills_preview_stale") previewError.value = messageFrom(cause)
    error.value = messageFrom(cause)
  } finally { busy.value = false }
}
function update(id: string): void {
  if (busy.value || !connection.supports("skills.update")) return
  if (!window.confirm(s(`Update ${id}? Its source is read again and the installed files are replaced.`, `更新 ${id}？将重新读取其来源并替换已安装的文件。`))) return
  void action(async () => {
    const result = await connection.client.call("skills.update", { id }, readSkillInstall, SOURCE_TIMEOUT_MS)
    status.value = result.previous?.revision === result.installation.revision
      ? s(`${id} is already up to date.`, `${id} 已是最新。`)
      : s(`Updated ${id} to ${result.installation.revision.slice(0, 16)}.`, `已将 ${id} 更新到 ${result.installation.revision.slice(0, 16)}。`)
    await readCatalog()
    await loadInstallations()
  })
}
function remove(id: string): void {
  if (busy.value || !connection.supports("skills.remove")) return
  if (!window.confirm(s(`Remove ${id} and the skills it installed? The source is not deleted.`, `移除 ${id} 及其安装的技能？来源不会被删除。`))) return
  void action(async () => {
    await connection.client.call("skills.remove", { id }, readAck, SOURCE_TIMEOUT_MS)
    status.value = s(`Removed ${id}.`, `已移除 ${id}。`)
    await readCatalog()
    await loadInstallations()
  })
}
async function packageAction(method: string, params: Record<string, unknown>): Promise<void> {
  if (busy.value) return
  if (["plugin.enable", "plugin.reload", "plugin.update"].includes(method) && !window.confirm(s('Skill packages may also contain executable plugin code and configured commands. Continue?', '技能包也可能包含可执行插件代码和配置命令。是否继续？'))) return
  if (method === "plugin.remove" && !window.confirm(s('Remove this installed package and all its skills? The original source is not deleted.', '移除此安装包及其全部技能？原始来源不会被删除。'))) return
  busy.value = true
  error.value = ""
  status.value = ""
  try {
    await connection.client.call(method, params, readAck, SOURCE_TIMEOUT_MS)
    if (method === "plugin.install") { packageSource.value = ""; packageRef.value = ""; packageId.value = "" }
    await readPackages()
    await connection.client.call("skills.refresh", scope(), readAck)
    await readCatalog()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function installPackage(): void {
  if (!packageSource.value.trim()) return
  void packageAction("plugin.install", { source: packageType.value === "path" ? { path: packageSource.value.trim() } : { git: packageSource.value.trim(), ...(packageRef.value.trim() ? { ref: packageRef.value.trim() } : {}) }, ...(packageId.value.trim() ? { id: packageId.value.trim() } : {}), enable: false })
}
reloadSettingsOnReconnect(connection, () => load(), dirty)
onMounted(() => load())
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Skills', '技能') }}</h2><p>{{ s('Discover installed skills and choose which the agent may use. Skill files are never edited here.', '发现已安装的技能并选择代理可使用的技能。此处不会编辑技能文件。') }}</p></header>
    <form class="settings-card settings-form" @submit.prevent="load()"><label><span>{{ s('Project directory (optional, absolute path)', '项目目录（可选，绝对路径）') }}</span><input v-model="cwd" class="ui-field" data-size="md" placeholder="/path/to/project" :disabled="busy" /></label><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Load', '加载') }}</button><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy || !connection.supports('skills.refresh')" @click="load(true)">{{ s('Rescan skills', '重新扫描技能') }}</button></div></form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <p v-if="status" role="status" class="settings-banner">{{ status }}</p>

    <form class="settings-card settings-form" @submit.prevent="loadPreview">
      <h3>{{ s('Install from a source', '从来源安装') }}</h3>
      <p>{{ s('Preview the skills of a local directory or Git repository, without a plugin manifest, then install only the ones you choose into your user skills.', '预览本地目录或 Git 仓库中的技能（无需插件清单），然后只把你选中的技能安装到用户技能目录。') }}</p>
      <fieldset :disabled="busy || !connection.supports('skills.preview')">
        <label><span>{{ s('Source type', '来源类型') }}</span><select v-model="sourceType" class="ui-field" data-size="md"><option value="path">{{ s('Local directory', '本地目录') }}</option><option value="git">Git</option></select></label>
        <label><span>{{ s('Source', '来源') }}</span><input v-model="source" class="ui-field" data-size="md" required :placeholder="sourceType === 'path' ? '/path/to/skills' : 'https://github.com/owner/skills.git'" /></label>
        <label v-if="sourceType === 'git'"><span>{{ s('Git ref (optional)', 'Git 引用（可选）') }}</span><input v-model="gitRef" class="ui-field" data-size="md" /></label>
        <label><span>{{ s('Installation ID (optional)', '安装 ID（可选）') }}</span><input v-model="installId" class="ui-field" data-size="md" :placeholder="s('derived from the source', '由来源推导')" /></label>
        <button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !source.trim()">{{ s('Preview', '预览') }}</button>
      </fieldset>
      <p v-if="previewError" class="ui-banner" data-tone="danger">{{ previewError }}</p>
      <div v-if="preview">
        <p><code>{{ readSkillSourceText(preview.source) }}</code></p>
        <p v-for="issue in preview.issues" :key="`${issue.path}:${issue.message}`" class="ui-banner" data-tone="warning"><code>{{ issue.path }}</code> — {{ issue.message }}</p>
        <p v-for="conflict in preview.conflicts" :key="`conflict:${conflict.path}`" class="ui-banner" data-tone="warning">{{ s('Conflicts with', '与以下技能冲突') }} <code>{{ conflict.by }}</code>: {{ conflict.name }}</p>
        <p v-for="shadow in preview.shadows" :key="`shadow:${shadow.path}`" class="ui-banner" data-tone="warning">{{ s('Will hide', '将隐藏') }} <code>{{ shadow.by }}</code></p>
        <ul class="skill-preview">
          <li v-for="skill in preview.skills" :key="skill.path">
            <label><input type="checkbox" :checked="selection.includes(skill.path)" :disabled="busy" @change="toggleSelection(skill.path)" /> <strong>{{ skill.name }}</strong></label>
            <p>{{ skill.description }}</p>
            <code>{{ skill.path }}</code>
          </li>
        </ul>
        <div class="settings-actions">
          <button class="ui-button" data-size="md" data-variant="primary" type="button" :disabled="busy || !selection.length || !connection.supports('skills.install')" @click="install(false)">{{ s('Install selected', '安装选中项') }}</button>
          <small>{{ s(`${selection.length} of ${preview.skills.length} selected`, `已选 ${selection.length} / ${preview.skills.length}`) }}</small>
        </div>
      </div>
    </form>

    <section class="settings-card">
      <h3>{{ s('Installed sources', '已安装来源') }}</h3>
      <p v-if="installationsError" class="ui-banner" data-tone="danger">{{ installationsError }}</p>
      <div v-for="item in installations" :key="item.id" class="skill-installation">
        <div class="skill-heading"><h4>{{ item.id }}</h4><div class="settings-actions"><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || !connection.supports('skills.update')" @click="update(item.id)">{{ s('Update', '更新') }}</button><button class="ui-button" data-size="sm" data-variant="danger-ghost" :disabled="busy || !connection.supports('skills.remove')" @click="remove(item.id)">{{ s('Remove', '移除') }}</button></div></div>
        <p><code>{{ readSkillSourceText(item.source) }}</code></p>
        <small>{{ item.skills.map((skill) => skill.path).join(', ') }}</small>
      </div>
      <p v-if="!installations.length && connection.supports('skills.installations')" role="status">{{ s('Nothing is installed from a source.', '还没有从来源安装任何技能。') }}</p>
    </section>

    <label class="skill-search"><span>{{ s('Search skills', '搜索技能') }}</span><input v-model="search" class="ui-field" data-size="md" type="search" /></label>
    <p v-for="issue in catalog?.issues" :key="`${issue.path}:${issue.message}`" class="ui-banner settings-banner" data-tone="warning"><code>{{ issue.path }}</code> — {{ issue.message }}</p>
    <section v-for="skill in entries" :key="skill.filePath" class="settings-card"><div class="skill-heading"><h3>{{ skill.name }}</h3><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy || !!skill.shadowedBy || !connection.supports('skills.set_enabled')" @click="toggle(skill)">{{ skill.enabled ? s('Disable', '禁用') : s('Enable', '启用') }}</button></div><p>{{ skill.description }}</p><p><code>{{ skill.filePath }}</code></p><small v-if="skill.allowedTools.length">{{ s('Declared tools', '声明的工具') }}: {{ skill.allowedTools.join(', ') }}</small><p v-if="skill.shadowedBy" class="ui-banner" data-tone="warning">{{ s('Shadowed by', '被以下技能覆盖') }}: {{ skill.shadowedBy }}</p></section>
    <p v-if="catalog && !entries.length" role="status">{{ s('No matching skills. Install a source above, install a skill package through Plugins, or add a skill to a configured skill root, then rescan.', '没有匹配的技能。请在上方安装来源、通过插件安装技能包，或将技能添加到已配置的技能目录，然后重新扫描。') }}</p>

    <section v-for="plugin in packages" :key="plugin.name" class="settings-card"><h3>{{ plugin.name }}</h3><p>{{ plugin.status ?? (plugin.off ? s('Disabled', '已禁用') : s('Content package', '内容包')) }}</p><p v-if="plugin.error" class="ui-banner" data-tone="danger">{{ plugin.error }}</p><div v-for="candidate in plugin.candidates.filter((item) => item.kinds.includes('skills'))" :key="candidate.id" class="settings-actions"><code>{{ candidate.id }}</code><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || plugin.locked || !connection.supports('plugin.enable')" @click="packageAction('plugin.enable', { name: plugin.name, use: candidate.id })">{{ s('Enable / use', '启用 / 使用') }}</button><button v-if="candidate.git" class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || !connection.supports('plugin.update')" @click="packageAction('plugin.update', { id: candidate.id })">{{ s('Update', '更新') }}</button><button v-if="candidate.id !== 'builtin' && candidate.id !== 'default'" class="ui-button" data-size="sm" data-variant="danger-ghost" :disabled="busy || plugin.locked || !connection.supports('plugin.remove')" @click="packageAction('plugin.remove', { id: candidate.id })">{{ s('Remove package', '移除包') }}</button></div><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy || plugin.locked || plugin.off || !connection.supports('plugin.disable')" @click="packageAction('plugin.disable', { name: plugin.name })">{{ s('Disable package', '禁用包') }}</button></section>

    <form class="settings-card settings-form" @submit.prevent="installPackage"><h3>{{ s('Install a skill package', '安装技能包') }}</h3><p>{{ s('A Dim package declares its skills directory in dim-plugin.json and may also carry plugin code. A repository without a manifest is installed from the source preview above.', 'Dim 包在 dim-plugin.json 中声明技能目录，也可以包含插件代码。没有清单的仓库请使用上方的来源预览安装。') }}</p><fieldset :disabled="busy || !connection.supports('plugin.install')"><label><span>{{ s('Source type', '来源类型') }}</span><select v-model="packageType" class="ui-field" data-size="md"><option value="path">{{ s('Local directory', '本地目录') }}</option><option value="git">Git</option></select></label><label><span>{{ s('Source', '来源') }}</span><input v-model="packageSource" class="ui-field" data-size="md" required :placeholder="packageType === 'path' ? '/path/to/skill-package' : 'https://github.com/owner/skills.git'" /></label><label v-if="packageType === 'git'"><span>{{ s('Git ref (optional)', 'Git 引用（可选）') }}</span><input v-model="packageRef" class="ui-field" data-size="md" /></label><label><span>{{ s('Installation ID (optional)', '安装 ID（可选）') }}</span><input v-model="packageId" class="ui-field" data-size="md" /></label><button class="ui-button" data-size="md" data-variant="primary">{{ s('Install package', '安装包') }}</button></fieldset></form>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
fieldset label { display: grid; gap: var(--space-2); }
.skill-heading { display: flex; justify-content: space-between; align-items: center; gap: var(--space-3); }
.skill-heading h3, .skill-heading h4 { margin: 0; }
.skill-search { display: grid; gap: var(--space-2); margin-block: var(--space-4); }
.skill-preview { display: grid; gap: var(--space-3); list-style: none; margin: 0; padding: 0; }
.skill-preview li { display: grid; gap: var(--space-1); }
.skill-preview p { margin: 0; }
.skill-installation { display: grid; gap: var(--space-2); padding-block: var(--space-3); border-top: 1px solid var(--border); }
.skill-installation:first-of-type { border-top: 0; }
code { overflow-wrap: anywhere; }
</style>
