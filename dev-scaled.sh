#!/bin/bash
cleanup() {
  kill -TERM -$$ 2>/dev/null
}

trap cleanup SIGINT SIGTERM

./dev.sh 1101 &
./dev.sh 1102 no-clippy &
./dev-chat-worker.sh &
./dev-chat-worker.sh &
./dev-unit-technical-reviewer.sh &
./dev-unit-technical-reviewer.sh &

wait