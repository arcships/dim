import { Button } from "@heroui/react"
import { ArrowUp, CornerDownRight, Hourglass, Paperclip, Square, X, Zap } from "lucide-react"
import { useCallback, useEffect, useId, useImperativeHandle, useRef, useState, type FormEvent, type Ref } from "react"
import { alwaysCommand, fileGrant } from "../approvals"
import { quoteInto } from "../features/chat/quote"
import { moveImageOffsets, restoreQueuedContent, type RecalledImage } from "../features/chat/recall"
import { translate } from "./i18n"
import { linkMentions } from "../mentions"
import { type ApprovalRequest, type AskQuestion, type MemberApprovalDescriptor, type PendingQuestion, type QueuedInput, type QuestionAnswer, type SubmitMode } from "../rpc/protocol"
import { SKILL_TOKEN_PREFIX, skillSegments } from "../skill-token"
import type { SessionView } from "../stores/chat"
import { useComposerStore } from "../stores/composer"
import { useModels } from "../stores/models"
import { FileMentions, useFileMentions } from "./file-mentions"
import { CommandPalette, commandChoices, commandOrder, contextCommand, goalCommand, loopCommand, takesWords, type CommandChoice, type CommandName, type SkillChoice } from "./composer-commands"
import { ModelPicker, PermissionPicker, type PickerHandle } from "./composer-controls"
import { chat, connection, useStoreValue } from "./stores"

const composerSettings = useComposerStore()
const readSendMode = () => composerSettings.sendMode
const inlinePasteBytes = 16 * 1024
const maximumPasteBytes = 10 * 1024 * 1024

export type ComposerHandle = { quote: (text: string) => void }

export function Composer({ session, connected, branchId, readOnly = false, actions, ref }: {
  session: SessionView
  connected: boolean
  branchId: string
  readOnly?: boolean
  actions: {
    newSession: () => void
    scheduled: () => void
    branch?: (words: string) => Promise<void>
    fork?: () => void
  }
  ref?: Ref<ComposerHandle>
}) {
  const [draft, setDraft] = useState("")
  const [files, setFiles] = useState<File[]>([])
  const [pastes, setPastes] = useState<string[]>([])
  const [restored, setRestored] = useState<RecalledImage[]>([])
  const [uploadProgress, setUploadProgress] = useState<number>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [notice, setNotice] = useState("")
  const [focused, setFocused] = useState(false)
  const [composing, setComposing] = useState(false)
  const [literalSlash, setLiteralSlash] = useState(false)
  const [selectedCommand, setSelectedCommand] = useState(0)
  const [skills, setSkills] = useState<readonly SkillChoice[]>([])
  const commandId = useId()
  const picker = useRef<HTMLInputElement>(null)
  const messageField = useRef<HTMLTextAreaElement>(null)
  const highlights = useRef<HTMLDivElement>(null)
  const modelPicker = useRef<PickerHandle>(null)
  const permissionPicker = useRef<PickerHandle>(null)
  const working = useRef(false)
  const recallBatch = useRef(0)
  const branch = branchId === session.defaultBranchId ? session : session.branches[branchId]
  const enabled = connected && session.status === "ready" && branch?.status === "ready"
  const sendMode = useStoreValue(readSendMode)
  const alternate = sendMode === "steer" ? "queue" : "steer"
  const approval = session.approvals.find((request) => request.branchId === branchId)
  const questions = session.questions.filter((ask) => ask.branchId === branchId)
  const hasAttachment = session.methods.includes("attachment.put")
  const prompt = approval !== undefined || questions.length > 0
  const queued = branch?.queued ?? []
  const canWithdraw = enabled && session.methods.includes("session.input.withdraw")
  const hasDraft = draft.trim() !== "" || files.length > 0 || pastes.length > 0 || restored.length > 0
  const canSendNow = enabled && branch?.running && !branch.stopping && !busy && !prompt && !readOnly
    && (hasDraft || queued.length > 0) && (queued.length === 0 || canWithdraw)
  const canChooseModel = branchId === session.defaultBranchId && connection.supports("provider.models.query") && connection.supports("session.model.update")
  const permission = branchId === session.defaultBranchId && connection.supports("session.permission.get") && connection.supports("session.permission.update")
    ? session.permission : undefined
  const canReadSkills = enabled && session.methods.includes("session.skills")
  const [models] = useState(useModels)
  const currentModel = branch?.model
  const efforts = useStoreValue(useCallback(() => models.providers?.find(({ providerId }) => providerId === currentModel?.providerId)
    ?.models.find(({ id }) => id === currentModel?.modelId)?.reasoningEfforts, [models, currentModel]))
  useEffect(() => {
    if (!enabled || !canChooseModel || currentModel === undefined) return
    void models.load(currentModel)
    return () => models.clear()
  }, [enabled, canChooseModel, models, currentModel])
  useEffect(() => {
    if (!canReadSkills) return
    let current = true
    void chat.sessionSkills(session.sessionId, branchId).then((items) => {
      if (current) setSkills(items.filter(({ userInvocable }) => userInvocable))
    }, () => { if (current) setSkills([]) })
    return () => { current = false }
  }, [canReadSkills, session.sessionId, branchId])
  const available = new Set<CommandName>(["new"])
  if (canChooseModel) available.add("models")
  if (permission !== undefined) available.add("permissions")
  if (actions.branch) available.add("branch")
  if (actions.fork) available.add("fork")
  if (connection.supports("schedule.list")) available.add("scheduled")
  if (canChooseModel && efforts?.length) available.add("effort")
  if (session.methods.includes("session.context.compact")) available.add("compact")
  if (session.methods.includes("session.context.usage")) available.add("usage")
  if (session.methods.includes("session.goal.set")) available.add("goal")
  if (session.methods.includes("session.loop.start")) available.add("loop")
  const choices = enabled && !composing && !literalSlash && !prompt && !readOnly && !busy
    ? commandChoices(draft, available, canReadSkills ? skills : []) : []
  const menuOpen = focused && choices.length > 0
  const activeCommand = Math.min(selectedCommand, choices.length - 1)
  const skillHighlight = skillSegments(draft)

  function updateDraft(text: string) {
    if (restored.length > 0) {
      const positions = moveImageOffsets(draft, text, restored)
      setRestored(restored.map((image, index) => ({ ...image, at: positions[index]! })))
    }
    setDraft(text)
  }
  const mentions = useFileMentions(session.sessionId, enabled && !readOnly && !prompt && session.methods.includes("session.files.search"), updateDraft)

  useImperativeHandle(ref, () => ({
    quote(text) {
      if (working.current) return
      setDraft((current) => quoteInto(current, text))
      messageField.current?.focus()
    },
  }), [])

  async function operate(action: () => Promise<void>) {
    if (working.current) return
    working.current = true
    setBusy(true)
    setError("")
    setNotice("")
    try {
      await action()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      working.current = false
      setBusy(false)
    }
  }

  async function runCommand(name: CommandName, words: string): Promise<void> {
    if (!available.has(name)) {
      throw new Error(translate(name === "branch" ? "composer.branchUnavailable" : "command.failed"))
    }
    switch (name) {
      case "new": actions.newSession(); break
      case "scheduled": actions.scheduled(); break
      case "fork": actions.fork?.(); break
      case "models": modelPicker.current?.open(); break
      case "permissions": permissionPicker.current?.open(); break
      case "branch": await actions.branch?.(words); break
      case "compact":
      case "usage": setNotice(await contextCommand(name, session.sessionId, branchId)); break
      case "goal": setNotice(await goalCommand(session.sessionId, branchId, words)); break
      case "loop": setNotice(await loopCommand(session.sessionId, branchId, words)); break
      case "effort": {
        if (!words) setNotice(translate("command.effortChoices", { choices: efforts?.join(", ") ?? "" }))
        else if (efforts?.includes(words)) {
          const outcome = await chat.setEffort(session.sessionId, words, branchId)
          setNotice(translate("command.effortSet", { outcome, value: words }))
        } else throw new Error(translate(/\s/.test(words) ? "command.effortUsage" : "command.effortUnknown"))
        break
      }
    }
    updateDraft("")
    setLiteralSlash(false)
  }

  function selectCommand(choice: CommandChoice, complete: boolean) {
    setError("")
    setNotice("")
    if (choice.skill || complete || takesWords(choice.name)) {
      const prefix = choice.skill ? SKILL_TOKEN_PREFIX : "/"
      const space = choice.skill || takesWords(choice.name) ? " " : ""
      updateDraft(`${prefix}${choice.name}${space}`)
      setSelectedCommand(0)
      messageField.current?.focus()
    } else void operate(() => runCommand(choice.name, ""))
  }

  function handleCommand(): boolean {
    if (literalSlash || composing) return false
    const choice = choices[activeCommand]
    if (choice) {
      selectCommand(choice, false)
      return true
    }
    const match = /^\/([a-z]+)(?:\s+([\s\S]*))?$/i.exec(draft)
    if (!match) return false
    const input = match[1]!.toLowerCase()
    const name = commandOrder.find((command) => command === input || (command === "scheduled" && input === "schedule"))
    if (name === undefined) return false
    const words = match[2]?.trim() ?? ""
    if (name === "branch" && words === "" && available.has(name)) {
      selectCommand({ name }, true)
      return true
    }
    if (words !== "" && !takesWords(name)) {
      setError(translate("command.failed"))
      return true
    }
    void operate(() => runCommand(name, words))
    return true
  }

  async function submitDraft(text: string, images: RecalledImage[], mode: SubmitMode) {
    if (!text.trim() && files.length === 0 && pastes.length === 0 && images.length === 0) return
    let cursor = 0
    let body = ""
    const placements = [...images].sort((a, b) => a.at - b.at || b.batch - a.batch).map((image) => {
      const offset = Math.max(cursor, Math.min(image.at, text.length))
      body += linkMentions(text.slice(cursor, offset), mentions.mentions)
      cursor = offset
      return { ...image, at: body.length }
    })
    body += linkMentions(text.slice(cursor), mentions.mentions)
    try {
      await chat.submit(session.sessionId, branchId, body, { images: files, pastes, restored: placements, onProgress: setUploadProgress }, mode)
    } finally {
      setUploadProgress(undefined)
    }
    setDraft("")
    setLiteralSlash(false)
    setFiles([])
    setPastes([])
    setRestored([])
    mentions.clear()
  }

  function submit(mode: SubmitMode) {
    if (enabled && !prompt && !readOnly && hasDraft && !composing && !handleCommand()) {
      void operate(() => submitDraft(draft, restored, mode))
    }
  }

  function withdraw(input: QueuedInput, steer: boolean) {
    if (!canWithdraw) return
    void operate(async () => {
      const result = await chat.withdrawInput(session.sessionId, branchId, input.id, steer ? input.user : undefined)
      if (result.status === "notHeld") setError(translate("queue.notHeld"))
    })
  }

  function sendNow() {
    if (!canSendNow || handleCommand()) return
    void operate(async () => {
      const batch = ++recallBatch.current
      let next = { draft, images: restored, next: 0 }
      for (const input of queued) {
        try {
          const result = await chat.withdrawInput(session.sessionId, branchId, input.id, undefined)
          if (result.status === "deleted") next = restoreQueuedContent(next.draft, next.images, input.content, next.next, batch)
        } catch (cause) {
          setError(cause instanceof Error ? cause.message : String(cause))
        }
      }
      setDraft(next.draft)
      setRestored(next.images)
      try { await chat.stop(session.sessionId, branchId) }
      catch (cause) {
        throw new Error(translate("composer.sendNowFailed", { error: cause instanceof Error ? cause.message : String(cause) }))
      }
      await submitDraft(next.draft, next.images, "queue")
    })
  }

  return <>
    {queued.length > 0 && <ul className="rw-queue" aria-label={translate("queue.title")}>
      {queued.map((input, index) => <li key={input.id}>
        <span onMouseEnter={(event) => {
          const preview = event.currentTarget
          preview.title = preview.scrollWidth > preview.clientWidth ? input.text : ""
        }}>{input.text || "Attachment"}</span>
        {index === 0 && branch?.running && <Button size="sm" variant="secondary" isDisabled={!canWithdraw || busy}
          onPress={() => withdraw(input, true)}>{translate("queue.steer")}</Button>}
        <Button size="sm" variant="tertiary" isDisabled={!canWithdraw || busy}
          aria-label={`Remove queued message ${index + 1}`} onPress={() => withdraw(input, false)}>{translate("queue.remove")}</Button>
      </li>)}
    </ul>}
    {branch?.error && <p className="rw-error" role="alert">{branch.error}</p>}
    {error && <p className="rw-error" role="alert">{error}</p>}
    {notice && <p className="rw-command-result" role="status">{notice}</p>}
    {approval && <Approval key={approval.requestId} request={approval} sessionId={session.sessionId} enabled={enabled} />}
    {!approval && questions.map((question) => <Question key={`${question.requestId}:${question.callId}`} question={question} sessionId={session.sessionId} enabled={enabled} />)}
    {readOnly && !prompt && <p className="rw-notice">This line has ended.</p>}
    <form className="rw-composer" hidden={prompt || readOnly} onDragOver={(event) => {
      if (hasAttachment && event.dataTransfer.types.includes("Files")) event.preventDefault()
    }} onDrop={(event) => {
      if (!hasAttachment || !event.dataTransfer.types.includes("Files")) return
      event.preventDefault()
      if (!enabled || working.current) return
      const dropped = Array.from(event.dataTransfer.files)
      const images = dropped.filter((file) => file.type.startsWith("image/"))
      if (images.length !== dropped.length) setError(translate("composer.onlyImages"))
      if (images.length > 0) setFiles((previous) => [...previous, ...images])
    }} onSubmit={(event: FormEvent) => {
      event.preventDefault()
      submit(branch?.running ? sendMode : "queue")
    }}>
      {menuOpen && <CommandPalette id={commandId} choices={choices} selected={activeCommand}
        choose={(index) => selectCommand(choices[index]!, false)} />}
      <div className="rw-message-field">
      <div className="rw-skill-highlight" ref={highlights} aria-hidden="true">
        {skillHighlight.map(({ text, skill }, index) => <span key={index} className={skill ? "rw-skill-mark" : undefined}>{text}</span>)}
        {"\u200b"}
      </div>
      <textarea
        ref={messageField}
        aria-label="Message"
        placeholder="Ask Dim…"
        value={draft}
        rows={2}
        disabled={!enabled}
        readOnly={busy}
        aria-controls={menuOpen ? commandId : mentions.open && mentions.options.length > 0 ? mentions.listId : undefined}
        aria-activedescendant={menuOpen ? `${commandId}-${activeCommand}` : mentions.open && mentions.options.length > 0 ? mentions.activeId : undefined}
        onFocus={(event) => { setFocused(true); mentions.update(event.currentTarget) }}
        onBlur={() => { setFocused(false); mentions.blur() }}
        onScroll={(event) => {
          if (highlights.current) highlights.current.scrollTop = event.currentTarget.scrollTop
        }}
        onCompositionStart={() => setComposing(true)}
        onCompositionEnd={() => setComposing(false)}
        onSelect={(event) => mentions.update(event.currentTarget)}
        onChange={(event) => {
          updateDraft(event.currentTarget.value)
          setSelectedCommand(0)
          if (!event.currentTarget.value.startsWith("/")) setLiteralSlash(false)
          mentions.update(event.currentTarget)
        }}
        onPaste={(event) => {
          if (!hasAttachment) return
          const images = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"))
          if (images.length > 0) {
            event.preventDefault()
            setFiles((previous) => [...previous, ...images])
            return
          }
          const text = event.clipboardData.getData("text/plain")
          const bytes = new TextEncoder().encode(text).length
          if (bytes <= inlinePasteBytes) return
          event.preventDefault()
          if (bytes > maximumPasteBytes) setError(translate("composer.pasteTooLarge"))
          else setPastes((previous) => [...previous, text])
        }}
        onKeyDown={(event) => {
          if (busy || composing || event.nativeEvent.isComposing || event.keyCode === 229) return
          if (mentions.keyDown(event)) return
          if (event.key === "Escape" && draft.startsWith("/") && !literalSlash) {
            event.preventDefault()
            event.stopPropagation()
            setLiteralSlash(true)
            return
          }
          if (choices.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
            event.preventDefault()
            setSelectedCommand((activeCommand + (event.key === "ArrowDown" ? 1 : -1) + choices.length) % choices.length)
            return
          }
          if (choices.length && event.key === "Tab" && !event.shiftKey) {
            event.preventDefault()
            selectCommand(choices[activeCommand]!, true)
            return
          }
          if (event.key !== "Enter" || event.shiftKey || event.altKey) return
          event.preventDefault()
          if (handleCommand()) return
          if (branch?.running && (event.metaKey || event.ctrlKey)) sendNow()
          else event.currentTarget.form?.requestSubmit()
        }}
      />
      </div>
      <FileMentions state={mentions} field={messageField.current} />
      {(files.length > 0 || pastes.length > 0 || restored.length > 0) && <ul className="rw-files">{files.map((file, index) => <li key={`${file.name}:${index}`}>
        {file.name}<Button size="sm" variant="tertiary" isIconOnly isDisabled={busy}
          aria-label={`Remove ${file.name}`} onPress={() => setFiles((previous) => previous.filter((_, i) => i !== index))}><X size={13} /></Button>
      </li>)}
        {pastes.map((text, index) => <li key={`paste:${index}`}>
          {translate("composer.pastedText", { lines: text.split("\n").length })}
          <Button size="sm" variant="tertiary" isIconOnly isDisabled={busy} aria-label={`Remove pasted text ${index + 1}`}
            onPress={() => setPastes((previous) => previous.filter((_, i) => i !== index))}><X size={13} /></Button>
        </li>)}
        {restored.map((image, index) => <li key={`${image.image.source.fileId}:${index}`}>
          Restored image <Button size="sm" variant="tertiary" isIconOnly aria-label={`Remove restored image ${index + 1}`} isDisabled={busy}
            onPress={() => setRestored((previous) => previous.filter((_, i) => i !== index))}><X size={13} /></Button>
        </li>)}
      </ul>}
      {uploadProgress !== undefined && <progress className="rw-upload-progress" aria-label="Uploading attachments" max={100} value={uploadProgress} />}
      <div className="rw-composer-bar">
        {hasAttachment && <>
          <input ref={picker} type="file" accept="image/*" multiple hidden onChange={(event) => {
            setFiles((previous) => [...previous, ...Array.from(event.target.files ?? [])])
            event.target.value = ""
          }} />
          <Button size="sm" variant="tertiary" isIconOnly aria-label="Attach images" isDisabled={!enabled || busy} onPress={() => picker.current?.click()}><Paperclip size={17} /></Button>
        </>}
        {permission !== undefined &&
          <PermissionPicker ref={permissionPicker} policy={permission} disabled={!enabled} choose={async (preset) => {
            if (await chat.setPermission(session.sessionId, preset) === "conflict") throw new Error(translate("permission.conflict"))
          }} />}
        {canChooseModel
          ? <ModelPicker ref={modelPicker} model={branch?.model} disabled={!enabled} choose={(model) => chat.setModel(session.sessionId, model, branchId)} />
          : branch?.model && <span className="rw-model">{branch.model.modelId}</span>}
        {branch?.running && <Button size="sm" variant="tertiary" isIconOnly aria-label="Stop response" isDisabled={!enabled || branch.stopping || busy}
          onPress={() => { void operate(() => chat.stop(session.sessionId, branchId)) }}><Square size={15} /></Button>}
        {branch?.running && <Button size="sm" variant="tertiary" isIconOnly aria-label="Interrupt and send now"
          isDisabled={!canSendNow} onPress={sendNow}><Zap size={16} /></Button>}
        {branch?.running && <Button size="sm" variant="tertiary" isIconOnly
          aria-label={alternate === "queue" ? "Queue message" : "Steer message"} isDisabled={!enabled || busy || !hasDraft}
          onPress={() => submit(alternate)}>{alternate === "queue" ? <Hourglass size={16} /> : <CornerDownRight size={16} />}</Button>}
        <Button type="submit" size="sm" variant="primary" isIconOnly aria-label={branch?.running ? (sendMode === "queue" ? "Queue message" : "Steer message") : "Send message"}
          isDisabled={!enabled || !hasDraft} isPending={busy}><ArrowUp size={19} /></Button>
      </div>
    </form>
  </>
}

function Approval({ request, sessionId, enabled }: { request: ApprovalRequest; sessionId: string; enabled: boolean }) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [member, setMember] = useState<MemberApprovalDescriptor>()
  const [reading, setReading] = useState(false)
  const memberRequest = request.call.name === "member_approval_allow"
  const permanent = !memberRequest && alwaysCommand(request) !== undefined
    && connection.supports("permission.rules") && connection.supports("permission.set_rules")
  const sessionGrant = !memberRequest && fileGrant(request) !== undefined

  const loadMember = useCallback(async () => {
    setReading(true)
    setError("")
    try {
      const detail = await chat.memberApproval(sessionId, request)
      if (detail.state === "ended") {
        await chat.respond(sessionId, request, "deny")
        return false
      }
      setMember(detail.descriptor)
      return true
    } catch (cause) {
      setMember(undefined)
      setError(cause instanceof Error ? cause.message : String(cause))
      return false
    } finally {
      setReading(false)
    }
  }, [request, sessionId])

  useEffect(() => {
    if (memberRequest && enabled) void loadMember()
  }, [memberRequest, enabled, loadMember])

  async function decide(decision: "allow" | "deny" | "always" | "session") {
    if (!enabled || busy || (decision === "allow" && memberRequest && (reading || member === undefined))) return
    setBusy(true)
    setError("")
    try {
      if (decision === "allow" && memberRequest && !await loadMember()) return
      if (decision === "always") await chat.allowAlways(sessionId, request)
      else if (decision === "session") await chat.allowForSession(sessionId, request)
      else await chat.respond(sessionId, request, decision)
    }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  const call = member?.call ?? request.call
  return <section className={memberRequest ? "rw-prompt rw-member-approval" : "rw-prompt"} aria-label="Approval required">
    <h2>Approval required · {call.name}</h2>
    <p>{request.reason}</p>
    {memberRequest && <p>Leader branch: {request.branchId}</p>}
    {member && <>
      <p>Member: {member.member.agentId} · Session: {member.member.sessionId} · Branch: {member.member.branchId}</p>
      <p>Original reason: {member.reason}</p>
    </>}
    {reading && <p role="status">Reading member action…</p>}
    {(!memberRequest || member !== undefined) && <pre>{JSON.stringify(call.input, null, 2)}</pre>}
    {error && <p className="rw-error" role="alert">{error}</p>}
    <div>
      {memberRequest && !member && !reading && <Button size="sm" variant="tertiary" isDisabled={!enabled || busy}
        onPress={() => { void loadMember() }}>Retry reading member action</Button>}
      <Button size="sm" variant="tertiary" isDisabled={!enabled || busy} onPress={() => { void decide("deny") }}>Deny</Button>
      {sessionGrant && <Button size="sm" variant="secondary" isDisabled={!enabled || busy}
        onPress={() => { void decide("session") }}>{translate("prompt.allowSession")}</Button>}
      {permanent && <Button size="sm" variant="secondary" isDisabled={!enabled || busy}
        onPress={() => { void decide("always") }}>{translate("prompt.allowAlways")}</Button>}
      <Button size="sm" isDisabled={!enabled || busy || (memberRequest && (reading || !member))}
        onPress={() => { void decide("allow") }}>{translate("prompt.allowOnce")}</Button>
    </div>
  </section>
}

function Question({ question, sessionId, enabled }: { question: PendingQuestion; sessionId: string; enabled: boolean }) {
  const [answers, setAnswers] = useState<Record<string, QuestionAnswer>>({})
  const [busy, setBusy] = useState(false)
  const [sent, setSent] = useState(false)
  const [error, setError] = useState("")
  const id = useId()
  const complete = question.questions.every(({ id }) => {
    const answer = answers[id]
    return answer !== undefined && (answer.selected.length > 0 || Boolean(answer.custom?.trim()))
  })

  function select(item: AskQuestion, label: string) {
    const answer = answers[item.id]
    const previous = answer?.selected ?? []
    const selected = !item.multiSelect ? [label] : previous.includes(label) ? previous.filter((value) => value !== label) : [...previous, label]
    setAnswers({ ...answers, [item.id]: { questionId: item.id, selected, custom: item.multiSelect ? answer?.custom : undefined } })
  }

  async function send(declined: boolean) {
    setBusy(true)
    setError("")
    try {
      await chat.answerQuestion(sessionId, question, declined ? undefined : question.questions.map(({ id }) => {
        const answer = answers[id]!
        const custom = answer.custom?.trim()
        return { questionId: id, selected: answer.selected, ...(custom ? { custom } : {}) }
      }))
      setSent(true)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  return <section className="rw-prompt" aria-label="Questions">
    {question.questions.map((item) => <fieldset key={item.id} disabled={!enabled || busy || sent}>
      <legend>{item.question}</legend>
      {item.options.map((option) => <label key={option.label}>
        <input type={item.multiSelect ? "checkbox" : "radio"} name={`${id}-${item.id}`}
          checked={answers[item.id]?.selected.includes(option.label) ?? false} onChange={() => select(item, option.label)} />
        <span>{option.label}{option.description && <small>{option.description}</small>}</span>
      </label>)}
      {item.allowCustom && <input aria-label="Custom answer" placeholder="Custom answer" value={answers[item.id]?.custom ?? ""}
        onChange={(event) => {
          const custom = event.target.value
          const selected = item.multiSelect || !custom.trim() ? answers[item.id]?.selected ?? [] : []
          setAnswers({ ...answers, [item.id]: { questionId: item.id, selected, custom } })
        }} />}
    </fieldset>)}
    {error && <p className="rw-error" role="alert">{error}</p>}
    <div>
      <Button size="sm" variant="tertiary" isDisabled={!enabled || busy || sent} onPress={() => { void send(true) }}>Decline</Button>
      <Button size="sm" isDisabled={!enabled || !complete || busy || sent} onPress={() => { void send(false) }}>{sent ? "Sent" : "Send answers"}</Button>
    </div>
  </section>
}
