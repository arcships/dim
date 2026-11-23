import { Button, Dropdown, Modal } from "@heroui/react"
import { Ellipsis } from "lucide-react"
import { useState } from "react"
import type { LineView } from "../lines"
import { ROOT_BRANCH_ID } from "../rpc/protocol"
import { chat, connection } from "./stores"

export const branchStates: Record<LineView["state"]["kind"], string> = {
  main: "Root branch",
  working: "Running",
  waiting: "Waiting",
  ending: "Ending",
  ended: "Ended",
  "ended-itself": "Ended",
  "ended-by-operation": "Ended",
  "ended-with": "Ended with parent",
  "creation-failed": "Could not start",
}

export function BranchActions({ sessionId, line, lines, defaultBranchId, connected, useDefault, create }: {
  sessionId: string
  line: LineView
  lines: readonly LineView[]
  defaultBranchId: string
  connected: boolean
  useDefault: () => void
  create?: () => void
}) {
  const [confirm, setConfirm] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState("")
  const protectedLines = new Set([ROOT_BRANCH_ID, defaultBranchId])
  for (let entry = lines.find(({ id }) => id === defaultBranchId); entry?.parentId && !protectedLines.has(entry.parentId);) {
    const parentId = entry.parentId
    protectedLines.add(parentId)
    entry = lines.find(({ id }) => id === parentId)
  }

  const open = line.state.kind === "working" || line.state.kind === "waiting"
  const canEnd = open && !protectedLines.has(line.id) && connection.supports("session.branch_end")
  const canContinue = line.id !== defaultBranchId && (open || line.id === ROOT_BRANCH_ID) && connection.supports("session.branch_set_default")
  if (!canEnd && !canContinue && !create) return null
  const name = line.purpose || line.id

  async function run(action: "continue" | "end") {
    if (!connected || pending) return
    setPending(true)
    setError("")
    try {
      if (action === "end") {
        await chat.endLine(sessionId, line.id)
        setConfirm(false)
      } else {
        await chat.setDefaultBranch(sessionId, line.id)
        useDefault()
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setPending(false)
    }
  }

  return <>
    <Dropdown>
      <Dropdown.Trigger className="rw-sidebar-icon rw-branch-action" aria-label={`Actions for ${name}`} isDisabled={!connected || pending}>
        <Ellipsis size={15} aria-hidden="true" />
      </Dropdown.Trigger>
      <Dropdown.Popover placement="bottom end">
        <Dropdown.Menu aria-label={`Actions for ${name}`}>
          {create && <Dropdown.Item onAction={create}>New branch</Dropdown.Item>}
          {canContinue && <Dropdown.Item onAction={() => void run("continue")}>Continue from here</Dropdown.Item>}
          {canEnd && <Dropdown.Item variant="danger" onAction={() => { setError(""); setConfirm(true) }}>End branch</Dropdown.Item>}
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
    {error && !confirm && <small className="rw-branch-error" role="alert">{error}</small>}
    {confirm && <Modal isOpen onOpenChange={(value) => { if (!value && !pending) setConfirm(false) }}>
      <Modal.Backdrop isDismissable={!pending}><Modal.Container><Modal.Dialog>
        <Modal.Header><Modal.Heading>End {name}</Modal.Heading></Modal.Header>
        <Modal.Body>
          <p>This ends the branch and its child branches. Their history remains available.</p>
          {error && <p className="rw-error" role="alert">{error}</p>}
        </Modal.Body>
        <Modal.Footer>
          <Button size="sm" variant="tertiary" isDisabled={pending} onPress={() => setConfirm(false)}>Cancel</Button>
          <Button size="sm" variant="danger-soft" isPending={pending} onPress={() => void run("end")}>End branch</Button>
        </Modal.Footer>
      </Modal.Dialog></Modal.Container></Modal.Backdrop>
    </Modal>}
  </>
}
