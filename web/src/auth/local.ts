export function usesLocalBootstrap(): boolean {
  const boot = window.__TERNILO_BOOT__
  return Boolean(boot?.apiToken && !boot.remote && !boot.platform && !boot.offline)
}

export async function refreshLocalToken(): Promise<string> {
  const response = await fetch('/', {
    cache: 'no-store',
    credentials: 'same-origin',
    redirect: 'error',
    headers: { accept: 'text/html' },
  })
  if (!response.ok) throw new Error('Local bootstrap is unavailable')
  const html = await response.text()
  const payload = html.match(/<script>\s*window\.__TERNILO_BOOT__\s*=\s*(\{[\s\S]*?\});?\s*<\/script>/)?.[1]
  if (!payload) throw new Error('Local bootstrap is missing')
  const boot = JSON.parse(payload) as Record<string, unknown>
  if (boot.remote !== false || boot.platform || boot.offline || typeof boot.apiToken !== 'string' || !boot.apiToken.trim()) {
    throw new Error('The endpoint is not a local Ternilo bootstrap')
  }
  return boot.apiToken.trim()
}
