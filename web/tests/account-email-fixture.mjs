import assert from 'node:assert/strict'
import { createServer } from 'node:net'

export async function smtpFixture() {
  const messages = [], sockets = new Set(), failures = []
  const server = createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {})
    socket.write('220 fixture ESMTP\r\n')
    let buffer = '', data = false, lines = []
    socket.on('data', chunk => {
      buffer += chunk.toString()
      let index
      while ((index = buffer.indexOf('\r\n')) >= 0) {
        const line = buffer.slice(0, index); buffer = buffer.slice(index + 2)
        if (data) {
          if (line === '.') { messages.push(lines.join('\r\n')); lines = []; data = false; socket.write('250 accepted\r\n') }
          else lines.push(line.replace(/^\.\./, '.'))
        } else if (line.startsWith('EHLO')) socket.write('250-fixture\r\n250 AUTH PLAIN\r\n')
        else if (line.startsWith('AUTH PLAIN ')) {
          if (Buffer.from(line.slice(11), 'base64').toString() !== '\0fixture-user\0fixture-smtp-password') failures.push('wrong SMTP authentication')
          socket.write('235 authenticated\r\n')
        } else if (line.startsWith('MAIL FROM:') || line.startsWith('RCPT TO:') || line === 'RSET' || line === 'NOOP') socket.write('250 ok\r\n')
        else if (line === 'DATA') { data = true; socket.write('354 send message\r\n') }
        else if (line === 'QUIT') { socket.end('221 bye\r\n') }
        else { failures.push(`unexpected SMTP command ${line.split(' ')[0]}`); socket.write('500 unsupported\r\n') }
      }
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { port: server.address().port, messages, failures, close: async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)) } }
}
export function emailLink(message, action) {
  const [headers, ...parts] = message.split('\r\n\r\n')
  const body = parts.join('\r\n\r\n')
  const decoded = /Content-Transfer-Encoding: base64/i.test(headers) ? Buffer.from(body.replace(/\s/g, ''), 'base64').toString() : body.replace(/=\r\n/g, '').replace(/=([A-F\d]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)))
  const url = decoded.match(new RegExp(`http://127\\.0\\.0\\.1:\\d+/auth/${action}#[A-Za-z0-9_-]+`))?.[0]
  assert.ok(url, 'message contains the requested one-time link')
  return url
}
