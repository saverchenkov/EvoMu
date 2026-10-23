"""Python 2.7 marshal reader + import extractor for EVE code.ccp (read-only, outputs stay in scratch)."""
import struct, zipfile, zlib, os

CCP = os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/"
                         "Contents/Resources/build/code.ccp")


class Code(object):
    __slots__ = ("argcount", "nlocals", "stacksize", "flags", "code", "consts", "names", "varnames",
                 "freevars", "cellvars", "filename", "name", "firstlineno", "lnotab")


class Reader(object):
    def __init__(self, b):
        self.b = b; self.p = 0; self.refs = []

    def i32(self):
        v = struct.unpack_from("<i", self.b, self.p)[0]; self.p += 4; return v

    def obj(self):
        t = chr(self.b[self.p]); self.p += 1
        if t == '0': return None
        if t == 'N': return None
        if t == 'F': return False
        if t == 'T': return True
        if t == 'S': return StopIteration
        if t == '.': return Ellipsis
        if t == 'i': return self.i32()
        if t == 'I':
            v = struct.unpack_from("<q", self.b, self.p)[0]; self.p += 8; return v
        if t == 'f':
            n = self.b[self.p]; self.p += 1; s = self.b[self.p:self.p + n]; self.p += n; return float(s)
        if t == 'g':
            v = struct.unpack_from("<d", self.b, self.p)[0]; self.p += 8; return v
        if t == 'x':
            n = self.b[self.p]; self.p += 1; r = float(self.b[self.p:self.p + n]); self.p += n
            n = self.b[self.p]; self.p += 1; i = float(self.b[self.p:self.p + n]); self.p += n
            return complex(r, i)
        if t == 'y':
            r, i = struct.unpack_from("<dd", self.b, self.p); self.p += 16; return complex(r, i)
        if t == 'l':
            n = self.i32(); v = 0
            digs = struct.unpack_from("<%dH" % abs(n), self.b, self.p); self.p += 2 * abs(n)
            for d in reversed(digs): v = (v << 15) | d
            return -v if n < 0 else v
        if t in 'st':
            n = self.i32(); s = self.b[self.p:self.p + n]; self.p += n
            try: s = s.decode("latin-1")
            except Exception: pass
            if t == 't': self.refs.append(s)
            return s
        if t == 'R':
            return self.refs[self.i32()]
        if t == 'u':
            n = self.i32(); s = self.b[self.p:self.p + n]; self.p += n; return s.decode("utf-8", "replace")
        if t in '([<>':
            n = self.i32(); return tuple(self.obj() for _ in range(n))
        if t == '{':
            d = {}
            while True:
                if chr(self.b[self.p]) == '0': self.p += 1; break
                k = self.obj(); d[k] = self.obj()
            return d
        if t == 'c':
            c = Code()
            c.argcount = self.i32(); c.nlocals = self.i32(); c.stacksize = self.i32(); c.flags = self.i32()
            c.code = self.obj(); c.consts = self.obj(); c.names = self.obj(); c.varnames = self.obj()
            c.freevars = self.obj(); c.cellvars = self.obj(); c.filename = self.obj(); c.name = self.obj()
            c.firstlineno = self.i32(); c.lnotab = self.obj()
            if isinstance(c.code, str): c.code = c.code.encode("latin-1")
            return c
        raise ValueError("bad marshal type %r at %d" % (t, self.p - 1))


def load(b):
    try: b = zlib.decompress(b)
    except zlib.error: pass
    assert b[:4] == b"\x03\xf3\r\n", b[:4]
    return Reader(b[8:]).obj()


def ops(co):
    b = co.code; i = 0; ext = 0; n = len(b)
    while i < n:
        op = b[i]
        if op >= 90:
            arg = b[i + 1] | (b[i + 2] << 8) | ext; ext = 0; i += 3
            if op == 145: ext = arg << 16; continue
            yield op, arg
        else:
            i += 1; yield op, None


def walk(co):
    yield co
    for c in co.consts or ():
        if isinstance(c, Code):
            for x in walk(c): yield x


LOAD_CONST, IMPORT_NAME, IMPORT_FROM, IMPORT_STAR = 100, 108, 109, 84


def imports(co):
    """Yield (funcname, modname, level, fromlist) for every IMPORT_NAME in all nested code objects."""
    for c in walk(co):
        hist = []
        for op, arg in ops(c):
            if op == IMPORT_NAME:
                level = fromlist = "?"
                if len(hist) >= 2 and hist[-1][0] == LOAD_CONST and hist[-2][0] == LOAD_CONST:
                    fromlist = c.consts[hist[-1][1]]; level = c.consts[hist[-2][1]]
                yield c.name, c.names[arg], level, fromlist
            hist.append((op, arg))
            if len(hist) > 4: hist.pop(0)


def archive():
    z = zipfile.ZipFile(CCP)
    for n in z.namelist():
        yield n, z.read(n)
