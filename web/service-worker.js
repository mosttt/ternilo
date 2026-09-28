const VERSION = '__TERNILO_ASSET_VERSION__'
const CACHE_PREFIX = 'ternilo-web-'
const CACHE = `${CACHE_PREFIX}${VERSION}`
const OFFLINE_SHELL = '/offline.html'
const SHELL = [
  OFFLINE_SHELL,
  '/assets/offline-boot.js',
  '/assets/app.css',
  '/assets/app.js',
  '/assets/icon.svg',
  '/assets/icon-192.png',
  '/assets/icon-512.png',
  '/assets/icon-maskable-512.png',
  '/manifest.webmanifest',
]

self.addEventListener('install', event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(
    SHELL.map(path => new Request(path, { cache: 'reload' })),
  )).then(() => self.skipWaiting()))
})

self.addEventListener('activate', event => {
  event.waitUntil(caches.keys()
    .then(keys => Promise.all(
      keys.filter(key => key !== CACHE && key.startsWith(CACHE_PREFIX)).map(key => caches.delete(key)),
    ))
    .then(() => self.clients.claim()))
})

self.addEventListener('fetch', event => {
  const url = new URL(event.request.url)
  if (event.request.method !== 'GET' || url.origin !== self.location.origin) return
  if (event.request.mode === 'navigate') {
    event.respondWith(fetch(event.request).catch(async () => {
      const cached = await caches.match(OFFLINE_SHELL, { cacheName: CACHE })
      return cached ?? Response.error()
    }))
    return
  }
  if (!SHELL.includes(url.pathname) || url.pathname === OFFLINE_SHELL) return
  event.respondWith(caches.open(CACHE).then(async cache => {
    const cached = await cache.match(event.request, { ignoreSearch: true })
    return cached ?? fetch(event.request)
  }))
})

self.addEventListener('message', event => {
  if (event.data?.type === 'SKIP_WAITING') self.skipWaiting()
  if (event.data?.type === 'GET_VERSION') event.ports[0]?.postMessage({ version: VERSION, cache: CACHE })
})
