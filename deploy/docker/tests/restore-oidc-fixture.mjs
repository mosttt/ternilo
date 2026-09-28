import http from 'node:http'

const port = Number.parseInt(process.env.TERNILO_ACCEPTANCE_OIDC_PORT ?? '', 10)
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('TERNILO_ACCEPTANCE_OIDC_PORT must be a valid port')
}
const issuer = `http://127.0.0.1:${port}`
const discovery = {
  issuer,
  jwks_uri: `${issuer}/jwks`,
  authorization_endpoint: `${issuer}/authorize`,
  token_endpoint: `${issuer}/token`,
}
const server = http.createServer((request, response) => {
  const body = request.url === '/jwks'
    ? { keys: [] }
    : request.url === '/.well-known/openid-configuration'
      ? discovery
      : null
  if (body === null) {
    response.writeHead(404).end()
    return
  }
  response.writeHead(200, { 'content-type': 'application/json' })
  response.end(JSON.stringify(body))
})
server.listen(port, '127.0.0.1', () => {
  process.stdout.write(`restore OIDC fixture ready on ${issuer}\n`)
})
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => server.close(() => process.exit(0)))
}
