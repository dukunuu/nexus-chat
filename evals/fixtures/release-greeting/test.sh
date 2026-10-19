#!/bin/sh
set -eu
actual=$(sh greet.sh Ada)
expected='Hi, Ada!'
if [ "$actual" != "$expected" ]; then
    printf 'expected: %s\nactual: %s\n' "$expected" "$actual"
    exit 1
fi
printf 'greeting test passed\n'
