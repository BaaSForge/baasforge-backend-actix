#!/bin/bash

# Function to print the directory tree recursively
tree_print() {
  local dir="$1"
  local indent="$2"

  # Print the current directory name with indentation
  echo "$indent$dir"

  # Iterate through all items (files and directories) in the current directory
  for item in "$dir"/*; do
    # Check if the item is a directory
    if [ -d "$item" ]; then
      # Recursively call tree_print for the subdirectory, adding more indentation
      tree_print "$item" "$indent    "  # Four spaces for subdirectory indent
    else
      # If it's not a directory, print the file name with indentation
      echo "$indent    $item"  # Four spaces for file indent
    fi
  done
}

# Get the directory to start from (default to current directory if none provided)
if [ -z "$1" ]; then
  start_dir="./src"
else
  start_dir="$1"
fi

# Check if the start directory exists
if [ ! -d "$start_dir" ]; then
  echo "Error: Directory '$start_dir' does not exist."
  exit 1
fi

# Call the tree_print function to start printing from the specified directory
tree_print "$start_dir" ""  # Start with no initial indentation
