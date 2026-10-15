#!/usr/bin/python3
import os, sys
args = sys.argv[1:]
if args and args[0].endswith("rustc"):
    rustc_bin = args[0]
    rest = args[1:]
else:
    rustc_bin = "/opt/homebrew/bin/rustc"
    rest = args
cmd = [rustc_bin, f"-Clinker=/Users/sergeya/ai/EvoMu/tools/cargo-santa/ld_wrapper.py"] + rest
os.execv(rustc_bin, cmd)
