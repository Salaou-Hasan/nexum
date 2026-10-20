#!/bin/bash
# Show the tail of the probe's output on each platform: the step where the
# sequence stops is where the failure is.
cd /mnt/c/Users/Dell/AppData/Local/Temp
for job in ubuntu-latest macos-latest; do
  echo "=== $job ==="
  gh run view "$1" --log 2>/dev/null \
    | grep "build-and-test ($job)" \
    | grep -E "class [0-9]+ |done|mmap ->|open ->|write ->|size_class" \
    | sed -e 's/\x1b\[[0-9;]*m//g' \
    | sed -e 's/^[^ ]* *Test[^ ]* *//' \
    | tail -12
done
