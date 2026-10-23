#!/usr/bin/env python3
"""AUDIT E / T-10 prototype: deterministic client-expectation extraction from EVE V24.01 code.ccp.

Read-only on the client archive. Outputs (client-derived; keep OUT of every repo) go next to this script.

Method: abstract interpretation of Python 2.7 bytecode (marshal reader in ccp_reader.py, opcode table in
dis27.py). Per code object a linear stack simulation with jump-target stack snapshots; locals are
flow-insensitive (last store in linear order wins). Values are symbolic tuples:
  ('C', v)            constant                      ('P', 'a.b.c')   dotted global/import path
  ('SELF',)           first arg named self          ('SA', attr)     unresolved self.attr
  ('SVC', svc, via)   bound remote service/moniker  ('BM', svc, m, via) attribute of a service
  ('T', key, path)    tracked value (RPC reply or notify param); key = site id, path = tuple of steps
  ('ITER', T)         iterator over a tracked value ('TM', key, path, meth) method of tracked value
  ('CR', name)        result of calling something whose terminal name is `name` (factory resolution)
  ('A', name)         attribute `name` of an unknown object
Passes: (1) summaries -> service-factory fixpoint (functions returning Moniker('svc')/RemoteSvc('svc'),
self.attr bindings); (2) full interpretation -> RPC sites + passthrough wrappers; (3) final interpretation
with wrappers resolved -> reply shapes, notify params, session keys, constants.

Usage: extractor.py [--ccp PATH] [--out client_expectations.json]
"""
import argparse
import collections
import json
import os
import re
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ccp_reader as CR  # noqa: E402
from dis27 import instructions  # noqa: E402

SVC_BINDERS = {'RemoteSvc', 'ProxySvc', 'ConnectToRemoteService', 'ConnectToAnyService',
               'ConnectToSolServerService', 'ConnectToProxyServerService', 'ConnectToAllServices',
               'ConnectToAllProxyServerServices', 'ConnectToAllSolServerServices',
               'ConnectToAllNeighboringServices', 'ConnectToAllSiblingServices', 'Moniker'}
# Methods of the moniker/proxy object itself (client-local, never sent as an RPC).
LOCAL_PROXY_METHODS = {'Bind', 'Unbind', 'SetSessionCheck', 'GetSession', 'GetNodeID', 'ClearCache',
                       'IsBound', 'GetBoundService', 'SetCallTimeout', 'CallMethod'}
FACTORY_STOP = {'GetService', 'RemoteSvc', 'ProxySvc', 'Moniker', '__init__', 'Get', '__call__',
                '__getattr__', 'GetInstance', 'Instance', 'Startup', 'Run'}
TRACKED_METHODS = {'get', 'has_key', 'keys', 'values', 'items', 'iteritems', 'itervalues', 'iterkeys',
                   'index', 'count', 'copy', 'pop', 'setdefault', 'split', 'startswith', 'lower', 'upper',
                   'strip', 'format', 'encode', 'decode', 'Get', 'iterkeys', 'difference', 'union',
                   'intersection', 'issubset'}
ITER_BUILTINS = {'list', 'tuple', 'sorted', 'set', 'frozenset', 'dict', 'enumerate', 'iter', 'sum', 'min',
                 'max', 'any', 'all', 'zip', 'reversed', 'map', 'filter'}
SESSION_HANDLERS = {'OnSessionChanged', 'OnSessionMutated', 'DoSessionChanging', 'ProcessSessionChange',
                    'OnSessionChanging', 'OnSessionReset'}
CONST_WORDS = {'timer', 'timeout', 'cooldown', 'duration', 'delay', 'logoff', 'fatigue', 'range', 'seconds',
               'secs', 'sec', 'interval', 'msec', 'ms', 'minutes', 'hours', 'expiry', 'expire', 'expires',
               'lifetime', 'ttl', 'period', 'countdown'}
CMP = ['<', '<=', '==', '!=', '>', '>=', 'in', 'not in', 'is', 'is not', 'exception match', 'BAD']
BINOPS = {19, 20, 21, 22, 23, 24, 26, 27, 28, 29, 55, 56, 57, 58, 59, 62, 63, 64, 65, 66, 67, 75, 76, 77, 78, 79}
JREL = {93, 110, 120, 121, 122, 143}
MAX_PATH = 4
ID_PREFIXES = {'attribute', 'effect', 'event', 'type', 'group', 'category', 'dgm', 'error', 'message',
               'msg', 'log', 'sound', 'icon', 'graphic', 'flag', 'cmd', 'key'}


def is_rpc_method(meth):
    return bool(meth) and not meth.startswith('_') and meth not in LOCAL_PROXY_METHODS and \
        (meth[:1].isupper() or meth[:1].islower())


def words(name):
    s = re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', name)
    s = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1_\2', s)
    return [w for w in s.lower().split('_') if w]


def const_repr(v):
    if isinstance(v, bytes):
        v = v.decode('latin-1')
    if isinstance(v, (str, int, float, bool)) or v is None:
        return json.dumps(v)
    if isinstance(v, tuple):
        return '(' + ','.join(const_repr(x) for x in v) + ')'
    return '?'


def path_str(path):
    return ''.join(path) if path else '$'


def lineno_table(co):
    """offset -> line (CPython 2.7 lnotab)."""
    lt = co.lnotab
    if isinstance(lt, str):
        lt = lt.encode('latin-1')
    res = []
    addr, line = 0, co.firstlineno
    for i in range(0, len(lt or b''), 2):
        res.append((addr, line))
        addr += lt[i]
        line += lt[i + 1]
    res.append((addr, line))
    return res


def line_at(table, off):
    ln = table[0][1]
    for a, l in table:
        if a > off:
            break
        ln = l
    return ln


def walk_named(co, prefix=''):
    """Yield (qualname, code, parent_code) depth-first in const order (deterministic)."""
    stack = [(co, prefix, None)]
    while stack:
        c, q, parent = stack.pop(0)
        yield q, c, parent
        kids = []
        for k in c.consts or ():
            if isinstance(k, CR.Code):
                kq = k.name if c.name == '<module>' else '%s.%s' % (q, k.name)
                kids.append((k, kq, c))
        stack[0:0] = kids


def callee_name(v):
    if v is None:
        return None
    t = v[0]
    if t == 'P':
        return v[1].rsplit('.', 1)[-1]
    if t in ('A', 'SA'):
        return v[1]
    if t == 'BM':
        return v[2]
    if t == 'CR':
        return None
    return None


def callee_label(v):
    if v is None:
        return '?'
    t = v[0]
    if t == 'P':
        return v[1]
    if t == 'SA':
        return 'self.' + v[1]
    if t == 'A':
        return '?.' + v[1]
    if t == 'BM':
        return '%s.%s' % (v[1], v[2])
    if t == 'TM':
        return 'reply.' + v[3]
    return '?'


class World(object):
    def __init__(self):
        self.factories = {}       # terminal function name -> svc (unique, resolved)
        self.selfattr = {}        # (module, attr) -> ('SVC', svc, via)
        self.globals = {}         # (module, name) -> ('SVC', svc, via)
        self.attrnames = {}       # attribute name -> ('SVC', svc, via) when every store of that attr name is it
        self.wrappers = {}        # terminal function name -> site key
        self.wrapper_svc = {}     # terminal function name -> 'svc.Method'
        self.selfattr_ret = {}    # (module, attr) -> site key (RPC reply stored on self)
        self.notify_params = {}   # (module, qual) -> param list (seeded in pass 3)
        self.session_params = {}


class Interp(object):
    """One run over one code object. mode: 1=summary, 2=sites, 3=final."""

    def __init__(self, world, module, qual, co, mode, seeds=None):
        self.w, self.m, self.q, self.co, self.mode = world, module, qual, co, mode
        self.argnames = list(co.varnames[:co.argcount])
        self.is_method = bool(self.argnames) and self.argnames[0] == 'self'
        self.locals = dict(seeds or {})
        self.derefs = {}
        self.returns = []          # values returned
        self.selfstores = []       # (attr, value)
        self.attrstores = []       # (attr, value) on any object
        self.globstores = []       # (name, value)
        self.sites = []            # dicts
        self.obs = collections.defaultdict(set)   # key -> set((path, use))
        self.consts = []
        self.lt = None

    # ---- observation helpers
    def ob(self, v, use):
        if v is not None and v[0] == 'T':
            self.obs[v[1]].add((path_str(v[2]), use))

    def child(self, v, step):
        if len(v[2]) >= MAX_PATH:
            return ('T', v[1], v[2])
        return ('T', v[1], v[2] + (step,))

    def line(self, off):
        if self.lt is None:
            self.lt = lineno_table(self.co)
        return line_at(self.lt, off)

    # ---- service resolution
    def resolve_call(self, fn, pos, kwn, off):
        """Return the value produced by calling fn(pos..., kw...)."""
        w = self.w
        name = callee_name(fn)
        # RPC call on a bound service
        if fn is not None and fn[0] == 'BM':
            svc, meth, via = fn[1], fn[2], fn[3]
            if is_rpc_method(meth):
                key = '%s|%s|%d' % (self.m, self.q, off)
                self.sites.append({'kind': 'call', 'svc': svc, 'method': meth, 'module': self.m, 'func': self.q,
                                   'line': self.line(off), 'args': len(pos), 'kwargs': sorted(kwn),
                                   'via': via, 'key': key, 'star': False, 'starstar': False,
                                   'arg_consts': [const_repr(a[1]) if a is not None and a[0] == 'C' else None
                                                  for a in pos]})
                return ('T', key, ())
            return None
        # explicit binders: sm.RemoteSvc('x'), Moniker('x', ...), session.ConnectToRemoteService('x')
        if name in SVC_BINDERS and pos and pos[0] is not None and pos[0][0] == 'C' and isinstance(pos[0][1], str):
            return ('SVC', pos[0][1], name)
        if fn is not None and fn[0] == 'TM':
            _, key, path, meth = fn
            base = ('T', key, path)
            if meth in ('get', 'Get') and pos and pos[0] is not None and pos[0][0] == 'C':
                self.ob(base, 'get[%s]' % const_repr(pos[0][1]))
                return self.child(base, '[%s]' % const_repr(pos[0][1]))
            if meth == 'has_key' and pos and pos[0] is not None and pos[0][0] == 'C':
                self.ob(base, 'contains[%s]' % const_repr(pos[0][1]))
                return None
            if meth in ('keys', 'iterkeys'):
                return self.child(base, '.keys()')
            if meth in ('values', 'itervalues'):
                return self.child(base, '.values()')
            if meth in ('items', 'iteritems'):
                return self.child(base, '.items()')
            if meth == 'copy':
                return base
            return None
        if fn is not None and fn[0] == 'P':
            b = fn[1]
            tracked = [a for a in pos if a is not None and a[0] == 'T']
            if b == 'len' and tracked:
                self.ob(tracked[0], 'len')
                return None
            if b in ITER_BUILTINS and tracked:
                self.ob(tracked[0], 'iter')
                if b in ('list', 'tuple', 'sorted', 'reversed', 'iter', 'set', 'frozenset'):
                    return tracked[0]
                if b == 'dict':
                    return tracked[0]
                if b == 'enumerate':
                    return None
                return None
            if b == 'bool' and tracked:
                self.ob(tracked[0], 'truthy')
                return None
            if b in ('int', 'float', 'long', 'abs', 'round') and tracked:
                self.ob(tracked[0], 'number')
                return None
            if b in ('str', 'unicode', 'repr') and tracked:
                self.ob(tracked[0], 'stringified')
                return None
            if b == 'isinstance' and tracked and len(pos) > 1:
                self.ob(tracked[0], 'isinstance:%s' % callee_label(pos[1]))
                return None
            if b in ('getattr', 'hasattr') and tracked and len(pos) > 1 and pos[1] is not None and pos[1][0] == 'C':
                self.ob(pos[0], '%s[%s]' % (b, const_repr(pos[1][1])))
                if b == 'getattr' and pos[0] is not None and pos[0][0] == 'T':
                    return self.child(pos[0], '.%s' % pos[1][1])
                return None
        # passthrough wrappers (pass 3)
        if self.mode == 3 and name in w.wrappers and fn is not None and fn[0] in ('A', 'SA', 'P', 'BM'):
            key = w.wrappers[name]
            self.sites.append({'wrapper_use': key, 'module': self.m, 'func': self.q, 'line': self.line(off),
                               'via_wrapper': name})
            return ('T', key, ())
        # factory functions returning a service
        if name in w.factories and fn is not None and fn[0] in ('A', 'SA', 'P'):
            return ('SVC', w.factories[name], 'factory:' + name)
        if name is not None and fn is not None and fn[0] in ('A', 'SA', 'P'):
            return ('CR', name)
        return None

    # ---- the interpreter
    def run(self):
        co = self.co
        st = []
        snaps = {}
        dead = False
        names, varnames = co.names, co.varnames
        cf = tuple(co.cellvars or ()) + tuple(co.freevars or ())
        consts = co.consts
        last_store_const = None

        def pop():
            return st.pop() if st else None

        for off, op, arg, nxt in instructions(co):
            if dead:
                st = list(snaps[off]) if off in snaps else []
                dead = False
            elif off in snaps and not st and snaps[off]:
                st = list(snaps[off])
            if op == 1:      # POP_TOP
                self.ob(pop(), 'discard')
            elif op == 2:
                if len(st) >= 2:
                    st[-1], st[-2] = st[-2], st[-1]
            elif op == 3:
                if len(st) >= 3:
                    st[-1], st[-2], st[-3] = st[-2], st[-3], st[-1]
            elif op == 5:
                if len(st) >= 4:
                    st[-1], st[-2], st[-3], st[-4] = st[-2], st[-3], st[-4], st[-1]
            elif op == 4:
                st.append(st[-1] if st else None)
            elif op == 99:   # DUP_TOPX
                st.extend(st[-arg:] if len(st) >= arg else [None] * arg)
            elif op in (9, 0, 87):
                pass
            elif op == 12:   # UNARY_NOT
                v = pop()
                self.ob(v, 'truthy')
                st.append(None)
            elif op in (10, 11, 13, 15):
                v = pop()
                self.ob(v, 'number' if op != 13 else 'stringified')
                st.append(None)
            elif op == 25:   # BINARY_SUBSCR
                idx = pop()
                base = pop()
                cidx = idx[1] if idx is not None and idx[0] == 'C' and isinstance(idx[1], int) else None
                if base is not None and base[0] == 'SEQ' and cidx is not None and -len(base[1]) <= cidx < len(base[1]):
                    st.append(base[1][cidx])
                elif base is not None and base[0] == 'SA' and cidx is not None:
                    sk = '%s[%d]' % (base[1], cidx)
                    sv = self.w.selfattr.get((self.m, sk))
                    st.append(sv if sv is not None else ('SA', sk))
                elif base is not None and base[0] == 'T':
                    k = const_repr(idx[1]) if idx is not None and idx[0] == 'C' else '*'
                    self.ob(base, 'subscript[%s]' % k)
                    st.append(self.child(base, '[%s]' % k))
                else:
                    st.append(None)
            elif op in BINOPS:
                b = pop()
                a = pop()
                for v in (a, b):
                    self.ob(v, 'arith')
                if op in (22, 59) and a is not None and a[0] == 'C' and isinstance(a[1], str):
                    self.ob(b, 'format_arg')
                st.append(None)
            elif 30 <= op <= 33:
                for _ in range({30: 0, 31: 1, 32: 1, 33: 2}[op]):
                    pop()
                v = pop()
                self.ob(v, 'slice')
                st.append(v if v is not None and v[0] == 'T' else None)
            elif 40 <= op <= 43:
                for _ in range({40: 2, 41: 3, 42: 3, 43: 4}[op]):
                    pop()
            elif 50 <= op <= 53:
                for _ in range({50: 1, 51: 2, 52: 2, 53: 3}[op]):
                    pop()
            elif op == 54:   # STORE_MAP
                k = pop()
                v = pop()
                self.ob(v, 'packed')
            elif op == 60:   # STORE_SUBSCR
                idx = pop()
                obj = pop()
                v = pop()
                self.ob(v, 'stored_subscr')
            elif op == 61:
                pop()
                pop()
            elif op == 68:   # GET_ITER
                v = pop()
                self.ob(v, 'iter')
                st.append(('ITER', v) if v is not None and v[0] == 'T' else None)
            elif op == 93:   # FOR_ITER
                it = st[-1] if st else None
                snaps.setdefault(nxt + arg, list(st[:-1]))
                if it is not None and it[0] == 'ITER':
                    st.append(self.child(it[1], '[]'))
                else:
                    st.append(None)
            elif op in (70, 71):
                pop()
            elif op == 73:
                pop()
                pop()
            elif op == 74:
                pop()
            elif op == 72:
                pass
            elif op == 80:   # BREAK_LOOP
                dead = True
            elif op == 81:   # WITH_CLEANUP
                pop()
            elif op == 82:
                st.append(None)
            elif op == 83:   # RETURN_VALUE
                v = pop()
                self.ob(v, 'returned')
                self.returns.append(v)
                dead = True
            elif op == 84:
                pop()
            elif op == 85:
                pop()
                pop()
                pop()
            elif op == 86:
                v = pop()
                self.ob(v, 'yielded')
                st.append(None)
            elif op == 88:   # END_FINALLY
                pop()
            elif op == 89:
                pop()
                pop()
                pop()
                st.append(None)
            elif op in (90, 97):   # STORE_NAME / STORE_GLOBAL
                v = pop()
                nm = names[arg]
                self.ob(v, 'stored_global')
                if v is not None and v[0] in ('SVC', 'CR'):
                    self.globstores.append((nm, v))
                self.const_store(nm, v, off)
            elif op in (91, 98, 126, 96 - 1000):
                pass
            elif op == 92:   # UNPACK_SEQUENCE
                v = pop()
                if v is not None and v[0] == 'T':
                    self.ob(v, 'unpack:%d' % arg)
                    for i in reversed(range(arg)):
                        st.append(self.child(v, '[%d]' % i))
                else:
                    st.extend([None] * arg)
            elif op == 94:
                self.ob(pop(), 'collected')
            elif op == 95:   # STORE_ATTR
                obj = pop()
                v = pop()
                nm = names[arg]
                if not (v is not None and v[0] == 'C' and v[1] is None):
                    self.attrstores.append((nm, v))
                if obj is not None and obj[0] == 'SELF':
                    if v is not None and v[0] in ('SVC', 'CR', 'SA'):
                        self.selfstores.append((nm, v))
                    if v is not None and v[0] == 'SEQ':
                        for i, e in enumerate(v[1]):
                            if e is not None and e[0] in ('SVC', 'CR', 'SA'):
                                self.selfstores.append(('%s[%d]' % (nm, i), e))
                    if v is not None and v[0] == 'T' and not v[2]:
                        self.selfstores.append((nm, v))
                self.ob(v, 'stored_attr:%s' % nm)
                self.const_store(nm, v, off)
            elif op == 96:
                pop()
            elif op == 100:
                c = consts[arg]
                st.append(('C', c) if not isinstance(c, CR.Code) else None)
            elif op in (101, 116):   # LOAD_NAME / LOAD_GLOBAL
                nm = names[arg]
                g = self.w.globals.get((self.m, nm))
                st.append(g if g is not None else ('P', nm))
            elif op in (102, 103, 104):
                el = [pop() for _ in range(arg)][::-1]
                for v in el:
                    self.ob(v, 'packed')
                if op != 104 and any(v is not None and v[0] in ('SVC', 'CR', 'SA') for v in el):
                    st.append(('SEQ', tuple(el)))
                else:
                    st.append(None)
            elif op == 105:
                st.append(None)
            elif op == 106:  # LOAD_ATTR
                v = pop()
                nm = names[arg]
                if v is None or v[0] in ('A', 'CR', 'SA', 'TM', 'ITER', 'SEQ', 'C') and nm in self.w.attrnames:
                    st.append(self.w.attrnames[nm] if nm in self.w.attrnames else ('A', nm))
                elif v[0] == 'P':
                    st.append(('P', v[1] + '.' + nm))
                elif v[0] == 'SELF':
                    sv = self.w.selfattr.get((self.m, nm))
                    if sv is None and self.mode == 3 and (self.m, nm) in self.w.selfattr_ret:
                        sv = ('T', self.w.selfattr_ret[(self.m, nm)], ())
                    if sv is None and nm in self.w.attrnames:
                        sv = self.w.attrnames[nm]
                    st.append(sv if sv is not None else ('SA', nm))
                elif v[0] == 'SVC':
                    st.append(('BM', v[1], nm, v[2]))
                elif v[0] == 'T':
                    if nm in TRACKED_METHODS:
                        self.ob(v, 'method:%s' % nm)
                        st.append(('TM', v[1], v[2], nm))
                    else:
                        self.ob(v, 'attr:%s' % nm)
                        st.append(self.child(v, '.%s' % nm))
                elif v[0] == 'CR' and is_rpc_method(nm):
                    # unresolved factory result: try late resolution
                    s = self.w.factories.get(v[1])
                    st.append(('BM', s, nm, 'factory:' + v[1]) if s else ('A', nm))
                else:
                    st.append(('A', nm))
            elif op == 107:  # COMPARE_OP
                b = pop()
                a = pop()
                opn = CMP[arg] if arg < len(CMP) else '?'
                if opn in ('in', 'not in') and b is not None and b[0] == 'T':
                    k = const_repr(a[1]) if a is not None and a[0] == 'C' else '*'
                    self.ob(b, 'contains[%s]' % k)
                else:
                    for x, y in ((a, b), (b, a)):
                        if x is not None and x[0] == 'T':
                            if y is not None and y[0] == 'C':
                                self.ob(x, 'compare%s%s' % (opn.replace(' ', '_'), const_repr(y[1])))
                            elif y is not None and y[0] == 'P':
                                self.ob(x, 'compare%s%s' % (opn.replace(' ', '_'), y[1]))
                            else:
                                self.ob(x, 'compare%s' % opn.replace(' ', '_'))
                st.append(None)
            elif op == 108:  # IMPORT_NAME
                pop()
                pop()
                st.append(('P', names[arg]))
            elif op == 109:  # IMPORT_FROM
                st.append(('P', names[arg]))
            elif op in (110, 113):   # JUMP_FORWARD / JUMP_ABSOLUTE
                tgt = nxt + arg if op == 110 else arg
                if tgt > off:
                    snaps.setdefault(tgt, list(st))
                dead = True
            elif op in (111, 112):
                v = st[-1] if st else None
                self.ob(v, 'truthy')
                snaps.setdefault(arg, list(st))
                pop()
            elif op in (114, 115):
                v = pop()
                self.ob(v, 'truthy')
                snaps.setdefault(arg, list(st))
            elif op == 119:
                dead = True
            elif op == 120:
                snaps.setdefault(nxt + arg, list(st))
            elif op == 121:
                snaps.setdefault(nxt + arg, list(st) + [None, None, None])
            elif op == 122:
                snaps.setdefault(nxt + arg, list(st) + [None])
            elif op == 143:  # SETUP_WITH
                v = pop()
                self.ob(v, 'context_manager')
                st.append(None)
                snaps.setdefault(nxt + arg, list(st) + [None])
                st.append(None)
            elif op == 124:  # LOAD_FAST
                nm = varnames[arg]
                if nm in self.locals:
                    st.append(self.locals[nm])
                elif self.is_method and arg == 0:
                    st.append(('SELF',))
                else:
                    st.append(None)
            elif op == 125:
                self.locals[varnames[arg]] = pop()
            elif op == 130:
                for _ in range(arg):
                    pop()
                dead = True
            elif op in (131, 140, 141, 142):
                na, nk = arg & 0xff, (arg >> 8) & 0xff
                star = op in (140, 142)
                starstar = op in (141, 142)
                if starstar:
                    self.ob(pop(), 'passed_kwargs')
                if star:
                    self.ob(pop(), 'passed_varargs')
                kw = {}
                for _ in range(nk):
                    v = pop()
                    k = pop()
                    kw[k[1] if k is not None and k[0] == 'C' else '?'] = v
                pos = [pop() for _ in range(na)][::-1]
                fn = pop()
                for i, a in enumerate(pos):
                    if a is not None and a[0] == 'BM' and is_rpc_method(a[2]):
                        # bound RPC method handed to a dispatcher (uthread.new, PerformSelectiveSessionChange,..)
                        self.sites.append({'kind': 'callback:%s' % callee_label(fn), 'svc': a[1], 'method': a[2],
                                           'module': self.m, 'func': self.q, 'line': self.line(off),
                                           'args': len(pos) - i - 1, 'kwargs': sorted(kw), 'via': a[3],
                                           'key': '%s|%s|%d|cb%d' % (self.m, self.q, off, i), 'star': star,
                                           'starstar': starstar,
                                           'arg_consts': [const_repr(x[1]) if x is not None and x[0] == 'C' else None
                                                          for x in pos[i + 1:]]})
                nsites = len(self.sites)
                r = self.resolve_call(fn, pos, list(kw), off)
                if len(self.sites) > nsites and 'svc' in self.sites[-1]:
                    self.sites[-1]['star'] = star
                    self.sites[-1]['starstar'] = starstar
                lab = callee_label(fn)
                if not (fn is not None and fn[0] == 'P' and fn[1] in ITER_BUILTINS | {'len', 'bool', 'int', 'float', 'long', 'str', 'unicode', 'repr', 'isinstance', 'getattr', 'hasattr', 'abs', 'round'}) \
                        and not (fn is not None and fn[0] == 'TM'):
                    for i, a in enumerate(pos):
                        self.ob(a, 'passed_to:%s#%d' % (lab, i))
                    for k, a in kw.items():
                        self.ob(a, 'passed_to:%s(%s=)' % (lab, k))
                st.append(r)
            elif op in (132, 134):
                pop()
                if op == 134:
                    pop()
                for _ in range(arg):
                    pop()
                st.append(None)
            elif op == 133:
                for _ in range(arg):
                    pop()
                st.append(None)
            elif op == 135:
                st.append(None)
            elif op == 136:
                st.append(self.derefs.get(cf[arg]) if arg < len(cf) else None)
            elif op == 137:
                v = pop()
                if arg < len(cf):
                    self.derefs[cf[arg]] = v
            elif op == 146:
                self.ob(pop(), 'collected')
            elif op == 147:
                pop()
                self.ob(pop(), 'collected')
            else:
                raise ValueError('unknown opcode %d at %s:%s+%d' % (op, self.m, self.q, off))
        return self

    def const_store(self, nm, v, off):
        if self.mode != 3 or v is None or v[0] != 'C':
            return
        val = v[1]
        if isinstance(val, bool) or not isinstance(val, (int, float)):
            return
        ws = set(words(nm))
        hit = sorted(ws & CONST_WORDS)
        if hit:
            wl = words(nm)
            kind = 'id_like' if wl and wl[0] in ID_PREFIXES else 'rule_candidate'
            self.consts.append({'module': self.m, 'scope': self.q, 'name': nm, 'value': val,
                                'line': self.line(off), 'matched': hit, 'kind': kind})


def load_all(ccp):
    z = zipfile.ZipFile(ccp)
    mods = []
    for n in sorted(z.namelist()):
        try:
            co = CR.load(z.read(n))
        except Exception as e:  # noqa: BLE001
            mods.append((n, None, repr(e)))
            continue
        if isinstance(co, CR.Code):
            mods.append((n, co, None))
        else:
            mods.append((n, None, 'not a code object'))
    return mods


def iter_code(mods):
    for n, co, err in mods:
        if co is None:
            continue
        for q, c, parent in walk_named(co, '<module>'):
            yield n, q, c, parent


def pass1(world, mods):
    """Summaries -> factories / self-attr / global service bindings (fixpoint)."""
    rets = collections.defaultdict(list)     # terminal func name -> [descriptor]
    selfs = collections.defaultdict(list)    # (module, attr) -> [descriptor]
    globs = collections.defaultdict(list)
    anames = collections.defaultdict(list)
    for n, q, c, parent in iter_code(mods):
        it = Interp(world, n, q, c, 1).run()
        if c.name not in ('<module>',) and not c.name.startswith('<'):
            descs = []
            for v in it.returns:
                if v is None:
                    descs.append(None)
                elif v[0] == 'C' and v[1] is None:
                    continue
                elif v[0] == 'SVC':
                    descs.append(('SVC', v[1]))
                elif v[0] == 'CR':
                    descs.append(('CR', v[1]))
                elif v[0] == 'SA':
                    descs.append(('SA', n, v[1]))
                elif v[0] == 'A':
                    descs.append(('AN', v[1]))
                else:
                    descs.append(None)
            rets[c.name].append(descs)
        for a, v in it.attrstores:
            if v is None:
                anames[a].append(None)
            elif v[0] == 'SVC':
                anames[a].append(('SVC', v[1]))
            elif v[0] == 'CR':
                anames[a].append(('CR', v[1]))
            elif v[0] == 'SA':
                anames[a].append(('SA', n, v[1]))
            else:
                anames[a].append(None)
        for a, v in it.selfstores:
            if v[0] == 'SVC':
                selfs[(n, a)].append(('SVC', v[1]))
            elif v[0] == 'CR':
                selfs[(n, a)].append(('CR', v[1]))
            elif v[0] == 'SA':
                selfs[(n, a)].append(('SA', n, v[1]))
        for a, v in it.globstores:
            if v[0] == 'SVC':
                globs[(n, a)].append(('SVC', v[1]))
            elif v[0] == 'CR':
                globs[(n, a)].append(('CR', v[1]))

    fac, sa, an = {}, {}, {}

    def res(d):
        if d is None:
            return None
        if d[0] == 'SVC':
            return d[1]
        if d[0] == 'CR':
            return fac.get(d[1])
        if d[0] == 'SA':
            return sa.get((d[1], d[2])) or an.get(d[2])
        if d[0] == 'AN':
            return an.get(d[1])
        return None

    def fac_candidate(name, strict):
        svcs = set()
        for descs in rets[name]:
            if not descs:
                return None
            for d in descs:
                if d is None:
                    return None
                if d[0] == 'CR' and d[1] == name:
                    continue
                s = res(d)
                if s is None:
                    if strict:
                        return None
                    continue
                svcs.add(s)
        return next(iter(svcs)) if len(svcs) == 1 else None

    # Optimistic fixpoint (unresolved descriptors ignored, so mutual recursion through self.attr caches
    # converges), followed by strict verification; names failing verification are blacklisted and the
    # whole fixpoint is recomputed until stable.
    black = set()
    for _round in range(20):
        fac.clear(); sa.clear(); an.clear()
        for _ in range(20):
            changed = False
            for name in sorted(rets):
                if name in FACTORY_STOP or name in black:
                    continue
                s = fac_candidate(name, False)
                if s is not None and fac.get(name) != s:
                    fac[name] = s
                    changed = True
            for key in sorted(selfs):
                svcs = set(res(d) for d in selfs[key])
                svcs.discard(None)
                if len(svcs) == 1:
                    s = next(iter(svcs))
                    if sa.get(key) != s:
                        sa[key] = s
                        changed = True
            for a in sorted(anames):
                svcs = set(res(d) for d in anames[a])
                if None in svcs or len(svcs) != 1 or not a:
                    continue
                s = next(iter(svcs))
                if an.get(a) != s:
                    an[a] = s
                    changed = True
            if not changed:
                break
        bad = sorted(n for n in fac if fac_candidate(n, True) != fac[n])
        if not bad:
            break
        black.update(bad)
    world.attrnames = {k: ('SVC', v, 'attr:' + k) for k, v in an.items()}
    world.factories = fac
    world.selfattr = {k: ('SVC', v, 'self.' + k[1]) for k, v in sa.items()}
    gl = {}
    for key in sorted(globs):
        svcs = set(res(d) for d in globs[key])
        svcs.discard(None)
        if len(svcs) == 1:
            gl[key] = ('SVC', next(iter(svcs)), 'global:' + key[1])
    world.globals = gl


def pass2(world, mods):
    """Sites + passthrough wrappers."""
    wrap = collections.defaultdict(list)
    defs = collections.Counter()
    sret = collections.defaultdict(set)
    for n, q, c, parent in iter_code(mods):
        it = Interp(world, n, q, c, 2).run()
        defs[c.name] += 1
        rs = [v for v in it.returns if not (v is not None and v[0] == 'C' and v[1] is None)]
        if rs and all(v is not None and v[0] == 'T' and not v[2] for v in rs):
            keys = set(v[1] for v in rs)
            wrap[c.name].append(keys)
        for a, v in it.selfstores:
            if v[0] == 'T':
                sret[(n, a)].add(v[1])
    site_rpc = {}
    for n, q, c, parent in iter_code(mods):
        pass
    world.wrappers = {}
    for name in sorted(wrap):
        if name in FACTORY_STOP or name.startswith('<') or len(wrap[name]) != defs[name]:
            continue
        allk = set().union(*wrap[name])
        if len(allk) == 1:
            world.wrappers[name] = next(iter(allk))
    world.selfattr_ret = {k: next(iter(v)) for k, v in sret.items() if len(v) == 1}


def notify_classes(mods):
    """(module, classqual, classcode, [event names]) for every class body that assigns __notifyevents__."""
    out = []
    for n, q, c, parent in iter_code(mods):
        if 'notifyevents' not in ' '.join(c.names):
            continue
        pending = []
        for off, op, arg, nxt in instructions(c):
            if op == 100:
                v = c.consts[arg]
                if isinstance(v, str):
                    pending.append(v)
                elif isinstance(v, tuple):
                    pending.extend(x for x in v if isinstance(x, str))
            elif op in (90, 97, 95):
                nm = c.names[arg]
                if nm == '__notifyevents__':
                    evs = sorted(set(x for x in pending if re.match(r'^[A-Za-z_][A-Za-z0-9_]*$', x)))
                    out.append((n, q, c, evs))
                pending = []
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--ccp', default=CR.CCP)
    ap.add_argument('--out', default=os.path.join(HERE, 'client_expectations.json'))
    a = ap.parse_args()
    mods = load_all(a.ccp)
    world = World()
    pass1(world, mods)
    pass2(world, mods)

    # notify handler seeds
    ncls = notify_classes(mods)
    handler_codes = {}   # (module, qual) -> event
    notify_index = collections.defaultdict(list)
    for n, q, c, evs in ncls:
        kids = {k.name: k for k in c.consts if isinstance(k, CR.Code)}
        for ev in evs:
            notify_index[ev].append((n, q, kids.get(ev)))
            if ev in kids:
                hq = kids[ev].name if c.name == '<module>' else '%s.%s' % (q, ev)
                handler_codes[(n, hq)] = ev

    sites, obs, consts, wrapper_uses = [], collections.defaultdict(set), [], []
    nobs = {}
    sess = collections.defaultdict(set)
    errors = []
    for n, q, c, parent in iter_code(mods):
        seeds = {}
        hk = (n, q)
        argn = list(c.varnames[:c.argcount + (1 if c.flags & 4 else 0) + (1 if c.flags & 8 else 0)])
        if hk in handler_codes or c.name in SESSION_HANDLERS:
            for i, p in enumerate(argn):
                if i == 0 and p == 'self':
                    continue
                seeds[p] = ('T', 'N|%s|%s|%s' % (n, q, p), ())
        try:
            it = Interp(world, n, q, c, 3, seeds).run()
        except Exception as e:  # noqa: BLE001
            errors.append('%s:%s %r' % (n, q, e))
            continue
        for s in it.sites:
            if 'svc' in s:
                sites.append(s)
            else:
                wrapper_uses.append(s)
        for k, v in it.obs.items():
            if k.startswith('N|'):
                nobs.setdefault(k, set()).update(v)
            else:
                obs[k].update(v)
        consts.extend(it.consts)
        if c.name in SESSION_HANDLERS:
            for p in argn:
                k = 'N|%s|%s|%s' % (n, q, p)
                for path, use in it.obs.get(k, ()):
                    m = re.match(r'^(contains|get|subscript)\[(.*)\]$', use)
                    if m and path == '$' and m.group(2).startswith('"'):
                        sess[json.loads(m.group(2)) if m.group(2).startswith('"') else m.group(2)].add(
                            '%s:%s(%s)' % (n, q, p))

    # ---- assemble RPC section
    key_to_rpc = {s['key']: '%s.%s' % (s['svc'], s['method']) for s in sites}
    rpc = {}
    for s in sorted(sites, key=lambda s: (s['svc'], s['method'], s['module'], s['func'], s['line'], s['key'])):
        name = '%s.%s' % (s['svc'], s['method'])
        e = rpc.setdefault(name, {'call_sites': [], 'reply_shape': {}, 'wrappers': []})
        uses = sorted('%s %s' % (p, u) for p, u in obs.get(s['key'], ()))
        e['call_sites'].append({'kind': s['kind'], 'module': s['module'], 'func': s['func'], 'line': s['line'], 'args': s['args'],
                                'kwargs': s['kwargs'], 'star': s['star'], 'starstar': s['starstar'],
                                'via': s['via'], 'arg_consts': s['arg_consts'], 'uses': uses})
        for p, u in obs.get(s['key'], ()):
            e['reply_shape'].setdefault(p, set()).add(u)
    for wname, key in sorted(world.wrappers.items()):
        r = key_to_rpc.get(key)
        if r:
            rpc[r]['wrappers'].append(wname)
    for e in rpc.values():
        e['reply_shape'] = {p: sorted(v) for p, v in sorted(e['reply_shape'].items())}
        e['summary'] = summarize(e['reply_shape'])

    # ---- notify section
    notify = {}
    for ev in sorted(notify_index):
        ent = {'handlers': [], 'params': [], 'reads': [], 'unresolved_handler_in': []}
        params_seen = []
        reads = set()
        for n, q, code in sorted(notify_index[ev], key=lambda x: (x[0], x[1])):
            if code is None:
                ent['unresolved_handler_in'].append('%s:%s' % (n, q))
                continue
            hq = code.name if q == '<module>' else '%s.%s' % (q, ev)
            ent['handlers'].append('%s:%s' % (n, hq))
            ps = list(code.varnames[:code.argcount])
            if ps and ps[0] == 'self':
                ps = ps[1:]
            if code.flags & 4:
                ps.append('*' + code.varnames[code.argcount])
            if code.flags & 8:
                ps.append('**' + code.varnames[code.argcount + (1 if code.flags & 4 else 0)])
            params_seen.append(ps)
            argn = list(code.varnames[:code.argcount + (1 if code.flags & 4 else 0) + (1 if code.flags & 8 else 0)])
            idx = 0
            for p in argn:
                if p == 'self' and idx == 0:
                    idx += 1
                    continue
                pos = idx - (1 if argn and argn[0] == 'self' else 0)
                idx += 1
                for path, use in nobs.get('N|%s|%s|%s' % (n, hq, p), ()):
                    if use.startswith(('passed_to:', 'packed', 'collected', 'stored_')):
                        kind = 'forwarded'
                    else:
                        kind = 'read'
                    reads.add('arg%d(%s)%s %s%s' % (pos, p, '' if path == '$' else path, use,
                                                    '' if kind == 'read' else ''))
        # canonical params: most common arity, then lexicographic
        if params_seen:
            cnt = collections.Counter(tuple(p) for p in params_seen)
            ent['params'] = list(sorted(cnt.items(), key=lambda kv: (-kv[1], kv[0]))[0][0])
            ent['param_variants'] = sorted([list(p), c] for p, c in cnt.items())
            ent['arity_min'] = min(len([x for x in p if not x.startswith('*')]) for p in params_seen)
            ent['arity_max'] = max(len([x for x in p if not x.startswith('*')]) for p in params_seen)
        ent['reads'] = sorted(reads)
        notify[ev] = ent

    consts.sort(key=lambda d: (d['module'], d['scope'], d['line'], d['name'], repr(d['value'])))
    session_keys = [{'key': k, 'readers': sorted(v)} for k, v in sorted(sess.items(), key=lambda kv: str(kv[0]))]
    doc = {
        'meta': {'source': 'code.ccp (V24.01 client)', 'modules': len(mods),
                 'modules_failed': sorted('%s %s' % (n, e) for n, co, e in mods if co is None),
                 'interp_errors': sorted(errors),
                 'factories': dict(sorted(world.factories.items())),
                 'wrappers': {k: key_to_rpc.get(v, '?') for k, v in sorted(world.wrappers.items())},
                 'n_selfattr_bindings': len(world.selfattr), 'n_global_bindings': len(world.globals),
                 'attrname_bindings': {k: v[1] for k, v in sorted(world.attrnames.items())}},
        'rpc': rpc,
        'notify': notify,
        'constants': consts,
        'session_keys': session_keys,
    }
    with open(a.out, 'w', encoding='utf-8') as f:
        json.dump(doc, f, sort_keys=True, indent=1, ensure_ascii=True)
        f.write('\n')
    print('rpc=%d notify=%d consts=%d session_keys=%d errors=%d factories=%d wrappers=%d' % (
        len(rpc), len(notify), len(consts), len(session_keys), len(errors), len(world.factories),
        len(world.wrappers)))


def summarize(shape):
    """Compact human-readable expected reply shape from the observation tree."""
    root = shape.get('$', [])
    s = {}
    unp = sorted(set(u.split(':')[1] for u in root if u.startswith('unpack:')))
    if unp:
        s['tuple_arity'] = [int(x) for x in unp]
    attrs = sorted(p[1:] for p in shape if p.startswith('.') and p.count('.') == 1 and '[' not in p)
    if attrs:
        s['attrs'] = attrs
    keys = sorted(set(p for p in shape if re.match(r'^\[[^\]]*\]$', p) and p not in ('[]', '[*]')
                      and not re.match(r'^\[-?\d+\]$', p)))
    if keys:
        s['keys'] = keys
    if '[]' in shape:
        el = {}
        ea = sorted(p[3:] for p in shape if p.startswith('[].') and p.count('.') == 1 and '[' not in p[2:])
        if ea:
            el['attrs'] = ea
        eu = sorted(set(u.split(':')[1] for u in shape['[]'] if u.startswith('unpack:')))
        if eu:
            el['tuple_arity'] = [int(x) for x in eu]
        s['iterated_elements'] = el
    flags = []
    for u, f in (('iter', 'iterable'), ('len', 'sized'), ('truthy', 'truthiness'), ('discard', 'ignored'),
                 ('returned', 'returned_to_caller'), ('arith', 'numeric'), ('number', 'numeric'),
                 ('stringified', 'string')):
        if u in root and f not in flags:
            flags.append(f)
    if any(u.startswith('contains[') for u in root):
        flags.append('container')
    if any(u.startswith('compare') for u in root):
        flags.append('compared')
    if any(u.startswith('stored_attr:') for u in root):
        flags.append('stored_on_object')
    if any(u.startswith('passed_to:') for u in root):
        flags.append('passed_on')
    s['usage'] = flags
    return s


if __name__ == '__main__':
    main()
