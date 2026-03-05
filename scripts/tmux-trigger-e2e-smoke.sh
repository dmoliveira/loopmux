#!/usr/bin/env bash
set -euo pipefail

session_name="${LOOPMUX_SMOKE_SESSION:-loopmux-trigger-smoke}"
target="${LOOPMUX_SMOKE_TARGET:-}"
token="${LOOPMUX_SMOKE_TOKEN:-<NEXT-CONTACT>}"
trigger_file="${LOOPMUX_SMOKE_TRIGGER_FILE:-/tmp/loopmux-trigger-smoke-input.log}"
log_file="${LOOPMUX_SMOKE_LOG:-/tmp/loopmux-trigger-smoke.log}"
capture_file="${LOOPMUX_SMOKE_CAPTURE:-/tmp/loopmux-trigger-smoke-capture.log}"
send_marker="${LOOPMUX_SMOKE_PROMPT:-LOOPMUX_TRIGGER_SMOKE_SEND}"
iterations="${LOOPMUX_SMOKE_ITERATIONS:-3}"
emit_count="${LOOPMUX_SMOKE_EMIT_COUNT:-6}"
poll_seconds="${LOOPMUX_SMOKE_POLL:-1}"

cleanup() {
	tmux kill-session -t "${session_name}" >/dev/null 2>&1 || true
	rm -f "${trigger_file}" "${capture_file}"
}
trap cleanup EXIT

tmux kill-session -t "${session_name}" >/dev/null 2>&1 || true
tmux new-session -d -s "${session_name}" -x 120 -y 30 "cat"
sleep 1

if [ -z "${target}" ]; then
	target="$(tmux list-panes -t "${session_name}" -F '#S:#I.#P' | awk 'NR==1 {print; exit}')"
fi

if [ -z "${target}" ]; then
	printf "smoke failed: unable to determine target for %s\n" "${session_name}" >&2
	exit 1
fi

printf "smoke target: %s\n" "${target}"
printf "smoke token: %s\n" "${token}"

: >"${trigger_file}"

(
	i=1
	while [ "${i}" -le "${emit_count}" ]; do
		printf "%s\nseq=%s\n" "${token}" "${i}" >>"${trigger_file}"
		sleep 1
		i=$((i + 1))
	done
) &
emitter_pid=$!

CI=true cargo run -- run \
	-t "${target}" \
	--file "${trigger_file}" \
	-n "${iterations}" \
	--tail 4 \
	--prompt "${send_marker}" \
	--trigger "${token}" \
	--trigger-exact-line \
	--trigger-confirm-seconds 0 \
	--no-trigger-edge \
	--no-recheck-before-send \
	--poll "${poll_seconds}" \
	--initial-poll 1 \
	--name smoke-trigger-e2e \
	>"${log_file}" 2>&1

wait "${emitter_pid}" || true

tmux capture-pane -pt "${target}" -S -200 >"${capture_file}"

send_count="$(
	CAPTURE_FILE="${capture_file}" SEND_MARKER="${send_marker}" python3 - <<'PY'
from pathlib import Path
import os
capture = Path(os.environ['CAPTURE_FILE']).read_text()
marker = os.environ['SEND_MARKER']
print(capture.count(marker))
PY
)"

if [ "${send_count}" -lt "${iterations}" ]; then
	printf "smoke failed: expected at least %s sends but saw %s\n" "${iterations}" "${send_count}" >&2
	printf "log file: %s\n" "${log_file}" >&2
	printf "capture file: %s\n" "${capture_file}" >&2
	exit 1
fi

printf "smoke ok: observed %s sends (expected >= %s)\n" "${send_count}" "${iterations}"
