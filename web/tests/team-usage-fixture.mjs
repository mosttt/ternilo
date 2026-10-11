import assert from 'node:assert/strict'
import { mkdir } from 'node:fs/promises'
import path from 'node:path'
import { until } from './model-device-fixture.mjs'

export async function verifyTeamUsage(page, request, sessionId, locale = 'zh', artifact = locale) {
  const labels = { zh: ['打开 Agent Team', '用量', '未知'], en: ['Open Agent Team', 'Usage', 'Unknown'], ko: ['에이전트 팀 열기', '사용량', '알 수 없음'] }[locale]
  const snapshot = await request(`/sessions/${sessionId}/team`)
  const state = await request('/state')
  const parent = state.sessions.find(session => session.identity.session_id === sessionId)
  const children = state.sessions.filter(session => session.parent_session_id === sessionId)
  const totals = []
  for (const session of [parent, ...children]) {
    const events = await request(`/sessions/${session.identity.session_id}/events`)
    const responses = events.filter(event => event.type === 'assistant_message' && event.response?.usage && !event.response.replayed)
    totals.push({ session, input: responses.reduce((sum, event) => sum + event.response.usage.input_tokens, 0), output: responses.reduce((sum, event) => sum + event.response.usage.output_tokens, 0) })
  }
  if (page.viewportSize().width < 400) {
    await page.getByRole('button', { name: { zh: '更多会话操作', en: 'More session actions', ko: '세션 작업 더 보기' }[locale], exact: true }).click()
    await page.getByRole('menuitem', { name: locale === 'ko' ? '에이전트 팀' : 'Agent Team', exact: true }).click()
  } else await page.getByRole('button', { name: labels[0], exact: true }).first().click()
  const panel = page.locator('[data-agent-team-panel]')
  await panel.getByRole('tab', { name: labels[1], exact: true }).click()
  const usage = panel.locator('[data-agent-team-usage]')
  await until(() => usage.getAttribute('aria-busy'), value => value === 'false', 'loaded usage')
  assert.equal(await usage.locator('[data-usage-member]').count(), snapshot.members.length)
  for (const total of totals) {
    const member = total.session.identity.session_id === sessionId ? snapshot.members.find(member => member.role === 'lead') : snapshot.members.find(member => member.subagent_id === total.session.subagent.subagent_id)
    const facts = usage.locator(`[data-usage-member="${member.id}"] dd`)
    assert.equal((await facts.nth(0).innerText()).replaceAll(',', ''), String(total.input))
    assert.equal((await facts.nth(1).innerText()).replaceAll(',', ''), String(total.output))
  }
  assert.ok((await usage.innerText()).includes('lead-model'))
  assert.ok((await usage.innerText()).includes('role-model'))
  assert.ok((await usage.locator('[data-usage-estimate]').innerText()).includes(labels[2]))
  await usage.locator('summary').click()
  for (const input of await usage.locator('input[type="number"]').all()) await input.fill('2')
  const expected = totals.reduce((sum, total) => sum + total.input + total.output, 0) * 2 / 1_000_000
  assert.ok((await usage.locator('[data-usage-estimate]').innerText()).includes(`USD ${expected.toLocaleString(undefined, { maximumFractionDigits: 6 })}`))
  await usage.locator('select').selectOption('CNY')
  assert.ok((await usage.locator('[data-usage-estimate]').innerText()).includes(labels[2]))
  await usage.locator('select').selectOption('USD')
  assert.equal(await usage.locator('input').first().inputValue(), '2')
  if (page.viewportSize().width < 400) {
    const body = panel.locator('[role="tabpanel"]:visible').locator('..')
    await usage.locator('summary').scrollIntoViewIfNeeded()
    const bounds = await body.boundingBox(), before = await body.evaluate(element => element.scrollTop)
    const touch = await page.context().newCDPSession(page)
    const x = bounds.x + bounds.width - 14, y = bounds.y + bounds.height - 20
    await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
    for (let step = 1; step <= 8; step++) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - 170 * step / 8 }] })
    await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await until(() => body.evaluate(element => element.scrollTop), value => value > before, 'native usage panel scroll')
    await touch.detach()
  }
  await usage.locator('[data-usage-member]').last().scrollIntoViewIfNeeded()
  const bounds = await usage.locator('[data-usage-member]').last().boundingBox()
  assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width)
  assert.equal(await panel.evaluate(element => element.scrollWidth > element.clientWidth), false)
  if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
    await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
    await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `team-usage-${artifact}.png`) })
  }
  await page.keyboard.press('Escape')
}
