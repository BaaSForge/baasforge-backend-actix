#!/bin/bash
clear

PID=$(lsof -ti :1100)
if [ -n "$PID" ]; then
    echo "Killing process on port 1100: $PID"
    kill -9 $PID
fi

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" cargo watch -w src -x run -s 'clear'
