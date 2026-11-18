#!/bin/sh
set -eu

source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
destination=$1
identity=${2:--}

mkdir -p "$destination/Contents/MacOS" "$destination/Contents/Resources"
cp "$source_dir/Info.plist" "$destination/Contents/Info.plist"
cp "$source_dir/Auram.icns" "$destination/Contents/Resources/Auram.icns"
cp "$source_dir/../../../assets/icon.png" "$destination/Contents/Resources/Auram.png"
/usr/bin/xcrun clang -arch arm64 -mmacosx-version-min=12.0 -O2 -Wall -Wextra -Werror \
    -framework AppKit "$source_dir/launcher.m" -o "$destination/Contents/MacOS/Auram"
if [ "$identity" = "-" ]; then
    timestamp=--timestamp=none
else
    timestamp=--timestamp
fi
/usr/bin/codesign --force --sign "$identity" "$timestamp" --options runtime \
    --entitlements "$source_dir/Entitlements.plist" "$destination"
