import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { chmod, mkdir, readFile, readdir, symlink } from 'node:fs/promises'
import path from 'node:path'
import { execute, repository, waitForHttp } from './platform-e2e-fixture.mjs'

export async function isolatedEnvironment(directory) {
  const environment = { PATH: process.env.PATH, HOME: directory, LANG: 'C.UTF-8', TZ: 'UTC' }
  for (const [key, child] of Object.entries({ XDG_DATA_HOME: 'data', XDG_STATE_HOME: 'state', XDG_CONFIG_HOME: 'config', XDG_CACHE_HOME: 'cache', XDG_RUNTIME_DIR: 'runtime', TMPDIR: 'tmp' })) {
    environment[key] = path.join(directory, child)
    await mkdir(environment[key], { recursive: true, mode: 0o700 })
  }
  return environment
}

export async function digest(filename) {
  return createHash('sha256').update(await readFile(filename)).digest('hex')
}

export async function installPackages(directory, sources, environment, checks) {
  const output = path.join(directory, 'packages')
  const installation = path.join(directory, 'installation')
  await mkdir(installation)
  const binaries = {}
  for (const [component, names] of Object.entries({ local: ['ternilo', 'ternilo-plugin'], server: ['ternilo-server'] })) {
    const input = path.join(directory, `input-${component}`)
    await mkdir(input)
    for (const name of names) await symlink(path.resolve(sources[name]), path.join(input, name))
    await execute(path.join(repository, 'scripts/package-release.sh'), ['--no-build', '--component', component, '--version', 'installation-validation', '--target-name', `${process.platform}-${process.arch}`, '--bin-dir', input, '--output-dir', output], { env: environment, cwd: directory, timeout: 120_000 })
    const archive = (await readdir(output)).find(name => name.startsWith(`ternilo-${component}-`) && name.endsWith('.tar.gz'))
    const checksum = await readFile(path.join(output, `${archive}.sha256`), 'utf8')
    assert.equal(checksum.trim().split(/\s+/)[0], await digest(path.join(output, archive)))
    await execute('tar', ['-xzf', path.join(output, archive), '-C', installation], { env: environment })
    const root = path.join(installation, archive.slice(0, -7))
    const metadata = await readFile(path.join(root, 'RELEASE'), 'utf8')
    assert.ok(metadata.includes(`component=${component}\n`))
    assert.deepEqual((await readdir(path.join(root, 'bin'))).sort(), [...names].sort())
    assert.ok(!(await readdir(path.join(root, 'docs'))).includes('development'))
    for (const name of names) {
      binaries[name] = path.join(root, 'bin', name)
      assert.equal(await digest(binaries[name]), await digest(sources[name]))
      await execute(binaries[name], ['--help'], { cwd: root, env: environment })
    }
    checks.push({ component, archive, sha256: await digest(path.join(output, archive)), binaries: names })
  }
  return { binaries, localRoot: path.dirname(path.dirname(binaries.ternilo)), installation }
}

export async function serve(binary, args, environment, cwd, origin, processes) {
  const child = spawn(binary, args, { cwd, env: environment, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = ''
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  const application = { child, diagnostics: () => output }
  processes.push(application)
  await waitForHttp(origin, application)
  return application
}

export async function archiveDirectory(source, archive, environment) {
  await execute('tar', ['-czf', archive, '-C', source, '.'], { env: environment })
  await chmod(archive, 0o600)
}

export async function extractDirectory(archive, destination, environment) {
  await mkdir(destination, { recursive: true, mode: 0o700 })
  await execute('tar', ['-xzf', archive, '-C', destination], { env: environment })
}

export async function verifyAssets(origin) {
  for (const asset of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${asset}`)
    assert.equal(response.status, 200)
    const actual = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
    assert.equal(actual, await digest(path.join(repository, 'web/dist/assets', asset)))
  }
}

export async function openSession(browser, origin, sessionId, credentials, evidence) {
  const context = await browser.newContext({ viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
  await context.addInitScript(() => localStorage.setItem('ternilo.locale', 'zh'))
  const page = await context.newPage()
  page.setDefaultTimeout(15000)
  page.on('pageerror', error => evidence.errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') evidence.errors.push(message.text()) })
  page.on('response', response => {
    const entry = { pathname: new URL(response.url()).pathname, status: response.status(), method: response.request().method() }
    evidence.network.push(entry)
    if (entry.status >= 400) evidence.errors.push(entry)
  })
  page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') evidence.errors.push(request.failure()) })
  await page.goto(origin)
  if (credentials) {
    await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
    await page.getByLabel('密码', { exact: true }).fill(credentials.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
  }
  const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
  await row.waitFor({ state: 'attached' })
  if (!await row.isVisible()) await page.locator('[data-sidebar-workspace-button]').first().click()
  await row.click()
  await page.locator('[data-input-bar]').waitFor()
  return page
}

export async function captureLayouts(page, artifacts, name) {
  for (const width of [1366, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await page.mouse.move(width - 4, 4)
    if (width < 760) {
      await page.locator('[data-app-frame][data-mobile="true"]').waitFor()
      const close = page.locator('[data-app-sidebar-column]:not([inert]) [data-mobile-sidebar-close]')
      if (await close.count()) await close.click()
      await page.locator('[data-app-sidebar-column][inert]').waitFor({ state: 'attached' })
    }
    await page.keyboard.press('Escape')
    await page.waitForFunction(() => !document.querySelector('[data-radix-popper-content-wrapper]'))
    await page.evaluate(async () => {
      await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
    })
    await page.locator('[data-input-bar]').waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.equal(await page.locator('[data-app-center-column][inert]').count(), 0)
    const input = await page.locator('[data-input-bar]').boundingBox()
    assert.ok(input.x >= 0 && input.x + input.width <= width + 1)
    await page.screenshot({ path: path.join(artifacts, `${name}-${width}.png`) })
  }
}
