#!/usr/bin/env python3
"""Isolated Linux WAN-test backends; bind loopback only, never existing services."""
import asyncio
import json
from pathlib import Path
import ssl
import sys


async def echo(reader, writer):
    try:
        while data := await reader.read(16384):
            writer.write(data)
            await writer.drain()
    finally:
        writer.close()
        await writer.wait_closed()


async def http(reader, writer):
    try:
        while True:
            try:
                head = await reader.readuntil(b"\r\n\r\n")
            except (asyncio.IncompleteReadError, ConnectionError):
                break
            lines = head.decode().split("\r\n")
            headers = dict(line.split(": ", 1) for line in lines[1:] if ": " in line)
            length = int(headers.get("Content-Length", "0"))
            body = await reader.readexactly(length) if length else b""
            data = json.dumps({"request": lines[0], "headers": headers, "body": body.decode()}).encode()
            unhealthy = lines[0].split()[1] == "/health" and Path("/var/lib/rustfrp-test/unhealthy").exists()
            status = b"503 Unhealthy" if unhealthy else b"200 OK"
            writer.write(b"HTTP/1.1 " + status + b"\r\nContent-Type: application/json\r\nContent-Length: "
                         + str(len(data)).encode() + b"\r\n\r\n" + data)
            await writer.drain()
            if headers.get("Connection", "").lower() == "close":
                break
    finally:
        writer.close()
        await writer.wait_closed()


async def proxy_echo(reader, writer):
    first = await reader.readexactly(6)
    if first == b"PROXY ":
        head = first + await reader.readuntil(b"\r\n")
        Path("/var/lib/rustfrp-test/proxy-v1.json").write_text(json.dumps({"head": head.decode()}))
    else:
        head = first + await reader.readexactly(10)
        head += await reader.readexactly(int.from_bytes(head[14:16], "big"))
        Path("/var/lib/rustfrp-test/proxy-v2.json").write_text(json.dumps({"head": head.hex()}))
    await echo(reader, writer)


class Udp(asyncio.DatagramProtocol):
    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, address):
        self.transport.sendto(data, address)


async def main():
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(sys.argv[1], sys.argv[2])
    unix = Path("/var/lib/rustfrp-test/backend.sock")
    unix.unlink(missing_ok=True)
    servers = [await asyncio.start_server(echo, "127.0.0.1", 28081),
               await asyncio.start_server(http, "127.0.0.1", 28083),
               await asyncio.start_server(http, "127.0.0.1", 28084, ssl=context),
               await asyncio.start_server(proxy_echo, "127.0.0.1", 28085),
               await asyncio.start_unix_server(echo, str(unix))]
    await asyncio.get_running_loop().create_datagram_endpoint(Udp, local_addr=("127.0.0.1", 28082))
    print("WAN backends ready: loopback TCP/UDP/HTTP/HTTPS", flush=True)
    await asyncio.gather(*(server.serve_forever() for server in servers))


if __name__ == "__main__":
    asyncio.run(main())
