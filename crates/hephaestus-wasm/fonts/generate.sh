#!/usr/bin/env bash
# Regenerate the bundled default faces. Run by hand when the font or the
# coverage changes; the results are committed, so `build.sh` and CI need
# neither network access nor fontTools.
#
# Requires: python fontTools (pip install fonttools brotli), curl.
#
# Roboto, OFL-1.1 (Google relicensed it from Apache-2.0; `ofl/roboto/` is the
# current home). No Reserved Font Name is declared, so a subsetted, instanced
# derivative may keep the family name — which matters, because the theme
# refers to it by name. OFL.txt travels with the faces, as the licence
# requires; it covers the fonts only, not the crate around them.
#
# Static instances rather than the variable font: `gvar`
# deltas survive charset subsetting, so a variable roman+italic pair at this
# coverage is ~1 MB against ~260 kB brotli for four static faces. The cost is
# that a theme asking for weight 500 snaps to 400 or 700 instead of
# interpolating; the built-in themes only use 400 and 700.
set -euo pipefail
cd "$(dirname "$0")"

REPO=https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/roboto
curl -sL -o /tmp/Roboto.ttf        "$REPO/Roboto%5Bwdth,wght%5D.ttf"
curl -sL -o /tmp/Roboto-Italic.ttf "$REPO/Roboto-Italic%5Bwdth,wght%5D.ttf"
curl -sL --fail -o OFL-Roboto.txt  "$REPO/OFL.txt"

# Google's own subset ranges, so coverage matches what a web font would give:
# latin, latin-ext, Greek, Cyrillic and Vietnamese. CJK is deliberately absent
# — a CJK face is megabytes and stays a bring-your-own case.
LATIN='U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+2074,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD'
LATIN_EXT='U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF'
GREEK='U+0370-0377,U+037A-037F,U+0384-038A,U+038C,U+038E-03A1,U+03A3-03FF'
CYRILLIC='U+0301,U+0400-045F,U+0490-0491,U+04B0-04B1,U+2116'
VIETNAMESE='U+0102-0103,U+0110-0111,U+0128-0129,U+0168-0169,U+01A0-01A1,U+01AF-01B0,U+0300-0301,U+0303-0304,U+0308-0309,U+0323,U+0329,U+1EA0-1EF9,U+20AB'
RANGES="$LATIN,$LATIN_EXT,$GREEK,$CYRILLIC,$VIETNAMESE"

emit () { # source, weight, output
  fonttools varLib.instancer -o /tmp/inst.ttf "$1" wght="$2" wdth=100 >/dev/null
  pyftsubset /tmp/inst.ttf --unicodes="$RANGES" --layout-features='*' --output-file="$3"
  printf "  %-26s %7d raw  %7d brotli\n" "$3" \
    "$(wc -c < "$3" | tr -d ' ')" "$(brotli -q 11 -c "$3" | wc -c | tr -d ' ')"
}

echo "generating:"
emit /tmp/Roboto.ttf        400 roboto-regular.ttf
emit /tmp/Roboto.ttf        700 roboto-bold.ttf
emit /tmp/Roboto-Italic.ttf 400 roboto-italic.ttf
emit /tmp/Roboto-Italic.ttf 700 roboto-bolditalic.ttf
