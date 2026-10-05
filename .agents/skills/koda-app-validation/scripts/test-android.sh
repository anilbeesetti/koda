#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/env.sh"
cd "$KODA_REPOSITORY"

# Debug asset discovery needs the executable beneath the checkout even though
# Cargo's large build cache is outside it. Resolve artifacts from Cargo's JSON,
# rather than retaining an obsolete executable hash after features change.
artifact_directory="$KODA_REPOSITORY/target/cloud-tests"
mkdir -p "$artifact_directory"
artifact_log=$(mktemp "$artifact_directory/artifacts.XXXXXX.jsonl")
trap 'rm -f "$artifact_log"' EXIT
preview_features=()
bundled_preview_available=$(python3 - <<'PY'
import tomllib
with open('crates/android_tools/Cargo.toml', 'rb') as manifest:
    print('bundled-preview' in tomllib.load(manifest).get('features', {}))
PY
)
if [[ "$bundled_preview_available" == True ]]; then
    preview_features=(--features bundled-preview)
fi
cargo test --locked -p android_tools "${preview_features[@]}" --lib --no-run --message-format=json-render-diagnostics > "$artifact_log"
cargo test --locked -p android_ui --lib --no-run --message-format=json-render-diagnostics >> "$artifact_log"
mapfile -t test_executables < <(python3 - "$artifact_log" <<'PY'
import json, sys
executables = {}
for line in open(sys.argv[1]):
    message = json.loads(line)
    if message.get('reason') == 'compiler-artifact' and message.get('profile', {}).get('test') and message.get('executable'):
        name = message['target']['name']
        if name in ('android_ui', 'android_tools'):
            executables[name] = message['executable']
for name in ('android_tools', 'android_ui'):
    if name not in executables:
        sys.exit(f'Missing test executable for {name}')
    print(executables[name])
PY
)
test "${#test_executables[@]}" -eq 2
for executable in "${test_executables[@]}"; do
    local_executable="$artifact_directory/$(basename "$executable")"
    rm -f "$local_executable"
    ln "$executable" "$local_executable" || cp "$executable" "$local_executable"
    "$local_executable" "$@"
done
