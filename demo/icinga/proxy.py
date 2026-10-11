#!/usr/bin/env python3
"""A TCP proxy in front of master-01's API, for the `dead-network` scenario
(demo/scenario.sh): it passes bytes both ways until the file /tmp/dead
exists, then holds every connection open and passes nothing (a dead
network or a hung proxy: TCP stays up, nothing arrives), new connections
included, until the file is gone again. TLS goes through untouched, so a
client sees master-01's own certificate.

Usage: proxy.py LISTEN_PORT TARGET_HOST TARGET_PORT
"""
import asyncio
import os
import sys

DEAD = "/tmp/dead"


def dead():
    return os.path.exists(DEAD)


async def pipe(reader, writer):
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            # Dead: hold it until the network is back (as TCP would
            # retransmit it), reading nothing more meanwhile.
            while dead():
                await asyncio.sleep(0.5)
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        writer.close()


async def handle(client_reader, client_writer, host, port):
    # A new connection is accepted, then hears nothing while dead.
    while dead():
        await asyncio.sleep(0.5)
    try:
        server_reader, server_writer = await asyncio.open_connection(host, port)
    except OSError:
        client_writer.close()
        return
    await asyncio.gather(pipe(client_reader, server_writer), pipe(server_reader, client_writer))


async def main(listen, host, port):
    server = await asyncio.start_server(lambda r, w: handle(r, w, host, port), "0.0.0.0", listen)
    print(f"proxy: :{listen} -> {host}:{port}; dead while {DEAD} exists", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: proxy.py LISTEN_PORT TARGET_HOST TARGET_PORT")
    asyncio.run(main(int(sys.argv[1]), sys.argv[2], int(sys.argv[3])))
