#!/usr/bin/env bash
set -euo pipefail

session_name="${LOOPMUX_SMOKE_SESSION:-loopmux-smoke}"
target="${LOOPMUX_SMOKE_TARGET:-}"
prompt="${LOOPMUX_SMOKE_PROMPT:-echo LOOPMUX_SMOKE_SENT}"
log_file="${LOOPMUX_SMOKE_LOG:-/tmp/loopmux-post-release-smoke.log}"

cleanup() {
	tmux kill-session -t "${session_name}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

tmux kill-session -t "${session_name}" >/dev/null 2>&1 || true
tmux new-session -d -s "${session_name}" -x 120 -y 30
sleep 1

if [ -z "${target}" ]; then
	target="$(tmux list-panes -t "${session_name}" -F '#S:#I.#P' | awk 'NR==1 {print; exit}')"
fi

if [ -z "${target}" ]; then
	printf "smoke failed: unable to determine tmux target for session %s\n" "${session_name}" >&2
	exit 1
fi

printf "smoke target: %s\n" "${target}"

CI=true cargo run -- run \
	-t "${target}" \
	-n 1 \
	--prompt "${prompt}" \
	--trigger ".*" \
	--once \
	--no-trigger-edge \
	--trigger-confirm-seconds 0 \
	>"${log_file}" 2>&1

tmux capture-pane -pt "${target}" -S -120

if grep -q "LOOPMUX_SMOKE_SENT" "${log_file}"; then
	printf "smoke ok: found LOOPMUX_SMOKE_SENT in %s\n" "${log_file}"
else
	printf "smoke failed: LOOPMUX_SMOKE_SENT not found in %s\n" "${log_file}" >&2
	exit 1
fi
