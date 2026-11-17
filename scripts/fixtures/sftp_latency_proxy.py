"""Disposable localhost-only latency/connection-cap proxy for SFTP fixtures."""
import argparse
import asyncio
import socket
from pathlib import Path
active={}
serial=0
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--upstream-port', type=int, required=True)
parser.add_argument('--listen-port', type=int, default=1427)
parser.add_argument('--delay-ms', type=float, default=10)
parser.add_argument('--limit-file', type=Path)
args=parser.parse_args()
limit_file=args.limit_file
def limit():
    try: return max(1, int(limit_file.read_text())) if limit_file else 1000
    except (FileNotFoundError, ValueError): return 1000
async def serve(reader, writer):
    global serial
    serial+=1
    identity=serial
    if len(active)>=limit():
        writer.write(b'SSH-2.0-Fixture_connection_limit\r\n')
        await writer.drain()
        writer.close()
        return
    active[identity]=writer
    other_writer=None
    try:
        other_reader, other_writer = await asyncio.open_connection('127.0.0.1', args.upstream_port)
        for item in (writer, other_writer):
            item.get_extra_info('socket').setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        async def copy(source, target):
            try:
                while chunk := await source.read(65536):
                    if identity not in sorted(active)[:limit()]: break
                    await asyncio.sleep(max(0, args.delay_ms) / 1000)
                    target.write(chunk)
                    await target.drain()
            finally: target.close()
        await asyncio.gather(copy(reader, other_writer), copy(other_reader, writer), return_exceptions=True)
    finally:
        writer.close()
        if other_writer: other_writer.close()
        active.pop(identity, None)
async def main():
    server = await asyncio.start_server(serve, '127.0.0.1', args.listen_port)
    async with server: await server.serve_forever()
try:
    asyncio.run(main())
except KeyboardInterrupt:
    pass
