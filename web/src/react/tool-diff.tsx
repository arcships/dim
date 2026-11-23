import { parsePatchFiles } from "@pierre/diffs"
import { FileDiff } from "@pierre/diffs/react"
import { useMemo } from "react"

export default function ToolDiff({ patch }: { patch: string }) {
  const files = useMemo(() => {
    try { return parsePatchFiles(patch).flatMap((entry) => entry.files) }
    catch { return [] }
  }, [patch])

  if (files.length === 0) return <pre>{patch}</pre>
  return <div className="rw-diff">
    {files.map((file, index) => <FileDiff key={`${file.name}:${index}`} fileDiff={file}
      disableWorkerPool options={{ preferredHighlighter: "shiki-js", diffStyle: "unified", overflow: "scroll" }} />)}
  </div>
}
