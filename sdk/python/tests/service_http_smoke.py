import json
import os

from ternilo import ServerClient, ServerError

config = json.loads(os.environ['TERNILO_SDK_TEST_CONFIG'])
with ServerClient(config['origin'], access_token=config['token'], tenant_id=config['tenant']) as client:
    assert client.request('/me')['user_id'] == config['service']
    assert client.history(config['session'], limit=100)['next_before_seq'] is None
    try:
        client.request('/auth/session')
        raise AssertionError('service credential entered browser session API')
    except ServerError as error:
        assert error.status == 403
    workspaces = client.request('/workspaces')['workspaces']
    assert len(workspaces) == 1
    assert all(workspace['owner_user_id'] == config['service'] for workspace in workspaces)
print('Python service HTTP verified')
