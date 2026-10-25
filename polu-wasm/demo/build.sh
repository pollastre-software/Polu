#!/bin/sh
# Builds the demo site into one folder: this page, polu-wasm, highs-js (npm `highs`, MIT: the
# official HiGHS compiled to WebAssembly) and every scenario in demo-scenarios/. The Pages workflow
# runs the same script, so what you serve locally is what gets deployed.
#
#   sh polu-wasm/demo/build.sh [out]        # default out: target/demo-site
#   python3 -m http.server -d target/demo-site 8000
#
# Needs wasm-pack, curl, tar and shasum or sha256sum. highs-js is downloaded once into
# target/highs-js-<version>; to upgrade it, change both lines below (the .tgz's SHA-256).
set -eu
HIGHS_VERSION=1.15.4
HIGHS_SHA256=a4a0b519778019c0ac31a66358348e4361afc5576576cad782e606633281def1

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/demo-site}
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$root"

# the page
rm -rf "$out/scenarios" "$out/pkg" "$out/vendor"
cp polu-wasm/demo/index.html polu-wasm/demo/main.js polu-wasm/demo/style.css "$out/"

# polu-wasm
wasm-pack build polu-wasm --target web --release --out-dir "$out/pkg"
rm -f "$out/pkg/.gitignore"

# highs-js, checked against the pinned checksum
highs=target/highs-js-$HIGHS_VERSION
if [ ! -f "$highs/highs.wasm" ]; then
  mkdir -p "$highs"
  curl -fsSL -o "$highs/highs.tgz" "https://registry.npmjs.org/highs/-/highs-$HIGHS_VERSION.tgz"
  if command -v shasum > /dev/null; then sum="shasum -a 256"; else sum=sha256sum; fi
  if [ "$($sum "$highs/highs.tgz" | cut -d' ' -f1)" != "$HIGHS_SHA256" ]; then
    rm "$highs/highs.tgz"
    echo "highs-$HIGHS_VERSION.tgz doesn't match its pinned checksum" >&2
    exit 1
  fi
  tar -xzf "$highs/highs.tgz" -C "$highs" --strip-components=2 package/build/highs.mjs package/build/highs.wasm
  tar -xzf "$highs/highs.tgz" -O package/LICENSE > "$highs/LICENSE"
  rm "$highs/highs.tgz"
fi
mkdir -p "$out/vendor"
cp "$highs/highs.mjs" "$highs/highs.wasm" "$out/vendor/"
cp "$highs/LICENSE" "$out/vendor/HIGHS-JS-LICENSE"

# scenarios: one folder each, holding its files and one *.polu.json; the page reads the index
mkdir -p "$out/scenarios"
index=""
for file in demo-scenarios/*/*.polu.json; do
  [ -f "$file" ] || continue
  dir=$(dirname "$file")
  name=$(basename "$dir")
  cp -R "$dir" "$out/scenarios/"
  index="$index${index:+,}
  {\"name\": \"$name\", \"scenario\": \"$name/$(basename "$file")\"}"
done
printf '[%s\n]\n' "$index" > "$out/scenarios/index.json"

echo "demo site is in $out"
