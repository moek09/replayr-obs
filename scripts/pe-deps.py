#!/usr/bin/env python3
"""Lists the DLLs a set of PE files needs, resolved recursively inside one folder.

usage: pe-deps.py <search-dir> <file> [<file>...]

Reads the import tables (normal and delay-load) of each file; names found in
<search-dir> (case-insensitive) are followed recursively, other names (system
DLLs such as KERNEL32.dll) are ignored. Prints the found file names, one per
line, sorted. Needs nothing but the Python standard library, so it also runs
on the Windows runner.
"""
import os
import struct
import sys


def imports(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:2] != b"MZ":
        raise SystemExit(f"{path}: not a PE file")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise SystemExit(f"{path}: no PE signature")
    nsec, = struct.unpack_from("<H", data, pe + 6)
    optsize, = struct.unpack_from("<H", data, pe + 20)
    opt = pe + 24
    magic, = struct.unpack_from("<H", data, opt)
    dd = opt + (112 if magic == 0x20B else 96)  # data directories
    sections = []
    sec = opt + optsize
    for i in range(nsec):
        vsize, va, rawsize, rawptr = struct.unpack_from("<IIII", data, sec + i * 40 + 8)
        sections.append((va, max(vsize, rawsize), rawptr))

    def off(rva):
        for va, size, ptr in sections:
            if va <= rva < va + size:
                return rva - va + ptr
        raise SystemExit(f"{path}: bad RVA {rva:#x}")

    def cstr(rva):
        o = off(rva)
        return data[o:data.index(b"\0", o)].decode("ascii", "replace")

    names = []
    # Import directory (index 1): 20-byte descriptors, name RVA at +12.
    rva, size = struct.unpack_from("<II", data, dd + 8)
    if rva:
        o = off(rva)
        while True:
            name_rva, = struct.unpack_from("<I", data, o + 12)
            if name_rva == 0:
                break
            names.append(cstr(name_rva))
            o += 20
    # Delay-load directory (index 13): 32-byte descriptors, name RVA at +4.
    rva, size = struct.unpack_from("<II", data, dd + 13 * 8)
    if rva:
        o = off(rva)
        while True:
            attrs, name_rva = struct.unpack_from("<II", data, o)[0:2]
            if name_rva == 0:
                break
            names.append(cstr(name_rva))
            o += 32
    return names


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    search = sys.argv[1]
    have = {n.lower(): n for n in os.listdir(search)}
    found = set()
    queue = [os.path.abspath(p) for p in sys.argv[2:]]
    while queue:
        path = queue.pop()
        for dep in imports(path):
            real = have.get(dep.lower())
            if real and real not in found:
                found.add(real)
                queue.append(os.path.join(search, real))
    for name in sorted(found):
        print(name)


if __name__ == "__main__":
    main()
