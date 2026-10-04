import assert from 'node:assert/strict'
import test from 'node:test'
import { randomBytes } from 'node:crypto'
import { createServer } from 'node:https'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { execute, freePort, repository, startProcess, stopProcess } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

test('A fresh Node handles a real WSS handshake without a CryptoProvider panic and rejects an untrusted certificate', { timeout: 60_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-tls-transport-'))
  let node, gateway
  const sockets = new Set()
  let tlsAttempts = 0, synchronizations = 0
  try {
    const key = path.join(directory, 'key.pem'), cert = path.join(directory, 'cert.pem')
    const root = path.join(directory, 'root.pem'), rootKey = path.join(directory, 'root-key.pem'), csr = path.join(directory, 'request.pem')
    await execute('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=Ternilo TLS fixture CA', '-keyout', rootKey, '-out', root])
    await execute('openssl', ['req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=localhost', '-addext', 'subjectAltName=IP:127.0.0.1', '-addext', 'basicConstraints=critical,CA:FALSE', '-addext', 'extendedKeyUsage=serverAuth', '-keyout', key, '-out', csr])
    await execute('openssl', ['x509', '-req', '-in', csr, '-CA', root, '-CAkey', rootKey, '-CAcreateserial', '-days', '1', '-copy_extensions', 'copy', '-out', cert])
    gateway = createServer({ key: await readFile(key), cert: await readFile(cert) }, (request, response) => {
      assert.equal(request.url, '/api/v1/executors/cleanup/sync')
      synchronizations += 1
      response.setHeader('Content-Type', 'application/json')
      response.end(JSON.stringify({ protocol_version: 1, server_id: 'ter_srv_tls_fixture', tenant_id: 'tenant-tls-fixture', executor_id: 'ter_pc_tls_fixture', credential_id: 'tls-fixture', authorizations: [], connection_allowed: true, requests: [] }))
    })
    gateway.on('upgrade', () => assert.fail('The WebSocket client must reject an untrusted certificate before HTTP upgrade'))
    gateway.on('connection', socket => { sockets.add(socket); socket.once('close', () => sockets.delete(socket)) })
    gateway.on('tlsClientError', () => { tlsAttempts += 1 })
    await new Promise((resolve, reject) => { gateway.once('error', reject); gateway.listen(0, '127.0.0.1', resolve) })
    const clean = Object.fromEntries(Object.keys(process.env).filter(name => name.startsWith('TERNILO_')).map(name => [name, undefined]))
    // HTTPS cleanup uses native trust; WSS uses bundled WebPKI roots. Trust the
    // fixture only for cleanup so the process reaches a real WSS handshake.
    node = startProcess(binary, ['serve', '--data-dir', path.join(directory, 'state'), '--listen', `127.0.0.1:${await freePort()}`, '--gateway-url', `wss://127.0.0.1:${gateway.address().port}/api/v1/executors/connect`, '--node-id', 'ter_pc_tls_fixture', '--token', `ter_n_${randomBytes(32).toString('base64url')}`], { ...clean, SSL_CERT_FILE: root, SSL_CERT_DIR: directory })
    const deadline = Date.now() + 30_000
    while (!/gateway connection failed:.*(?:UnknownIssuer|invalid peer certificate|certificate)/i.test(node.diagnostics())) {
      assert.doesNotMatch(node.diagnostics(), /panicked|Could not automatically determine|CryptoProvider/)
      assert.equal(node.child.exitCode, null, node.diagnostics())
      assert.ok(Date.now() < deadline, node.diagnostics())
      await new Promise(resolve => setTimeout(resolve, 50))
    }
    assert.ok(tlsAttempts > 0, 'The actual TLS server observed a client handshake')
    assert.ok(synchronizations > 0, 'The Node completed HTTPS cleanup before connecting over WSS')
    assert.doesNotMatch(node.diagnostics(), /panicked|Could not automatically determine|CryptoProvider/)
    assert.equal(node.child.exitCode, null, 'The service stays available after a rejected peer certificate')
  } finally {
    await stopProcess(node)
    for (const socket of sockets) socket.destroy()
    if (gateway?.listening) await new Promise(resolve => gateway.close(resolve))
    await rm(directory, { recursive: true, force: true })
  }
})
