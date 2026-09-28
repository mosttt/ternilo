import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const inputs = [
  'dist/index.html',
  'dist/assets/app.css',
  'dist/assets/app.js',
  'public/manifest.webmanifest',
  'public/assets/icon.svg',
  'public/assets/icon-192.png',
  'public/assets/icon-512.png',
  'public/assets/icon-maskable-512.png',
  'service-worker.js',
]

const digest = createHash('sha256')
for (const relative of inputs) {
  digest.update(relative)
  digest.update('\0')
  digest.update(await readFile(path.join(root, relative)))
  digest.update('\0')
}
await writeFile(path.join(root, 'dist', 'pwa-version.txt'), `${digest.digest('hex').slice(0, 20)}\n`)
