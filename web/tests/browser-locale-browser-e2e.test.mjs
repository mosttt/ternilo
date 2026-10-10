import assert from 'node:assert/strict'
import test from 'node:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')

test('Setup, login and the workbench follow browser language until the user saves a choice', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-browser-locale-'))
  const origin = `http://127.0.0.1:${await freePort()}`
  const clean = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
  const server = startProcess(binary, ['serve', '--config-dir', directory, '--listen', new URL(origin).host], clean)
  const owner = { username: 'locale-owner', email: 'locale-owner@example.test', password: 'locale-owner-password' }
  let browser
  const errors = []
  const labels = {
    zh: { setup: '设置 Ternilo Server', language: 'Language', option: 'zh-CN', tag: 'zh-CN', username: '用户名', password: '密码', login: '登录', settings: '用户设置' },
    en: { setup: 'Set up Ternilo Server', language: 'Language', option: 'English', tag: 'en', username: 'Username', password: 'Password', login: 'Sign in', settings: 'User settings' },
    ko: { setup: 'Ternilo 서버 설정', language: '언어', option: '한국어', tag: 'ko', username: '사용자 이름', password: '비밀번호', login: '로그인', settings: '사용자 설정' },
  }
  try {
    await waitForHttp(`${origin}/readyz`, server)
    const key = server.diagnostics().match(/Initialization Key: (\S+)/)?.[1]
    assert.ok(key)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const cases = []
    for (const [locale, language, manual] of [['zh-CN', 'zh', 'en'], ['en-US', 'en', 'zh'], ['ko-KR', 'ko', 'zh'], ['en-US', 'en', 'ko'], ['fr-FR', 'en', null]]) {
      const context = await browser.newContext({ locale, viewport: { width: 390, height: 844 }, serviceWorkers: 'block' })
      const page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      await page.goto(origin)
      await page.getByRole('heading', { name: labels[language].setup, exact: true }).waitFor()
      assert.equal(await page.evaluate(() => navigator.language), locale)
      assert.equal(await page.evaluate(() => document.documentElement.lang), labels[language].tag)
      assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.locale')), null, 'automatic language selection does not become a manual preference')
      if (manual) {
        await page.getByRole('combobox', { name: labels[language].language, exact: true }).click()
        await page.getByRole('option', { name: labels[manual].option, exact: true }).click()
        assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.locale')), manual)
        await page.reload()
        await page.getByRole('heading', { name: labels[manual].setup, exact: true }).waitFor()
      }
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
      cases.push({ context, page, language: manual ?? language })
    }
    await serverRequest(origin, '/setup', { body: { ...owner, setup_token: key, database: { kind: 'sqlite' }, public_url: origin } })
    for (const [locale, language] of [['zh-CN', 'zh'], ['en-US', 'en'], ['ko-KR', 'ko']]) {
      const context = await browser.newContext({ locale, viewport: { width: 1280, height: 800 }, serviceWorkers: 'block' })
      const page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      await page.goto(origin)
      cases.push({ context, page, language })
    }
    for (const { context, page, language } of cases) {
      await page.reload()
      await page.getByLabel(labels[language].username, { exact: true }).fill(owner.username)
      await page.getByLabel(labels[language].password, { exact: true }).fill(owner.password)
      await page.getByRole('button', { name: labels[language].login, exact: true }).click()
      await page.getByRole('button', { name: labels[language].settings, exact: true }).waitFor()
      assert.equal(await page.evaluate(() => document.documentElement.lang), labels[language].tag)
      await page.reload()
      await page.getByRole('button', { name: labels[language].settings, exact: true }).waitFor()
      await context.close()
    }
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
