-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Keep the installed runtime untouched and retain its screen restoration.
local source = assert(io.open(arg[1], "rb"))
local text = source:read("*a")
source:close()
local count
text, count = text:gsub('%./reader%.lua "%$@"', './luajit "$MOYUE_CENTER_ENTRY"')
assert(count == 1, "Unsupported KOReader launcher entry point")
text = text:gsub("\n([ \t]*)ko_update_check[ \t]*\n", "\n%1: # adaptation center skips reader updates\n")
text = text:gsub("crash%.log", '"${MOYUE_CENTER_LOG}"')
text = text:gsub("Starting KOReader", "Starting Moyue Center")
local output = assert(io.open(arg[2], "wb"))
assert(output:write(text))
output:close()
