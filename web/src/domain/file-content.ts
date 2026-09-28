import { imageDataSource } from './historical-images'
import type { SessionFileContent } from '@/components/files/files-api'

export { isTextMediaType as textFile } from './attachment-media-type'

export function fileBytes(file: SessionFileContent): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(file.content_base64), character => character.charCodeAt(0))
}

export function fileBlob(file: SessionFileContent): Blob {
  return new Blob([fileBytes(file)], { type: file.media_type })
}

export function fileImage(file: SessionFileContent): string | null {
  return imageDataSource({ name: file.name, media_type: file.media_type, content: `data:${file.media_type};base64,${file.content_base64}` })
}
