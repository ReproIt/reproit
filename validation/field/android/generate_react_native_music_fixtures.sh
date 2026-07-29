#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTPUT_DIRECTORY" >&2
    exit 2
fi

readonly OUTPUT_DIRECTORY=$1
readonly EXPECTED_FFMPEG_VERSION="ffmpeg version 7.1.5"

actual_ffmpeg_version=$(ffmpeg -version | awk 'NR == 1 { version = $0 } END { print version }')
if [[ $actual_ffmpeg_version != "$EXPECTED_FFMPEG_VERSION "* ]]; then
    echo "expected $EXPECTED_FFMPEG_VERSION, got: $actual_ffmpeg_version" >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIRECTORY"

generate_fixture() {
    local output=$1
    local frequency_hz=$2
    local title=$3
    local album=$4
    local track=$5
    local year=${6-}
    local -a year_metadata=()

    if [[ -n $year ]]; then
        year_metadata=(-metadata "date=$year")
    fi

    ffmpeg \
        -hide_banner \
        -loglevel error \
        -f lavfi \
        -i "sine=frequency=${frequency_hz}:duration=20" \
        -c:a libmp3lame \
        -b:a 64k \
        -metadata "title=$title" \
        -metadata "artist=Reproit Field Artist" \
        -metadata "album_artist=Reproit Field Artist" \
        -metadata "album=$album" \
        -metadata "track=$track" \
        "${year_metadata[@]}" \
        -y \
        "$OUTPUT_DIRECTORY/$output"
}

generate_fixture \
    field-year.mp3 \
    220 \
    "Field Year Track" \
    "Reproit Field Album" \
    1 \
    2024
generate_fixture \
    field-none.mp3 \
    330 \
    "Field No Year Track" \
    "Reproit Field Album" \
    2
generate_fixture \
    control-alpha.mp3 \
    440 \
    "Control Alpha Track" \
    "Reproit Control Alpha" \
    3 \
    2021
generate_fixture \
    control-beta.mp3 \
    550 \
    "Control Beta Track" \
    "Reproit Control Beta" \
    4 \
    2022

(
    cd "$OUTPUT_DIRECTORY"
    sha256sum --check <<'HASHES'
c3a008ba731f0e6b8b4d98b7723f02ede6711e98a2efb2fc150adc51d5a9b8aa  control-alpha.mp3
f0d456463d434d33fb726f140a72d6f35b4dd5bb9bd28d948d36220190c06b9d  control-beta.mp3
a184188fc41ac4a6babcdb15c9621cb082071547c863bb9abaed19266e285889  field-none.mp3
c211fb7935b9eeca663752b22fb12966a5812bd2a32e18cd12a23485d8861bee  field-year.mp3
HASHES
)
