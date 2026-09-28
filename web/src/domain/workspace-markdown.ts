export function displayWorkspacePath(html: string, replacement: string | null) {
  if (!replacement || !html.includes('local-workspace')) return html
  const document = new DOMParser().parseFromString(html, 'text/html')
  const nodes = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT)
  while (nodes.nextNode()) {
    const node = nodes.currentNode
    if (node.textContent?.includes('<local-workspace>')) node.textContent = node.textContent.replaceAll('<local-workspace>', replacement)
  }
  return document.body.innerHTML
}
