#!/usr/bin/env bash
# Regenerate compat/workspace-plans.json: what the installed Maestro's
# WorkspaceExecutionPlanner decides for each case in workspace-cases.json.
# Needs kotlinc and java.
set -euo pipefail
cd "$(dirname "$0")"
LIB="${MAESTRO_LIB:-$(dirname "$(dirname "$(python3 -c 'import os,shutil;print(os.path.realpath(shutil.which("maestro")))')")")/lib}"
CP=$(ls "$LIB"/*.jar | tr '\n' ':')
mkdir -p harness/build
if [ ! -f harness/build/workspace.jar ] || [ harness/WorkspacePlanDump.kt -nt harness/build/workspace.jar ]; then
  kotlinc harness/WorkspacePlanDump.kt -cp "$CP" -Xskip-metadata-version-check -d harness/build/workspace.jar 2>&1 | grep -v '^warning' || true
fi
java -cp "harness/build/workspace.jar:$CP" WorkspacePlanDumpKt \
  maestro-fixtures/maestro-orchestra/workspaces workspace-cases.json workspace-plans.json 2>/dev/null
