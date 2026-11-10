#!/bin/sh
# Start the stock account run on GitHub when it is due, from outside GitHub's cron.
#
# GitHub's schedule is best effort and can drop runs, but starting a run through the API
# (workflow_dispatch) is immediate. This script is meant to run every 30 minutes (see
# scripts/install-trigger.sh). Each time it asks: has a run already started, or finished
# successfully, since the latest weekday session settled at 16:30 New York time? If not,
# it starts one. It tries at most three times per session, so a broken run cannot loop all
# night, and the run itself skips when the account is already current, so a duplicate
# costs nothing. macOS only (BSD date); needs the GitHub CLI logged in (`gh auth login`).
set -eu

REPO="${PAPER_TRADER_REPO:-mansoor-mamnoon/paper-trader}"
GH="${GH:-$(command -v gh || echo /opt/homebrew/bin/gh)}"
MAX_TRIES="${PAPER_TRADER_MAX_TRIES:-3}"
NY=America/New_York

stamp() { date -u +%Y-%m-%dT%H:%M:%SZ; }
now="${PAPER_TRADER_NOW:-$(date +%s)}" # epoch seconds; overridable for testing

# The latest weekday 16:30 ET at or before now.
settled=""
back=0
while [ "$back" -le 7 ]; do
  day=$(TZ=$NY date -j -v-"${back}"d -f %s "$now" +%Y-%m-%d)
  weekday=$(TZ=$NY date -j -f %Y-%m-%d "$day" +%u)
  if [ "$weekday" -le 5 ]; then
    at=$(TZ=$NY date -j -f "%Y-%m-%d %H:%M:%S" "$day 16:30:00" +%s)
    if [ "$at" -le "$now" ]; then
      settled=$at
      break
    fi
  fi
  back=$((back + 1))
done
[ -n "$settled" ] || { echo "$(stamp) could not find the last session"; exit 1; }

# Runs of the account workflow created after that session settled: how many are queued,
# running or succeeded ("alive"), and how many there are in all ("tries").
since=$(date -u -r "$settled" +%Y-%m-%dT%H:%M:%SZ)
filter='[.[] | select(.createdAt >= "'"$since"'")]
  | "\([.[] | select(.status != "completed" or .conclusion == "success")] | length) \(length)"'
counts=$("$GH" run list --repo "$REPO" --workflow account.yml --limit 20 \
  --json createdAt,status,conclusion --jq "$filter")
alive=${counts% *}
tries=${counts#* }

if [ "$alive" -gt 0 ]; then
  echo "$(stamp) up to date: a run since $since is queued, running or succeeded"
  exit 0
fi
if [ "$tries" -ge "$MAX_TRIES" ]; then
  echo "$(stamp) $tries runs since $since all failed; not starting another until the next session"
  exit 0
fi
"$GH" workflow run account.yml --repo "$REPO"
echo "$(stamp) started a run for the session that settled at $since (attempt $((tries + 1)))"
