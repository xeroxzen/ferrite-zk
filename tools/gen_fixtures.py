# /// script
# requires-python = ">=3.9"
# dependencies = [
#     "pyzk @ git+https://github.com/fananimi/pyzk@1af361a00a662fafa56700e6bc5806afe0199c3c",
# ]
# ///
# SPDX-License-Identifier: GPL-2.0-only
#
# Golden-fixture generator for zkteco-proto.
#
# Runs pyzk (the reference implementation) against an in-process fake device
# and records what pyzk sends and how it interprets what it receives. The
# resulting JSON is plain data; this script is GPL-2.0-only because it imports
# pyzk, and it is never part of a published crate.
#
# Usage: uv run tools/gen_fixtures.py [OUT_DIR]
#        (default OUT_DIR: crates/zkteco-proto/tests/fixtures/generated)

import json
import random
import socket as std_socket
import struct
import sys
from collections import deque
from datetime import datetime, timedelta
from pathlib import Path

import zk.base as zkbase
from zk import const
from zk.base import ZK, make_commkey
from zk.finger import Finger
from zk.user import User

PYZK_COMMIT = "1af361a00a662fafa56700e6bc5806afe0199c3c"
SEED = 20261114
CASES = 64
SESSION = 0x2A51

# ---------------------------------------------------------------- fake device


def checksum(buf):
    if len(buf) % 2:
        buf += b"\0"
    s = 0
    for (w,) in struct.iter_unpack("<H", buf):
        s += w
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


def packet(cmd, session, reply, payload=b""):
    head = struct.pack("<4H", cmd, 0, session, reply)
    cs = checksum(head + payload)
    return struct.pack("<4H", cmd, cs, session, reply) + payload


class FakeDevice:
    """Answers pyzk's requests from an in-memory model. One packet per recv."""

    def __init__(self, tcp, sizes=b"", tables=None, options=None, chunked=False,
                 events=(), enroll=(), single_template=b""):
        self.tcp = tcp
        self.sizes = sizes
        self.tables = tables or {}
        self.options = options or {}
        self.chunked = chunked
        self.events = list(events)
        self.enroll = list(enroll)
        self.single_template = single_template
        self.staged = b""
        self.sent = []
        self.out = deque()

    # socket API used by pyzk
    def settimeout(self, _t):
        pass

    def connect_ex(self, _addr):
        return 0

    def close(self):
        pass

    def send(self, data):
        assert data[:4] == b"\x50\x50\x82\x7d"
        (length,) = struct.unpack("<I", data[4:8])
        assert length == len(data) - 8
        self.handle(data[8:])
        return len(data)

    def sendto(self, data, _addr):
        self.handle(data)
        return len(data)

    def recv(self, _n):
        if not self.out:
            raise std_socket.timeout("fake device idle")
        return self.out.popleft()

    # device model
    def push(self, cmd, payload=b"", session=SESSION, reply=0):
        p = packet(cmd, session, reply, payload)
        if self.tcp:
            p = struct.pack("<HHI", 0x5050, 0x7D82, len(p)) + p
        self.out.append(p)

    def handle(self, raw):
        cmd, _cs, _sess, reply = struct.unpack("<4H", raw[:8])
        payload = raw[8:]
        self.sent.append({"command": cmd, "reply_id": reply, "payload": payload})
        if cmd == const.CMD_ACK_OK:
            return  # acknowledgement of an event we pushed
        ok = const.CMD_ACK_OK
        if cmd == const.CMD_GET_FREE_SIZES:
            self.push(ok, self.sizes, reply=reply)
        elif cmd == const._CMD_PREPARE_BUFFER:
            (_one, inner, _fct, _ext) = struct.unpack("<bhii", payload)
            data = self.tables[inner]
            if self.chunked:
                self.staged = data
                ack = b"\0" + struct.pack("<II", len(data), len(data)) + b"\0" * 4
                self.push(ok, ack, reply=reply)
            else:
                self.push(const.CMD_DATA, data, reply=reply)
        elif cmd == const._CMD_READ_BUFFER:
            start, size = struct.unpack("<ii", payload)
            self.push(const.CMD_DATA, self.staged[start:start + size], reply=reply)
        elif cmd == const._CMD_GET_USERTEMP:
            self.push(const.CMD_DATA, self.single_template, reply=reply)
        elif cmd == const.CMD_OPTIONS_RRQ:
            key = payload.split(b"\0")[0].decode()
            if key in self.options:
                self.push(ok, self.options[key], reply=reply)
            else:
                self.push(const.CMD_ACK_ERROR, reply=reply)
        elif cmd == const.CMD_GET_VERSION:
            self.push(ok, self.options["__version__"], reply=reply)
        elif cmd == const.CMD_GET_PINWIDTH:
            self.push(ok, self.options["__pin_width__"], reply=reply)
        elif cmd == const.CMD_GET_TIME:
            self.push(ok, self.options["__time__"], reply=reply)
        elif cmd == const.CMD_REG_EVENT:
            self.push(ok, reply=reply)
            (flags,) = struct.unpack("<I", payload)
            if flags:
                for ev in self.events:
                    self.push(const.CMD_REG_EVENT, ev, session=flags, reply=0)
        elif cmd == const.CMD_STARTENROLL:
            self.push(ok, reply=reply)
            for ev in self.enroll:
                self.push(const.CMD_REG_EVENT, ev, session=const.EF_ENROLLFINGER, reply=0)
        else:
            self.push(ok, reply=reply)

    def payloads(self, cmd):
        return [s["payload"] for s in self.sent if s["command"] == cmd]


def connected(device, tcp=False):
    zkbase.socket = lambda *_a, **_k: device
    z = ZK("192.0.2.1", force_udp=not tcp, ommit_ping=True)
    z.connect()
    return z


def sizes_payload(users=0, fingers=0, records=0, face=None):
    words = [0] * 20
    words[4], words[6], words[8] = users, fingers, records
    out = struct.pack("<20i", *words)
    if face is not None:
        out += struct.pack("<3i", *face)
    return out


# ---------------------------------------------------------------- helpers

R = random.Random(SEED)


def hx(b):
    return b.hex()


def rand_dt():
    start = datetime(2000, 1, 1)
    return start + timedelta(seconds=R.randrange(int((datetime(2099, 12, 31, 23, 59, 59) - start).total_seconds())))


def iso(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%S")


ALNUM = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
UNICODE = "ÄÖÜßéñçøåł"


def rand_text(max_len, allow_empty=True, unicode=False):
    n = R.randrange(0 if allow_empty else 1, max_len + 1)
    pool = ALNUM + (UNICODE if unicode else "")
    while True:
        s = "".join(R.choice(pool) for _ in range(n))
        if len(s.encode()) <= max_len:
            return s
        n -= 1


def rand_digits(max_len):
    return str(R.randrange(1, 10 ** min(max_len, 9)))


# ---------------------------------------------------------------- generators


def gen_packets():
    z = ZK("192.0.2.1", force_udp=True, ommit_ping=True)
    create = z._ZK__create_header
    tcp_top = z._ZK__create_tcp_top
    cases = []
    replies = [0, 1, 65533, 65534] + [R.randrange(65535) for _ in range(CASES)]
    for reply_before in replies:
        cmd = R.choice([1000, 1001, 1102, 1500, 1503, 2000, R.randrange(65536)])
        session = R.randrange(65536)
        payload = bytes(R.randrange(256) for _ in range(R.choice([0, 1, 2, 3, 7, 8, 11, 24, 33])))
        pkt = create(cmd, payload, session, reply_before)
        (sent_reply,) = struct.unpack("<H", pkt[6:8])
        cases.append({
            "command": cmd, "session_id": session, "reply_id": sent_reply,
            "payload": hx(payload), "packet": hx(pkt), "tcp_frame": hx(tcp_top(pkt)),
        })
    return cases


def gen_commkey():
    cases = []
    keys = [0, 1, 12, 45, 123456, 999999, 0x7FFFFFFF] + [R.randrange(10 ** 8) for _ in range(CASES)]
    for key in keys:
        session = R.randrange(65536)
        ticks = R.choice([50, 0, 255, R.randrange(256)])
        try:
            out = make_commkey(key, session, ticks)
        except struct.error:
            continue  # pyzk cannot represent the 32-bit overflow case
        cases.append({"password": key, "session_id": session, "ticks": ticks, "key": hx(out)})
    return cases


def gen_time():
    z = ZK("192.0.2.1", force_udp=True, ommit_ping=True)
    enc, dec, dec6 = z._ZK__encode_time, z._ZK__decode_time, z._ZK__decode_timehex
    packed, compact = [], []
    fixed = [datetime(2000, 1, 1), datetime(2099, 12, 31, 23, 59, 59), datetime(2024, 2, 29, 12, 0, 0)]
    for dt in fixed + [rand_dt() for _ in range(CASES)]:
        v = enc(dt)
        assert dec(struct.pack("<I", v)) == dt
        packed.append({"datetime": iso(dt), "packed": v})
        six = bytes([dt.year - 2000, dt.month, dt.day, dt.hour, dt.minute, dt.second])
        compact.append({"datetime": iso(dec6(six)), "bytes": hx(six)})
    return {"packed": packed, "compact": compact}


def user_fields(u):
    return {"uid": u.uid, "privilege": u.privilege, "password": u.password, "name": u.name,
            "card": u.card, "group_id": u.group_id, "user_id": u.user_id}


def gen_user_tables():
    out = []
    for fmt in (28, 72, 120):
        for _ in range(CASES // 4):
            n = R.randrange(1, 6)
            recs = []
            for i in range(n):
                uid = R.randrange(1, 65536)
                priv = R.choice([0, 1, 2, 3, 6, 7, 14, 15, 4, 8])
                card = R.choice([0, R.randrange(2 ** 32)])
                if fmt == 28:
                    recs.append(struct.pack("<HB5s8sIxBhI", uid, priv, rand_text(5).encode(),
                                            rand_text(8).encode(), card, R.randrange(256),
                                            R.randrange(-5, 5), R.randrange(2 ** 32)))
                elif fmt == 72:
                    recs.append(struct.pack("<HB8s24sIB7sx24s", uid, priv, rand_text(8).encode(),
                                            rand_text(24, unicode=True).encode(), card, R.randrange(256),
                                            rand_digits(7).encode(), rand_digits(24).encode()))
                else:
                    rec = bytearray(R.randrange(256) for _ in range(120))
                    rec[0:11] = struct.pack("<HB8s", uid, priv, rand_text(8).encode())
                    rec[35:59] = rand_text(23).encode().ljust(24, b"\0")
                    rec[59:83] = rand_text(23).encode().ljust(24, b"\0")
                    rec[83:87] = struct.pack("<I", card)
                    rec[96:120] = rand_digits(23).encode().ljust(24, b"\0")
                    recs.append(bytes(rec))
            body = b"".join(recs)
            table = struct.pack("<I", len(body)) + body
            dev = FakeDevice(False, sizes=sizes_payload(users=n), tables={const.CMD_USERTEMP_RRQ: table})
            users = connected(dev).get_users()
            out.append({"record_len": fmt, "user_count": n, "table": hx(table),
                        "users": [user_fields(u) for u in users]})
    return out


def gen_user_writes():
    out = []
    for fmt in (28, 72):
        for _ in range(CASES // 2):
            uid = R.randrange(1, 65536)
            priv = R.choice([0, 14])  # pyzk coerces anything else
            if fmt == 28:
                args = dict(uid=uid, name=rand_text(8), privilege=priv, password=rand_text(5),
                            group_id=str(R.randrange(256)), user_id=str(R.randrange(2 ** 32)),
                            card=R.randrange(2 ** 32))
            else:
                args = dict(uid=uid, name=rand_text(24, unicode=True), privilege=priv,
                            password=rand_text(8), group_id=rand_digits(7), user_id=rand_digits(24),
                            card=R.randrange(2 ** 32))
            dev = FakeDevice(False)
            z = connected(dev)
            z.user_packet_size = fmt
            z.set_user(**args)
            (payload,) = dev.payloads(const.CMD_USER_WRQ)
            out.append({"record_len": fmt, "user": args, "payload": hx(payload)})
    return out


def att_fields(a):
    return {"uid": a.uid, "user_id": a.user_id, "timestamp": iso(a.timestamp),
            "status": a.status, "punch": a.punch}


def gen_attendance():
    z = ZK("192.0.2.1", force_udp=True, ommit_ping=True)
    enc = z._ZK__encode_time
    out = []
    prefix = b"\xff255\x00\x00\x00\x00\x00"
    for fmt in (8, 16, 40, 49):
        for _ in range(CASES // 4):
            n = R.randrange(1, 6)
            recs, workcodes = [], []
            for _ in range(n):
                t = struct.pack("<I", enc(rand_dt()))
                status, punch = R.randrange(256), R.randrange(256)
                if fmt == 8:
                    recs.append(struct.pack("<HB4sB", R.randrange(65536), status, t, punch))
                elif fmt == 16:
                    wc = R.randrange(2 ** 32)
                    workcodes.append(wc)
                    recs.append(struct.pack("<I4sBB2sI", R.randrange(2 ** 32), t, status, punch, b"\0\0", wc))
                else:
                    body = struct.pack("<H24sB4sB8s", R.randrange(65536), rand_digits(24).encode(),
                                       status, t, punch, b"\0" * 8)
                    recs.append(prefix + body if fmt == 49 else body)
            body = b"".join(recs)
            table = struct.pack("<I", len(body)) + body
            dev = FakeDevice(False, sizes=sizes_payload(records=n),
                             tables={const.CMD_ATTLOG_RRQ: table})
            atts = connected(dev).get_attendance()
            out.append({"record_len": fmt, "record_count": n, "table": hx(table),
                        "workcodes": workcodes, "records": [att_fields(a) for a in atts]})
    return out


def gen_events():
    out = []
    layouts = {10: "<HBB6s", 12: "<IBB6s", 14: "<HBB6s4s", 32: "<24sBB6s",
               36: "<24sBB6s4s", 37: "<24sBB6s5s", 52: "<24sBB6s20s"}

    def record(size):
        dt = rand_dt()
        six = bytes([dt.year - 2000, dt.month, dt.day, dt.hour, dt.minute, dt.second])
        fmt = layouts[size]
        if size in (10, 14):
            uid = R.randrange(1, 65536)
        elif size == 12:
            uid = R.randrange(1, 2 ** 32)
        else:
            uid = rand_digits(23).encode()
        fields = [uid, R.randrange(256), R.randrange(256), six]
        extra = {14: 4, 36: 4, 37: 5, 52: 20}.get(size)
        if extra:
            fields.append(bytes(R.randrange(256) for _ in range(extra)))
        return struct.pack(fmt, *fields)

    combos = [[s] for s in layouts for _ in range(4)] + [[52, 52], [52, 12], [52, 52, 32]]
    for tcp in (False, True):
        for combo in combos:
            payload = b"".join(record(s) for s in combo)
            dev = FakeDevice(tcp, sizes=sizes_payload(), events=[payload])
            z = connected(dev, tcp=tcp)
            got = []
            for att in z.live_capture(new_timeout=1):
                if att is None:
                    z.end_live_capture = True
                    continue
                got.append(att)
            out.append({"transport": "tcp" if tcp else "udp", "payload": hx(payload),
                        "records": [att_fields(a) for a in got]})
    return out


def gen_templates():
    out = []
    for _ in range(CASES // 4):
        n = R.randrange(1, 6)
        entries = []
        for _ in range(n):
            tmpl = bytes(R.randrange(256) for _ in range(R.randrange(1, 1500)))
            entries.append(struct.pack("<HHbb", len(tmpl) + 6, R.randrange(1, 65536),
                                       R.randrange(10), R.choice([0, 1, 3])) + tmpl)
        body = b"".join(entries)
        table = struct.pack("<I", len(body)) + body
        dev = FakeDevice(False, sizes=sizes_payload(fingers=n), tables={const.CMD_DB_RRQ: table})
        fingers = connected(dev).get_templates()
        out.append({"table": hx(table), "templates": [
            {"uid": f.uid, "fid": f.fid, "valid": f.valid, "data": hx(f.template)} for f in fingers]})
    return out


def gen_single_templates():
    out = []
    for pad in (False, True, False, True, False):
        tmpl = bytes(R.randrange(1, 256) for _ in range(R.randrange(100, 1300)))
        raw = tmpl + (b"\0" * 6 if pad else b"") + bytes([R.randrange(256)])
        uid, fid = R.randrange(1, 32768), R.randrange(10)
        dev = FakeDevice(False, single_template=raw)
        f = connected(dev).get_user_template(uid=uid, temp_id=fid)
        (req,) = dev.payloads(const._CMD_GET_USERTEMP)
        out.append({"uid": uid, "fid": fid, "request": hx(req), "reply": hx(raw), "data": hx(f.template)})
    return out


def gen_bulk_save():
    out = []
    for fmt in (28, 72):
        for _ in range(4):
            entries = []
            for _ in range(R.randrange(1, 4)):
                uid = R.randrange(1, 65536)
                if fmt == 28:
                    u = User(uid, rand_text(8), R.choice([0, 14]), rand_text(5),
                             str(R.randrange(256)), str(R.randrange(2 ** 32)), R.randrange(2 ** 32))
                else:
                    u = User(uid, rand_text(24), R.choice([0, 14]), rand_text(8),
                             rand_digits(7), rand_digits(24), R.randrange(2 ** 32))
                fingers = [Finger(uid, fid, 1, bytes(R.randrange(256) for _ in range(R.randrange(1, 600))))
                           for fid in R.sample(range(10), R.randrange(0, 3))]
                entries.append((u, fingers))
            dev = FakeDevice(False)
            z = connected(dev)
            z.user_packet_size = fmt
            z.HR_save_usertemplates(entries)
            buffer = b"".join(dev.payloads(const.CMD_DATA))
            (prep,) = dev.payloads(const.CMD_PREPARE_DATA)
            (save,) = dev.payloads(const._CMD_SAVE_USERTEMPS)
            out.append({
                "record_len": fmt,
                "entries": [{"user": user_fields(u),
                             "templates": [{"fid": f.fid, "data": hx(f.template)} for f in fs]}
                            for u, fs in entries],
                "buffer": hx(buffer), "prepare_data": hx(prep), "save_payload": hx(save),
                "data_packet_lens": [len(p) for p in dev.payloads(const.CMD_DATA)],
            })
    return out


def gen_sizes():
    out = []
    for face in (None, (3, 0, 400), None, (0, 7, 0)):
        words = [R.randrange(0, 200000) for _ in range(20)]
        payload = struct.pack("<20i", *words) + (struct.pack("<3i", *face) if face else b"")
        dev = FakeDevice(False, sizes=payload)
        z = connected(dev)
        z.read_sizes()
        out.append({"payload": hx(payload), "users": z.users, "fingers": z.fingers, "records": z.records,
                    "cards": z.cards, "fingers_cap": z.fingers_cap, "users_cap": z.users_cap,
                    "rec_cap": z.rec_cap, "fingers_av": z.fingers_av, "users_av": z.users_av,
                    "rec_av": z.rec_av, "faces": z.faces, "faces_cap": z.faces_cap})
    return out


def gen_device_info():
    opts = {
        "~SerialNumber": b"~SerialNumber=CKJA194760123\0",
        "~Platform": b"~Platform=ZMM220_TFT\0",
        "MAC": b"MAC=00:17:61:12:34:56\0",
        "~DeviceName": b"~DeviceName=K40/ID\0",
        "ZKFaceVersion": b"ZKFaceVersion=7\0",
        "~ZKFPVersion": b"~ZKFPVersion=10\0",
        "~ExtendFmt": b"~ExtendFmt=1\0",
        "~UserExtFmt": b"~UserExtFmt=1\0",
        "FaceFunOn": b"FaceFunOn=0\0",
        "CompatOldFirmware": b"CompatOldFirmware=0\0",
        "IPAddress": b"IPAddress=192.0.2.10\0",
        "NetMask": b"NetMask=255.255.255.0\0",
        "GATEIPAddress": b"GATEIPAddress=192.0.2.1\0",
        "__version__": b"Ver 6.60 Apr 28 2017\0",
        "__pin_width__": bytes([9]) + b"\0" * 8,
    }
    dt = datetime(2026, 11, 14, 9, 30, 0)
    z0 = ZK("192.0.2.1", force_udp=True, ommit_ping=True)
    opts["__time__"] = struct.pack("<I", z0._ZK__encode_time(dt))
    dev = FakeDevice(False, options=opts)
    z = connected(dev)
    results = {
        "serial_number": z.get_serialnumber(), "platform": z.get_platform(), "mac": z.get_mac(),
        "device_name": z.get_device_name(), "face_version": z.get_face_version(),
        "fp_version": z.get_fp_version(), "extend_fmt": z.get_extend_fmt(),
        "user_extend_fmt": z.get_user_extend_fmt(), "face_fun_on": z.get_face_fun_on(),
        "compat_old_firmware": z.get_compat_old_firmware(), "network": z.get_network_params(),
        "firmware_version": z.get_firmware_version(), "pin_width": z.get_pin_width(),
        "time": iso(z.get_time()),
    }
    replies = {k: hx(v) for k, v in opts.items()}
    requests = [{"command": s["command"], "payload": hx(s["payload"])} for s in dev.sent]
    return {"replies": replies, "results": results, "requests": requests}


def gen_command_payloads():
    """Request payloads for simple commands, keyed by a descriptive name."""
    out = []

    def capture(name, cmd, fn, tcp=False, **dev_args):
        dev = FakeDevice(tcp, **dev_args)
        z = connected(dev, tcp=tcp)
        fn(z)
        for p in dev.payloads(cmd):
            out.append({"name": name, "command": cmd, "payload": hx(p)})

    dt = rand_dt()
    capture("set_time " + iso(dt), const.CMD_SET_TIME, lambda z: z.set_time(dt))
    for s in (1, 3, 10):
        capture(f"unlock {s}", const.CMD_UNLOCK, lambda z, s=s: z.unlock(s))
    for i in (0, 5, 55):
        capture(f"test_voice {i}", const.CMD_TESTVOICE, lambda z, i=i: z.test_voice(i))
    capture("write_lcd 1 Hello", const.CMD_WRITE_LCD, lambda z: z.write_lcd(1, "Hello"))
    capture("reg_event 1", const.CMD_REG_EVENT, lambda z: z.reg_event(1))
    capture("reg_event 0", const.CMD_REG_EVENT, lambda z: z.reg_event(0))
    for uid in (1, 300, 65535):
        capture(f"delete_user {uid}", const.CMD_DELETE_USER, lambda z, uid=uid: z.delete_user(uid=uid))
    for uid, fid in ((1, 0), (300, 9)):
        capture(f"delete_user_template {uid} {fid}", const.CMD_DELETE_USERTEMP,
                lambda z, uid=uid, fid=fid: z.delete_user_template(uid=uid, temp_id=fid))
    capture("get_pin_width", const.CMD_GET_PINWIDTH, lambda z: z.get_pin_width(),
            options={"__pin_width__": b"\x09"})
    capture("sdk_build", const.CMD_OPTIONS_WRQ, lambda z: z.set_sdk_build_1())
    for fct_cmd, table in ((const.CMD_USERTEMP_RRQ, "users"), (const.CMD_ATTLOG_RRQ, "attendance"),
                           (const.CMD_DB_RRQ, "templates")):
        capture(f"prepare_buffer {table}", const._CMD_PREPARE_BUFFER,
                lambda z, c=fct_cmd: z.read_with_buffer(c, {const.CMD_USERTEMP_RRQ: const.FCT_USER,
                                                             const.CMD_DB_RRQ: const.FCT_FINGERTMP}.get(c, 0)),
                tables={fct_cmd: b"\0\0\0\0"})
    enroll_ok = [struct.pack("<H", 0x64)] * 6 + [struct.pack("<HHH", 0, 1243, 3)]
    capture("enroll tcp 1234 3", const.CMD_STARTENROLL,
            lambda z: z.enroll_user(uid=1, temp_id=3, user_id="1234"), tcp=True, enroll=enroll_ok)
    capture("enroll udp 1234 3", const.CMD_STARTENROLL,
            lambda z: z.enroll_user(uid=1, temp_id=3, user_id="1234"), enroll=enroll_ok)
    return out


def gen_chunked_reads():
    """The (start, size) READ_BUFFER requests pyzk makes for a staged table."""
    out = []
    for tcp, size in ((False, 100), (False, 16384), (False, 21600), (True, 72000), (True, 65472)):
        data = bytes(R.randrange(256) for _ in range(size))
        dev = FakeDevice(tcp, chunked=True, tables={const.CMD_ATTLOG_RRQ: data})
        z = connected(dev, tcp=tcp)
        got, total = z.read_with_buffer(const.CMD_ATTLOG_RRQ)
        assert got == data and total == size
        reads = [list(struct.unpack("<ii", p)) for p in dev.payloads(const._CMD_READ_BUFFER)]
        out.append({"transport": "tcp" if tcp else "udp", "size": size, "reads": reads,
                    "free_data_sent": bool(dev.payloads(const.CMD_FREE_DATA))})
    return out


GENERATORS = {
    "packets": gen_packets,
    "commkey": gen_commkey,
    "time": gen_time,
    "user_tables": gen_user_tables,
    "user_writes": gen_user_writes,
    "attendance": gen_attendance,
    "events": gen_events,
    "templates": gen_templates,
    "single_templates": gen_single_templates,
    "bulk_save": gen_bulk_save,
    "sizes": gen_sizes,
    "device_info": gen_device_info,
    "command_payloads": gen_command_payloads,
    "chunked_reads": gen_chunked_reads,
}


def main():
    out_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "crates/zkteco-proto/tests/fixtures/generated")
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, gen in GENERATORS.items():
        doc = {"generator": "tools/gen_fixtures.py", "pyzk_commit": PYZK_COMMIT, "seed": SEED,
               "cases": gen()}
        (out_dir / f"{name}.json").write_text(json.dumps(doc, indent=1, ensure_ascii=False) + "\n")
        print(f"wrote {name}.json")


if __name__ == "__main__":
    main()
