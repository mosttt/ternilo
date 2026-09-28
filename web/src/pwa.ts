export type PwaUpdateListener = () => void

/** Register the root worker and report only replacements, never the first install. */
export async function registerPwa(onUpdate: PwaUpdateListener): Promise<() => void> {
  if (!('serviceWorker' in navigator)) return () => undefined
  const hadController = navigator.serviceWorker.controller !== null
  let reported = false
  const report = () => {
    if (!hadController || reported) return
    reported = true
    onUpdate()
  }
  const onControllerChange = () => report()
  navigator.serviceWorker.addEventListener('controllerchange', onControllerChange)
  try {
    const registration = await navigator.serviceWorker.register('/service-worker.js', {
      scope: '/',
      updateViaCache: 'none',
    })
    if (registration.waiting && hadController) registration.waiting.postMessage({ type: 'SKIP_WAITING' })
    const check = () => {
      if (document.visibilityState === 'visible') void registration.update().catch(() => undefined)
    }
    document.addEventListener('visibilitychange', check)
    return () => {
      navigator.serviceWorker.removeEventListener('controllerchange', onControllerChange)
      document.removeEventListener('visibilitychange', check)
    }
  } catch {
    navigator.serviceWorker.removeEventListener('controllerchange', onControllerChange)
    return () => undefined
  }
}
