<script setup lang="ts">
import { computed, nextTick, onMounted, ref, useId, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { ArrowUp, Square, X } from "lucide-vue-next"
import type { DraftAttachments, FileCandidate, ImageBlock, ModelId, PermissionPreset, PermissionPolicy } from "../../rpc/protocol"
import { linkMentions, mentionTrigger, type FileMention, type MentionTrigger } from "../../mentions"
import { useModelsStore } from "../../stores/models"
import { useSessionsStore } from "../../stores/sessions"
import ComposerModelMenu from "./ComposerModelMenu.vue"
import ComposerPermissionMenu from "./ComposerPermissionMenu.vue"
import { quoteInto } from "./quote"

/** The Session commands the menu lists, each only when given. Errors are reported by the callee. */
type SessionCommands = {
  compact?: () => Promise<void>
  usage?: () => Promise<void>
  fork?: () => Promise<void>
  /** Receives the words after `/goal`; empty shows the goal. */
  goal?: (words: string) => Promise<void>
  /** Receives the words after `/effort`; empty lists the choices. */
  effort?: (words: string) => Promise<void>
}

/**
 * The message composer: a growing text field, the model menu, and one button
 * that sends, or stops the run while one is in flight. Enter sends,
 * Shift+Enter breaks the line.
 */
const props = withDefaults(defineProps<{
  running: boolean
  stopping?: boolean
  model: ModelId | undefined
  /** False when this connection may not change the model; the menu then only names it. */
  modelSelectable: boolean
  /** Sends the text and the draft's attachments; resolves once the server accepted it. */
  send: (text: string, attachments: DraftAttachments) => Promise<void>
  /** False where a draft cannot carry attachments, as on a new Session's page. */
  attachments?: boolean
  /** True when the Session stores long pastes through `attachment.put`. */
  storesPastes?: boolean
  /** Finds the files an `@` mention offers for a query; absent where the Session serves no file search. */
  searchFiles?: (query: string) => Promise<FileCandidate[]>
  /** The Session policy and its independent controls; absent where permission methods are unavailable. */
  permission?: { policy: PermissionPolicy | undefined; select: (preset: PermissionPreset) => Promise<void> }
  stop?: () => Promise<void>
  selectModel: (model: ModelId) => Promise<void> | void
  /** The narrower line panel: no model menu, less vertical chrome. */
  variant?: "main" | "panel"
  placeholder?: string
  createBranch?: (prompt: string) => Promise<void>
  openLines?: () => void
  /** Session commands the menu lists, each only when given. */
  sessionCommands?: SessionCommands
}>(), { attachments: true })
const { t } = useI18n()
const route = useRoute()
const router = useRouter()

const models = useModelsStore()
const sessions = useSessionsStore()
const field = ref<HTMLTextAreaElement>()
const modelMenu = ref<InstanceType<typeof ComposerModelMenu>>()
const permissionMenu = ref<InstanceType<typeof ComposerPermissionMenu>>()
const menuId = useId()
const text = ref("")
const sending = ref(false)
const changingModel = ref(false)
const error = ref("")
const composing = ref(false)
const focused = ref(false)
const dismissed = ref(false)
/** The files inserted into the draft; each sends as a Markdown link to its path. */
const fileMentions = ref<FileMention[]>([])
const mention = ref<MentionTrigger | undefined>()
const mentionDismissed = ref<string | undefined>()
const candidates = ref<FileCandidate[]>([])
const candidateActive = ref(0)
let searchSeq = 0
let searchTimer: ReturnType<typeof setTimeout> | undefined
const mentionKey = computed(() => mention.value === undefined ? undefined : `${mention.value.start}:${mention.value.query}`)
const mentionOpen = computed(() => focused.value && props.searchFiles !== undefined && mention.value !== undefined && mentionKey.value !== mentionDismissed.value)
const mentionShown = computed(() => mentionOpen.value && candidates.value.length > 0)
const mentionMenuId = useId()
const activeIndex = ref(0)
/** The limits of `crates/dim-cli/src/attachments.rs`: a paste longer than this is stored as a Session attachment. */
const INLINE_PASTE = 16 * 1024
const MAX_PASTE = 10 * 1024 * 1024
const MAX_IMAGES = 20
const images = ref<Array<{ id: number; url: string; block: ImageBlock }>>([])
const pastes = ref<Array<{ id: number; text: string }>>([])
let attachmentSeq = 0

const availableCommands = computed(() => ([
  { category: "model", name: "models", available: props.modelSelectable && models.available && !changingModel.value },
  { category: "session", name: "branch", available: props.createBranch !== undefined },
  { category: "session", name: "compact", available: props.sessionCommands?.compact !== undefined },
  { category: "session", name: "usage", available: props.sessionCommands?.usage !== undefined },
  { category: "session", name: "fork", available: props.sessionCommands?.fork !== undefined },
  { category: "session", name: "goal", available: props.sessionCommands?.goal !== undefined },
  { category: "model", name: "effort", available: props.sessionCommands?.effort !== undefined },
  { category: "model", name: "permissions", available: props.permission !== undefined && props.variant !== "panel" },
  { category: "session", name: "lines", available: props.openLines !== undefined },
  { category: "session", name: "new", available: true },
] as const).filter((command) => command.available))
const commandPrefix = computed(() => /^\/[a-z]*$/i.test(text.value) ? text.value.slice(1).toLowerCase() : undefined)
const matches = computed(() => {
  const prefix = commandPrefix.value
  return prefix === undefined ? [] : availableCommands.value.filter((command) => command.name.startsWith(prefix))
})
const commandMode = computed(() => !composing.value && !dismissed.value && matches.value.length > 0)
const menuOpen = computed(() => focused.value && commandMode.value)

watch(text, () => {
  if (!text.value.startsWith("/")) dismissed.value = false
}, { flush: "sync" })
watch(matches, () => { activeIndex.value = 0 }, { flush: "sync" })

const showStop = computed(() => props.running && props.stop !== undefined && text.value.trim() === "" && images.value.length === 0 && pastes.value.length === 0)
const canSend = computed(() => (text.value.trim() !== "" || images.value.length > 0 || pastes.value.length > 0) && !sending.value)

function resize(): void {
  const element = field.value
  if (element === undefined) return
  element.style.height = "auto"
  element.style.height = `${Math.min(element.scrollHeight, 300)}px`
}

function readImage(file: File): Promise<{ url: string; block: ImageBlock }> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const url = String(reader.result)
      resolve({ url, block: { type: "image", source: { type: "base64", mediaType: file.type, data: url.slice(url.indexOf(",") + 1) } } })
    }
    reader.onerror = () => reject(reader.error ?? new Error(t("composer.imageReadFailed")))
    reader.readAsDataURL(file)
  })
}

async function addImages(files: File[]): Promise<void> {
  for (const file of files) {
    try {
      const image = await readImage(file)
      if (images.value.length >= MAX_IMAGES) {
        error.value = t("composer.imageLimit")
        return
      }
      images.value = [...images.value, { id: ++attachmentSeq, ...image }]
    } catch {
      error.value = t("composer.imageReadFailed")
    }
  }
}

function addPaste(value: string): void {
  if (new TextEncoder().encode(value).length > MAX_PASTE) {
    error.value = t("composer.pasteTooLarge")
    return
  }
  pastes.value = [...pastes.value, { id: ++attachmentSeq, text: value }]
}

function onPaste(event: ClipboardEvent): void {
  if (!props.attachments) return
  const pictures = [...(event.clipboardData?.files ?? [])].filter((file) => file.type.startsWith("image/"))
  if (pictures.length > 0) {
    event.preventDefault()
    void addImages(pictures)
    return
  }
  const value = event.clipboardData?.getData("text/plain") ?? ""
  if (props.storesPastes && new TextEncoder().encode(value).length > INLINE_PASTE) {
    event.preventDefault()
    addPaste(value)
  }
}

function onDrop(event: DragEvent): void {
  event.preventDefault()
  if (!props.attachments) return
  const files = [...(event.dataTransfer?.files ?? [])]
  const pictures = files.filter((file) => file.type.startsWith("image/"))
  if (pictures.length < files.length) error.value = t("composer.onlyImages")
  void addImages(pictures)
}

function removeImage(id: number): void {
  images.value = images.value.filter((image) => image.id !== id)
}

function removePaste(id: number): void {
  pastes.value = pastes.value.filter((paste) => paste.id !== id)
}

/** The `@` mention the caret is in, read from the field's own value (the field is the source of truth for the caret). */
function updateMention(): void {
  const element = field.value
  mention.value = element === undefined || props.searchFiles === undefined ? undefined : mentionTrigger(element.value, element.selectionStart)
}

watch(() => mention.value?.query, (query) => {
  clearTimeout(searchTimer)
  candidateActive.value = 0
  const search = props.searchFiles
  if (query === undefined || search === undefined) {
    candidates.value = []
    return
  }
  const seq = ++searchSeq
  searchTimer = setTimeout(() => {
    search(query).then((found) => {
      if (seq === searchSeq) candidates.value = found
    }, (cause: unknown) => {
      if (seq !== searchSeq) return
      candidates.value = []
      error.value = cause instanceof Error ? cause.message : String(cause)
    })
  }, 80)
})

/** Inserts the chosen file as `@path` (a space after it) or steps into a directory as `@dir/`, as the TUI picker does. */
function acceptCandidate(candidate: FileCandidate): void {
  const element = field.value
  const trigger = mention.value
  if (element === undefined || trigger === undefined) return
  const cursor = element.selectionStart
  const label = `@${candidate.relativePath}${candidate.dir ? "/" : ""}`
  const separator = candidate.dir ? "" : " "
  text.value = `${text.value.slice(0, trigger.start)}${label}${separator}${text.value.slice(cursor)}`
  if (!candidate.dir) fileMentions.value = [...fileMentions.value, { label: `@${candidate.relativePath}`, path: candidate.absolutePath }]
  const caret = trigger.start + label.length + separator.length
  void nextTick(() => {
    element.setSelectionRange(caret, caret)
    resize()
    updateMention()
  })
}

async function submit(): Promise<void> {
  if (!canSend.value || composing.value) return
  if (commandMode.value) {
    const command = matches.value[activeIndex.value]
    if (command !== undefined) await runCommand(command.name)
    return
  }
  const message = text.value
  const branchCommand = !dismissed.value && /^\/branch(?:\s+([\s\S]*))?$/i.exec(message)
  if (branchCommand && !branchCommand[1]?.trim()) {
    await runCommand("branch")
    return
  }
  const goal = props.sessionCommands?.goal
  const goalCommand = !dismissed.value && goal !== undefined ? /^\/goal(?:\s+([\s\S]*))?$/i.exec(message) : null
  if (goal && goalCommand) {
    text.value = ""
    await nextTick()
    resize()
    await goal(goalCommand[1]?.trim() ?? "")
    return
  }
  const effort = props.sessionCommands?.effort
  const effortCommand = !dismissed.value && effort !== undefined ? /^\/effort(?:\s+([\s\S]*))?$/i.exec(message) : null
  if (effort && effortCommand) {
    text.value = ""
    await nextTick()
    resize()
    await effort(effortCommand[1]?.trim() ?? "")
    return
  }
  sending.value = true
  error.value = ""
  try {
    if (branchCommand) {
      if (!props.createBranch) throw new Error(t("composer.branchUnavailable"))
      await props.createBranch(branchCommand[1]!.trim())
    } else {
      const sentMentions = fileMentions.value
      const sentImages = images.value
      const sentPastes = pastes.value
      await props.send(linkMentions(message, sentMentions), { images: sentImages.map((image) => image.block), pastes: sentPastes.map((paste) => paste.text) })
      fileMentions.value = fileMentions.value.filter((mention) => !sentMentions.includes(mention))
      images.value = images.value.filter((image) => !sentImages.includes(image))
      pastes.value = pastes.value.filter((paste) => !sentPastes.includes(paste))
    }
    if (text.value === message) text.value = ""
    await nextTick()
    resize()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    sending.value = false
  }
}

async function stop(): Promise<void> {
  error.value = ""
  try {
    await props.stop?.()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  }
}

async function onModel(model: ModelId): Promise<void> {
  changingModel.value = true
  error.value = ""
  try {
    await props.selectModel(model)
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    changingModel.value = false
  }
}

async function runCommand(name: typeof availableCommands.value[number]["name"]): Promise<void> {
  if (composing.value) return
  error.value = ""
  const argument = name === "branch" || name === "goal" || name === "effort"
  text.value = argument ? `/${name} ` : ""
  await nextTick()
  resize()
  if (argument) field.value?.focus()
  else if (name === "models") modelMenu.value?.open()
  else if (name === "lines") props.openLines?.()
  else if (name === "new") {
    try {
      const cwd = typeof route.params.sessionId === "string" ? sessions.byId.get(route.params.sessionId)?.cwd : undefined
      await router.push({ name: "chat-new", ...(cwd === undefined ? {} : { query: { cwd } }) })
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
    }
  } else if (name === "permissions") permissionMenu.value?.open()
  else if (name === "compact" || name === "usage" || name === "fork") await props.sessionCommands?.[name]?.()
}

function onKeydown(event: KeyboardEvent): void {
  if (event.isComposing || composing.value || event.keyCode === 229) return
  if (mentionOpen.value && candidates.value.length > 0) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault()
      candidateActive.value = (candidateActive.value + (event.key === "ArrowDown" ? 1 : -1) + candidates.value.length) % candidates.value.length
      return
    }
    if ((event.key === "Enter" && !event.shiftKey && !event.altKey) || event.key === "Tab") {
      event.preventDefault()
      acceptCandidate(candidates.value[candidateActive.value]!)
      return
    }
  }
  if (mentionOpen.value && event.key === "Escape") {
    event.preventDefault()
    event.stopPropagation()
    mentionDismissed.value = mentionKey.value
    return
  }
  if (event.key === "Escape" && text.value.startsWith("/") && !dismissed.value) {
    event.preventDefault()
    event.stopPropagation()
    dismissed.value = true
    return
  }
  if (menuOpen.value && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
    event.preventDefault()
    activeIndex.value = (activeIndex.value + (event.key === "ArrowDown" ? 1 : -1) + matches.value.length) % matches.value.length
    return
  }
  if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
    event.preventDefault()
    void submit()
  }
}

/** Quotes `message` at the end of the draft (see quote.ts). */
async function quote(message: string): Promise<void> {
  text.value = quoteInto(text.value, message)
  await nextTick()
  resize()
  const element = field.value
  element?.focus()
  element?.setSelectionRange(text.value.length, text.value.length)
}

onMounted(() => field.value?.focus())
defineExpose({ focus: () => field.value?.focus(), quote })
</script>

<template>
  <section class="composer-panel" :data-variant="variant ?? 'main'" :aria-label="t('chat.composer')">
    <p v-if="error" class="inline-error" role="alert">{{ error }}</p>
    <div class="composer-surface" @click.self="field?.focus()" @dragover.prevent @drop="onDrop">
      <div v-if="images.length > 0 || pastes.length > 0" class="draft-attachments">
        <span v-for="image in images" :key="image.id" class="draft-chip">
          <img :src="image.url" :alt="t('composer.imageAttachment')" class="draft-thumb">
          <button type="button" class="ui-icon-button" data-variant="ghost" data-size="xs" :aria-label="t('composer.removeAttachment')" @click="removeImage(image.id)"><X aria-hidden="true" /></button>
        </span>
        <span v-for="paste in pastes" :key="paste.id" class="draft-chip">
          <span>{{ t("composer.pastedText", { lines: paste.text.split("\n").length }) }}</span>
          <button type="button" class="ui-icon-button" data-variant="ghost" data-size="xs" :aria-label="t('composer.removeAttachment')" @click="removePaste(paste.id)"><X aria-hidden="true" /></button>
        </span>
      </div>
      <div v-if="menuOpen" :id="menuId" class="command-menu ui-popover" role="listbox" :aria-label="t('composer.commands')">
        <button
          v-for="(command, index) in matches"
          :id="`${menuId}-${index}`"
          :key="command.name"
          type="button"
          role="option"
          class="ui-menu-item command-item"
          :aria-selected="index === activeIndex"
          :data-highlighted="index === activeIndex ? '' : undefined"
          @mousedown.prevent
          @mouseenter="activeIndex = index"
          @click.stop="runCommand(command.name)"
        >
          <span class="command-category">{{ index === 0 || matches[index - 1]?.category !== command.category ? t(`composer.category.${command.category}`) : '' }}</span>
          <span class="command-name">/{{ command.name }}<span v-if="command.name === 'branch'" class="command-argument"> &lt;prompt&gt;</span><span v-else-if="command.name === 'goal' || command.name === 'effort'" class="command-argument"> [args]</span></span>
          <span class="command-description">{{ t(`composer.command.${command.name}`) }}</span>
        </button>
        <p class="command-keys">{{ t("composer.commandKeys") }}</p>
      </div>
      <div v-if="mentionShown" :id="mentionMenuId" class="command-menu ui-popover" role="listbox" :aria-label="t('composer.files')">
        <button
          v-for="(candidate, index) in candidates"
          :key="candidate.absolutePath"
          type="button"
          role="option"
          class="ui-menu-item command-item"
          :aria-selected="index === candidateActive"
          :data-highlighted="index === candidateActive ? '' : undefined"
          @mousedown.prevent
          @mouseenter="candidateActive = index"
          @click.stop="acceptCandidate(candidate)"
        >
          <span class="command-name">{{ candidate.relativePath }}<template v-if="candidate.dir">/</template></span>
        </button>
        <p class="command-keys">{{ t("composer.mentionKeys") }}</p>
      </div>
      <textarea
        ref="field"
        v-model="text"
        class="composer-input"
        rows="2"
        :placeholder="placeholder ?? t('composer.sendMessage')"
        :aria-label="placeholder ?? t('composer.sendMessage')"
        :aria-controls="menuOpen ? menuId : mentionShown ? mentionMenuId : undefined"
        :aria-expanded="menuOpen || mentionShown"
        :aria-activedescendant="menuOpen ? `${menuId}-${activeIndex}` : undefined"
        @input="resize(); updateMention()"
        @keydown="onKeydown"
        @keyup="updateMention"
        @click="updateMention"
        @paste="onPaste"
        @focus="focused = true"
        @blur="focused = false"
        @compositionstart="composing = true"
        @compositionend="composing = false"
      />
      <footer class="composer-toolbar">
        <div class="composer-tool-group">
          <ComposerPermissionMenu v-if="permission && variant !== 'panel'" ref="permissionMenu" :policy="permission.policy" @select="permission.select" />
        </div>
        <div class="composer-run-group">
          <ComposerModelMenu v-if="variant !== 'panel'" ref="modelMenu" :model="model" :selectable="modelSelectable" :disabled="changingModel" @select="onModel" />
          <button
            class="ui-icon-button"
            :data-variant="showStop ? 'contrast' : 'primary'"
            data-size="sm"
            type="button"
            :aria-label="showStop ? t('composer.stopResponse') : t('composer.send')"
            v-tooltip="showStop ? t('composer.stopResponse') : t('composer.send')"
            :disabled="showStop ? stopping : !canSend"
            @click.stop="showStop ? stop() : submit()"
          >
            <Square v-if="showStop" fill="currentColor" aria-hidden="true" />
            <ArrowUp v-else :stroke-width="2.25" aria-hidden="true" />
          </button>
        </div>
      </footer>
    </div>
    <div v-if="$slots.status" class="composer-statusbar">
      <div class="composer-status-start"><slot name="status" /></div>
    </div>
  </section>
</template>

<style scoped>
.composer-input { display: block; width: 100%; min-height: 60px; max-height: 300px; resize: none; padding: 0; border: 0; background: transparent; color: var(--foreground); font: var(--text-base)/var(--leading-relaxed) var(--font-sans); outline: none; }
.composer-input::placeholder { color: var(--muted-foreground); }
.composer-panel[data-variant="panel"] { padding: var(--space-2) var(--space-3) var(--space-3); }
.composer-panel[data-variant="panel"] .composer-input { min-height: 48px; }
.command-menu { position: absolute; bottom: calc(100% + var(--space-2)); left: 0; width: 100%; max-width: none; max-height: min(320px, 50dvh); overflow-y: auto; transform-origin: bottom left; --popover-enter-y: 4px; }
.command-item { display: grid; grid-template-columns: 48px 128px minmax(0, 1fr); }
.command-category, .command-description { color: var(--muted-foreground); }
.command-category { font-size: var(--text-xs); }
.command-name { font-family: var(--font-mono); }
.command-argument { color: var(--muted-foreground); font-size: var(--text-xs); }
.draft-attachments { display: flex; flex-wrap: wrap; gap: var(--space-2); padding: var(--space-2) var(--space-3) 0; }
.draft-chip { display: inline-flex; min-width: 0; align-items: center; gap: var(--space-1); padding: 2px var(--space-1) 2px var(--space-2); border: 1px solid var(--border); border-radius: var(--control-sm-radius); background: var(--muted); font-size: var(--text-xs); }
.draft-thumb { width: 36px; height: 36px; border-radius: var(--control-xs-radius); object-fit: cover; }
.command-keys { margin: var(--space-1) 0 0; padding: var(--space-2); border-top: 1px solid var(--border); color: var(--muted-foreground); font-size: var(--text-xs); }

</style>
