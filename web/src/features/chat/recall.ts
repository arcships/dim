import type { OperatorBlock, RestoredImage } from "../../rpc/protocol"

export type RecalledImage = RestoredImage & { batch: number }

/** Restores queued content at its original position, preserving image order between text runs. */
export function restoreQueuedContent(
  draft: string,
  images: readonly RecalledImage[],
  blocks: readonly OperatorBlock[],
  at: number,
  batch: number,
): { draft: string; images: RecalledImage[]; next: number } {
  if (blocks.length === 0) return { draft, images: [...images], next: at }
  const position = Math.min(at, draft.length)
  const before = draft.slice(0, position)
  let inserted = position > 0 && !before.endsWith("\n") ? "\n" : ""
  let previousText = false
  const placements: RecalledImage[] = []
  for (const block of blocks) {
    if (block.type === "image") {
      placements.push({ at: position + inserted.length, image: block, batch })
      previousText = false
    } else {
      inserted += (previousText ? "\n" : "") + block.text
      previousText = true
    }
  }
  if (position < draft.length) inserted += "\n"
  const kept = images.map((image) => image.at > position || (image.at === position && image.batch !== batch)
    ? { ...image, at: image.at + inserted.length }
    : image)
  return { draft: before + inserted + draft.slice(position), images: [...kept, ...placements], next: position + inserted.length }
}

/** Returns stored image placements after an edit to the plain-text draft. */
export function moveImageOffsets(previous: string, next: string, images: readonly RecalledImage[]): number[] {
  let start = 0
  while (start < next.length && start < previous.length && next[start] === previous[start]) start++
  let end = 0
  while (end < next.length - start && end < previous.length - start && next[next.length - 1 - end] === previous[previous.length - 1 - end]) end++
  const delta = next.length - previous.length
  const tail = previous.length - end
  return images.map(({ at }) => at <= start ? at : at >= tail ? at + delta : start)
}
