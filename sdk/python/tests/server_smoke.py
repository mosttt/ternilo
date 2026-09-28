import json
import os

from ternilo import ServerClient, ServerError

config = json.loads(os.environ['TERNILO_SDK_TEST_CONFIG'])


def phase(value):
    print(json.dumps({'phase': value}), flush=True)


with ServerClient(config['origin'], access_token=config['token'], tenant_id=config['tenant']) as client:
    session, language = config['session'], config['language']
    assert session in json.dumps(client.state())
    written = client.run(session, f'/write sdk-{language}.txt {language} remote proof')
    assert written.status == 'idle'
    read = client.run(session, f'/read sdk-{language}.txt')
    assert read.status == 'idle'
    assert json.loads(read.answer)['content'] == f'{language} remote proof'
    try:
        client.run(session, 'This task needs an unconfigured model')
        raise AssertionError('unconfigured task was accepted')
    except ServerError as error:
        assert error.code == 'invalid_input' and 'No model is configured' in str(error)
    page = client.history(session, limit=2)
    assert len(page['events']) == 2 and page['next_before_seq'] is not None
    older = client.history(session, before_seq=page['next_before_seq'], limit=2)
    assert all(event['seq'] < page['events'][0]['seq'] for event in older['events'])
    cursor, ready, resumed = page['events'][-1]['seq'], False, False
    sequences = []
    stream = client.watch(session, after_seq=cursor, timeout=60)
    try:
        for batch in stream:
            cursor = batch['next_seq'] - 1
            sequences.extend(event['seq'] for event in batch['events'])
            if batch['complete'] and not ready:
                ready = True
                phase('ready')
            if any(event['run_id'] == f'offline-sdk-{language}' and event['type'] == 'turn_finished' for event in batch['events']):
                resumed = True
                break
    finally:
        stream.close()
    assert resumed and len(set(sequences)) == len(sequences)
    phase('resumed')
    revoke = False
    try:
        for batch in client.watch(session, after_seq=cursor, timeout=20):
            if batch['complete'] and not revoke:
                revoke = True
                phase('revoke')
        raise AssertionError('revoked subscription unexpectedly completed')
    except ServerError as error:
        assert error.code in ('policy_denied', 'invalid_input')
    try:
        client.history(session)
        raise AssertionError('revoked history was readable')
    except ServerError as error:
        assert error.status in (400, 403)
    phase('done')
