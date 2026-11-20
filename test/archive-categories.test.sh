#!/usr/bin/env bash
# Run the shipped category selection: every category the vehicle records is
# archived unless ARCHIVE_CATEGORIES names a subset.
set -euo pipefail
cd "$(dirname "$0")/.."
eval "$(awk '$0 ~ "^function archive_selected_categories([ (]|$)" {keep=1} keep {print} keep && /^}/ {exit}' run/archiveloop)"
selected() { archive_selected_categories | tr '\n' ' '; }
expect() {
  local got
  got=$(selected)
  if [[ "$got" != "$1" ]]; then
    printf 'archive categories: expected "%s", got "%s" (%s)\n' "$1" "$got" "$2" >&2
    exit 1
  fi
}

# A vehicle with only continuous recordings and no selection: as before.
unset ARCHIVE_CATEGORIES RECORDING_EVENT_CATEGORIES
expect 'Continuous ' 'no event categories, no selection'
RECORDING_EVENT_CATEGORIES=''
expect 'Continuous ' 'empty event category list'

# Event categories archive by default, in profile order.
RECORDING_EVENT_CATEGORIES='Incidents Saved GearGuard'
expect 'Continuous Incidents Saved GearGuard ' 'no selection archives everything'

# A selection limits archiving to the named categories, whatever its order.
ARCHIVE_CATEGORIES='Saved,Continuous'
expect 'Continuous Saved ' 'subset including continuous'
ARCHIVE_CATEGORIES='Incidents'
expect 'Incidents ' 'events only'

# Names must match whole categories; ones this vehicle lacks are ignored.
ARCHIVE_CATEGORIES='Save,Incident,GearGuardX,Launch'
expect '' 'partial and foreign names select nothing'
ARCHIVE_CATEGORIES='Continuous,*'
expect 'Continuous ' 'a glob is not a category'

# An empty value is no selection, the way the setup wizard reads it:
# ARCHIVE_RECORDINGS=false is what turns archiving off.
ARCHIVE_CATEGORIES=''
expect 'Continuous Incidents Saved GearGuard ' 'empty value archives everything'

echo 'archive categories: default, subsets, unknown names and empty value passed'
