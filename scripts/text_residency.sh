#!/bin/sh
# Usage: scripts/text_residency.sh ./binary [args...]
#
# Reports, at the program's exit, the resident size of every mapping and how
# much of it is the binary's own text. A compiled Gossamer program's resident
# floor is dominated by the pages the loader faults in around its entry point,
# so "how much of my own text is resident" is the number that moves when the
# startup path is reordered or trimmed.
#
# Needs gdb. Prints one RSS line per mapping, largest first, then a TEXT line
# with the binary's own executable residency.
set -eu

bin="$1"
shift

gdb -batch \
    -ex 'catch syscall exit_group' \
    -ex 'run > /dev/null' \
    -ex 'python
import gdb, os
pid = gdb.selected_inferior().pid
target = os.path.basename("'"$bin"'")
name = None
perm = None
rng = None
rows = []
own_text = 0
for line in open(f"/proc/{pid}/smaps"):
    parts = line.split()
    if len(parts) >= 5 and "-" in parts[0] and ":" not in parts[0]:
        rng, perm = parts[0], parts[1]
        name = parts[5] if len(parts) > 5 else "[anon]"
    elif parts and parts[0] == "Rss:":
        kb = int(parts[1])
        if kb:
            rows.append((kb, os.path.basename(name), perm, rng))
            if os.path.basename(name) == target and "x" in perm:
                own_text += kb
rows.sort(reverse=True)
print("TOTAL %d KB" % sum(r[0] for r in rows))
for kb, nm, pm, rg in rows:
    print("RSS %6d KB  %s  %s  %s" % (kb, pm, rg, nm))
print("TEXT %d KB resident of the binary own executable mappings" % own_text)
' --args "$bin" "$@" 2>&1 | grep -E '^(TOTAL|RSS|TEXT) '
