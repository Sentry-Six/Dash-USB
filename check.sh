#! /bin/bash

shopt -s globstar nullglob extglob

# print shellcheck version so we know what Github uses
shellcheck -V

# SC1091 - Don't complain about not being able to find files that don't exist.
shellcheck --exclude=SC1091 \
           ./pi-gen-sources/00-dashusb-tweaks/files/rc.local \
           ./pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary \
           ./run/archiveloop \
           ./run/remountfs_rw \
           ./run/send-push-message \
           ./run/temperature_monitor \
           ./run/waitforidle
