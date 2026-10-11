#!/bin/sh
# LightVM deterministic test fixture (ADR-0019). ASCII only; run with `sh`.
#
#   sh fx.sh mostra   hides the cursor, clears the screen and prints the band
#                     B1 B2 B3 B4 (12x3 cells each, 24-bit colours) and below
#                     it B5 (36x3 cells): yellow (250,250,6) if the first 11
#                     bytes of /dev/vda are LIGHTVM-FX1, cyan (6,250,250)
#                     otherwise.
#   sh fx.sh scrivi   writes LIGHTVM-FX1 at the start of /dev/vda (the
#                     overlay of the run), then `mostra`.
#
# B4 is computed by commands with a known output: (6*7, 200+13, 3^4) =
# (42, 213, 81). Deviation from ADR-0019: 3^4 is written $((3*3*3*3))
# because `sh` (dash) has no `**` operator.

W12='            '
W36='                                    '

blk() {
    printf '\033[48;2;%s;%s;%sm%s\033[0m' "$1" "$2" "$3" "$4"
}

mostra() {
    r4=$((6*7))
    g4=$((200+13))
    b4=$((3*3*3*3))
    m=$(sudo head -c 11 /dev/vda 2>/dev/null)
    if [ "$m" = "LIGHTVM-FX1" ]; then
        r5=250; g5=250; b5=6
    else
        r5=6; g5=250; b5=250
    fi
    printf '\033[?25l\033[2J\033[H'
    for _ in 1 2 3; do
        blk 1 254 3 "$W12"
        blk 251 2 249 "$W12"
        blk 4 5 250 "$W12"
        blk "$r4" "$g4" "$b4" "$W12"
        printf '\n'
    done
    for _ in 1 2 3; do
        blk "$r5" "$g5" "$b5" "$W36"
        printf '\n'
    done
}

scrivi() {
    printf 'LIGHTVM-FX1' | sudo dd of=/dev/vda bs=512 count=1 conv=fsync status=none
    mostra
}

case "${1:-}" in
    mostra) mostra ;;
    scrivi) scrivi ;;
    *) echo "usage: sh fx.sh mostra|scrivi" >&2; exit 2 ;;
esac
