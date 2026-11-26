import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    initialization = json.load(handle)
namespace = {}
exec(compile(initialization["source"], "<bar strategy>", "exec"), namespace)
on_bars = namespace["on_bars"]
params = initialization["params"]
state = {}
print("ready", flush=True)
for line in sys.stdin.buffer:
    at_ms, split, bars = json.loads(line)
    actions = on_bars(at_ms, split, bars, params, state)
    print(json.dumps([at_ms, actions], separators=(",", ":"), allow_nan=False), flush=True)
