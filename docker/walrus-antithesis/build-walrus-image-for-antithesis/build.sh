#!/bin/sh
# Copyright (c) Mysten Labs, Inc.
# SPDX-License-Identifier: Apache-2.0

# fast fail.
set -e

DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
DOCKERFILE="$DIR/Dockerfile"
GIT_REVISION="$(git describe --always --abbrev=12 --dirty --exclude '*')"
BUILD_DATE="$(date -u +'%Y-%m-%d')"

# option to build using debug symbols
if [ "$1" = "--debug-symbols" ]; then
  PROFILE="bench"
  echo "Building with full debug info enabled ... WARNING: binary size might significantly increase"
  shift
else
  PROFILE="release"
fi

echo
printf "Building '%s' docker images\n" "$WALRUS_IMAGE_NAME"
printf "Dockerfile: \t%s\n" "$DOCKERFILE"
printf "docker context: %s\n" "$REPO_ROOT"
printf "build date: \t%s\n" "$BUILD_DATE"
printf "git revision: \t%s\n" "$GIT_REVISION"
echo

docker build -f "$DOCKERFILE" "$REPO_ROOT" \
  --build-arg GIT_REVISION="$GIT_REVISION" \
  --build-arg BUILD_DATE="$BUILD_DATE" \
  --build-arg PROFILE="$PROFILE" \
  --platform linux/"$(uname -m)" \
  "$@"
