"""CDP WebSocket relay recording each command's browser-side interval.

Usage: cdp_relay.py UPSTREAM_WS_URL LOG_PATH; prints the relay's ws URL on stdout.
Each JSON line in LOG_PATH is {method, start_ns, end_ns} on time.monotonic_ns.
"""

import asyncio
import json
import sys
import time

import websockets


async def main(upstream_url, log_path):
    log = open(log_path, 'a', buffering=1)

    async def handle(client):
        pending = {}
        async with websockets.connect(upstream_url, max_size=None, ping_interval=None) as upstream:
            async def down():
                async for message in client:
                    try:
                        data = json.loads(message)
                        if 'id' in data:
                            pending[(data['id'], data.get('sessionId'))] = (data.get('method'), time.monotonic_ns())
                    except ValueError:
                        pass
                    await upstream.send(message)

            async def up():
                async for message in upstream:
                    end = time.monotonic_ns()
                    try:
                        data = json.loads(message)
                        if 'id' in data:
                            sent = pending.pop((data['id'], data.get('sessionId')), None)
                            if sent:
                                log.write(json.dumps({'method': sent[0], 'start_ns': sent[1], 'end_ns': end}) + '\n')
                    except ValueError:
                        pass
                    await client.send(message)

            tasks = [asyncio.create_task(down()), asyncio.create_task(up())]
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            for task in tasks:
                task.cancel()

    async with websockets.serve(handle, '127.0.0.1', 0, max_size=None, ping_interval=None) as server:
        port = server.sockets[0].getsockname()[1]
        print(f'ws://127.0.0.1:{port}/devtools/browser/relay', flush=True)
        await asyncio.Future()


if __name__ == '__main__':
    asyncio.run(main(sys.argv[1], sys.argv[2]))
