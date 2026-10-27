#!/bin/zsh
# usage: drive.sh <seconds> — drives a running datatable-spike through
# orca computer: page through rows, step a header sort, page back.
# Coordinates are window points for the default window size.
app=datatable-spike; end=$(( $(date +%s) + $1 ))
o() { orca computer "$@" --app $app --no-screenshot --json >/dev/null 2>&1; }
o click --x 150 --y 144
hdr=(270 400 560 650 740 840 930)
i=0
while [ $(date +%s) -lt $end ]; do
  for k in 1 2 3 4 5 6 7 8; do o press-key --key Page_Down; done
  o click --x ${hdr[$(( i % 7 + 1 ))]} --y 80
  for k in 1 2 3 4; do o press-key --key Page_Up; done
  for k in 1 2 3 4 5 6; do o press-key --key Down; done
  i=$((i+1))
done
echo "drove $i cycles"
