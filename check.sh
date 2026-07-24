#! /bin/bash

shopt -s globstar nullglob extglob

# print shellcheck version so we know what Github uses
shellcheck -V

# SC1091 - Don't complain about not being able to find files that don't exist.
shellcheck --exclude=SC1091 \
           ./setup/pi/setup-dashusb \
           ./pi-gen-sources/00-dashusb-tweaks/files/rc.local \
           ./pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary \
           ./run/archiveloop \
           ./run/auto.dashusb \
           ./run/awake_start \
           ./run/awake_stop \
           ./run/mountimage \
           ./run/mountoptsforimage \
           ./run/remountfs_rw \
           ./run/send-push-message \
           ./run/temperature_monitor \
           ./run/waitforidle
