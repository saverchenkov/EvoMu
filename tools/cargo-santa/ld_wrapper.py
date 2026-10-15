#!/usr/bin/python3
import os, sys, stat, subprocess

args = sys.argv[1:]
is_dylib = ("-dylib" in args) or ("-dynamiclib" in args) or any(a.endswith(".dylib") for a in args)

out_idx = None
for i, a in enumerate(args):
    if a == "-o" and i + 1 < len(args):
        out_idx = i + 1
        break

if is_dylib or out_idx is None:
    rc = subprocess.call(["/usr/bin/cc"] + args)
    sys.exit(rc)

orig_out = os.path.abspath(args[out_idx])
dylib_out = orig_out + ".santa.dylib"
args[out_idx] = dylib_out
args.append("-Wl,-dylib")

rc = subprocess.call(["/usr/bin/cc"] + args)
if rc != 0:
    sys.exit(rc)

trampoline = f"""#!/usr/bin/python3
import ctypes, os, sys

dylib_path = {repr(dylib_out)}
if not os.path.exists(dylib_path):
    dylib_path = os.path.abspath(__file__) + ".santa.dylib"

libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
libc._NSGetArgc.restype = ctypes.POINTER(ctypes.c_int)
libc._NSGetArgv.restype = ctypes.POINTER(ctypes.POINTER(ctypes.c_char_p))
libc.fflush.argtypes = [ctypes.c_void_p]

raw_args = [os.fsencode(a) for a in sys.argv]
argc = len(raw_args)
argv_type = ctypes.c_char_p * (argc + 1)
argv = argv_type(*raw_args, None)

libc._NSGetArgc().contents.value = argc
libc._NSGetArgv().contents = ctypes.cast(argv, ctypes.POINTER(ctypes.c_char_p))

lib = ctypes.CDLL(dylib_path)
lib.main.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_char_p)]
lib.main.restype = ctypes.c_int
rc = lib.main(argc, ctypes.cast(argv, ctypes.POINTER(ctypes.c_char_p)))
libc.fflush(None)
os._exit(rc)
"""

with open(orig_out, "w") as f:
    f.write(trampoline)
os.chmod(orig_out, 0o755)
