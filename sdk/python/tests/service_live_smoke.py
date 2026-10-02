import json
import os
from ternilo import ServerClient
config = json.loads(os.environ['TERNILO_SDK_TEST_CONFIG'])
with ServerClient(config['origin'], access_token=config['token'], tenant_id=config['tenant']) as client:
    assert config['session'] in json.dumps(client.state())
    written = client.run(config['session'], '/write service-python.txt service Python proof', timeout=30)
    assert written.status == 'idle'
    read = client.run(config['session'], '/read service-python.txt', timeout=30)
    assert json.loads(read.answer)['content'] == 'service Python proof'
print('Python service Live run verified')
