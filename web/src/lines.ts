import { ROOT_BRANCH_ID, type SessionEvent } from "./rpc/protocol"
import type { BranchEntry } from "./rpc/read"

export type LineState =
  | { kind: typeof ROOT_BRANCH_ID }
  | { kind: "working" }
  | { kind: "waiting" }
  | { kind: "ending" }
  | { kind: "ended" }
  | { kind: "ended-itself" }
  | { kind: "ended-by-operation" }
  | { kind: "ended-with"; ancestor: string }
  | { kind: "creation-failed" }

export type LineView = {
  id: string
  parentId: string | null
  /** The first line of the purpose the line opened with. */
  purpose: string
  state: LineState
  /** Where it was cut from its parent; `null` for main. */
  cut: { sequence: number; messages: number | undefined } | null
  depth: number
}

type LineNode = {
  id: string
  parentId: string
  purpose: string
  at: number
  order: number
  cutMessages?: number
  failed: boolean
  ended: LineState | undefined
}

const endsItself = { kind: "ended-itself" } as const
const endsByOperation = { kind: "ended-by-operation" } as const

export class Lines {
  private nodes = new Map<string, LineNode>()
  private running = new Set<string>()
  private snapshot: LineView[] | undefined

  directory(entries: readonly BranchEntry[]): void {
    this.nodes.clear()
    this.snapshot = undefined
    for (const entry of entries) this.detail(entry)
  }

  detail(entry: BranchEntry): void {
    if (entry.branchId === ROOT_BRANCH_ID) return
    this.snapshot = undefined
    const previous = this.nodes.get(entry.branchId)
    this.nodes.set(entry.branchId, {
      id: entry.branchId, parentId: entry.parentBranchId!, at: entry.branchPoint!,
      purpose: entry.purpose === undefined ? previous?.purpose ?? "" : firstLine(entry.purpose),
      cutMessages: entry.cutMessages ?? previous?.cutMessages,
      order: previous?.order ?? this.nodes.size, failed: entry.state === "creation_failed",
      ended: entry.state === "ending" ? { kind: "ending" } : entry.end === undefined ? (entry.state === "ended" ? { kind: "ended" } : previous?.ended) : endedState(entry.end.source, entry.end.ancestor),
    })
  }

  /** Folds one event of `branchId` into the tree. Repeated facts are idempotent. */
  apply(branchId: string, event: SessionEvent): void {
    const content = event.content
    switch (content.kind) {
      case "branch-create": {
        if (this.nodes.has(content.branchId)) return
        this.snapshot = undefined
        this.nodes.set(content.branchId, {
          id: content.branchId,
          parentId: branchId,
          purpose: firstLine(content.prompt),
          at: content.at ?? event.sequence,
          order: this.nodes.size,
          failed: false,
          ended: undefined,
        })
        break
      }
      case "branch-create-failed": {
        this.snapshot = undefined
        const node = this.nodes.get(content.branchId) ?? {
          id: content.branchId,
          parentId: branchId,
          purpose: "",
          at: event.sequence,
          order: this.nodes.size,
          failed: false,
          ended: undefined,
        }
        node.failed = true
        this.nodes.set(node.id, node)
        break
      }
      case "branch-end": {
        const node = this.nodes.get(branchId)
        if (node !== undefined) {
          node.ended = { kind: "ending" }
          this.snapshot = undefined
        }
        break
      }
      case "branch-ended": {
        const node = this.nodes.get(content.branchId)
        if (node !== undefined) {
          node.ended = endedState(content.source, content.ancestor)
          this.snapshot = undefined
        }
        break
      }
      default:
        break
    }
  }

  setRunning(branchId: string, running: boolean): void {
    if (branchId !== ROOT_BRANCH_ID && this.running.has(branchId) !== running) this.snapshot = undefined
    if (running) this.running.add(branchId)
    else this.running.delete(branchId)
  }

  has(branchId: string): boolean {
    return this.nodes.has(branchId)
  }

  /** Every line in tree order: main first, then children by cut and opening order. */
  views(): LineView[] {
    if (this.snapshot) return this.snapshot
    const out: LineView[] = [
      { id: ROOT_BRANCH_ID, parentId: null, purpose: "", state: { kind: ROOT_BRANCH_ID }, cut: null, depth: 0 },
    ]
    const children = new Map<string, LineNode[]>()
    for (const node of this.nodes.values()) {
      const siblings = children.get(node.parentId) ?? []
      siblings.push(node)
      children.set(node.parentId, siblings)
    }
    for (const siblings of children.values()) {
      siblings.sort((left, right) => left.at - right.at || left.order - right.order)
    }
    const seen = new Set<string>()
    const walk = (parent: string, depth: number): void => {
      for (const node of children.get(parent) ?? []) {
        if (seen.has(node.id)) continue
        seen.add(node.id)
        out.push({
          id: node.id,
          parentId: node.parentId,
          purpose: node.purpose,
          state: this.stateOf(node),
          cut: { sequence: node.at, messages: node.cutMessages },
          depth,
        })
        walk(node.id, depth + 1)
      }
    }
    walk(ROOT_BRANCH_ID, 1)
    for (const node of this.nodes.values()) {
      if (seen.has(node.id)) continue
      seen.add(node.id)
      out.push({
        id: node.id,
        parentId: node.parentId,
        purpose: node.purpose,
        state: this.stateOf(node),
        cut: { sequence: node.at, messages: node.cutMessages },
        depth: 1,
      })
      walk(node.id, 2)
    }
    this.snapshot = out
    return out
  }

  private stateOf(node: LineNode): LineState {
    if (node.failed) return { kind: "creation-failed" }
    if (node.ended !== undefined) return node.ended
    return this.running.has(node.id) ? { kind: "working" } : { kind: "waiting" }
  }

}
function firstLine(text: string): string {
  return text.split("\n")[0]?.trim() ?? ""
}

function endedState(source: { kind: string }, ancestor: string | undefined): LineState {
  if (ancestor !== undefined) return { kind: "ended-with", ancestor }
  return source.kind === "tool" ? endsItself : endsByOperation
}
