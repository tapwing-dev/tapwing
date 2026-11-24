#!/usr/bin/env bash
# Regenerate compat/maestro-ir from compat/maestro-fixtures with the installed
# Maestro (its jars are the reference parser). Needs kotlinc and java.
set -euo pipefail
cd "$(dirname "$0")"
LIB="${MAESTRO_LIB:-$(dirname "$(dirname "$(python3 -c 'import os,shutil;print(os.path.realpath(shutil.which("maestro")))')")")/lib}"
CP=$(ls "$LIB"/*.jar | tr '\n' ':')
mkdir -p harness/build
if [ ! -f harness/build/harness.jar ] || [ harness/MaestroIrDump.kt -nt harness/build/harness.jar ]; then
  kotlinc harness/MaestroIrDump.kt -cp "$CP" -Xskip-metadata-version-check -d harness/build/harness.jar 2>&1 | grep -v '^warning' || true
fi
rm -rf maestro-ir probes-ir
java -cp "harness/build/harness.jar:$CP" MaestroIrDumpKt maestro-fixtures maestro-ir 2>/dev/null
java -cp "harness/build/harness.jar:$CP" MaestroIrDumpKt probes probes-ir 2>/dev/null
