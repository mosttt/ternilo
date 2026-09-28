export function isTextMediaType(mediaType: string): boolean {
  const mime = mediaType.split(';', 1)[0].trim().toLowerCase()
  return !mime.startsWith('image/') && (mime.startsWith('text/') || /^application\/(json|xml|javascript|yaml|x-yaml|toml)$/.test(mime)
    || mime.endsWith('+json') || mime.endsWith('+xml'))
}
