#!/bin/bash
clear

export PORT=${1:-1101}

PID=$(lsof -ti :$PORT)
if [ -n "$PID" ]; then
    echo "Killing process on port $PORT: $PID"
    kill -9 $PID
fi

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" cargo watch -w src -x run -s 'clear'
