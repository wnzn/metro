#!/bin/sh
set -eu

cd "$(dirname "$0")"
test "$(./greet.sh)" = 'Hello, Metro!'
test "$(./greet.sh 'Build team')" = 'Hello, Build team!'
printf '%s\n' 'Greeting checks passed.'
