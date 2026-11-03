#!/usr/bin/env python3
"""从原始 ClientHello 算 JA4 并拆出逐项特征；从 capture.py 的记录算 HTTP/2 指纹（Akamai 格式）。

    ja4.py out.jsonl
"""
import hashlib
import json
import sys


def is_grease(value):
    return value & 0x0F0F == 0x0A0A and value >> 8 == value & 0xFF


def hello_body(raw):
    """去掉记录头，返回 ClientHello 消息（可能跨记录）和第一条记录的版本。"""
    pos, msg = 0, bytearray()
    version = raw[1:3].hex()
    while pos + 5 <= len(raw):
        length = int.from_bytes(raw[pos + 3 : pos + 5], "big")
        msg += raw[pos + 5 : pos + 5 + length]
        pos += 5 + length
    assert msg[0] == 1, "not a ClientHello"
    return bytes(msg[4 : 4 + int.from_bytes(msg[1:4], "big")]), version


def u16s(data):
    return [int.from_bytes(data[i : i + 2], "big") for i in range(0, len(data), 2)]


def parse(raw):
    body, record_version = hello_body(raw)
    pos = 34
    session_len = body[pos]
    session = body[pos + 1 : pos + 1 + session_len]
    pos += 1 + session_len
    length = int.from_bytes(body[pos : pos + 2], "big")
    suites = u16s(body[pos + 2 : pos + 2 + length])
    pos += 2 + length
    length = body[pos]
    compression = body[pos + 1 : pos + 1 + length].hex()
    pos += 1 + length
    total = int.from_bytes(body[pos : pos + 2], "big")
    pos += 2
    end = pos + total
    exts = []
    while pos < end:
        kind = int.from_bytes(body[pos : pos + 2], "big")
        length = int.from_bytes(body[pos + 2 : pos + 4], "big")
        exts.append((kind, body[pos + 4 : pos + 4 + length]))
        pos += 4 + length
    return {
        "record_version": record_version,
        "legacy_version": body[:2].hex(),
        "session_id_len": session_len,
        "session_id": session.hex(),
        "suites": suites,
        "compression": compression,
        "exts": exts,
    }


def details(hello):
    """各扩展的内容（去掉随机部分），两边逐项比较用。"""
    out = {}
    for kind, data in hello["exts"]:
        key = f"{kind:04x}"
        if is_grease(kind):
            out["grease_ext_len"] = sorted(out.get("grease_ext_len", []) + [len(data)])
        elif kind == 0x0000:
            out[key + " server_name"] = "present"
        elif kind == 0x0005:
            out[key + " status_request"] = data.hex()
        elif kind == 0x000a:
            out[key + " supported_groups"] = ["GREASE" if is_grease(g) else f"{g:04x}" for g in u16s(data[2:])]
        elif kind == 0x000b:
            out[key + " ec_point_formats"] = data.hex()
        elif kind == 0x000d:
            out[key + " signature_algorithms"] = ["GREASE" if is_grease(s) else f"{s:04x}" for s in u16s(data[2:])]
        elif kind == 0x0010:
            names, pos = [], 2
            while pos < len(data):
                n = data[pos]
                names.append(data[pos + 1 : pos + 1 + n].decode())
                pos += 1 + n
            out[key + " alpn"] = names
        elif kind == 0x0012:
            out[key + " sct"] = data.hex()
        elif kind == 0x0017:
            out[key + " extended_master_secret"] = data.hex()
        elif kind == 0x001b:
            out[key + " compress_certificate"] = data.hex()
        elif kind == 0x0023:
            out[key + " session_ticket"] = data.hex()
        elif kind == 0x002b:
            out[key + " supported_versions"] = ["GREASE" if is_grease(v) else f"{v:04x}" for v in u16s(data[1:])]
        elif kind == 0x002d:
            out[key + " psk_kex_modes"] = data.hex()
        elif kind == 0x0033:
            shares, pos = [], 2
            while pos < len(data):
                group = int.from_bytes(data[pos : pos + 2], "big")
                n = int.from_bytes(data[pos + 2 : pos + 4], "big")
                shares.append(["GREASE" if is_grease(group) else f"{group:04x}", n])
                pos += 4 + n
            out[key + " key_share"] = shares
        elif kind in (0x4469, 0x44cd):
            out[f"{key} application_settings"] = data.hex()
        elif kind == 0xfe0d:
            # 外层类型、KDF、AEAD 固定；config_id 和内容随机；只比长度形状
            enc_len = int.from_bytes(data[6:8], "big")
            out[key + " ech_outer"] = {
                "type": data[0],
                "kdf": data[1:3].hex(),
                "aead": data[3:5].hex(),
                "enc_len": enc_len,
                "payload_len": int.from_bytes(data[8 + enc_len : 10 + enc_len], "big"),
                "total_len": len(data),
            }
        elif kind == 0xff01:
            out[key + " renegotiation_info"] = data.hex()
        elif kind == 0x0029:
            out[key + " pre_shared_key"] = len(data)
        elif kind == 0x0015:
            out[key + " padding"] = len(data)
        else:
            out[key] = data.hex()
    return out


def ja4(hello, tcp=True):
    suites = [s for s in hello["suites"] if not is_grease(s)]
    exts = [k for k, _ in hello["exts"] if not is_grease(k)]
    data = dict((k, d) for k, d in hello["exts"])
    version = "12"
    if 0x002b in data:
        versions = [v for v in u16s(data[0x002b][1:]) if not is_grease(v)]
        version = {0x0304: "13", 0x0303: "12", 0x0302: "11", 0x0301: "10"}.get(max(versions), "00")
    sni = "d" if 0x0000 in data else "i"
    alpn = "00"
    if 0x0010 in data and len(data[0x0010]) > 3:
        n = data[0x0010][2]
        name = data[0x0010][3 : 3 + n].decode("latin-1")
        alpn = name[0] + name[-1]
    head = f"{'t' if tcp else 'q'}{version}{sni}{len(suites):02d}{len(exts):02d}{alpn}"
    part_b = ",".join(f"{s:04x}" for s in sorted(suites))
    sorted_exts = ",".join(f"{e:04x}" for e in sorted(e for e in exts if e not in (0x0000, 0x0010)))
    # JA4 规范：GREASE 从密码套件、扩展、签名算法里都去掉（否则同一个 Chrome 每次的哈希都不同）
    sigs = ",".join(f"{s:04x}" for s in u16s(data.get(0x000d, b"\0\0")[2:]) if not is_grease(s))
    part_c = sorted_exts + ("_" + sigs if sigs else "")
    digest = lambda text: hashlib.sha256(text.encode()).hexdigest()[:12] if text else "000000000000"
    return {
        "ja4": f"{head}_{digest(part_b)}_{digest(part_c)}",
        "ja4_r": f"{head}_{part_b}_{part_c}",
    }


def akamai(rec):
    """`SETTINGS|WINDOW_UPDATE|PRIORITY|伪头部` 加上 HEADERS 帧的优先级、第一个请求的请求头顺序。"""
    frames = rec.get("frames", [])
    settings = next((f["settings"] for f in frames if f["type"] == 4 and "settings" in f), [])
    update = next((f["increment"] for f in frames if f["type"] == 8 and f["stream"] == 0), 0)
    priority = [f["priority_frame"] for f in frames if f["type"] == 2]
    requests = rec.get("requests", [])
    fields = requests[0]["fields"] if requests else []
    pseudo = ",".join(f["name"][1] for f in fields if f["name"].startswith(":"))
    first_headers = next((f for f in frames if f["type"] == 1), {})
    order = [f["type"] for f in frames[:4]]
    return {
        "akamai": "|".join(
            [
                ";".join(f"{k}:{v}" for k, v in settings),
                str(update),
                ",".join(priority) or "0",
                pseudo,
            ]
        ),
        "headers_flags": first_headers.get("flags"),
        "headers_priority": first_headers.get("priority"),
        "first_frame_types": order,
        "header_order": [f["name"] for f in fields],
        "header_values": {f["name"]: f["value"] for f in fields},
    }


def summarize(rec):
    raw = bytes.fromhex(rec["client_hello"])
    hello = parse(raw)
    out = {
        "ja4": ja4(hello),
        "record_version": hello["record_version"],
        "legacy_version": hello["legacy_version"],
        "session_id_len": hello["session_id_len"],
        "suites": ["GREASE" if is_grease(s) else f"{s:04x}" for s in hello["suites"]],
        "compression": hello["compression"],
        "ext_wire_order": [f"{k:04x}" for k, _ in hello["exts"]],
        "ext_set": sorted(f"{k:04x}" for k, _ in hello["exts"] if not is_grease(k)),
        "details": details(hello),
        "negotiated": {k: rec.get(k) for k in ("tls_version", "cipher", "group", "alpn")},
    }
    if rec.get("frames") is not None:
        out["http2"] = akamai(rec)
    if rec.get("h1_requests"):
        lines = rec["h1_requests"][0].split("\r\n")
        out["http1"] = {"request_line": lines[0], "headers": lines[1:]}
    if "error" in rec:
        out["error"] = rec["error"]
    return out


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8") as lines:
        for line in lines:
            print(json.dumps(summarize(json.loads(line)), indent=1))
