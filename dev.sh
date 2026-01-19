#!/bin/bash

# Run the application and capture stdout and stderr
output=$(cargo watch -w src -x run)

# Print the output to the terminal
echo "$output"

# Optionally, save the output to a file
# echo "$output" > output.log
