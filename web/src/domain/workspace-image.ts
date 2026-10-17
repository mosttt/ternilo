/** Only filesystem references enter the authenticated workspace preview route. */
export function isWorkspaceImageReference(value: string): boolean {
  return Boolean(value) && !value.startsWith('//')
    && (/^file:\/\//i.test(value) || /^[a-z]:[\\/]/i.test(value) || !/^[a-z][a-z\d+.-]*:/i.test(value))
    && !value.startsWith('#')
}

export function workspaceImagePath(value: string, root: string): string | null {
  if (!isWorkspaceImageReference(value)) return null
  let path = value
  try {
    if (/^file:\/\//i.test(path)) {
      const url = new URL(path)
      if (url.hostname && url.hostname !== 'localhost') return null
      path = url.pathname.replace(/^\/([a-z]:\/)/i, '$1')
    }
    path = decodeURIComponent(path)
  } catch { return null }
  path = path.replaceAll('\\', '/')
  const base = root.replaceAll('\\', '/').replace(/\/+$/, '')
  const windows = /^[a-z]:\//i.test(path)
  if (path.startsWith('<local-workspace>/')) path = path.slice('<local-workspace>/'.length)
  else if (path.startsWith('/') || windows) {
    const compare = (text: string) => windows ? text.toLowerCase() : text
    if (!root || !compare(path).startsWith(`${compare(base)}/`)) return null
    path = path.slice(base.length + 1)
  }
  const parts = path.split('/').filter(part => part !== '' && part !== '.')
  if (!parts.length || parts.some(part => part === '..' || part.includes('\0'))) return null
  return parts.join('/')
}
