#!/bin/bash
cleanup() {
  kill -TERM -$$ 2>/dev/null
}

trap cleanup SIGINT SIGTERM

./dev.sh 1101 &
./dev.sh 1102 &
./dev-chat-worker.sh &
./dev-chat-worker.sh &

wait