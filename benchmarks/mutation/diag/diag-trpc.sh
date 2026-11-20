#!/bin/bash
DIAG_OUT=${DIAG_OUT:-/tmp} exec bash "$(dirname "$0")/trpc-withping.sh"
