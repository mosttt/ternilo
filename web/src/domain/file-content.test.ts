import { expect, it } from 'vitest'
import { fileBlob, fileBytes, fileImage, textFile } from './file-content'

it('preserves every byte of binary downloads instead of UTF-8 encoding base64 or binary characters', async () => {
  const file = { name: 'bytes.bin', media_type: 'application/octet-stream', content_base64: 'AAH/gPCQgIA=' }
  const expected = [0, 1, 255, 128, 240, 144, 128, 128]
  expect([...fileBytes(file)]).toEqual(expected)
  const blob = fileBlob(file)
  expect(blob.type).toBe('application/octet-stream')
  const contents = await new Promise<ArrayBuffer>((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(reader.result as ArrayBuffer)
    reader.onerror = () => reject(reader.error)
    reader.readAsArrayBuffer(blob)
  })
  expect([...new Uint8Array(contents)]).toEqual(expected)
})

it('preserves UTF-8 text that happens to contain a data URL as literal file contents', () => {
  const text = 'data:text/plain;base64,SGVsbG8=\n中文与 emoji 📄\n'
  const file = { name: 'literal.txt', media_type: 'text/plain', content_base64: Buffer.from(text).toString('base64') }
  expect(new TextDecoder().decode(fileBytes(file))).toBe(text)
  expect(fileImage(file)).toBeNull()
  expect(textFile('text/html; charset=utf-8')).toBe(true)
  expect(textFile('application/problem+json')).toBe(true)
  expect(textFile('application/pdf')).toBe(false)
})

it('builds an image source from the resolved immutable bytes', () => {
  const file = { name: 'image.png', media_type: 'image/png', content_base64: 'iVBORw0KGgo=' }
  expect(fileImage(file)).toBe('data:image/png;base64,iVBORw0KGgo=')
})
