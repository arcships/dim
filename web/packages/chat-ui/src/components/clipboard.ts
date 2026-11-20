import { useOptionalChatUiHost } from '../contracts/host'

export function useChatUiCopy(): (text: string) => Promise<void> {
  const host = useOptionalChatUiHost()
  if (host === undefined) return text => navigator.clipboard.writeText(text)
  return text => host.commands.copy(text)
}
