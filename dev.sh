#!/bin/bash
clear
CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" cargo watch -w src -x run -s 'clear'
