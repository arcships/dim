import { File, Folder } from "lucide-react"
import { useEffect, useId, useLayoutEffect, useState, type KeyboardEvent } from "react"
import { mentionTrigger, type FileMention, type MentionTrigger } from "../mentions"
import type { FileCandidate } from "../rpc/protocol"
import { chat } from "./stores"

export function useFileMentions(sessionId: string, enabled: boolean, replace: (text: string) => void) {
  const [trigger, setTrigger] = useState<MentionTrigger>()
  const [dismissed, setDismissed] = useState<string>()
  const [options, setOptions] = useState<FileCandidate[]>([])
  const [active, setActive] = useState(0)
  const [error, setError] = useState("")
  const [mentions, setMentions] = useState<FileMention[]>([])
  const [caret, setCaret] = useState<{ field: HTMLTextAreaElement; position: number }>()
  const listId = useId()
  const key = trigger && `${trigger.start}:${trigger.query}`
  const open = enabled && trigger !== undefined && key !== dismissed
  const activeId = `${listId}-${active}`

  useEffect(() => {
    setOptions([])
    setActive(0)
    setError("")
    if (!open || !trigger) return
    let current = true
    const timeout = window.setTimeout(() => {
      void chat.searchFiles(sessionId, trigger.query).then((found) => {
        if (current) setOptions(found)
      }, (cause) => {
        if (current) setError(cause instanceof Error ? cause.message : String(cause))
      })
    }, 80)
    return () => { current = false; clearTimeout(timeout) }
  }, [open, key, sessionId])

  useLayoutEffect(() => {
    if (!caret) return
    caret.field.focus()
    caret.field.setSelectionRange(caret.position, caret.position)
    update(caret.field)
    setCaret(undefined)
  }, [caret])

  useEffect(() => {
    if (open && options.length > 0) document.getElementById(activeId)?.scrollIntoView({ block: "nearest" })
  }, [open, activeId, options.length])

  function update(field: HTMLTextAreaElement) {
    const next = enabled && field.selectionStart === field.selectionEnd ? mentionTrigger(field.value, field.selectionStart) : undefined
    if (next?.start !== trigger?.start || next?.query !== trigger?.query) setDismissed(undefined)
    setTrigger(next)
  }

  function choose(field: HTMLTextAreaElement, candidate: FileCandidate) {
    if (!trigger) return
    const label = `@${candidate.relativePath}`
    const inserted = candidate.dir ? `${label}/` : `${label} `
    replace(field.value.slice(0, trigger.start) + inserted + field.value.slice(field.selectionStart))
    if (!candidate.dir) setMentions((previous) => [...previous.filter((item) => item.label !== label), { label, path: candidate.absolutePath }])
    setCaret({ field, position: trigger.start + inserted.length })
  }

  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (!open || event.nativeEvent.isComposing || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return false
    if (event.key === "Escape") {
      setDismissed(key)
    } else if (options.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      setActive((index) => (index + (event.key === "ArrowDown" ? 1 : options.length - 1)) % options.length)
    } else if (options.length && (event.key === "Enter" || event.key === "Tab")) {
      const option = options[active]
      if (!option) return false
      choose(event.currentTarget, option)
    } else return false
    event.preventDefault()
    event.stopPropagation()
    return true
  }

  return {
    mentions, listId, activeId, options, active, open, error, update, choose, keyDown,
    blur: () => { setTrigger(undefined); setDismissed(undefined) },
    clear: () => { setMentions([]); setTrigger(undefined); setDismissed(undefined) },
  }
}

export function FileMentions({ state, field }: { state: ReturnType<typeof useFileMentions>; field: HTMLTextAreaElement | null }) {
  if (!state.open) return null
  return <>
    {state.error && <p className="rw-error" role="status">{state.error}</p>}
    {state.options.length > 0 && <ul className="rw-file-mentions" id={state.listId} role="listbox" aria-label="Workspace files">
      {state.options.map((candidate, index) => <li key={candidate.absolutePath} id={`${state.listId}-${index}`}
        role="option" aria-selected={state.active === index} onPointerDown={(event) => event.preventDefault()}
        onClick={() => { if (field) state.choose(field, candidate) }}>
        {candidate.dir ? <Folder size={15} /> : <File size={15} />}<span>{candidate.relativePath}{candidate.dir ? "/" : ""}</span>
      </li>)}
    </ul>}
  </>
}
