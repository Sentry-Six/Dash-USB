#! /bin/bash

shopt -s globstar nullglob extglob

# Print the version so the CI log records which shellcheck GitHub ran.
shellcheck -V

# SC1091: don't flag sourced files that can't be resolved at lint time.
shellcheck --exclude=SC1091 \
           ./pi-gen-sources/00-dashusb-tweaks/files/rc.local \
           ./pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary \
           ./run/archiveloop \
           ./run/remountfs_rw \
           ./run/send-push-message \
           ./run/temperature_monitor \
           ./run/waitforidle
