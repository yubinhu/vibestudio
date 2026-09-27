#!/bin/sh
# Deterministic terminal output for the real Codex detector; no AI credentials.
if [ "${1:-}" = --version ]; then
  printf 'codex 0.0.0\n'
  exit 0
fi

# Capture the actual launch recipe so history tests can prove exact-ID resume.
printf '%s\n' "$@" > .e2e-agent-args

frame=0
while :; do
  state=working
  if [ -f .e2e-agent-state ]; then
    IFS= read -r state < .e2e-agent-state
  fi
  case "$state" in
    exit) exit 0 ;;
    working) title='⠋ E2E session' ;;
    blocked) title='Action Required' ;;
    *) title='E2E session' ;;
  esac
  # Idle agents also repaint: terminal activity must not mean "Working".
  printf '\033]2;%s\007\033[H\033[2KFixture %s, frame %s\n' "$title" "$state" "$frame"
  frame=$((frame + 1))
  sleep 0.1
done
