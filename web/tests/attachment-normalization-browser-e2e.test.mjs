import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
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
    child.once('exit', code => reject(new Error(`Ternilo web exited with ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin, diagnostics: () => diagnostics }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathEditor.fill(workspace)
  await pathEditor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

test('phone photos are normalized before preview and remain operable on mobile', { timeout: 90_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-image-normalization-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    const input = page.getByRole('textbox', { name: '输入任务' })

    const sourceBytes = await input.evaluate(async element => {
      const canvas = document.createElement('canvas')
      canvas.width = 2800
      canvas.height = 1800
      const context = canvas.getContext('2d')
      const pixels = context.createImageData(canvas.width, canvas.height)
      let state = 0x12345678
      for (let index = 0; index < pixels.data.length; index += 4) {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0
        pixels.data[index] = state & 255
        pixels.data[index + 1] = (state >>> 8) & 255
        pixels.data[index + 2] = (state >>> 16) & 255
        pixels.data[index + 3] = 255
      }
      context.putImageData(pixels, 0, 0)
      const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/jpeg', 1))
      const transfer = new DataTransfer()
      transfer.items.add(new File([blob], 'phone.jpg', { type: 'image/jpeg' }))
      element.dispatchEvent(new ClipboardEvent('paste', {
        bubbles: true, cancelable: true, clipboardData: transfer,
      }))
      return blob.size
    })
    assert.equal(sourceBytes > 4 * 1024 * 1024, true, `source fixture was only ${sourceBytes} bytes`)
    assert.equal(sourceBytes <= 20 * 1024 * 1024, true, `source fixture was ${sourceBytes} bytes`)

    const preview = page.getByRole('button', { name: '预览 phone.jpg' })
    await preview.waitFor()
    await page.waitForFunction(() => document.querySelector('button[aria-label="预览 phone.jpg"] img')?.naturalWidth > 0)
    const normalized = await preview.locator('img').evaluate(image => ({
      width: image.naturalWidth,
      height: image.naturalHeight,
      encodedLength: image.src.length,
      mediaType: image.src.slice(5, image.src.indexOf(';')),
    }))
    assert.equal(normalized.width * normalized.height <= 2048 * 2048 + 4096, true, JSON.stringify(normalized))
    assert.equal(normalized.encodedLength <= 4 * 1024 * 1024 * 4 / 3 + 64, true, JSON.stringify(normalized))
    assert.equal(normalized.mediaType, 'image/jpeg')

    await page.setViewportSize({ width: 390, height: 844 })
    const remove = page.getByRole('button', { name: '移除 phone.jpg' })
    const [previewBox, removeBox, overflow] = await Promise.all([
      preview.boundingBox(),
      remove.boundingBox(),
      page.evaluate(() => ({ viewport: innerWidth, document: document.documentElement.scrollWidth, body: document.body.scrollWidth })),
    ])
    assert.equal(previewBox.width >= 40 && previewBox.height >= 40, true, JSON.stringify(previewBox))
    assert.equal(removeBox.width >= 40 && removeBox.height >= 40, true, JSON.stringify(removeBox))
    assert.equal(overflow.document <= overflow.viewport && overflow.body <= overflow.viewport, true, JSON.stringify(overflow))
    await remove.click()
    await preview.waitFor({ state: 'detached' })
    assert.deepEqual(pageErrors, [])
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
