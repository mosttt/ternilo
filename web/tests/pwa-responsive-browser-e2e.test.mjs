import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_'))),
      XDG_CACHE_HOME: path.join(dataDirectory, 'xdg-cache'),
      XDG_CONFIG_HOME: path.join(dataDirectory, 'xdg-config'),
      XDG_DATA_HOME: path.join(dataDirectory, 'xdg-data'),
      XDG_STATE_HOME: path.join(dataDirectory, 'xdg-state'),
    },
  })
  let diagnostics = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { diagnostics += chunk })
  const origin = new Promise((resolve, reject) => {
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      output += chunk
      const match = output.match(/Ternilo local web: (http:\/\/[^\s]+)/)
      if (match) resolve(match[1])
    })
    child.once('error', reject)
    child.once('exit', code => reject(new Error(`Ternilo exited ${code}: ${diagnostics}`)))
  })
  return { child, origin }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function api(page, endpoint, init = {}) {
  return page.evaluate(async ({ endpoint, init }) => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1${endpoint}`, {
      ...init,
      headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        ...(init.body === undefined ? {} : { 'content-type': 'application/json' }),
      },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { endpoint, init })
}

async function workerVersion(page) {
  return page.evaluate(async () => {
    const registration = await navigator.serviceWorker.ready
    const worker = registration.active
    if (!worker) throw new Error('active service worker missing')
    return new Promise((resolve, reject) => {
      const channel = new MessageChannel()
      const timer = setTimeout(() => reject(new Error('service worker version timeout')), 5_000)
      channel.port1.onmessage = event => {
        clearTimeout(timer)
        resolve(event.data)
      }
      worker.postMessage({ type: 'GET_VERSION' }, [channel.port2])
    })
  })
}

async function geometry(page) {
  return page.evaluate(() => {
    const frame = document.querySelector('[data-app-frame]')
    const sidebar = document.querySelector('[data-app-sidebar-column]')
    const center = document.querySelector('[data-app-center-column]')
    if (!(frame instanceof HTMLElement) || !(sidebar instanceof HTMLElement) || !(center instanceof HTMLElement)) return null
    const sidebarBox = sidebar.getBoundingClientRect()
    const centerBox = center.getBoundingClientRect()
    return {
      viewport: document.documentElement.clientWidth,
      documentWidth: document.documentElement.scrollWidth,
      bodyWidth: document.body.scrollWidth,
      mobile: frame.hasAttribute('data-mobile'),
      sidebarWidth: sidebarBox.width,
      sidebarRight: sidebarBox.right,
      centerLeft: centerBox.left,
      centerRight: centerBox.right,
      centerWidth: centerBox.width,
    }
  })
}

test('PWA installs, upgrades, opens an explicit credential-free offline shell, and spans 320..1440', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-pwa-'))
  const workspacePath = path.join(dataDirectory, 'visible-online-workspace')
  await mkdir(workspacePath)
  const server = startTernilo(dataDirectory)
  let context
  try {
    const origin = await server.origin
    context = await chromium.launchPersistentContext(path.join(dataDirectory, 'browser-profile'), {
      headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
      viewport: { width: 1440, height: 900 }, serviceWorkers: 'allow',
    })
    const page = await context.newPage()
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.locator('[data-app-frame]').waitFor()

    const manifestResponse = await page.request.get(`${origin}/manifest.webmanifest`)
    assert.equal(manifestResponse.headers()['content-type'], 'application/manifest+json; charset=utf-8')
    const manifest = await manifestResponse.json()
    assert.equal(manifest.name, 'Ternilo')
    assert.equal(manifest.display, 'standalone')
    assert.deepEqual(manifest.icons.filter(icon => icon.type === 'image/png').map(icon => [icon.sizes, icon.purpose]), [
      ['192x192', 'any'], ['512x512', 'any'], ['512x512', 'maskable'],
    ])
    for (const icon of manifest.icons) {
      const response = await page.request.get(new URL(icon.src, origin).href)
      assert.equal(response.ok(), true, icon.src)
      assert.match(response.headers()['content-type'], icon.type === 'image/png' ? /^image\/png/ : /^image\/svg\+xml/)
    }
    const cdp = await context.newCDPSession(page)
    await cdp.send('Page.enable')
    const appManifest = await cdp.send('Page.getAppManifest')
    assert.equal(appManifest.errors.length, 0, JSON.stringify(appManifest.errors))
    const installability = await cdp.send('Page.getInstallabilityErrors')
    assert.deepEqual(installability.installabilityErrors, [])

    await page.waitForFunction(() => navigator.serviceWorker.controller !== null)
    const builtVersion = (await readFile(path.join(webRoot, 'dist', 'pwa-version.txt'), 'utf8')).trim()
    assert.deepEqual(await workerVersion(page), {
      version: builtVersion,
      cache: `ternilo-web-${builtVersion}`,
    })
    const cacheInventory = await page.evaluate(async () => {
      const names = await caches.keys()
      const urls = []
      for (const name of names) {
        const cache = await caches.open(name)
        urls.push(...(await cache.keys()).map(request => request.url))
      }
      const offline = await (await caches.match('/offline.html')).text()
      return { names, paths: urls.map(value => new URL(value).pathname), offline }
    })
    assert.deepEqual(cacheInventory.names, [`ternilo-web-${builtVersion}`])
    assert.equal(cacheInventory.paths.includes('/'), false)
    assert.equal(cacheInventory.paths.includes('/assets/boot.js'), false)
    assert.equal(cacheInventory.paths.some(value => value.startsWith('/api/')), false)
    assert.match(cacheInventory.offline, /assets\/offline-boot\.js/)
    assert.doesNotMatch(cacheInventory.offline, /apiToken/)

    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    await api(page, '/sessions', {
      method: 'POST',
      body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' },
    })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByText('visible-online-workspace', { exact: true }).first().waitFor()

    // A newly activated worker removes only old Ternilo caches and surfaces a real update prompt.
    await page.evaluate(() => caches.open('ternilo-web-obsolete-browser-fixture'))
    await page.evaluate(async () => {
      await navigator.serviceWorker.register('/service-worker.js?release=browser-update', {
        scope: '/', updateViaCache: 'none',
      })
    })
    await page.locator('[data-pwa-update]').waitFor()
    await page.waitForFunction(async () => !(await caches.keys()).includes('ternilo-web-obsolete-browser-fixture'))
    const updateButton = page.getByRole('button', { name: '刷新应用' })
    await updateButton.waitFor()
    const updateBox = await updateButton.boundingBox()
    assert.ok(updateBox && updateBox.height >= 40)

    // Width sweep locks the mobile breakpoint, 780px tablet rail, and desktop restoration.
    const widths = [320, 360, 390, 480, 600, 759, 760, 761, 780, 900, 1023, 1024, 1200, 1440]
    for (const width of widths) {
      await page.setViewportSize({ width, height: 900 })
      await page.waitForFunction(expected => {
        const frame = document.querySelector('[data-app-frame]')
        const sidebar = document.querySelector('[data-app-sidebar-column]')
        if (!(frame instanceof HTMLElement) || !(sidebar instanceof HTMLElement)) return false
        const mobile = frame.hasAttribute('data-mobile')
        const sidebarRight = sidebar.getBoundingClientRect().right
        return mobile === (expected <= 760) && (expected > 760 || sidebarRight <= 1)
      }, width)
      // Grid/sidebar tracks deliberately animate on breakpoint changes. Assert
      // the settled contract instead of sampling an intermediate frame.
      await page.waitForTimeout(350)
      const value = await geometry(page)
      assert.ok(value, `${width}: frame missing`)
      assert.equal(value.documentWidth <= value.viewport, true, `${width}: document overflow ${JSON.stringify(value)}`)
      assert.equal(value.bodyWidth <= value.viewport, true, `${width}: body overflow ${JSON.stringify(value)}`)
      assert.equal(value.mobile, width <= 760, `${width}: breakpoint`)
      assert.equal(value.centerLeft >= -1 && value.centerRight <= width + 1, true, `${width}: center ${JSON.stringify(value)}`)
      if (width <= 760) assert.equal(value.sidebarRight <= 1, true, `${width}: closed drawer ${JSON.stringify(value)}`)
      if (width > 760 && width < 1024) {
        assert.equal(Math.abs(value.sidebarWidth - 56) <= 1, true, JSON.stringify(value))
        assert.equal(Math.abs(value.centerWidth - (width - 56)) <= 1, true, JSON.stringify(value))
      }
      if (width === 1024) assert.equal(Math.abs(value.sidebarWidth - 280) <= 1, true, JSON.stringify(value))
    }
    for (const viewport of [{ width: 390, height: 844 }, { width: 390, height: 430 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      await page.waitForTimeout(350)
      const value = await geometry(page)
      assert.ok(value)
      assert.equal(value.documentWidth <= value.viewport && value.bodyWidth <= value.viewport, true, `${viewport.width}x${viewport.height}`)
    }
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileSidebar = page.getByRole('button', { name: '打开侧边栏' })
    const mobileBox = await mobileSidebar.boundingBox()
    assert.ok(mobileBox && mobileBox.width >= 40 && mobileBox.height >= 40)

    await Promise.all([
      page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
      updateButton.click(),
    ])
    await page.getByText('visible-online-workspace', { exact: true }).first().waitFor()

    await context.setOffline(true)
    await page.reload({ waitUntil: 'domcontentloaded' })
    const offlineAlert = page.locator('[data-offline-shell]')
    await offlineAlert.waitFor()
    assert.match(await offlineAlert.textContent(), /离线应用外壳/)
    assert.equal(await page.evaluate(() => window.__TERNILO_BOOT__?.offline), true)
    assert.equal(await page.getByText('visible-online-workspace', { exact: true }).count(), 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)

    await context.setOffline(false)
    // Verify recovery below without coupling the click to service-worker navigation bookkeeping.
    await offlineAlert.getByRole('button', { name: '重试连接' }).click({ noWaitAfter: true })
    await page.waitForFunction(() => window.__TERNILO_BOOT__?.offline !== true)
    await page.getByText('visible-online-workspace', { exact: true }).first().waitFor()
    assert.equal(await page.locator('[data-offline-shell]').count(), 0)
    assert.deepEqual(pageErrors, [])
  } finally {
    if (context) await context.close()
    await stopProcess(server.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
