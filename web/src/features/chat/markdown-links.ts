/**
 * The workspace path a Markdown link points at, or undefined when the browser
 * can open the href itself (http(s) in a new tab, mailto:, an in-page anchor).
 *
 * A path has nowhere to go here: `docs/packages/cli/README.md` resolves against
 * the chat URL and a click would leave the session for a not-found page.
 */
const URL_SCHEME = /^[a-z][a-z\d+.-]*:/i

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

export function markdownLinkPath(href: string): string | undefined {
  if (href === "" || href.startsWith("#")) return undefined
  if (href.startsWith("file:")) return decodePath(href.replace(/^file:(\/\/)?/, ""))
  if (URL_SCHEME.test(href)) return undefined
  return decodePath(href.split(/[?#]/)[0] ?? href)
}
