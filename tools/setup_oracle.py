#!/usr/bin/env python3
"""Build the task-owned i386 runtime used by run_oracle.py.

Runtime recipe adapted from openvcl/docs/sce-vcl/setup.sh (read solely for
emulation setup). No openvcl implementation, instruction data, or output is used.
An existing compatible Qiling virtualenv can be reused with --venv-source; the
runtime packages can be supplied with --package-cache to avoid network access.
"""

import argparse
import hashlib
import pathlib
import shutil
import subprocess
import sys
import tempfile
import urllib.request


PACKAGES = (
    ("pool/main/g/glibc/libc6_2.31-0ubuntu9_i386.deb", "f3508da9bcd7b052df8e30d6c82902655809b68f8210edbc907b3fdc99d9b0ab"),
    ("pool/universe/g/gcc-3.3/libstdc++5_3.3.6-30ubuntu2_i386.deb", "cfb0e2db802c863026ec32dcac2edcbe76a509853c9a91cae500ad1f7af1dda0"),
    ("pool/main/g/gcc-10/libgcc-s1_10-20200411-0ubuntu1_i386.deb", "c2b7a0c246b4bb41d2a3fe00f440737c6fd78764fb19daadbbbfdaf6967d0ba0"),
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--home", type=pathlib.Path, default=pathlib.Path("local/analysis/runtime"))
    parser.add_argument("--package-cache", type=pathlib.Path)
    parser.add_argument("--venv-source", type=pathlib.Path)
    args = parser.parse_args()
    home = args.home.resolve()
    home.mkdir(parents=True, exist_ok=True)
    rootfs = home / "rootfs"
    for part in ("bin", "work", "tmp"):
        (rootfs / part).mkdir(parents=True, exist_ok=True)
    packages = home / "debs"
    packages.mkdir(exist_ok=True)
    for remote, expected in PACKAGES:
        package = packages / pathlib.Path(remote).name
        if not package.exists():
            cached = args.package_cache / package.name if args.package_cache else None
            if cached and cached.exists():
                shutil.copy2(cached, package)
            else:
                urllib.request.urlretrieve("https://archive.ubuntu.com/ubuntu/" + remote, package)
        actual = hashlib.sha256(package.read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit(f"SHA256 mismatch: {package}: {actual}")
        with tempfile.TemporaryDirectory(dir=home) as directory:
            subprocess.run(["ar", "x", str(package)], cwd=directory, check=True)
            archive = next(pathlib.Path(directory).glob("data.tar.*"))
            subprocess.run(["tar", "-xf", str(archive), "-C", str(rootfs)], check=True)
    libdir = rootfs / "lib/i386-linux-gnu"
    shutil.copy2(rootfs / "usr/lib/i386-linux-gnu/libstdc++.so.5.0.7", libdir)
    link = libdir / "libstdc++.so.5"
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to("libstdc++.so.5.0.7")
    venv = home / "venv"
    if not venv.exists():
        if args.venv_source:
            shutil.copytree(args.venv_source, venv, symlinks=True)
        else:
            subprocess.run([sys.executable, "-m", "venv", str(venv)], check=True)
            subprocess.run([str(venv / "bin/python"), "-m", "pip", "install", "qiling==1.4.6", "unicorn==2.1.4", "setuptools"], check=True)
    # Qiling imports Keystone but only needs its constants while executing ELF.
    # Keystone's macOS native library does not support the installed Python.
    keystone = next(venv.glob("lib/python*/site-packages/keystone/__init__.py"))
    if "keystone stub" not in keystone.read_text():
        shutil.copy2(keystone, keystone.with_suffix(".py.original"))
        keystone.write_text('''# keystone stub: Qiling execution never invokes the assembler.
from .keystone_const import *
from .x86_const import *
from .arm_const import *
from .arm64_const import *
from .mips_const import *
from .ppc_const import *
__version__ = "0.0-stub"
class KsError(Exception): pass
class Ks:
    def __init__(self, arch, mode): self.arch, self.mode = arch, mode
    def asm(self, *args, **kwargs): raise KsError("assembler unavailable")
def ks_version(): return (0, 0)
def ks_arch_supported(arch): return False
def version_bind(): return (0, 0)
def debug(): return "stub"
''')
    print(home)


if __name__ == "__main__":
    main()
