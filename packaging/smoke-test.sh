#!/bin/sh
# Release smoke test: run the built CLI and level a 5-second test tone into every output
# format, so a missing or broken encoder fails the build. Used by .github/workflows/release.yml.
#
# Usage: packaging/smoke-test.sh <path/to/leveler>
set -e
cli=$1
work=$(mktemp -d)
"$cli" --version
py=$(command -v python3 || command -v python)   # Windows runners call it python
"$py" - "$work/tone.wav" <<'PY'
import math, struct, sys, wave
w = wave.open(sys.argv[1], "wb")
w.setnchannels(2); w.setsampwidth(2); w.setframerate(44100)
w.writeframes(b"".join(struct.pack("<hh", v, v) for v in
              (int(8000 * math.sin(2 * math.pi * 440 * i / 44100)) for i in range(5 * 44100))))
w.close()
PY
for f in flac wave mp3 ogg aac opus; do
    "$cli" "$work/tone.wav" -o "$work/$f" -f "$f"
done
ls -lR "$work"
rm -rf "$work"
