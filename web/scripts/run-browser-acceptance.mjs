import { readdirSync } from 'node:fs'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const tests = readdirSync(path.join(webRoot, 'tests'))
  .filter(name => name.endsWith('browser-e2e.test.mjs'))
  .sort()
  .map(name => path.join('tests', name))

if (tests.length === 0) {
  console.error('No browser acceptance tests were found.')
  process.exit(1)
}

console.log(`Running ${tests.length} browser acceptance files serially.`)
const environment = { ...process.env }
delete environment.TERNILO_CLOUD_E2E_CONTAINER
delete environment.TERNILO_CLOUD_E2E_IMAGE
delete environment.TERNILO_RELAY_BROWSER_IMAGE
const result = spawnSync(process.execPath, [
  '--test',
  '--test-concurrency=1',
  ...tests,
], {
  cwd: webRoot,
  env: environment,
  stdio: 'inherit',
})

if (result.error) throw result.error
process.exit(result.status ?? 1)
