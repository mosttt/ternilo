import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, initializeServer, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

for (const serverMode of [false, true]) {
  test(`About displays the running ${serverMode ? 'Server' : 'Ternilo'} version without a selected workspace`, { timeout: 90_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-about-version-'))
    const binary = serverMode
      ? process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
      : process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const expected = (await execute(binary, ['--version'])).stdout.trim().split(/\s+/).at(-1)
    const origin = `http://127.0.0.1:${await freePort()}`
    let service, browser, page
    try {
      if (serverMode) service = await initializeServer({ directory: path.join(directory, 'server'), origin })
      else {
        service = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'local')])
        await waitForHttp(origin, service)
      }
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', serviceWorkers: 'block' })
      const errors = []
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      await page.goto(serverMode ? `${origin}/settings/about` : origin)
      if (serverMode) {
        await page.getByLabel('用户名', { exact: true }).fill(service.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(service.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
        await page.locator('[data-about-version]').waitFor()
      }
      for (const [locale, settings, about, version, openSidebar] of [
        ['zh', '设置', '关于与诊断', '当前版本', '打开侧边栏'],
        ['en', 'Settings', 'About & diagnostics', 'Current version', 'Open sidebar'],
        ['ko', '설정', '정보 및 진단', '현재 버전', '사이드바 열기'],
      ]) {
        await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
        await page.setViewportSize({ width: locale === 'zh' ? 1365 : 390, height: locale === 'zh' ? 900 : 600 })
        await page.goto(serverMode ? `${origin}/settings/about` : origin)
        if (!serverMode) {
          if (locale !== 'zh') await page.getByRole('button', { name: openSidebar, exact: true }).click()
          await page.getByRole('button', { name: settings, exact: true }).click()
          await page.getByRole('dialog').getByRole('button', { name: about, exact: true }).click()
        }
        const value = page.locator('[data-about-version]')
        await value.waitFor()
        assert.equal(await value.textContent(), expected)
        await page.getByText(version, { exact: true }).waitFor()
        assert.equal(await page.evaluate(() => window.__TERNILO_BOOT__.version), expected)
        const panel = page.locator('[data-settings-page-content]')
        assert.ok((await panel.textContent()).includes(serverMode ? 'Ternilo Server' : 'Ternilo'))
        await value.scrollIntoViewIfNeeded()
        const bounds = await value.boundingBox()
        assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width)
        if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
          await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
          await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `about-version-${serverMode ? 'server' : 'local'}-${locale}.png`), animations: 'disabled' })
        }
      }
      assert.deepEqual(errors, [])
    } catch (error) {
      throw new Error(`${error.stack}\n${service?.diagnostics() ?? ''}`)
    } finally {
      await browser?.close()
      await stopProcess(service)
      await rm(directory, { recursive: true, force: true })
    }
  })
}
