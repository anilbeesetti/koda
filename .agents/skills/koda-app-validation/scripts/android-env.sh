#!/usr/bin/env bash
# Source this for Android project discovery, builds, and real Compose renders.
source "$(dirname -- "${BASH_SOURCE[0]}")/env.sh"

export JAVA_HOME="${JAVA_HOME:-$KODA_ENVIRONMENT_ROOT/jdk/jdk-21.0.12.1+1}"
export ANDROID_HOME="${ANDROID_HOME:-$KODA_ENVIRONMENT_ROOT/sdk}"
export GRADLE_USER_HOME="${GRADLE_USER_HOME:-$KODA_ENVIRONMENT_ROOT/gradle}"
export ANDROID_USER_HOME="${ANDROID_USER_HOME:-$KODA_ENVIRONMENT_ROOT/android-user}"
case ":$PATH:" in *":$JAVA_HOME/bin:"*) ;; *) export PATH="$JAVA_HOME/bin:$PATH" ;; esac
case ":$PATH:" in *":$KODA_ENVIRONMENT_ROOT/bin:"*) ;; *) export PATH="$KODA_ENVIRONMENT_ROOT/bin:$PATH" ;; esac

# Java does not consume the session's HTTP_PROXY or system CA bundle by default.
# Respect explicit Java settings and derive defaults from the public policy file.
if [[ "${JAVA_TOOL_OPTIONS:-}" != *-Dhttps.proxyHost=* ]]; then
    read -r koda_proxy_host koda_proxy_port < <(python3 - <<'PY'
import json
from urllib.parse import urlparse
from pathlib import Path
policy_path = Path('/etc/codex/network-policy.json')
proxy = urlparse(json.loads(policy_path.read_text()).get('http_proxy', '')) if policy_path.is_file() else urlparse('')
if proxy.hostname:
    print(proxy.hostname, proxy.port or (443 if proxy.scheme == 'https' else 80))
PY
    ) || true
    if test -n "${koda_proxy_host:-}"; then
        export JAVA_TOOL_OPTIONS="${JAVA_TOOL_OPTIONS:+$JAVA_TOOL_OPTIONS }-Dhttps.proxyHost=$koda_proxy_host -Dhttps.proxyPort=$koda_proxy_port -Dhttp.proxyHost=$koda_proxy_host -Dhttp.proxyPort=$koda_proxy_port"
    fi
    unset koda_proxy_host koda_proxy_port
fi
if [[ "${JAVA_TOOL_OPTIONS:-}" != *-Djavax.net.ssl.trustStore=* ]]; then
    export JAVA_TOOL_OPTIONS="${JAVA_TOOL_OPTIONS:+$JAVA_TOOL_OPTIONS }-Djavax.net.ssl.trustStore=$KODA_ENVIRONMENT_ROOT/java-cacerts -Djavax.net.ssl.trustStorePassword=changeit"
fi

# HOME is read-only here. These overrides apply only to the QA JVMs, not HOME.
mkdir -p "$KODA_ENVIRONMENT_ROOT/java-user" "$KODA_ENVIRONMENT_ROOT/java-prefs"
if [[ "${JAVA_TOOL_OPTIONS:-}" != *-Duser.home=* ]]; then
    export JAVA_TOOL_OPTIONS="${JAVA_TOOL_OPTIONS:+$JAVA_TOOL_OPTIONS }-Duser.home=$KODA_ENVIRONMENT_ROOT/java-user -Djava.util.prefs.userRoot=$KODA_ENVIRONMENT_ROOT/java-prefs"
fi
