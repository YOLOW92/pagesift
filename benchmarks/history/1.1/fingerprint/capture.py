#!/usr/bin/env python3
"""本地 TLS 服务：记下每条连接的原始 ClientHello 和 HTTP/2、HTTP/1.1 的请求特征，写成 JSONL。

    capture.py --cert c.pem --key k.pem --port 18443 --alpn h2 --out out.jsonl

用系统 OpenSSL 做服务端（3.5 起默认支持 X25519MLKEM768），ClientHello 在交给 OpenSSL 之前原样留一份。
"""
import argparse
import json
import socket
import ssl
import struct
import sys
import threading
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from hpack_tables import HUFFMAN, STATIC  # noqa: E402

PREFACE = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
BODY = b"<!doctype html><html><head><title>fingerprint capture</title><link rel=icon href=data:,></head>" \
       b"<body><article><h1>capture</h1><p>" + b"local fingerprint capture page. " * 40 + b"</p></article></body></html>"
DECODE = {(bits, code): sym for sym, (code, bits) in HUFFMAN.items()}


def huffman(data):
    out, code, bits = bytearray(), 0, 0
    for byte in data:
        for i in range(7, -1, -1):
            code, bits = (code << 1) | ((byte >> i) & 1), bits + 1
            sym = DECODE.get((bits, code))
            if sym is not None:
                out.append(sym)
                code, bits = 0, 0
    return out.decode("latin-1")


class Hpack:
    """只解码，够看请求头（RFC 7541）。"""

    def __init__(self):
        self.dynamic = []

    def entry(self, index):
        if index <= len(STATIC):
            return STATIC[index - 1]
        return self.dynamic[index - len(STATIC) - 1]

    @staticmethod
    def integer(block, pos, prefix):
        mask = (1 << prefix) - 1
        value = block[pos] & mask
        pos += 1
        if value == mask:
            shift = 0
            while True:
                byte = block[pos]
                pos += 1
                value += (byte & 0x7F) << shift
                shift += 7
                if not byte & 0x80:
                    break
        return value, pos

    def string(self, block, pos):
        coded = bool(block[pos] & 0x80)
        length, pos = self.integer(block, pos, 7)
        raw = block[pos : pos + length]
        return (huffman(raw) if coded else raw.decode("latin-1")), pos + length

    def decode(self, block):
        fields, pos = [], 0
        while pos < len(block):
            head = block[pos]
            if head & 0x80:
                index, pos = self.integer(block, pos, 7)
                name, value = self.entry(index)
                fields.append({"name": name, "value": value, "rep": "indexed"})
            elif head & 0xE0 == 0x20:
                _, pos = self.integer(block, pos, 5)
            else:
                prefix, rep = (6, "incremental") if head & 0x40 else (4, "never" if head & 0x10 else "without")
                index, pos = self.integer(block, pos, prefix)
                if index:
                    name = self.entry(index)[0]
                else:
                    name, pos = self.string(block, pos)
                value, pos = self.string(block, pos)
                if rep == "incremental":
                    self.dynamic.insert(0, (name, value))
                fields.append({"name": name, "value": value, "rep": rep})
        return fields


def read_client_hello(sock):
    """读到一条完整的握手消息为止，返回读到的全部字节（可能跨记录）。"""
    data = bytearray()
    records = 0
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            return bytes(data), records
        data += chunk
        pos, handshake = 0, bytearray()
        records = 0
        while pos + 5 <= len(data):
            length = struct.unpack(">H", data[pos + 3 : pos + 5])[0]
            if pos + 5 + length > len(data):
                break
            handshake += data[pos + 5 : pos + 5 + length]
            records += 1
            pos += 5 + length
        if len(handshake) >= 4:
            need = int.from_bytes(handshake[1:4], "big") + 4
            if len(handshake) >= need:
                return bytes(data), records


class Tls:
    """内存 BIO 上的服务端：先喂预先读到的字节，再接着读套接字。"""

    def __init__(self, sock, ctx, first):
        self.sock = sock
        self.incoming = ssl.MemoryBIO()
        self.outgoing = ssl.MemoryBIO()
        self.obj = ctx.wrap_bio(self.incoming, self.outgoing, server_side=True)
        self.incoming.write(first)

    def flush(self):
        data = self.outgoing.read()
        if data:
            self.sock.sendall(data)

    def pump(self):
        self.flush()
        chunk = self.sock.recv(65536)
        if not chunk:
            self.incoming.write_eof()
            return False
        self.incoming.write(chunk)
        return True

    def handshake(self):
        while True:
            try:
                self.obj.do_handshake()
                self.flush()
                return
            except ssl.SSLWantReadError:
                if not self.pump():
                    raise

    def read(self):
        while True:
            try:
                return self.obj.read(65536)
            except ssl.SSLWantReadError:
                if not self.pump():
                    return b""
            except ssl.SSLZeroReturnError:
                return b""

    def write(self, data):
        self.obj.write(data)
        self.flush()


def frame(kind, flags, stream, payload=b""):
    return len(payload).to_bytes(3, "big") + bytes([kind, flags]) + struct.pack(">I", stream) + payload


def serve_h2(tls, rec):
    buf = bytearray()
    while len(buf) < len(PREFACE):
        chunk = tls.read()
        if not chunk:
            return
        buf += chunk
    rec["preface_ok"] = bytes(buf[: len(PREFACE)]) == PREFACE
    del buf[: len(PREFACE)]
    tls.write(frame(4, 0, 0, b""))
    hpack = Hpack()
    frames, requests, fragments = rec.setdefault("frames", []), rec.setdefault("requests", []), {}
    answered = 0
    while True:
        while len(buf) >= 9:
            length = int.from_bytes(buf[:3], "big")
            if len(buf) < 9 + length:
                break
            kind, flags = buf[3], buf[4]
            stream = struct.unpack(">I", buf[5:9])[0] & 0x7FFFFFFF
            payload = bytes(buf[9 : 9 + length])
            del buf[: 9 + length]
            item = {"type": kind, "flags": flags, "stream": stream, "length": length}
            if kind == 4 and not flags & 1:
                item["settings"] = [
                    [struct.unpack(">H", payload[i : i + 2])[0], struct.unpack(">I", payload[i + 2 : i + 6])[0]]
                    for i in range(0, length, 6)
                ]
                tls.write(frame(4, 1, 0))
            elif kind == 8:
                item["increment"] = struct.unpack(">I", payload)[0] & 0x7FFFFFFF
            elif kind == 6 and not flags & 1:
                tls.write(frame(6, 1, 0, payload))
            elif kind == 2:
                item["priority_frame"] = payload.hex()
            elif kind in (1, 9):
                pos = 0
                if kind == 1:
                    if flags & 8:
                        pad = payload[0]
                        pos, payload = 1, payload[: len(payload) - pad]
                    if flags & 0x20:
                        dep = struct.unpack(">I", payload[pos : pos + 4])[0]
                        item["priority"] = {
                            "exclusive": bool(dep >> 31),
                            "depends_on": dep & 0x7FFFFFFF,
                            "weight": payload[pos + 4] + 1,
                        }
                        pos += 5
                fragments[stream] = fragments.get(stream, b"") + payload[pos:]
                if flags & 4:
                    fields = hpack.decode(fragments.pop(stream))
                    requests.append({"stream": stream, "fields": fields})
                    item["fields"] = len(fields)
                    head = bytes([0x88]) + b"\x0f\x10\x18text/html; charset=utf-8"
                    tls.write(frame(1, 4, stream, head))
                    tls.write(frame(0, 1, stream, BODY))
                    answered += 1
            frames.append(item)
        chunk = tls.read()
        if not chunk:
            return
        buf += chunk


def serve_h1(tls, rec):
    buf = bytearray()
    heads = rec.setdefault("h1_requests", [])
    while True:
        while b"\r\n\r\n" in buf:
            head, _, rest = bytes(buf).partition(b"\r\n\r\n")
            buf = bytearray(rest)
            heads.append(head.decode("latin-1"))
            tls.write(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: "
                + str(len(BODY)).encode()
                + b"\r\n\r\n"
                + BODY
            )
        chunk = tls.read()
        if not chunk:
            return
        buf += chunk


def handle(sock, ctx, out, lock):
    rec = {}
    try:
        sock.settimeout(8)
        first, records = read_client_hello(sock)
        if not first:  # run.sh 探测端口的空连接
            return
        rec.update(client_hello=first.hex(), hello_records=records)
        tls = Tls(sock, ctx, first)
        tls.handshake()
        rec.update(
            tls_version=tls.obj.version(),
            cipher=tls.obj.cipher()[0],
            alpn=tls.obj.selected_alpn_protocol(),
        )
        if hasattr(tls.obj, "group"):
            rec["group"] = tls.obj.group()
        if rec["alpn"] == "h2":
            serve_h2(tls, rec)
        else:
            serve_h1(tls, rec)
    except (OSError, ssl.SSLError, ValueError, IndexError, struct.error) as error:
        rec["error"] = f"{type(error).__name__}: {error}"
    finally:
        sock.close()
        if rec:
            with lock:
                out.write(json.dumps(rec) + "\n")
                out.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--alpn", default="h2", help="h2 / http/1.1 / 逗号分隔")
    parser.add_argument("--max-tls", default="1.3", choices=["1.2", "1.3"])
    parser.add_argument("--groups", help="服务端允许的密钥交换组，如 X25519:P-256")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(args.cert, args.key)
    ctx.set_alpn_protocols(args.alpn.split(","))
    if args.max_tls == "1.2":
        ctx.maximum_version = ssl.TLSVersion.TLSv1_2
    if args.groups and hasattr(ctx, "set_groups"):
        ctx.set_groups(args.groups)
    listener = socket.create_server(("127.0.0.1", args.port))
    lock = threading.Lock()
    with open(args.out, "a", encoding="utf-8") as out:
        print(f"listening on {args.port}", flush=True)
        while True:
            sock, _ = listener.accept()
            threading.Thread(target=handle, args=(sock, ctx, out, lock), daemon=True).start()


if __name__ == "__main__":
    main()
