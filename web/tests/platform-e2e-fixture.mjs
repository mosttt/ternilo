import { createHash, createPrivateKey, createPublicKey, randomBytes, sign } from 'node:crypto'
import { execFile } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { promisify } from 'node:util'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'

export const execute = promisify(execFile)
export const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
export const repository = path.resolve(webRoot, '..')

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return server.address().port
}

export async function closeServer(server) {
  if (!server.listening) return
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
}

export async function freePort() {
  const server = createServer()
  const port = await listen(server)
  await closeServer(server)
  return port
}

export async function selectSpace(page, tenantId) {
  const trigger = page.getByRole('combobox', { name: '切换空间', exact: true })
  if (await trigger.getAttribute('data-space-id') === tenantId) return
  await trigger.click()
  await page.locator(`[data-space-menu] [role="option"][data-space-id="${tenantId}"]`).click()
  await page.locator(`[data-space-switcher] [role="combobox"][data-space-id="${tenantId}"][aria-busy="false"]:visible`).waitFor()
}

export function base64Url(value) {
  return Buffer.from(value).toString('base64url')
}

export async function body(request) {
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  return Buffer.concat(chunks).toString('utf8')
}

export function json(response, status, value) {
  const encoded = JSON.stringify(value)
  response.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(encoded) })
  response.end(encoded)
}

export async function startOidcServer({
  audience,
  subject,
  email,
  name,
  identities,
  initialIdentity,
  clientRefreshToken = 'browser-e2e-refresh',
  validateClient,
  opaqueAccessTokens = false,
  idTokenClaims = claims => claims,
  transformIdToken = token => token,
  userInfoClaims = claims => claims,
  omitIdTokenOnRefresh = false,
} = {}) {
  const privatePem = await readFile(path.join(repository, 'crates/ternilo-control/tests/fixtures/oidc-private.pem'), 'utf8')
  const privateKey = createPrivateKey(privatePem)
  const jwk = createPublicKey(privateKey).export({ format: 'jwk' })
  const codes = new Map()
  const identityEntries = new Map(Object.entries(identities ?? {
    default: { subject, email, name },
  }))
  let activeIdentity = initialIdentity ?? identityEntries.keys().next().value
  if (!identityEntries.has(activeIdentity)) throw new Error(`unknown initial OIDC identity: ${activeIdentity}`)
  const refreshTokens = new Map()
  const accessTokens = new Map()
  let issuer
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, issuer)
    if (url.pathname === '/.well-known/openid-configuration') {
      return json(response, 200, {
        issuer,
        jwks_uri: `${issuer}/jwks`,
        authorization_endpoint: `${issuer}/authorize`,
        token_endpoint: `${issuer}/token`,
        userinfo_endpoint: `${issuer}/userinfo`,
        id_token_signing_alg_values_supported: ['RS256'],
      })
    }
    if (url.pathname === '/jwks') {
      return json(response, 200, { keys: [{ ...jwk, kid: 'browser-e2e', alg: 'RS256', use: 'sig' }] })
    }
    if (url.pathname === '/authorize') {
      const challenge = url.searchParams.get('code_challenge')
      if (url.searchParams.get('response_type') !== 'code' || url.searchParams.get('code_challenge_method') !== 'S256' || !challenge) {
        return json(response, 400, { error: 'invalid_request' })
      }
      const code = base64Url(randomBytes(24))
      codes.set(code, { challenge, identity: activeIdentity, nonce: url.searchParams.get('nonce'), clientId: url.searchParams.get('client_id') })
      const redirect = new URL(url.searchParams.get('redirect_uri'))
      redirect.searchParams.set('code', code)
      redirect.searchParams.set('state', url.searchParams.get('state'))
      response.writeHead(302, { location: redirect.toString() })
      return response.end()
    }
    if (url.pathname === '/token' && request.method === 'POST') {
      const form = new URLSearchParams(await body(request))
      if (validateClient && !validateClient(form, request.headers.authorization)) {
        return json(response, 401, { error: 'invalid_client' })
      }
      let authorization
      if (form.get('grant_type') === 'authorization_code') {
        authorization = codes.get(form.get('code'))
        const actual = base64Url(createHash('sha256').update(form.get('code_verifier') || '').digest())
        if (!authorization || authorization.challenge !== actual) return json(response, 400, { error: 'invalid_grant' })
        codes.delete(form.get('code'))
      } else if (form.get('grant_type') === 'refresh_token') {
        authorization = refreshTokens.get(form.get('refresh_token'))
        if (!authorization) return json(response, 400, { error: 'invalid_grant' })
        refreshTokens.delete(form.get('refresh_token'))
      } else {
        return json(response, 400, { error: 'invalid_grant' })
      }
      const identity = identityEntries.get(authorization.identity)
      if (!identity) return json(response, 400, { error: 'invalid_grant' })
      const refreshToken = `${clientRefreshToken}-${base64Url(randomBytes(12))}`
      refreshTokens.set(refreshToken, authorization)
      const accessToken = opaqueAccessTokens ? `opaque-access-${base64Url(randomBytes(24))}` : jwt(privateKey, issuer, { audience, ...identity })
      accessTokens.set(accessToken, identity)
      const claims = idTokenClaims({ nonce: authorization.nonce, aud: authorization.clientId, at_hash: base64Url(createHash('sha256').update(accessToken).digest().subarray(0, 16)) })
      const idToken = claims && !(omitIdTokenOnRefresh && form.get('grant_type') === 'refresh_token')
        ? transformIdToken(jwt(privateKey, issuer, { audience: authorization.clientId, ...identity, claims })) : undefined
      return json(response, 200, {
        access_token: accessToken,
        id_token: idToken,
        token_type: 'Bearer',
        expires_in: 3600,
        refresh_token: refreshToken,
        scope: 'openid profile email',
      })
    }
    if (url.pathname === '/userinfo') {
      const identity = accessTokens.get(request.headers.authorization?.replace(/^Bearer /, ''))
      if (!identity) return json(response, 401, { error: 'invalid_token' })
      return json(response, 200, userInfoClaims({ sub: identity.subject, email: identity.email, name: identity.name }))
    }
    response.writeHead(404)
    response.end()
  })
  const port = await listen(server)
  issuer = `http://127.0.0.1:${port}`
  return {
    issuer,
    accessToken(identityKey = activeIdentity) {
      const identity = identityEntries.get(identityKey)
      if (!identity) throw new Error(`unknown OIDC identity: ${identityKey}`)
      return jwt(privateKey, issuer, { audience, ...identity })
    },
    selectIdentity(identity) {
      if (!identityEntries.has(identity)) throw new Error(`unknown OIDC identity: ${identity}`)
      activeIdentity = identity
    },
    close: () => closeServer(server),
  }
}

function jwt(privateKey, issuer, { audience, subject, email, name, claims = {} }) {
  const header = base64Url(JSON.stringify({ alg: 'RS256', typ: 'JWT', kid: 'browser-e2e' }))
  const now = Math.floor(Date.now() / 1000)
  const payload = base64Url(JSON.stringify({
    iss: issuer,
    sub: subject,
    aud: audience,
    exp: now + 3600,
    iat: now,
    nbf: now - 1,
    email,
    name,
    ...claims,
  }))
  const signature = base64Url(sign('RSA-SHA256', Buffer.from(`${header}.${payload}`), privateKey))
  return `${header}.${payload}.${signature}`
}

export async function startPostgres({ prefix, database }) {
  const container = `${prefix}-${process.pid}-${Date.now()}`
  await execute('docker', [
    'run', '-d', '--rm', '--name', container,
    '-e', 'POSTGRES_PASSWORD=ternilo-test-password',
    '-e', `POSTGRES_DB=${database}`,
    '-p', '127.0.0.1::5432', 'postgres:17',
  ])
  for (let attempt = 0; attempt < 40; attempt++) {
    try {
      await execute('docker', ['exec', container, 'pg_isready', '-h', '127.0.0.1', '-U', 'postgres', '-d', database])
      const { stdout } = await execute('docker', ['port', container, '5432/tcp'])
      const port = stdout.trim().split(':').at(-1)
      return {
        url: `postgres://postgres:ternilo-test-password@127.0.0.1:${port}/${database}`,
        query: async sql => {
          const { stdout } = await execute('docker', [
            'exec', container,
            'psql', '--no-psqlrc', '--set', 'ON_ERROR_STOP=1', '--tuples-only', '--no-align',
            '-U', 'postgres', '-d', database, '-c', sql,
          ])
          return stdout.trim()
        },
        dump: async () => {
          const { stdout } = await execute('docker', [
            'exec', container,
            'pg_dump', '--data-only', '--no-owner', '--no-privileges',
            '-U', 'postgres', '-d', database,
          ])
          return stdout
        },
        stop: () => execute('docker', ['stop', container]).catch(() => {}),
      }
    } catch {
      await new Promise(resolve => setTimeout(resolve, 500))
    }
  }
  await execute('docker', ['stop', container]).catch(() => {})
  throw new Error('PostgreSQL test container did not become ready')
}

export function startProcess(binary, args, environment = {}) {
  const child = spawn(binary, args, {
    cwd: repository,
    env: { ...process.env, ...environment },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  child.stdout.setEncoding('utf8')
  child.stderr.setEncoding('utf8')
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  return { child, diagnostics: () => output }
}

export async function stopProcess(process) {
  if (!process || process.child.exitCode !== null || process.child.signalCode !== null) return
  await new Promise(resolve => {
    const timeout = setTimeout(() => process.child.kill('SIGKILL'), 5000)
    process.child.once('exit', () => { clearTimeout(timeout); resolve() })
    process.child.kill('SIGINT')
  })
}

export async function waitForHttp(url, process) {
  for (let attempt = 0; attempt < 80; attempt++) {
    if (process.child.exitCode !== null) throw new Error(`process exited early: ${process.diagnostics()}`)
    try {
      const response = await fetch(url)
      if (response.ok) return
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 250))
  }
  throw new Error(`HTTP service did not become ready: ${process.diagnostics()}`)
}


export async function serverRequest(origin, resource, { token = '', tenantId, body, method = body === undefined ? 'GET' : 'POST' } = {}) {
  const response = await fetch(`${origin}/api/v1${resource}`, {
    method,
    headers: {
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(tenantId ? { 'x-ternilo-tenant': tenantId } : {}),
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  })
  if (!response.ok) throw new Error(`${method} ${resource}: ${response.status} ${await response.text()}`)
  return response.status === 204 ? null : response.json()
}

export async function registerOidcUser(origin, token, username) {
  const result = await serverRequest(origin, '/auth/oidc/register', { token, body: { username, email: `${username}-oidc@example.test` } })
  if (result.status !== 'active') throw new Error('OIDC fixture registration must be active')
  return serverRequest(origin, '/auth/session', { token })
}

export async function selectProject(page, dialog, name) {
  await dialog.getByRole('combobox', { name: '项目', exact: true }).click()
  await page.getByRole('option', { name, exact: true }).click()
}

export async function initializeServer({
  directory,
  origin,
  binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'),
  environment = {},
  databaseUrl,
  migrationDatabaseUrl,
  oidc,
  ownerOidcToken,
  owner: ownerInput = {},
  mode = 'multi_user',
  workerPolicy,
  managedExecutionEnabled = false,
}) {
  const owner = { username: 'browser-owner', password: 'browser-owner-password', ...ownerInput }
  owner.email ??= `${owner.username}@example.test`
  const configPath = path.join(directory, 'config.json')
  await mkdir(directory, { recursive: true })
  const runtimeEnvironment = {
    ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])),
    ...environment,
  }
  const args = ['init', '--non-interactive', '--config-dir', path.dirname(configPath), '--listen', new URL(origin).host, '--public-url', origin]
  if (databaseUrl) args.push('--database-url', databaseUrl)
  if (migrationDatabaseUrl) args.push('--migration-database-url', migrationDatabaseUrl)
  await execute(binary, args, { cwd: repository, env: {
    ...process.env,
    ...runtimeEnvironment,
    TERNILO_SERVER_OWNER_USERNAME: owner.username,
    TERNILO_SERVER_OWNER_EMAIL: owner.email,
    TERNILO_SERVER_OWNER_PASSWORD: owner.password,
  } })
  const config = JSON.parse(await readFile(configPath, 'utf8'))
  if (oidc) config.oidc = { scopes: 'openid profile email', allow_insecure: true, ...oidc }
  config.managed_execution_enabled = managedExecutionEnabled
  if (workerPolicy) config.worker_policy = workerPolicy
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
  const application = startProcess(binary, ['serve', '--config-dir', path.dirname(configPath)], runtimeEnvironment)
  try {
    await waitForHttp(`${origin}/readyz`, application)
    let session = await serverRequest(origin, '/auth/login', { body: { username: owner.username, password: owner.password } })
    if (ownerOidcToken) await serverRequest(origin, '/auth/oidc-link', { token: session.access_token, body: { access_token: ownerOidcToken } })
    if (session.instance.mode !== mode) {
      const instance = await serverRequest(origin, '/admin/instance', { token: session.access_token, method: 'PATCH', body: { mode, revision: session.instance.revision } })
      session = { ...session, instance }
    }
    return { ...application, origin, configPath, owner: { ...owner, session } }
  } catch (error) {
    await stopProcess(application)
    throw error
  }
}
