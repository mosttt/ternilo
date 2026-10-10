import json
import socket
from http.client import RemoteDisconnected
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from ternilo import ServerClient, ServerError

try:
    from websockets.sync.server import serve
except ImportError:
    serve = None


class ServerClientTest(unittest.TestCase):
    @unittest.skipIf(serve is None, 'install ternilo-sdk[remote] for Live tests')
    def test_malformed_http_upgrade_is_not_retried(self):
        from websockets.exceptions import InvalidMessage

        with socket.create_server(('127.0.0.1', 0)) as listener:
            port = listener.getsockname()[1]

            def malformed_server():
                connection, _ = listener.accept()
                with connection:
                    connection.recv(16 * 1024)
                    connection.sendall(b'Not an HTTP response\r\n\r\n')

            thread = threading.Thread(target=malformed_server, daemon=True); thread.start()
            try:
                with ServerClient(f'http://127.0.0.1:{port}', access_token='token', tenant_id='team') as client:
                    with self.assertRaises(InvalidMessage):
                        next(client.watch('shared', timeout=0.5, reconnect_timeout=0.5))
            finally:
                thread.join(2)
                self.assertFalse(thread.is_alive())

    @unittest.skipIf(serve is None, 'install ternilo-sdk[remote] for Live tests')
    def test_reconnects_when_restart_closes_the_http_upgrade(self):
        ready = threading.Event()
        servers, receipts, failures = [], [], []

        def handler(connection):
            try:
                receipts.append(json.loads(connection.recv()))
                connection.send(json.dumps({'type': 'ready', 'protocol_version': 1}))
                receipts.append(json.loads(connection.recv()))
                connection.send(json.dumps({'type': 'event_batch', 'session_id': 'shared', 'subscription_id': 1,
                                           'reset': False, 'complete': True, 'next_seq': 9,
                                           'events': [{'seq': 8, 'run_id': 'run', 'type': 'turn_started'}]}))
            except Exception as error:
                failures.append(str(error))

        with socket.create_server(('127.0.0.1', 0)) as listener:
            port = listener.getsockname()[1]

            def restarting_server():
                first, _ = listener.accept()
                with first:
                    first.recv(16 * 1024)
                with serve(handler, sock=listener) as server:
                    servers.append(server)
                    ready.set()
                    server.serve_forever()

            thread = threading.Thread(target=restarting_server, daemon=True); thread.start()
            try:
                with ServerClient(f'http://127.0.0.1:{port}', access_token='private-token', tenant_id='team') as client:
                    stream = client.watch('shared', after_seq=7, timeout=3, reconnect_timeout=2)
                    try:
                        self.assertEqual([event['seq'] for event in next(stream)['events']], [8])
                        self.assertEqual(receipts[-1]['after_seq'], 7)
                        self.assertEqual(failures, [])
                    finally:
                        stream.close()
            finally:
                ready.wait(2)
                for server in servers:
                    server.shutdown()
                thread.join(2)
                self.assertFalse(thread.is_alive())

    @unittest.skipIf(serve is None, 'install ternilo-sdk[remote] for Live tests')
    def test_timeout_closes_a_silent_handshake(self):
        from websockets.exceptions import ConnectionClosed
        flap = threading.Event()

        def handler(socket):
            try:
                socket.recv()
                if flap.is_set():
                    socket.send(json.dumps({'type': 'ready', 'protocol_version': 1}))
                    return
                socket.recv()
            except ConnectionClosed:
                pass

        with serve(handler, '127.0.0.1', 0) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
            try:
                with ServerClient(f'http://127.0.0.1:{server.socket.getsockname()[1]}', access_token='token', tenant_id='team') as client:
                    with self.assertRaises(TimeoutError):
                        next(client.watch('session', timeout=0.1))
                    flap.set()
                    started = time.monotonic()
                    with self.assertRaises(TimeoutError):
                        next(client.watch('session', timeout=2, reconnect_timeout=0.2))
                    self.assertLess(time.monotonic() - started, 1)
            finally:
                server.shutdown(); thread.join(2)

    @unittest.skipIf(serve is None, 'install ternilo-sdk[remote] for Live tests')
    def test_reconnect_replay_reset_and_revocation(self):
        receipts, failures, connections = [], [], []

        def handler(socket):
            try:
                connections.append(True)
                index = len(connections)
                receipts.append(json.loads(socket.recv()))
                socket.send(json.dumps({'type': 'ready', 'protocol_version': 1}))
                receipts.append(json.loads(socket.recv()))

                def batch(seqs, reset=False):
                    return json.dumps({'type': 'event_batch', 'session_id': 'shared', 'subscription_id': 1,
                                       'reset': reset, 'complete': True, 'next_seq': max(seqs) + 1,
                                       'events': [{'seq': seq, 'run_id': 'run', 'type': 'turn_started'} for seq in seqs]})
                if index == 1:
                    socket.send(batch([1]))
                else:
                    socket.send(batch([1, 2]))
                    socket.send(batch([0], True))
                    socket.send(json.dumps({'type': 'error', 'subscription_id': 1, 'code': 'policy_denied', 'message': 'share revoked'}))
            except Exception as error:
                failures.append(str(error))

        with serve(handler, '127.0.0.1', 0) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
            try:
                with ServerClient(f'http://127.0.0.1:{server.socket.getsockname()[1]}', access_token='private-token', tenant_id='team') as client:
                    stream = client.watch('shared', after_seq=0, timeout=5)
                    self.assertEqual([event['seq'] for event in next(stream)['events']], [1])
                    self.assertEqual([event['seq'] for event in next(stream)['events']], [2])
                    reset = next(stream)
                    self.assertTrue(reset['reset'])
                    self.assertEqual([event['seq'] for event in reset['events']], [0])
                    with self.assertRaises(ServerError) as error:
                        next(stream)
                    self.assertEqual(error.exception.code, 'policy_denied')
                    self.assertEqual(len(connections), 2)
                    self.assertEqual([frame['after_seq'] for frame in receipts if frame['type'] == 'subscribe'], [0, 1])
                    self.assertEqual([frame for frame in receipts if frame['type'] == 'hello'], [
                        {'type': 'hello', 'protocol_version': 1, 'bearer_token': 'private-token', 'tenant_id': 'team'}] * 2)
                    self.assertEqual(failures, [])
            finally:
                server.shutdown(); thread.join(2)

    def test_mutations_are_not_retried_and_redirects_do_not_leak_credentials(self):
        calls = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                calls.append(('POST', self.path, self.headers.get('Authorization'), self.headers.get('X-Ternilo-Tenant')))
                self.rfile.read(int(self.headers['Content-Length']))
                self.close_connection = True

            def do_GET(self):
                calls.append(('GET', self.path))
                self.send_response(302)
                self.send_header('Location', '/credential-leak')
                self.end_headers()

        with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
            try:
                with ServerClient(f'http://127.0.0.1:{server.server_port}', access_token='private-token', tenant_id='team') as client:
                    with self.assertRaises(RemoteDisconnected):
                        client.request('/mutate', method='POST', body={'input': 'one task'})
                    with self.assertRaises(ServerError) as error:
                        client.request('/redirect')
                    self.assertEqual(error.exception.status, 302)
                    self.assertEqual(calls, [('POST', '/api/v1/mutate', 'Bearer private-token', 'team'), ('GET', '/api/v1/redirect')])
            finally:
                server.shutdown(); thread.join(2)


if __name__ == '__main__':
    unittest.main()
