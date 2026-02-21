#!/bin/bash
./dev.sh 1101 &
./dev.sh 1102 &
./dev-chat-worker.sh &
./dev-chat-worker.sh &
wait