import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

async function swipeUp(page, scroll) {
  const box = await scroll.boundingBox()
  assert.ok(box && box.height > 80)
  const cdp = await page.context().newCDPSession(page)
  const x = box.x + box.width / 2
  const y = box.y + box.height - 20
  const distance = Math.min(250, box.height - 40)
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
  for (let step = 1; step <= 8; step++) {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - distance * step / 8 }] })
  }
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
  await cdp.detach()
}

test('computer details scroll by touch while actions remain visible on mobile and short viewports', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-computer-scroll-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let server, browser, page
  const errors = []
  try {
    server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    const identity = server.owner.session
    const scope = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const { enrollment } = await serverRequest(server.origin, `/tenants/${scope.tenantId}/my-computer-enrollments`, {
      token: scope.token, body: { name: 'mobile-details-fixture', ttl_seconds: 600 },
    })
    await serverRequest(server.origin, '/enrollments/consume', { body: { token: enrollment.token } })
    const endpoint = `/tenants/${scope.tenantId}/my-computers/${enrollment.executor_id}`
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, colorScheme: 'light', serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(`${server.origin}/settings/computers`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    const card = page.locator(`[data-platform-computer="${enrollment.executor_id}"]`)
    await card.waitFor()
    for (const [width, height] of [[430, 640], [390, 640], [320, 640]]) {
      await page.setViewportSize({ width, height })
      await card.getByRole('button', { name: '详情与编辑', exact: true }).click()
      const dialog = page.locator('[data-computer-details]')
      const scroll = dialog.locator('[data-computer-details-scroll]')
      await dialog.getByLabel('电脑名称', { exact: true }).waitFor()
      assert.ok(await scroll.evaluate(element => element.scrollHeight > element.clientHeight))
      await swipeUp(page, scroll)
      await page.waitForFunction(() => document.querySelector('[data-computer-details-scroll]')?.scrollTop > 0)
      const footer = dialog.locator('[data-slot="dialog-footer"]')
      const checkFooter = async () => {
        await footer.scrollIntoViewIfNeeded()
        const box = await footer.boundingBox()
        assert.ok(box && box.y >= 0 && box.y + box.height <= (await page.viewportSize()).height + 1)
      }
      await checkFooter()
      await dialog.getByLabel('备注', { exact: true }).fill(`touch-${width}`)
      await dialog.getByLabel('备注', { exact: true }).focus()
      await page.setViewportSize({ width, height: 430 })
      await checkFooter()
      await page.waitForFunction(() => {
        const scroll = document.querySelector('[data-computer-details-scroll]')
        const focused = document.activeElement
        if (!scroll || !focused || !scroll.contains(focused)) return false
        const area = scroll.getBoundingClientRect(), field = focused.getBoundingClientRect()
        return Math.min(area.bottom, field.bottom) - Math.max(area.top, field.top) >= 40
      })
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await page.screenshot({ path: path.join(artifacts, `computer-details-scroll-${width}.png`) })
      await dialog.getByRole('button', { name: '保存电脑信息', exact: true }).click()
      await dialog.waitFor({ state: 'detached' })
      assert.equal((await serverRequest(server.origin, endpoint, scope)).details.management.notes, `touch-${width}`)
      await page.setViewportSize({ width, height })
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'computer-details-scroll-failure.png') }).catch(() => {})
    error.message += `\n${server?.diagnostics() ?? ''}`
    throw error
  } finally {
    await browser?.close()
    if (server) await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
