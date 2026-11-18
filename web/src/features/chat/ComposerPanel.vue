<script setup lang="ts">
import { computed, nextTick, onActivated, onMounted, onUnmounted, ref, useId, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import type { DraftAttachments, FileCandidate, ModelId, PermissionPreset, PermissionPolicy, QueuedInput, RestoredImage, SubmitMode, UserInput, WithdrawResult } from "../../rpc/protocol"
import { linkMentions, mentionTrigger, type FileMention, type MentionTrigger } from "../../mentions"
import { SKILL_TOKEN_PREFIX, namesSkillToken, skillCommandQuery, skillSegments, startsWithSkillToken } from "../../skill-token"
import { useComposerStore } from "../../stores/composer"
import { useModels } from "../../stores/models"
import { useSessionsStore } from "../../stores/sessions"
import { useConnectionStore } from "../../stores/connection"
import ComposerModelMenu from "./ComposerModelMenu.vue"
import ComposerPermissionMenu from "./ComposerPermissionMenu.vue"
import type { ModelMenuPreview } from "./new-session-model"
import { quoteInto } from "./quote"

/** The Session commands the menu lists, each only when given. Errors are reported by the callee. */
type SessionCommands = {
  compact?: () => Promise<void>
  usage?: () => Promise<void>
  fork?: () => Promise<void>
  /** Receives the words after `/goal`; empty shows the goal. */
  goal?: (words: string) => Promise<void>
  /** Receives the words after `/loop`; empty lists the line's loops. */
  loop?: (words: string) => Promise<void>
  /** Receives the words after `/effort`; empty lists the choices. */
  effort?: (words: string) => Promise<void>
}

/**
 * The message composer: a growing text field, the model menu, and the actions
 * that send, hold or stop. Enter sends, Shift+Enter breaks the line. During a
 * run the default action steers into the running turn unless this browser
 * chose holding as its default, the other action does the opposite, and
 * `Ctrl+Enter` (`⌘+Enter` on macOS) stops the run and sends the draft with
 * every held message recalled ahead of it (web-workspace §4).
 */
const props = withDefaults(defineProps<{
  running: boolean
  stopping?: boolean
  model: ModelId | undefined
  /**
   * The new Session's model resolution: the state the menu shows and the two
   * actions it offers. Absent where the Session's own model is authoritative.
   */
  modelPreview?: ModelMenuPreview
  /** False when this connection may not change the model; the menu then only names it. */
  modelSelectable: boolean
  /** Sends the text and the draft's attachments in `mode`; resolves once the server accepted it. */
  send: (text: string, attachments: DraftAttachments, mode: SubmitMode) => Promise<void>
  /** False where a draft cannot carry attachments, as on a new Session's page. */
  attachments?: boolean
  /** True when the Session stores long pastes through `attachment.put`. */
  storesPastes?: boolean
  /** Finds the files an `@` mention offers for a query; absent where the Session serves no file search. */
  searchFiles?: (query: string) => Promise<FileCandidate[]>
  /** The Session policy and its independent controls; absent where permission methods are unavailable. */
  permission?: { policy: PermissionPolicy | undefined; select: (preset: PermissionPreset) => Promise<void> }
  stop?: () => Promise<void>
  /**
   * The messages the server holds for this branch, oldest first: the composer
   * recalls them into the draft through `withdrawHeld`.
   */
  held?: readonly QueuedInput[]
  /** Withdraws one held message; absent where the Session serves no `session.input.withdraw`. */
  withdrawHeld?: (input: QueuedInput) => Promise<WithdrawResult["status"]>
  /** The Session whose stored image files a recalled message names, for its thumbnail. */
  sessionId?: string
  selectModel: (model: ModelId) => Promise<void> | void
  /** The narrower line panel: no model menu, less vertical chrome. */
  variant?: "main" | "panel"
  placeholder?: string
  createBranch?: (prompt: string) => Promise<void>
  openLines?: () => void
  /** Session commands the menu lists, each only when given. */
  sessionCommands?: SessionCommands
  /** The Skills the menu offers by `/skill:<name>`, from `session.skills`; absent where the Session serves none. */
  skills?: ReadonlyArray<{ name: string; description: string }>
}>(), { attachments: true })
const { t } = useI18n()
const route = useRoute()
const router = useRouter()

const models = useModels()
const sessions = useSessionsStore()
const connection = useConnectionStore()
const field = ref<HTMLTextAreaElement>()
const highlightBody = ref<HTMLElement>()
const modelMenu = ref<InstanceType<typeof ComposerModelMenu>>()
const permissionMenu = ref<InstanceType<typeof ComposerPermissionMenu>>()
const menuId = useId()
const text = ref("")
const sending = ref(false)
const uploadProgress = ref<number>()
const changingModel = ref(false)
const error = ref("")
const composing = ref(false)
const focused = ref(false)
const dismissed = ref(false)
/** The draft's words, with each `/skill:<name>` token marked for the highlight layer. */
const highlight = computed(() => skillSegments(text.value))
/** The files inserted into the draft; each sends as a Markdown link to its path. */
const fileMentions = ref<FileMention[]>([])
const mention = ref<MentionTrigger | undefined>()
const mentionDismissed = ref<string | undefined>()
const candidates = ref<FileCandidate[]>([])
const candidateActive = ref(0)
let searchSeq = 0
/** Keeps the chip layer on the field when the window or the panel changes its width. */
let observer: ResizeObserver | undefined
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
const images = ref<Array<{ id: number; url: string; file: File }>>([])
const pastes = ref<Array<{ id: number; text: string }>>([])
/**
 * A recalled image: the protocol's anchor, plus the send-now gesture that recalled
 * it. The gesture decides two things text offsets cannot: whether the image moves
 * past text inserted at its own offset, and which of several images at one offset
 * comes first.
 */
type RecalledImage = RestoredImage & { batch: number }

/** The stored images a recalled message carried, each with the draft offset it keeps its place at. */
const restored = ref<RecalledImage[]>([])
let attachmentSeq = 0
const composerSettings = useComposerStore()
/**
 * A `Ctrl+Enter` whose withdrawals and stop are out: the draft it merged waits
 * for the stop's reply, and no action sends anything until then.
 */
const sendNow = ref(false)
/** The messages the server holds for this branch, oldest first. */
const held = computed(() => props.held ?? [])
/** The mode a draft goes out in during a run, and the other action's mode. */
const alternateMode = computed<SubmitMode>(() => composerSettings.alternate())

const availableCommands = computed(() => ([
  { category: "model", name: "models", available: props.modelSelectable && models.available && !changingModel.value },
  { category: "session", name: "branch", available: props.createBranch !== undefined },
  { category: "session", name: "compact", available: props.sessionCommands?.compact !== undefined },
  { category: "session", name: "usage", available: props.sessionCommands?.usage !== undefined },
  { category: "session", name: "fork", available: props.sessionCommands?.fork !== undefined },
  { category: "session", name: "goal", available: props.sessionCommands?.goal !== undefined },
  { category: "session", name: "loop", available: props.sessionCommands?.loop !== undefined },
  { category: "model", name: "effort", available: props.sessionCommands?.effort !== undefined },
  { category: "model", name: "permissions", available: props.permission !== undefined && props.variant !== "panel" },
  { category: "session", name: "lines", available: props.openLines !== undefined },
  { category: "session", name: "new", available: true },
  { category: "session", name: "scheduled", available: connection.supports("schedule.list") },
] as const).filter((command) => command.available))
const commandPrefix = computed(() => /^\/[a-z]*(?::[A-Za-z0-9_.-]*)?$/i.test(text.value) ? text.value.slice(1) : undefined)
/** The Skills the menu offers: enabled, unshadowed and user-invocable is the caller's filter; a name the token cannot carry is left out here. */
const offeredSkills = computed(() => (props.skills ?? []).filter((skill) => namesSkillToken(skill.name)))
const skillMatches = computed(() => {
  const prefix = commandPrefix.value
  if (prefix === undefined) return []
  const { name, skillsOnly } = skillCommandQuery(`/${prefix}`)
  return offeredSkills.value.filter((skill) => skill.name.toLowerCase().startsWith(name) && (skillsOnly || name.length > 0))
})
const matches = computed(() => {
  const prefix = commandPrefix.value
  if (prefix === undefined) return []
  const { name, skillsOnly } = skillCommandQuery(`/${prefix}`)
  if (skillsOnly) return []
  return availableCommands.value.filter((command) => command.name.startsWith(name))
})
type MenuRow =
  | { kind: "command"; key: string; category: string; name: string; description: string; command: (typeof availableCommands.value)[number] }
  | { kind: "skill"; key: string; category: "skill"; name: string; description: string }

/** One list the menu draws: commands first, then Skills, as the TUI orders them. */
const menuRows = computed<MenuRow[]>(() => [
  ...matches.value.map((command) => ({ kind: "command" as const, key: `/command/${command.name}`, category: command.category, name: `/${command.name}`, description: t(`composer.command.${command.name}`), command })),
  ...skillMatches.value.map((skill) => ({ kind: "skill" as const, key: `/skill/${skill.name}`, category: "skill" as const, name: `${SKILL_TOKEN_PREFIX}${skill.name}`, description: skill.description })),
])
const commandMode = computed(() => !composing.value && !dismissed.value && menuRows.value.length > 0)
const menuOpen = computed(() => focused.value && commandMode.value)

watch(text, () => {
  if (!text.value.startsWith("/")) dismissed.value = false
}, { flush: "sync" })
watch(matches, () => { activeIndex.value = 0 }, { flush: "sync" })

const hasDraft = computed(() => text.value.trim() !== "" || images.value.length > 0 || pastes.value.length > 0 || restored.value.length > 0)
const showStop = computed(() => props.running && props.stop !== undefined && !hasDraft.value)
const canSend = computed(() => hasDraft.value && !sending.value)
/** Where the last draft sent during this run went; cleared when the run ends, with the recalled identities. */
const destination = ref<SubmitMode | undefined>()
/**
 * Held messages this composer already recalled. A message that left the queue
 * is not put back or sent again, even if the strip still lists it while the
 * withdrawal's fact is in flight.
 */
const recalled = ref<ReadonlySet<string>>(new Set())
watch(() => props.running, (running) => {
  if (running) return
  destination.value = undefined
  recalled.value = new Set()
})

/** The mode a draft goes out in: this browser's choice during a run, `queue` while the run is idle. */
const primaryMode = computed<SubmitMode>(() => props.running ? composerSettings.sendMode : "queue")
/** `Ctrl+Enter` sends now only into a run; idle it submits like Enter. */
const canSendNow = computed(() => props.running && props.stop !== undefined && (hasDraft.value || held.value.length > 0) && (held.value.length === 0 || props.withdrawHeld !== undefined) && !sending.value && !sendNow.value)
const holdKey = /Mac|iPhone|iPad/.test(navigator.userAgent) ? "⌘+Enter" : "Ctrl+Enter"

function resize(): void {
  const element = field.value
  if (element === undefined) return
  element.style.height = "auto"
  element.style.height = `${Math.min(element.scrollHeight, 300)}px`
  followScroll()
}

/**
 * Keeps the highlight layer on the draft while the textarea scrolls or resizes:
 * its body takes the textarea's content width, so both wrap at the same column
 * whether or not a scrollbar is shown, and moves by the textarea's own scroll
 * offset. The font comes from the same custom properties, so nothing drifts.
 */
function followScroll(): void {
  const element = field.value
  const body = highlightBody.value
  if (element === undefined || body === undefined) return
  body.style.width = `${element.clientWidth}px`
  body.style.transform = `translateY(${-element.scrollTop}px)`
}

function readImage(file: File): { url: string; file: File } {
  return { url: URL.createObjectURL(file), file }
}

onUnmounted(() => {
  observer?.disconnect()
  images.value.forEach((image) => URL.revokeObjectURL(image.url))
})

async function addImages(files: File[]): Promise<void> {
  for (const file of files) {
    try {
      const image = readImage(file)
      if (images.value.length >= MAX_IMAGES) {
        URL.revokeObjectURL(image.url)
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
  images.value.filter((image) => image.id === id).forEach((image) => URL.revokeObjectURL(image.url))
  images.value = images.value.filter((image) => image.id !== id)
}

function removePaste(id: number): void {
  pastes.value = pastes.value.filter((paste) => paste.id !== id)
}

function removeRestored(index: number): void {
  restored.value = restored.value.filter((_, at) => at !== index)
}

/** The draft text the restored images' offsets were last measured in. */
let anchored = ""
/** True while a recall writes the draft itself, so its own insertions are not read as edits. */
let recalling = false
/**
 * The send-now gesture the composer is recalling for. Its images stand ahead of
 * the draft's older ones at the same offset, in the queue order they were
 * recalled in.
 */
let recallBatch = 0

/**
 * An edit around a recalled image moves it with the text it stood between, as a
 * token does: the edit is the draft's common prefix and suffix, so typing or
 * deleting shifts the offsets after it and an image inside a replaced run
 * settles at the run's start. Only the offset moves: the anchor keeps its
 * identity, so a submit that is already out still recognizes the image it took
 * when its reply arrives.
 */
watch(text, (next) => {
  const previous = anchored
  anchored = next
  if (recalling || next === previous || restored.value.length === 0) return
  let start = 0
  while (start < next.length && start < previous.length && next[start] === previous[start]) start++
  let end = 0
  while (end < next.length - start && end < previous.length - start && next[next.length - 1 - end] === previous[previous.length - 1 - end]) end++
  const delta = next.length - previous.length
  const tail = previous.length - end
  for (const block of restored.value) block.at = block.at <= start ? block.at : block.at >= tail ? block.at + delta : start
}, { flush: "sync" })

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

/**
 * Puts a withdrawn message back into the composer at `at`: its content blocks in
 * the order the server holds them, behind the messages recalled before it and
 * ahead of the typed draft, and returns the offset just past what it inserted.
 * The layout is the TUI's `restore_at`: a newline before the message when the
 * draft there does not end a line, one between two adjacent text blocks, and one
 * after the message when typed text follows. The field is plain text, so an
 * image takes no room in it: the offset recorded for it is where it stood between
 * the words, and the submit sends it back there.
 *
 * The message goes in as one insertion, so an image already in the draft moves
 * with the words it stood between instead of staying at an offset the inserted
 * text pushed it past. An image of an earlier gesture that stood exactly where
 * the message goes in moves past the whole message: its words followed the
 * message's, even when the message is images alone and inserts no text.
 */
function recall(item: QueuedInput, at: number): number {
  if (recalled.value.has(item.id)) return at
  recalled.value = new Set([...recalled.value, item.id])
  const blocks = item.content
  if (blocks.length === 0) return at
  recalling = true
  try {
    const draft = text.value
    const position = Math.min(at, draft.length)
    const followed = position < draft.length
    let inserted = ""
    const placements: RecalledImage[] = []
    let opened = false
    let previousText = false
    for (const block of blocks) {
      if (!opened) {
        opened = true
        if (position > 0 && !draft.slice(0, position).endsWith("\n")) inserted += "\n"
      } else if (block.type === "text" && previousText) {
        inserted += "\n"
      }
      if (block.type === "image") {
        previousText = false
        placements.push({ at: position + inserted.length, image: block, batch: recallBatch })
        continue
      }
      inserted += block.text
      previousText = true
    }
    if (followed) inserted += "\n"
    for (const block of restored.value) {
      if (block.at > position || (block.at === position && block.batch !== recallBatch)) block.at += inserted.length
    }
    text.value = draft.slice(0, position) + inserted + draft.slice(position)
    restored.value = [...restored.value, ...placements]
    return position + inserted.length
  } finally {
    recalling = false
  }
}

async function submit(sendMode: SubmitMode = primaryMode.value): Promise<void> {
  if (!canSend.value || composing.value) return
  if (sendNow.value) {
    error.value = t("composer.stillStopping")
    return
  }
  if (commandMode.value) {
    const row = menuRows.value[activeIndex.value]
    if (row?.kind === "skill") completeSkill(row.name.slice(SKILL_TOKEN_PREFIX.length))
    else if (row !== undefined) await runCommand(row.command.name)
    return
  }
  const message = text.value
  if (!dismissed.value && connection.supports("schedule.list") && /^\/schedule(?:d)?\s*$/i.test(message)) {
    await runCommand("scheduled")
    return
  }
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
  const loop = props.sessionCommands?.loop
  const loopCommand = !dismissed.value && loop !== undefined ? /^\/loop(?:\s+([\s\S]*))?$/i.exec(message) : null
  if (loop && loopCommand) {
    text.value = ""
    await nextTick()
    resize()
    await loop(loopCommand[1]?.trim() ?? "")
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
  const wentTo = props.running ? sendMode : undefined
  try {
    if (branchCommand) {
      if (!props.createBranch) throw new Error(t("composer.branchUnavailable"))
      await props.createBranch(branchCommand[1]!.trim())
    } else {
      const sentMentions = fileMentions.value
      const sentImages = images.value
      const sentPastes = pastes.value
      const sentRestored = restored.value
      // A recalled image keeps the place it held in the draft: each gap around
      // one is linked on its own, so a link never straddles it and every offset
      // still lands between the words it held between. At one offset the later
      // gesture's images come first, in the queue order they were recalled in.
      const gaps: Array<{ at: number; image: typeof sentRestored[number]["image"] }> = []
      let body = ""
      let cursor = 0
      for (const { at, image } of [...sentRestored].sort((left, right) => left.at - right.at || right.batch - left.batch)) {
        const offset = Math.max(cursor, Math.min(at, message.length))
        body += linkMentions(message.slice(cursor, offset), sentMentions)
        gaps.push({ at: body.length, image })
        cursor = offset
      }
      body += linkMentions(message.slice(cursor), sentMentions)
      await props.send(body, { images: sentImages.map((image) => image.file), pastes: sentPastes.map((paste) => paste.text), ...(gaps.length === 0 ? {} : { restored: gaps }), onProgress: (percent) => { uploadProgress.value = percent } }, sendMode)
      fileMentions.value = fileMentions.value.filter((mention) => !sentMentions.includes(mention))
      sentImages.forEach((image) => URL.revokeObjectURL(image.url))
      images.value = images.value.filter((image) => !sentImages.includes(image))
      pastes.value = pastes.value.filter((paste) => !sentPastes.includes(paste))
      restored.value = restored.value.filter((block) => !sentRestored.includes(block))
      destination.value = wentTo
    }
    if (text.value === message) text.value = ""
    await nextTick()
    resize()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    sending.value = false
    uploadProgress.value = undefined
  }
}

/** The action beside Enter: sends the draft in the other mode. */
async function submitAlternate(): Promise<void> {
  if (commandMode.value) return
  await submit(alternateMode.value)
}

/**
 * Interrupt and send now: withdraws every held message oldest first into the
 * draft, asks the run to stop, and submits the combined draft once the stop's
 * reply says the turn has settled. A submit written before the last interrupt
 * cannot start a run after it, so the order is the point. A stop that fails
 * keeps the whole draft, and a second trigger during the same stop sends
 * nothing: one reply decides one submit.
 */
async function sendNowAndStop(): Promise<void> {
  if (sendNow.value || composing.value) return
  if (!props.running) {
    await submit()
    return
  }
  if (!canSendNow.value) return
  sendNow.value = true
  error.value = ""
  destination.value = undefined
  recallBatch++
  let at = 0
  for (const item of held.value) {
    if (props.withdrawHeld === undefined) break
    try {
      if (await props.withdrawHeld(item) === "deleted") at = recall(item, at)
    } catch (cause) {
      // A message that stays held is not recalled and so is not sent twice;
      // the reason is reported and the gesture goes on, as the TUI's Esc does.
      error.value = cause instanceof Error ? cause.message : String(cause)
    }
  }
  await nextTick()
  resize()
  try {
    await props.stop?.()
  } catch (cause) {
    sendNow.value = false
    error.value = t("composer.sendNowFailed", { error: cause instanceof Error ? cause.message : String(cause) })
    return
  }
  sendNow.value = false
  await submit("queue")
  // This draft opens the run after the stopped one, so neither destination
  // above describes it.
  destination.value = undefined
}

async function stop(): Promise<void> {
  if (sendNow.value) return
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

/** Fills `/skill:<name> ` into the draft and sends nothing; the token is part of the message. */
function completeSkill(name: string): void {
  text.value = `${SKILL_TOKEN_PREFIX}${name} `
  dismissed.value = true
  void nextTick(() => {
    resize()
    const element = field.value
    element?.focus()
    element?.setSelectionRange(text.value.length, text.value.length)
  })
}

async function runCommand(name: typeof availableCommands.value[number]["name"]): Promise<void> {
  if (composing.value) return
  error.value = ""
  const argument = name === "branch" || name === "goal" || name === "loop" || name === "effort"
  text.value = argument ? `/${name} ` : ""
  await nextTick()
  resize()
  if (argument) field.value?.focus()
  else if (name === "models") modelMenu.value?.open()
  else if (name === "lines") props.openLines?.()
  else if (name === "scheduled") {
    const cwd = typeof route.params.sessionId === "string" ? sessions.byId.get(route.params.sessionId)?.cwd : undefined
    await router.push({ name: "settings-scheduled", ...(cwd === undefined ? {} : { query: { cwd } }) })
  } else if (name === "new") {
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
    activeIndex.value = (activeIndex.value + (event.key === "ArrowDown" ? 1 : -1) + menuRows.value.length) % menuRows.value.length
    return
  }
  const picker = menuOpen.value || mentionShown.value
  if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
    event.preventDefault()
    // A picker takes a modified Enter as Enter itself, as the TUI does. No
    // picker: during a run it is interrupt and send now, and while the run is
    // idle it submits as Enter does.
    if ((event.ctrlKey || event.metaKey) && !picker && !sendNow.value) {
      if (props.running) void sendNowAndStop()
      else void submit()
      return
    }
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

onMounted(() => {
  const element = field.value
  element?.focus()
  if (element !== undefined) {
    observer = new ResizeObserver(() => followScroll())
    observer.observe(element)
  }
})
onActivated(() => {
  resize()
  field.value?.focus()
})
defineExpose({ focus: () => field.value?.focus(), quote })
</script>

<template>
  <section class="composer-panel" :data-variant="variant ?? 'main'" :aria-label="t('chat.composer')">
    <p v-if="error" class="inline-error" role="alert">{{ error }}</p>
    <div class="composer-surface" @click.self="field?.focus()" @dragover.prevent @drop="onDrop">
      <p v-if="destination" class="composer-destination" role="status" :data-mode="destination">
        <UIcon :name="destination === 'steer' ? 'i-lucide-corner-down-right' : 'i-lucide-hourglass'" class="size-3.5 shrink-0" aria-hidden="true" />
        <span>{{ destination === 'steer' ? t('composer.sentIntoRun') : t('composer.heldForNextTurn') }}</span>
      </p>
      <div v-if="images.length > 0 || pastes.length > 0 || restored.length > 0" class="draft-attachments">
        <span v-for="image in images" :key="image.id" class="draft-chip">
          <img :src="image.url" :alt="t('composer.imageAttachment')" class="draft-thumb">
          <UButton color="neutral" variant="ghost" size="xs" square icon="i-lucide-x" :aria-label="t('composer.removeAttachment')" @click="removeImage(image.id)" />
        </span>
        <span v-for="(block, index) in restored" :key="index" class="draft-chip">
          <img v-if="sessionId" :src="`/api/sessions/${encodeURIComponent(sessionId)}/files/${encodeURIComponent(block.image.source.fileId)}`" :alt="t('composer.imageAttachment')" class="draft-thumb" loading="lazy">
          <span v-else>{{ t("composer.imageAttachment") }}</span>
          <UButton color="neutral" variant="ghost" size="xs" square icon="i-lucide-x" :aria-label="t('composer.removeAttachment')" @click="removeRestored(index)" />
        </span>
        <span v-for="paste in pastes" :key="paste.id" class="draft-chip">
          <span>{{ t("composer.pastedText", { lines: paste.text.split("\n").length }) }}</span>
          <UButton color="neutral" variant="ghost" size="xs" square icon="i-lucide-x" :aria-label="t('composer.removeAttachment')" @click="removePaste(paste.id)" />
        </span>
      </div>
      <div v-if="menuOpen" :id="menuId" class="command-menu" role="listbox" :aria-label="t('composer.commands')">
        <button
          v-for="(row, index) in menuRows"
          :id="`${menuId}-${index}`"
          :key="row.key"
          type="button"
          role="option"
          class="command-menu-item command-item"
          :aria-selected="index === activeIndex"
          :data-highlighted="index === activeIndex ? '' : undefined"
          @mousedown.prevent
          @mouseenter="activeIndex = index"
          @click.stop="row.kind === 'skill' ? completeSkill(row.name.slice(SKILL_TOKEN_PREFIX.length)) : runCommand(row.command.name)"
        >
          <span class="command-category">{{ index === 0 || menuRows[index - 1]?.category !== row.category ? t(`composer.category.${row.category}`) : '' }}</span>
          <span class="command-name">{{ row.name }}<template v-if="row.kind === 'command'"><span v-if="row.command.name === 'branch'" class="command-argument"> &lt;prompt&gt;</span><span v-else-if="row.command.name === 'goal' || row.command.name === 'loop' || row.command.name === 'effort'" class="command-argument"> [args]</span></template></span>
          <span class="command-description">{{ row.description }}</span>
        </button>
        <p class="command-keys">{{ t("composer.commandKeys") }}</p>
      </div>
      <div v-if="mentionShown" :id="mentionMenuId" class="command-menu" role="listbox" :aria-label="t('composer.files')">
        <button
          v-for="(candidate, index) in candidates"
          :key="candidate.absolutePath"
          type="button"
          role="option"
          class="command-menu-item command-item"
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
      <div class="composer-field">
        <div class="composer-highlight" aria-hidden="true"><div ref="highlightBody" class="composer-highlight-body"><span v-for="(part, index) in highlight" :key="index" :class="{ 'skill-chip': part.skill }">{{ part.text }}</span></div></div>
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
          @scroll="followScroll"
          @paste="onPaste"
          @focus="focused = true"
          @blur="focused = false"
          @compositionstart="composing = true"
          @compositionend="composing = false"
        />
      </div>
      <footer class="composer-toolbar">
        <div class="composer-tool-group">
          <ComposerPermissionMenu v-if="permission && variant !== 'panel'" ref="permissionMenu" :policy="permission.policy" @select="permission.select" />
        </div>
        <div class="composer-run-group">
          <progress v-if="uploadProgress !== undefined" :value="uploadProgress" max="100" aria-label="File upload" />
          <ComposerModelMenu v-if="variant !== 'panel'" ref="modelMenu" :model="model" :preview="modelPreview" :selectable="modelSelectable" :disabled="changingModel" @select="onModel" @follow="modelPreview?.follow()" @retry="modelPreview?.retry()" />
          <UTooltip v-if="sendNow" :text="t('composer.stopping')">
            <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-loader-circle" :aria-label="t('composer.stopping')" disabled />
          </UTooltip>
          <template v-else-if="running">
            <UTooltip :text="primaryMode === 'steer' ? t('composer.queueAlternate') : t('composer.steerAlternate')">
              <UButton color="neutral" variant="ghost" size="sm" :square="variant === 'panel'" :icon="alternateMode === 'steer' ? 'i-lucide-corner-down-right' : 'i-lucide-hourglass'" :aria-label="alternateMode === 'steer' ? t('composer.steerAlternate') : t('composer.queueAlternate')" :disabled="!canSend || sending || sendNow" @click.stop="submitAlternate()">
                <template v-if="variant !== 'panel'">{{ alternateMode === 'steer' ? t('composer.steer') : t('composer.queue') }}</template>
              </UButton>
            </UTooltip>
            <UTooltip :text="t('composer.sendNowHint', { key: holdKey })">
              <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-zap" :aria-label="t('composer.sendNowHint', { key: holdKey })" :disabled="!canSendNow" @click.stop="sendNowAndStop()" />
            </UTooltip>
          </template>
          <UTooltip :text="showStop ? t('composer.stopResponse') : t('composer.send')">
            <UButton
              color="neutral"
              variant="solid"
              size="sm"
              square
              :icon="showStop ? 'i-lucide-square' : running && primaryMode === 'queue' ? 'i-lucide-hourglass' : 'i-lucide-arrow-up'"
              :aria-label="showStop ? t('composer.stopResponse') : t('composer.send')"
              :aria-description="showStop || !running ? undefined : primaryMode === 'steer' ? t('composer.steerAlternate') : t('composer.queueAlternate')"
              :disabled="showStop ? stopping || sendNow : !canSend || sending || sendNow"
              @click.stop="showStop ? stop() : submit()"
            />
          </UTooltip>
        </div>
      </footer>
    </div>
    <div v-if="$slots.status" class="composer-statusbar">
      <div class="composer-status-start"><slot name="status" /></div>
    </div>
  </section>
</template>

<style scoped>
.composer-field { position: relative; }
/* The layer behind the field paints only the chip backgrounds: its text is transparent and keeps the field's font, size, line height, wrapping, content width and scroll offset, so a chip's box lands under the characters the field itself draws. The field's own text stays above it, so the caret, the selection, the placeholder and IME pre-edit are the browser's. */
.composer-highlight { position: absolute; inset: 0; overflow: hidden; font: var(--text-base)/var(--leading-relaxed) var(--font-sans); white-space: pre-wrap; overflow-wrap: break-word; color: transparent; pointer-events: none; }
.composer-highlight .skill-chip { border-radius: var(--radius-sm); background: var(--skill-chip); }
.composer-highlight-body { transform: translateY(0); }
/* The field paints the draft in its own colour over a transparent background, so the chip fill shows through under the glyphs. It never changes weight or padding: either would move the glyphs off the chips behind them. */
.composer-input { display: block; position: relative; width: 100%; min-height: 60px; max-height: 300px; resize: none; padding: 0; border: 0; background: transparent; color: var(--foreground); font: var(--text-base)/var(--leading-relaxed) var(--font-sans); outline: none; overflow-y: auto; }
.composer-input::placeholder { color: var(--muted-foreground); }
.composer-panel[data-variant="panel"] { padding: var(--space-2) var(--space-3) var(--space-3); }
.composer-panel[data-variant="panel"] .composer-input { min-height: 48px; }
.command-menu { position: absolute; z-index: var(--z-popover); bottom: calc(100% + var(--space-2)); left: 0; width: 100%; max-width: none; max-height: min(320px, 50dvh); overflow-y: auto; padding: var(--space-1); border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--card); color: var(--card-foreground); box-shadow: var(--shadow-pop); transform-origin: bottom left; --popover-enter-y: 4px; }
.command-menu-item { display: flex; width: 100%; min-width: 0; min-height: var(--control-md); align-items: center; gap: var(--space-2); margin: 0; padding: 0 var(--space-2); border: 0; border-radius: var(--control-md-radius); background: transparent; color: var(--foreground); font: inherit; font-size: var(--text-md); font-weight: var(--weight-ui); text-align: left; cursor: pointer; outline: none; transition: background-color var(--duration-hover) ease, color var(--duration-hover) ease; }
.command-menu-item:is(:hover, :focus-visible, [data-highlighted]) { background: var(--hover); }
.command-item { display: grid; grid-template-columns: 48px 128px minmax(0, 1fr); }
.command-category, .command-description { color: var(--muted-foreground); }
.command-category { font-size: var(--text-xs); }
.command-name { font-family: var(--font-mono); }
.command-argument { color: var(--muted-foreground); font-size: var(--text-xs); }
.draft-attachments { display: flex; flex-wrap: wrap; gap: var(--space-2); padding: var(--space-2) var(--space-3) 0; }
.draft-chip { display: inline-flex; min-width: 0; align-items: center; gap: var(--space-1); padding: 2px var(--space-1) 2px var(--space-2); border: 1px solid var(--border); border-radius: var(--control-sm-radius); background: var(--muted); font-size: var(--text-xs); }
.draft-thumb { width: 36px; height: 36px; border-radius: var(--control-xs-radius); object-fit: cover; }
/* Where the last draft went: quiet, one line, and gone when the run ends. */
.composer-destination { display: flex; align-items: center; gap: var(--space-1); margin: 0; padding: 0 var(--space-1); color: var(--muted-foreground); font-size: var(--text-xs); }
.composer-destination[data-mode="steer"] { color: var(--primary); }
.composer-destination > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.command-keys { margin: var(--space-1) 0 0; padding: var(--space-2); border-top: 1px solid var(--border); color: var(--muted-foreground); font-size: var(--text-xs); }

</style>
