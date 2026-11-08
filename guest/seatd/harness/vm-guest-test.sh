#!/bin/sh
printf '\033[48;5;201m SEATD_VM_MARKER \033[0m\n'
echo VM_PROMPT_READY > /dev/console
if read -r line && [ "$line" = laputa ]; then
  echo VM_INPUT_OK > /dev/console
  sleep 120
else
  echo VM_INPUT_FAILED > /dev/console
  exit 1
fi
