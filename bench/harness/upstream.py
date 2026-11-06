#!/usr/bin/env python3
"""Tiny HTTP/1.1 upstream that serves pages in random-size writes, for the accuracy check.

  upstream.py <port> <root>

GET /<mode>/<seed>/<path>
  mode: cl      -> Content-Length, body in random writes
        chunked -> Transfer-Encoding: chunked, one HTTP chunk per random write
  seed: integer, makes the write schedule reproducible

Writes are separated by short sleeps (TCP_NODELAY) so the proxy tends to see them as separate
reads, and the sizes cluster around the 1 KiB head window. Always closes the connection.
"""
import asyncio
import os
import random
import socket
import sys

TYPES = {".html": "text/html; charset=utf-8", ".txt": "text/plain; charset=utf-8"}


def schedule(length, seed):
    rng = random.Random(seed)
    sizes, offset = [], 0
    big = max(1, length // 16)
    while offset < length:
        if offset < 4096:
            size = rng.choice([1, 2, 3, 5, 17, 100, 511, 1000, 1023, 1024, 1025, rng.randint(1, 1500)])
        else:
            size = rng.randint(1, big)
        size = min(size, length - offset)
        sizes.append(size)
        offset += size
    return sizes


async def handle(reader, writer, root):
    try:
        writer.get_extra_info("socket").setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        request = await reader.readuntil(b"\r\n\r\n")
        target = request.split(b" ", 2)[1].decode()
        _, mode, seed, path = target.split("/", 3)
        full = os.path.realpath(os.path.join(root, path))
        if not full.startswith(os.path.realpath(root) + os.sep) or not os.path.isfile(full):
            writer.write(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
        with open(full, "rb") as f:
            body = f.read()
        ctype = TYPES.get(os.path.splitext(full)[1], "application/octet-stream")
        head = f"HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nConnection: close\r\n"
        head += f"Content-Length: {len(body)}\r\n" if mode == "cl" else "Transfer-Encoding: chunked\r\n"
        writer.write(head.encode() + b"\r\n")
        await writer.drain()
        rng = random.Random(int(seed) ^ 0x5EED)
        offset = 0
        for size in schedule(len(body), int(seed)):
            piece = body[offset : offset + size]
            offset += size
            writer.write(piece if mode == "cl" else b"%x\r\n%s\r\n" % (size, piece))
            await writer.drain()
            if rng.random() < 0.5:
                await asyncio.sleep(0.0005)
        if mode != "cl":
            writer.write(b"0\r\n\r\n")
        await writer.drain()
    except (asyncio.IncompleteReadError, ConnectionError, ValueError):
        pass
    finally:
        writer.close()


async def main():
    port, root = int(sys.argv[1]), sys.argv[2]
    server = await asyncio.start_server(lambda r, w: handle(r, w, root), "127.0.0.1", port, backlog=1024)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
