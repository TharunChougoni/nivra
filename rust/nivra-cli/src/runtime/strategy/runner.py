import io
import json
import sys
import time


def main():
    if len(sys.argv) < 4:
        sys.stderr.write("usage: runner.py <strategy.py> <params.json> <generation> [site_packages]\n")
        sys.exit(2)

    strategy_path = sys.argv[1]
    params_path = sys.argv[2]
    generation = int(sys.argv[3])
    if len(sys.argv) >= 5 and sys.argv[4]:
        sys.path.append(sys.argv[4])

    with open(strategy_path, "rb") as f:
        source_bytes = f.read()
    with open(params_path, "r", encoding="utf-8") as f:
        params = json.load(f)

    proto_in = sys.stdin
    proto_out = sys.stdout
    sink = io.StringIO()
    sys.stdin = io.StringIO("")
    sys.stdout = sink

    ns = {"__name__": "strategy_script"}
    code = compile(source_bytes, strategy_path, "exec")
    exec(code, ns)

    on_observation = ns.get("on_observation")
    if not callable(on_observation):
        proto_out.write(
            json.dumps(
                {
                    "v": 1,
                    "type": "error",
                    "generation": generation,
                    "error": "strategy_missing_on_observation",
                }
            )
            + "\n"
        )
        proto_out.flush()
        sys.exit(2)

    on_start = ns.get("on_start")
    if callable(on_start):
        on_start({"params": params})

    proto_out.write(
        json.dumps(
            {
                "v": 1,
                "type": "ready",
                "abi": "strategy_actions_v1",
                "generation": generation,
            }
        )
        + "\n"
    )
    proto_out.flush()

    for raw_line in proto_in:
        if not raw_line:
            break
        req = json.loads(raw_line)
        if (
            not isinstance(req, dict)
            or req.get("v") != 1
            or req.get("generation") != generation
            or type(req.get("seq")) is not int
            or "observation" not in req
        ):
            proto_out.write(
                json.dumps(
                    {
                        "v": 1,
                        "type": "error",
                        "generation": generation,
                        "seq": req.get("seq") if isinstance(req, dict) else None,
                        "error": "strategy_protocol_invalid",
                    }
                )
                + "\n"
            )
            proto_out.flush()
            sys.exit(3)

        sink.seek(0)
        sink.truncate(0)
        t0 = time.monotonic_ns()
        result = on_observation(req["observation"])
        elapsed_us = (time.monotonic_ns() - t0) // 1000
        payload = {"actions": result} if isinstance(result, list) else result
        try:
            line = json.dumps(
                {
                    "v": 1,
                    "type": "result",
                    "generation": generation,
                    "seq": req["seq"],
                    "output": payload,
                    "elapsed_us": elapsed_us,
                },
                allow_nan=False,
            )
        except ValueError:
            line = json.dumps(
                {
                    "v": 1,
                    "type": "error",
                    "generation": generation,
                    "seq": req["seq"],
                    "error": "strategy_output_non_finite",
                }
            )
        proto_out.write(line + "\n")
        proto_out.flush()


if __name__ == "__main__":
    main()
