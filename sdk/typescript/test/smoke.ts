import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

import { HarnessClient } from '../src/client.ts'

const binary = process.env.TERNILO_BIN
if (binary === undefined) throw new Error('TERNILO_BIN is required')
const root = await mkdtemp(join(tmpdir(), 'ternilo-typescript-sdk-'))
try {
  const workspace = join(root, 'workspace')
  await mkdir(workspace)
  const client = new HarnessClient({
    command: resolve(binary),
    args: ['rpc', '--data-dir', join(root, 'data')],
  })
  try {
    const written = await client.run('/write proof.txt typescript sdk proof', { workspacePath: workspace })
    assert.equal(written.status, 'idle')
    const result = await client.run('/read proof.txt', { workspacePath: workspace })
    assert.equal(result.status, 'idle')
    assert.equal(JSON.parse(result.answer).content, 'typescript sdk proof')
    assert.equal(await readFile(join(workspace, 'proof.txt'), 'utf8'), 'typescript sdk proof')
    assert(result.events.some(event => event.type === 'tool_call_finished'))
    const unconfigured = await client.run('Needs a model', { workspacePath: workspace })
    assert.equal(unconfigured.status, 'failed')
    assert.match(JSON.stringify(unconfigured.notifications), /Configure a Provider/)
  } finally {
    await client.close()
  }
} finally {
  await rm(root, { recursive: true, force: true })
}
